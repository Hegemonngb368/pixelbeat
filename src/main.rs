mod cli;
mod config;
mod daemon;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::fs;
use std::io::Write;

#[derive(Parser)]
#[command(
    name = "px",
    about = "pixelbeat — pixel-art terminal music player",
    version,
    styles = get_styles(),
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the pixelbeat daemon
    #[command(alias = "d")]
    Daemon {
        /// Immediately play from this path
        #[arg(short, long)]
        play: Option<PathBuf>,
    },

    /// Play a file or directory (or resume if no path given)
    Play {
        /// Path to audio file or directory
        path: Option<PathBuf>,
    },

    /// Pause playback
    Pause,

    /// Toggle play/pause
    Toggle,

    /// Stop playback
    Stop,

    /// Next track
    Next,

    /// Previous track
    Prev,

    /// Set volume (0.0 - 1.0)
    #[command(alias = "v")]
    Vol {
        /// Volume level (0.0 - 1.0)
        level: f32,
    },

    /// Toggle shuffle
    Shuffle,

    /// Toggle repeat
    Repeat,

    /// Get player status (for status bar integration)
    Status {
        /// Format string. Tokens: {title}, {icon}, {bar:N}, {elapsed},
        /// {duration}, {spectrum:N}, {vol}
        #[arg(short, long)]
        format: Option<String>,
    },

    /// Play internet radio (chillhop, lofi)
    #[command(alias = "r")]
    Radio {
        /// Station name: chillhop, lofi. Omit to list stations.
        station: Option<String>,
    },

    /// Play a YouTube playlist via yt-dlp
    #[command(alias = "youtube")]
    Yt {
        /// YouTube playlist URL
        url: String,
    },

    /// Open the pixel-art TUI
    #[command(alias = "ui")]
    Tui,

    /// Stop the daemon
    Quit,

    /// Show setup instructions for status bar integration
    Setup {
        /// Target: claude-code, tmux, starship
        target: String,
    },
}

fn get_styles() -> clap::builder::Styles {
    clap::builder::Styles::styled()
        .header(
            clap::builder::styling::AnsiColor::Yellow
                .on_default()
                .bold(),
        )
        .usage(
            clap::builder::styling::AnsiColor::Yellow
                .on_default()
                .bold(),
        )
        .literal(clap::builder::styling::AnsiColor::BrightWhite.on_default())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Daemon { play } => {
            let cfg = config::Config::load();
            let mut player =
                daemon::player::Player::new().expect("Failed to initialize audio player");

            // Apply config defaults
            player.set_volume(cfg.volume);
            if cfg.repeat {
                player.set_repeat(true);
            }
            if cfg.shuffle {
                player.set_shuffle(true);
            }

            player.cookies_browser = cfg.youtube_cookies_browser.clone();

            let mut autoplay_radio = None;

            if let Some(ref path) = play {
                player.load_path(path)?;
                player.play()?;
            } else if let Some(ref source) = cfg.source {
                match source.as_str() {
                    "local" => {
                        if let Some(dir) = cfg.music_dir_expanded().filter(|p| p.exists()) {
                            player.load_path(&dir)?;
                            player.play()?;
                        }
                    }
                    "youtube" => {
                        // YouTube autoplay deferred to after socket is ready
                        if let Some(ref yt_url) = cfg.youtube_url {
                            autoplay_radio = Some(format!("youtube:{}", yt_url));
                        } else {
                            eprintln!("pixelbeat: source=youtube but no youtube_url configured");
                        }
                    }
                    station => {
                        // Defer radio start to after socket is ready
                        autoplay_radio = Some(station.to_string());
                    }
                }
            } else {
                if let Some(dir) = cfg.music_dir_expanded().filter(|p| p.exists()) {
                    player.load_path(&dir)?;
                    player.play()?;
                }
            }

            let player = Arc::new(Mutex::new(player));
            daemon::ipc::start_server(player, autoplay_radio)?;
        }
        Commands::Play { path } => {
            let path_str = path.map(|p| {
                p.canonicalize()
                    .unwrap_or(p.clone())
                    .to_string_lossy()
                    .to_string()
            });
            cli::commands::handle_play(path_str)?;
        }
        Commands::Pause => cli::commands::handle_pause()?,
        Commands::Toggle => cli::commands::handle_toggle()?,
        Commands::Stop => cli::commands::handle_stop()?,
        Commands::Next => cli::commands::handle_next()?,
        Commands::Prev => cli::commands::handle_prev()?,
        Commands::Vol { level } => cli::commands::handle_volume(level)?,
        Commands::Shuffle => {
            // Toggle: get current state first
            if let Ok(resp) = daemon::ipc::send_command(&daemon::ipc::Command::Status) {
                if let Some(state) = resp.state {
                    cli::commands::handle_shuffle(!state.shuffle)?;
                }
            }
        }
        Commands::Repeat => {
            // Toggle: get current state first
            if let Ok(resp) = daemon::ipc::send_command(&daemon::ipc::Command::Status) {
                if let Some(state) = resp.state {
                    cli::commands::handle_repeat(!state.repeat)?;
                }
            }
        }
        Commands::Status { format } => cli::commands::handle_status(format)?,
        Commands::Radio { station } => {
            match station {
                Some(name) => cli::commands::handle_radio(&name)?,
                None => {
                    let stations = daemon::radio::list_stations();
                    eprintln!("Available stations: {}", stations.join(", "));
                    eprintln!("Usage: px radio chillhop");
                }
            }
        }
        Commands::Yt { url } => cli::commands::handle_youtube(&url)?,
        Commands::Tui => tui::app::run_tui()?,
        Commands::Quit => cli::commands::handle_quit()?,
        Commands::Setup { target } => print_setup(&target),
    }

    Ok(())
}

fn print_setup(target: &str) {
    const ORANGE: &str = "\x1b[38;2;227;137;62m";
    const DIM: &str = "\x1b[38;2;140;85;40m";
    const RESET: &str = "\x1b[0m";

    match target.to_lowercase().as_str() {
        "claude-code" | "claude" | "cc" => {
            setup_claude_code(ORANGE, DIM, RESET);
        }
        "tmux" => {
            println!(
                r#"
{ORANGE}pixelbeat{RESET} — tmux Status Line Integration
{DIM}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{RESET}

Add to your {ORANGE}~/.tmux.conf{RESET}:

  set -g status-right '#(px status --format "{{icon}} {{title:.20}} {{bar:8}} {{elapsed}}" 2>/dev/null)'
  set -g status-interval 1

Then reload: {ORANGE}tmux source-file ~/.tmux.conf{RESET}
"#
            );
        }
        "starship" => {
            println!(
                r#"
{ORANGE}pixelbeat{RESET} — Starship Prompt Integration
{DIM}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{RESET}

Add to your {ORANGE}~/.config/starship.toml{RESET}:

  [custom.music]
  command = "px status --format '{{icon}} {{title:.15}} {{elapsed}}'"
  when = "px status"
  format = "[$output]($style) "
  style = "bold #E3893E"
"#
            );
        }
        _ => {
            println!("Unknown target: {}. Available: claude-code, tmux, starship", target);
        }
    }
}

fn setup_claude_code(orange: &str, dim: &str, reset: &str) {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Could not determine home directory");
            return;
        }
    };

    let claude_dir = home.join(".claude");
    let statusline_path = claude_dir.join("statusline.sh");
    let settings_path = claude_dir.join("settings.json");

    println!(
        "\n{orange}pixelbeat{reset} — Claude Code Setup\n{dim}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━{reset}\n"
    );

    // Ensure ~/.claude/ exists
    if !claude_dir.exists() {
        let _ = fs::create_dir_all(&claude_dir);
    }

    // The pixelbeat snippet to inject
    let snippet = r#"
# ── pixelbeat music player ─────────────────────────────
PX="$HOME/.cargo/bin/px"
PX_SOCK="${TMPDIR:-/tmp}/pixelbeat.sock"
if [ -x "$PX" ]; then
    px_cassette=$("$PX" status --format "{cassette:70}" 2>/dev/null)
    if [ -n "$px_cassette" ]; then
        printf "\n%b" "$px_cassette"
    fi
fi
"#;

    let marker = "# ── pixelbeat music player";

    // Handle statusline.sh
    if statusline_path.exists() {
        let content = fs::read_to_string(&statusline_path).unwrap_or_default();
        if content.contains(marker) {
            println!("  {dim}✓{reset} statusline.sh already has pixelbeat integration");
        } else {
            // Append before the last `exit 0` if present, otherwise append at end
            let new_content = if let Some(pos) = content.rfind("\nexit 0") {
                let (before, after) = content.split_at(pos);
                format!("{}{}{}", before, snippet, after)
            } else {
                format!("{}{}", content, snippet)
            };
            fs::write(&statusline_path, new_content).unwrap_or_else(|e| {
                eprintln!("  Failed to write statusline.sh: {}", e);
            });
            println!("  {orange}✓{reset} Added pixelbeat to statusline.sh");
        }
    } else {
        // Create a minimal statusline.sh
        let content = format!(
            r#"#!/bin/bash
set -f

input=$(cat)

if [ -z "$input" ]; then
    printf "Claude"
    exit 0
fi
{snippet}
exit 0
"#
        );
        fs::write(&statusline_path, &content).unwrap_or_else(|e| {
            eprintln!("  Failed to create statusline.sh: {}", e);
        });

        // Make executable
        #[cfg(unix)]
        {{
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&statusline_path, fs::Permissions::from_mode(0o755));
        }}

        println!("  {orange}✓{reset} Created statusline.sh with pixelbeat integration");
    }

    // Handle settings.json — ensure statusLine command is configured
    let statusline_cmd = r#"bash "$HOME/.claude/statusline.sh""#;
    if settings_path.exists() {
        let content = fs::read_to_string(&settings_path).unwrap_or_default();
        if content.contains("statusLine") || content.contains("statusline") {
            println!("  {dim}✓{reset} settings.json already has statusLine configured");
        } else {
            // Parse and add statusLine
            if let Ok(mut json) = serde_json::from_str::<serde_json::Value>(&content) {
                json["statusLine"] = serde_json::json!({
                    "type": "command",
                    "command": statusline_cmd
                });
                if let Ok(pretty) = serde_json::to_string_pretty(&json) {
                    fs::write(&settings_path, pretty).unwrap_or_else(|e| {
                        eprintln!("  Failed to write settings.json: {}", e);
                    });
                    println!("  {orange}✓{reset} Added statusLine config to settings.json");
                }
            }
        }
    } else {
        let settings = serde_json::json!({
            "statusLine": {
                "type": "command",
                "command": statusline_cmd
            }
        });
        if let Ok(pretty) = serde_json::to_string_pretty(&settings) {
            fs::write(&settings_path, pretty).unwrap_or_else(|e| {
                eprintln!("  Failed to create settings.json: {}", e);
            });
            println!("  {orange}✓{reset} Created settings.json with statusLine config");
        }
    }

    println!("\n  {orange}Done!{reset} Now start the daemon:\n");
    println!("    px daemon &");
    println!("    px radio lofi\n");
    println!("  Restart Claude Code to see pixelbeat in the status line.\n");
}
