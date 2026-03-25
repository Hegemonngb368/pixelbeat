use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};

/// A single track from a YouTube playlist
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct YtTrack {
    pub video_id: String,
    pub title: String,
    pub duration: f64,
}

const MPV_SOCKET: &str = "/tmp/pixelbeat-mpv.sock";

/// Check if yt-dlp is installed
pub fn is_ytdlp_available() -> bool {
    Command::new("which")
        .arg("yt-dlp")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if mpv is installed
pub fn is_mpv_available() -> bool {
    Command::new("which")
        .arg("mpv")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Fetch all tracks from a YouTube playlist/video URL using yt-dlp
pub fn fetch_playlist(url: &str) -> Result<Vec<YtTrack>> {
    if !is_ytdlp_available() {
        anyhow::bail!("yt-dlp is not installed. Install with: brew install yt-dlp");
    }

    eprintln!("pixelbeat: fetching YouTube playlist info...");

    let output = Command::new("yt-dlp")
        .args([
            "--flat-playlist",
            "--dump-json",
            "--no-warnings",
            "--quiet",
            url,
        ])
        .output()
        .context("Failed to run yt-dlp")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("yt-dlp failed: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut tracks = Vec::new();

    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: serde_json::Value = serde_json::from_str(line)
            .with_context(|| "Failed to parse yt-dlp JSON")?;

        let video_id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if video_id.is_empty() {
            continue;
        }

        let title = entry.get("title").and_then(|v| v.as_str()).unwrap_or("Unknown").to_string();
        let duration = entry.get("duration").and_then(|v| v.as_f64()).unwrap_or(0.0);

        tracks.push(YtTrack { video_id, title, duration });
    }

    eprintln!("pixelbeat: found {} tracks in playlist", tracks.len());
    Ok(tracks)
}

/// mpv process handle for streaming YouTube audio
pub struct MpvPlayer {
    process: Option<Child>,
}

impl MpvPlayer {
    pub fn new() -> Self {
        Self { process: None }
    }

    /// Start streaming a YouTube URL via mpv (instant playback, no download)
    pub fn play_url(&mut self, url: &str, volume: f32) -> Result<()> {
        if !is_mpv_available() {
            anyhow::bail!("mpv is not installed. Install with: brew install mpv");
        }

        // Kill existing mpv process
        self.stop();

        // Clean up stale socket
        std::fs::remove_file(MPV_SOCKET).ok();

        let vol = (volume * 100.0) as u32;

        eprintln!("pixelbeat: streaming via mpv...");

        let child = Command::new("mpv")
            .args([
                "--no-video",
                "--ytdl-format=bestaudio",
                &format!("--volume={}", vol),
                "--really-quiet",
                &format!("--input-ipc-server={}", MPV_SOCKET),
                "--cookies-from-browser=chrome",
                url,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("Failed to spawn mpv")?;

        self.process = Some(child);

        // Wait for IPC socket to appear
        for _ in 0..30 {
            if std::path::Path::new(MPV_SOCKET).exists() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        Ok(())
    }

    /// Send a JSON command to mpv via IPC
    fn send_command(&self, cmd: &serde_json::Value) -> Result<serde_json::Value> {
        let mut stream = UnixStream::connect(MPV_SOCKET)
            .context("Cannot connect to mpv IPC socket")?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).ok();

        let mut cmd_str = serde_json::to_string(cmd)?;
        cmd_str.push('\n');
        stream.write_all(cmd_str.as_bytes())?;

        let reader = BufReader::new(&stream);
        for line in reader.lines() {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&line) {
                // Skip event messages, wait for command response
                if resp.get("error").is_some() {
                    return Ok(resp);
                }
            }
        }
        anyhow::bail!("No response from mpv")
    }

    /// Get a property from mpv (time-pos, duration, media-title, pause, etc.)
    pub fn get_property(&self, prop: &str) -> Option<serde_json::Value> {
        let cmd = serde_json::json!({"command": ["get_property", prop]});
        self.send_command(&cmd)
            .ok()
            .and_then(|r| r.get("data").cloned())
    }

    /// Set a property on mpv
    pub fn set_property(&self, prop: &str, value: serde_json::Value) -> Result<()> {
        let cmd = serde_json::json!({"command": ["set_property", prop, value]});
        self.send_command(&cmd)?;
        Ok(())
    }

    /// Pause mpv
    pub fn pause(&self) -> Result<()> {
        self.set_property("pause", serde_json::json!(true))
    }

    /// Resume mpv
    pub fn resume(&self) -> Result<()> {
        self.set_property("pause", serde_json::json!(false))
    }

    /// Toggle pause
    pub fn toggle_pause(&self) -> Result<()> {
        let paused = self.get_property("pause")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.set_property("pause", serde_json::json!(!paused))
    }

    /// Set volume (0.0 - 1.0)
    pub fn set_volume(&self, vol: f32) -> Result<()> {
        self.set_property("volume", serde_json::json!((vol * 100.0) as u32))
    }

    /// Get current playback position in seconds
    pub fn get_position(&self) -> f64 {
        self.get_property("time-pos")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    }

    /// Get total duration in seconds
    pub fn get_duration(&self) -> f64 {
        self.get_property("duration")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
    }

    /// Get media title
    pub fn get_title(&self) -> String {
        self.get_property("media-title")
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_default()
    }

    /// Check if mpv is still running
    pub fn is_running(&mut self) -> bool {
        if let Some(ref mut child) = self.process {
            match child.try_wait() {
                Ok(Some(_)) => {
                    self.process = None;
                    false
                }
                Ok(None) => true,
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// Check if playback has reached end of file
    pub fn is_eof(&self) -> bool {
        self.get_property("eof-reached")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Check if mpv is paused
    pub fn is_paused(&self) -> bool {
        self.get_property("pause")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    }

    /// Stop and kill mpv process
    pub fn stop(&mut self) {
        if let Some(ref mut child) = self.process {
            child.kill().ok();
            child.wait().ok();
        }
        self.process = None;
        std::fs::remove_file(MPV_SOCKET).ok();
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}
