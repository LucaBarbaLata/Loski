use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};

/// Port of the login callback Spotify registered for librespot's client ID.
pub const LIBRESPOT_AUTH_PORT: u16 = 8898;

#[derive(Debug, Clone, Copy)]
pub enum AudioBitrate {
    /// Match the voice channel's own bitrate setting.
    Auto,
    Fixed(i32),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub discord_token: String,
    pub owner_id: u64,
    pub spotify_client_id: String,
    pub spotify_client_secret: String,
    pub auth_port: u16,
    pub data_dir: PathBuf,
    pub leave_after: Duration,
    pub idle_leave: Duration,
    pub bitrate: AudioBitrate,
    pub normalise: bool,
    pub bot_name: String,
    /// Keep the bot's name, avatar and banner in line with the repo.
    pub sync_profile: bool,
    /// Where "today" and "this month" are, for /stats and the monthly recap.
    pub timezone: chrono_tz::Tz,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let owner_id = required("OWNER_ID")?
            .parse()
            .context("OWNER_ID must be your numeric Discord user ID")?;

        let bitrate = match optional("AUDIO_BITRATE").as_deref() {
            None | Some("auto") => AudioBitrate::Auto,
            Some(v) => {
                let bits: i32 = v
                    .parse()
                    .context("AUDIO_BITRATE must be \"auto\" or a number")?;
                if !(8_000..=512_000).contains(&bits) {
                    bail!("AUDIO_BITRATE must be between 8000 and 512000");
                }
                AudioBitrate::Fixed(bits)
            }
        };

        let timezone = match optional("TIMEZONE") {
            Some(name) => name.parse().map_err(|_| {
                anyhow::anyhow!("TIMEZONE must be a name like Europe/Bucharest, not \"{name}\"")
            })?,
            None => chrono_tz::UTC,
        };

        Ok(Self {
            discord_token: required("DISCORD_TOKEN")?,
            owner_id,
            spotify_client_id: required("SPOTIFY_CLIENT_ID")?,
            spotify_client_secret: required("SPOTIFY_CLIENT_SECRET")?,
            auth_port: match optional("AUTH_PORT") {
                Some(v) => v.parse().context("AUTH_PORT must be a port number")?,
                None => 5588,
            },
            data_dir: optional("DATA_DIR").map_or_else(|| PathBuf::from("data"), PathBuf::from),
            leave_after: seconds("LEAVE_AFTER_SECS", 60)?,
            idle_leave: seconds("IDLE_LEAVE_SECS", 300)?,
            bitrate,
            normalise: flag("NORMALISE", true),
            bot_name: optional("BOT_NAME").unwrap_or_else(|| "Loski".to_string()),
            sync_profile: flag("SYNC_PROFILE", true),
            timezone,
        })
    }

    /// The page that starts the one-time Spotify login.
    pub fn login_url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.auth_port)
    }

    pub fn web_redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.auth_port)
    }
}

fn optional(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn flag(key: &str, default: bool) -> bool {
    match optional(key) {
        None => default,
        Some(v) => matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
    }
}

fn required(key: &str) -> Result<String> {
    match optional(key) {
        Some(v) => Ok(v),
        None => bail!("{key} is not set. Copy .env.example to .env and fill it in."),
    }
}

fn seconds(key: &str, default: u64) -> Result<Duration> {
    let secs = match optional(key) {
        Some(v) => v
            .parse()
            .with_context(|| format!("{key} must be a number of seconds"))?,
        None => default,
    };
    Ok(Duration::from_secs(secs))
}
