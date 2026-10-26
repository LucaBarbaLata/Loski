//! /status: whether Spotify, Discord and the audio are all healthy.

use std::time::{Duration, Instant};

use anyhow::Result;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::channel::message::Component;

use super::{
    App, LONG_SECS,
    cards::{self, color},
    icons::{Icon, Icons},
    interact::Ix,
    updates, voice,
};
use crate::{
    engine::{Health, Status},
    util::{escape_md, fmt_duration_words, truncate},
};

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "status",
    desc = "How Loski is doing: Spotify, Discord, audio and uptime"
)]
pub struct StatusCmd;

/// Everything the status card shows, gathered first so the card itself is
/// a plain function (and testable).
pub struct Report {
    pub spotify: Spotify,
    pub web_api: Option<Result<Duration, String>>,
    pub gateway_ping: Option<Duration>,
    pub voice: Option<VoiceReport>,
    pub health: Option<Health>,
    pub uptime: Duration,
    pub memory_mb: Option<u64>,
    pub timezone: String,
    pub device_name: String,
    /// A newer Loski release, if GitHub has one.
    pub newer: Option<String>,
}

pub enum Spotify {
    /// Still waiting for the one-time login.
    LoggedOut,
    Reconnecting,
    Connected {
        username: String,
        plan: Option<String>,
        country: String,
    },
}

pub struct VoiceReport {
    pub channel: String,
    pub bitrate_kbps: u32,
    pub region: Option<String>,
}

pub async fn status(ix: &Ix, _: StatusCmd) -> Result<()> {
    // Timing a Spotify request takes a moment.
    ix.defer().await?;
    let report = gather(&ix.app).await;
    ix.reply_temp(vec![status_card(&ix.app.icons, &report)], LONG_SECS)
        .await
}

async fn gather(app: &App) -> Report {
    let core = app.core.get();
    // One Web API request, timed, that also fetches your profile name
    // (librespot only knows the account's id).
    let (web_api, name) = match core {
        Some(core) => {
            let started = Instant::now();
            match core.spotify.web.display_name().await {
                Ok(name) => (Some(Ok(started.elapsed())), name),
                Err(e) => (Some(Err(e.to_string())), None),
            }
        }
        None => (None, None),
    };
    let spotify = match core.map(|c| c.spotify.session()) {
        None => Spotify::LoggedOut,
        Some(session) if session.is_invalid() => Spotify::Reconnecting,
        Some(session) => Spotify::Connected {
            username: name.unwrap_or_else(|| session.username()),
            plan: session.get_user_attribute("type"),
            country: session.country(),
        },
    };

    let voice = match (app.voice.guild(), app.voice.channel()) {
        (Some(guild), Some(channel)) => {
            let region = match app.songbird.get(guild) {
                Some(call) => call
                    .lock()
                    .await
                    .current_connection()
                    .and_then(|c| region(&c.endpoint)),
                None => None,
            };
            Some(VoiceReport {
                channel: app
                    .cache
                    .channel(channel)
                    .and_then(|c| c.name.clone())
                    .unwrap_or_else(|| "a voice channel".to_string()),
                bitrate_kbps: voice::bitrate_kbps(app, guild),
                region,
            })
        }
        _ => None,
    };

    Report {
        spotify,
        web_api,
        gateway_ping: *app.ping.lock(),
        voice,
        health: core.map(|c| c.engine.health()),
        uptime: app.started.elapsed(),
        memory_mb: memory_mb(),
        timezone: app.timezone().name().to_string(),
        device_name: app.cfg.bot_name.clone(),
        newer: updates::newer_release(app).map(|r| r.version().to_string()),
    }
}

/// Discord's voice servers are named after the nearest airport, like
/// `c-fra12-1a2b3c4d.discord.media` for Frankfurt.
fn region(endpoint: &str) -> Option<String> {
    let host = endpoint.split(['.', ':']).next()?;
    let code: String = host
        .strip_prefix("c-")
        .unwrap_or(host)
        .chars()
        .take_while(char::is_ascii_alphabetic)
        .collect();
    let city = match code.as_str() {
        "ams" => "Amsterdam",
        "atl" => "Atlanta",
        "bom" => "Mumbai",
        "buh" | "otp" => "Bucharest",
        "cdg" | "par" => "Paris",
        "dfw" => "Dallas",
        "fra" => "Frankfurt",
        "gru" => "São Paulo",
        "hkg" => "Hong Kong",
        "iad" => "Virginia",
        "lax" => "Los Angeles",
        "lhr" | "lon" => "London",
        "mad" => "Madrid",
        "mil" | "mxp" => "Milan",
        "ord" => "Chicago",
        "rot" => "Rotterdam",
        "sea" => "Seattle",
        "sin" => "Singapore",
        "sto" | "arn" => "Stockholm",
        "syd" => "Sydney",
        "tyo" | "nrt" => "Tokyo",
        "waw" => "Warsaw",
        "" => return None,
        other => return Some(other.to_uppercase()),
    };
    Some(city.to_string())
}

/// Resident memory, from Linux's view of this process.
fn memory_mb() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let kb: u64 = status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kb / 1024)
}

const GOOD: &str = "🟢";
const MEH: &str = "🟡";
const BAD: &str = "🔴";

fn ms(d: Duration) -> String {
    format!("{} ms", d.as_millis())
}

pub fn status_card(icons: &Icons, report: &Report) -> Component {
    let mut healthy = true;

    let mut spotify = vec!["-# SPOTIFY".to_string()];
    match &report.spotify {
        Spotify::LoggedOut => {
            healthy = false;
            spotify.push(format!(
                "{BAD} Not logged in yet. Check your DMs for the one-time login."
            ));
        }
        Spotify::Reconnecting => {
            healthy = false;
            spotify.push(format!("{MEH} Connection dropped, reconnecting…"));
        }
        Spotify::Connected {
            username,
            plan,
            country,
        } => {
            let plan = match plan.as_deref() {
                Some("premium") => " · Premium".to_string(),
                Some(other) if !other.is_empty() => {
                    healthy = false;
                    format!(" · {other} (streaming needs Premium)")
                }
                _ => String::new(),
            };
            spotify.push(format!(
                "{GOOD} Streaming as **{}**{plan} · {country}",
                escape_md(username)
            ));
        }
    }
    match &report.web_api {
        Some(Ok(took)) => spotify.push(format!("{GOOD} Search and follow mode · {}", ms(*took))),
        Some(Err(e)) => {
            healthy = false;
            spotify.push(format!(
                "{BAD} Search and follow mode aren't answering: {}",
                truncate(e, 200)
            ));
        }
        None => {}
    }
    if let Some(health) = &report.health {
        spotify.push(if health.connect_ready {
            format!(
                "{GOOD} Shows up as **{}** in your Spotify app's devices",
                report.device_name
            )
        } else {
            format!("{MEH} Spotify Connect is starting up")
        });
    }

    let mut discord = vec!["-# DISCORD".to_string()];
    discord.push(match report.gateway_ping {
        Some(ping) if ping > Duration::from_millis(400) => {
            format!("{MEH} Connected · {} (slow)", ms(ping))
        }
        Some(ping) => format!("{GOOD} Connected · {}", ms(ping)),
        None => format!("{GOOD} Connected"),
    });
    discord.push(match &report.voice {
        Some(v) => {
            let region = v
                .region
                .as_deref()
                .map(|r| format!(" · voice server in {r}"))
                .unwrap_or_default();
            format!(
                "{GOOD} In **{}** · {} kbps{region}",
                v.channel, v.bitrate_kbps
            )
        }
        None => "⚪ Not in a voice channel. `/play` brings me in.".to_string(),
    });

    let mut audio = vec!["-# AUDIO".to_string()];
    if let Some(health) = &report.health {
        let state = match health.status {
            Status::Playing => "playing",
            Status::Loading => "starting a song",
            Status::Paused => "paused",
            Status::Idle => "nothing playing",
        };
        audio.push(format!("{} · {state}", health.mode));
        if health.status == Status::Playing {
            let fill = health.buffered_ms * 100 / health.buffer_capacity_ms.max(1);
            let (dot, how) = match fill {
                0..=9 => {
                    healthy = false;
                    (BAD, "running dry, the stream may stutter")
                }
                10..=39 => (MEH, "a little low"),
                _ => (GOOD, "smooth"),
            };
            audio.push(format!(
                "{dot} Buffer {} of {} ms ready · {how}",
                health.buffered_ms, health.buffer_capacity_ms
            ));
        }
        audio.push("Spotify 320 kbps · 44.1 kHz stereo".to_string());
    }

    let mut system = format!(
        "-# Up {} · stats in {} time",
        fmt_duration_words(report.uptime.as_millis() as u64),
        report.timezone
    );
    if let Some(mb) = report.memory_mb {
        system.push_str(&format!(" · {mb} MB memory"));
    }
    system.push_str(&format!(" · Loski {}", updates::RUNNING));
    if let Some(version) = &report.newer {
        system.push_str(&format!(
            "\n{MEH} Loski {version} is out. `.\\loski update` brings it in."
        ));
    }

    let headline = if healthy {
        "All good."
    } else {
        "Something needs a look."
    };
    let mut parts = vec![
        cards::text(format!("{} **Status** · {headline}", icons.md(Icon::Info))),
        cards::divider(),
        cards::text(spotify.join("\n")),
        cards::text(discord.join("\n")),
    ];
    if audio.len() > 1 {
        parts.push(cards::text(audio.join("\n")));
    }
    parts.push(cards::divider());
    parts.push(cards::text(system));
    let accent = if healthy {
        color::SUCCESS
    } else {
        color::ERROR
    };
    cards::card(Some(accent), parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_regions_come_from_the_server_name() {
        assert_eq!(
            region("c-fra12-1a2b3c4d.discord.media:443").as_deref(),
            Some("Frankfurt")
        );
        assert_eq!(region("c-xyz3-1a2b.discord.media").as_deref(), Some("XYZ"));
        assert_eq!(region("123.discord.media"), None);
    }
}
