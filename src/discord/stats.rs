//! /stats, its tabs and "play your top songs", and the monthly recap DM.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::Tz;
use futures::future::join_all;
use librespot_core::{SpotifyId, SpotifyUri};
use twilight_interactions::command::{CommandModel, CommandOption, CreateCommand, CreateOption};
use twilight_model::{
    channel::message::{Component, component::ButtonStyle},
    http::attachment::Attachment,
};

use super::{
    App, LONG_SECS, SHORT_SECS,
    cards::{self, Btn, color},
    commands::{self, Placement},
    icons::{Icon, Icons},
    interact::Ix,
    oops,
};
use crate::{
    picture::{self, StatsPicture},
    stats::{self, Bar, Range, Stats},
    util::{escape_md, fmt_duration_words, truncate},
};

/// Shown on the card; "play your top songs" queues more than this.
const SHOWN: usize = 5;
const PLAY_TOP: usize = 50;
/// Remembers which month the last recap covered.
const RECAP_MARKER: &str = "recap_sent";

#[derive(CommandOption, CreateOption)]
pub enum StatsRange {
    #[option(name = "Last 7 days", value = "week")]
    Week,
    #[option(name = "This month", value = "month")]
    Month,
    #[option(name = "All time", value = "all")]
    All,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "stats",
    desc = "Your listening time, top songs and artists, and streaks"
)]
pub struct StatsCmd {
    /// Which stretch of time (this month if you leave it out)
    range: Option<StatsRange>,
}

fn now(app: &App) -> DateTime<Tz> {
    Utc::now().with_timezone(&app.cfg.timezone)
}

async fn compute(app: &App, range: Range) -> Stats {
    let store = app.store.clone();
    let now = now(app);
    tokio::task::spawn_blocking(move || stats::compute(&store.all_history(), range, now))
        .await
        .expect("stats task")
}

pub async fn stats(ix: &Ix, cmd: StatsCmd) -> Result<()> {
    let range = match cmd.range {
        Some(StatsRange::Week) => Range::Week,
        Some(StatsRange::All) => Range::AllTime,
        Some(StatsRange::Month) | None => Range::Month,
    };
    show(ix, range, LONG_SECS * 2).await
}

/// Replies with the stats card for `range`, cleaned up after `secs`.
pub async fn show(ix: &Ix, range: Range, secs: u64) -> Result<()> {
    // Drawing the picture takes a moment.
    ix.defer().await?;
    respond(ix, range).await?;
    ix.delete_later(secs);
    Ok(())
}

/// A tab on the stats card was clicked.
pub async fn switch_tab(ix: &Ix, key: &str) -> Result<()> {
    ix.ack().await?;
    respond(ix, Range::from_key(key).unwrap_or(Range::Month)).await
}

/// Fills in the (deferred) reply with the card and its freshly drawn picture.
async fn respond(ix: &Ix, range: Range) -> Result<()> {
    let app = &ix.app;
    let stats = compute(app, range).await;
    let jpeg = picture(app, &stats).await;
    let reference = format!("attachment://{}", picture::FILENAME);
    let card = stats_card(
        &app.icons,
        &stats,
        jpeg.as_ref().map(|_| reference.as_str()),
    );
    let files: Vec<Attachment> = jpeg
        .into_iter()
        .map(|bytes| Attachment::from_bytes(picture::FILENAME.to_string(), bytes, 1))
        .collect();
    app.http
        .interaction(app.app_id)
        .update_response(&ix.token)
        .flags(cards::V2)
        .attachments(&files)
        .components(Some(&[card]))
        .await?;
    Ok(())
}

/// "31h 12m", "45m", "40s": the headline number on the picture.
fn short_duration(ms: u64) -> String {
    let secs = ms / 1000;
    match (secs / 3600, secs / 60 % 60) {
        (0, 0) => format!("{secs}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m}m"),
    }
}

/// The comparison, short enough for a chip: "+4 hr vs last week".
fn compared_short(stats: &Stats) -> Option<String> {
    let (first, last, before) = stats.previous?;
    if before == 0 {
        return None;
    }
    let then = match stats.range {
        Range::Week => "last week".to_string(),
        Range::WholeMonth { .. } => first.format("%B").to_string(),
        _ => days_span(first, last),
    };
    let diff = stats.listened_ms as i64 - before as i64;
    Some(match diff {
        d if d.unsigned_abs() < 60_000 => format!("Same as {then}"),
        d if d > 0 => format!("+{} vs {then}", fmt_duration_words(d as u64)),
        d => format!("-{} vs {then}", fmt_duration_words(d.unsigned_abs())),
    })
}

/// Downloads a cover thumbnail for the picture; a slow or missing one is left out.
async fn fetch_cover(http: &reqwest::Client, url: Option<String>) -> Option<image::RgbImage> {
    let bytes = http
        .get(url?)
        .timeout(Duration::from_secs(4))
        .send()
        .await
        .ok()?
        .bytes()
        .await
        .ok()?;
    image::load_from_memory(&bytes)
        .ok()
        .map(|img| img.to_rgb8())
}

/// Draws the stats picture, or None when there's nothing to show yet.
async fn picture(app: &App, stats: &Stats) -> Option<Vec<u8>> {
    if stats.plays == 0 {
        return None;
    }
    let top = &stats.top_songs[..stats.top_songs.len().min(SHOWN)];
    let covers = join_all(top.iter().map(|s| fetch_cover(&app.web, s.cover.clone()))).await;

    let mut chips = Vec::new();
    match stats.range {
        Range::WholeMonth { .. } if stats.best_streak > 1 => {
            chips.push(format!("{}-day best streak", stats.best_streak));
        }
        Range::WholeMonth { .. } => {}
        _ if stats.streak > 1 => chips.push(format!("{}-day streak", stats.streak)),
        _ => {}
    }
    chips.extend(compared_short(stats));
    let best = stats
        .bars
        .iter()
        .filter(|b| b.listened_ms > 0)
        .max_by_key(|b| b.listened_ms);
    if let Some(best) = best.filter(|_| stats.bars.len() > 1) {
        chips.push(if stats.monthly_bars {
            format!("Best month: {}", best.first_day.format("%B"))
        } else {
            format!("Best day: {}", best.first_day.format("%a, %b %-d"))
        });
    }
    if let Some(hour) = stats.peak_hour {
        chips.push(listener(hour).to_string());
    }

    let count = stats.bars.len();
    let bars = stats
        .bars
        .iter()
        .enumerate()
        .map(|(i, bar)| picture::Bar {
            value: bar.listened_ms,
            label: if stats.monthly_bars {
                Some(bar.first_day.format("%b").to_string())
            } else if count <= 7 {
                Some(bar.first_day.format("%a").to_string())
            } else {
                (i == 0 || i + 1 == count).then(|| day(bar.first_day))
            },
        })
        .collect();

    let picture = StatsPicture {
        kicker: kicker(stats),
        headline: short_duration(stats.listened_ms),
        subline: format!(
            "of music · {} · {} · {}",
            plural(stats.plays, "play", "plays"),
            plural(stats.songs as u64, "song", "songs"),
            plural(stats.artists as u64, "artist", "artists"),
        ),
        chips,
        bars,
        songs: top
            .iter()
            .zip(covers)
            .map(|(song, cover)| picture::Song {
                title: song.title.clone(),
                artist: song.artists.clone(),
                plays: plural(song.plays, "play", "plays"),
                cover,
            })
            .collect(),
        artists: stats
            .top_artists
            .iter()
            .take(3)
            .map(|a| picture::Artist {
                name: a.name.clone(),
                detail: format!(
                    "{} · {}",
                    plural(a.plays, "play", "plays"),
                    fmt_duration_words(a.listened_ms)
                ),
            })
            .collect(),
    };
    tokio::task::spawn_blocking(move || picture::render(&picture))
        .await
        .ok()
}

/// What the picture says for screen readers.
fn alt_text(stats: &Stats) -> String {
    let mut text = format!(
        "{}: {} of music, {}.",
        kicker(stats),
        fmt_duration_words(stats.listened_ms),
        plural(stats.plays, "play", "plays")
    );
    if let Some(song) = stats.top_songs.first() {
        text.push_str(&format!(" Top song: {} by {}.", song.title, song.artists));
    }
    if let Some(artist) = stats.top_artists.first() {
        text.push_str(&format!(" Top artist: {}.", artist.name));
    }
    text
}

/// "Play your top songs": the most played songs of that tab, most played first.
pub async fn play_top(ix: &Ix, key: &str) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    let guild = commands::require_voice(ix)?;
    let range = Range::from_key(key).unwrap_or(Range::Month);
    ix.ack().await?;
    let stats = compute(app, range).await;
    let uris: Vec<SpotifyUri> = stats
        .top_songs
        .iter()
        .take(PLAY_TOP)
        .filter_map(|s| SpotifyId::from_base62(s.track_id()).ok())
        .map(|id| SpotifyUri::Track { id })
        .collect();
    if uris.is_empty() {
        return Err(oops("Nothing to play yet. Listen to a few songs first."));
    }
    let what = match range {
        Range::Week => "your top songs this week",
        Range::Month => "your top songs this month",
        _ => "your top songs of all time",
    };
    let resolved = core.spotify.load_tracks(uris, what.to_string()).await;
    let card = commands::enqueue(
        app,
        &core,
        guild,
        resolved.tracks,
        Placement::End,
        resolved.source,
        None,
    )
    .await?;
    ix.followup_temp(vec![card], SHORT_SECS).await
}

fn plural(n: impl Into<u64>, one: &str, many: &str) -> String {
    let n = n.into();
    format!("{n} {}", if n == 1 { one } else { many })
}

/// "Oct 5".
fn day(d: NaiveDate) -> String {
    d.format("%b %-d").to_string()
}

/// "Oct 15–21", "Sep 28 – Oct 4", or just "Oct 21".
fn days_span(first: NaiveDate, last: NaiveDate) -> String {
    if first == last {
        day(first)
    } else if first.format("%Y-%m").to_string() == last.format("%Y-%m").to_string() {
        format!("{}–{}", day(first), last.format("%-d"))
    } else {
        format!("{} – {}", day(first), day(last))
    }
}

fn kicker(stats: &Stats) -> String {
    let month = stats.first_day.format("%B").to_string();
    match stats.range {
        Range::Week => format!(
            "LAST 7 DAYS · {}",
            days_span(stats.first_day, stats.last_day)
        ),
        Range::Month => format!("{month} SO FAR"),
        Range::AllTime if stats.plays > 0 => {
            format!("ALL TIME · SINCE {}", stats.first_day.format("%b %-d, %Y"))
        }
        Range::AllTime => "ALL TIME".to_string(),
        Range::WholeMonth { year, .. } => format!("{month} {year}"),
    }
    .to_uppercase()
}

/// How this stretch compares with the one before it.
fn compared(stats: &Stats) -> Option<String> {
    let (first, last, before) = stats.previous?;
    if before == 0 {
        return None;
    }
    let then = match stats.range {
        Range::Week => "the week before".to_string(),
        Range::WholeMonth { .. } => first.format("%B").to_string(),
        _ => days_span(first, last),
    };
    let diff = stats.listened_ms as i64 - before as i64;
    Some(if diff.unsigned_abs() < 60_000 {
        format!("about the same as {then}")
    } else if diff > 0 {
        format!("↑ {} more than {then}", fmt_duration_words(diff as u64))
    } else {
        format!(
            "↓ {} less than {then}",
            fmt_duration_words(diff.unsigned_abs())
        )
    })
}

/// One block character per bar, tallest for the busiest.
fn sparkline(bars: &[Bar]) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = bars.iter().map(|b| b.listened_ms).max().unwrap_or(0);
    bars.iter()
        .map(|b| match b.listened_ms {
            0 => LEVELS[0],
            ms => LEVELS[(1 + ms * 6 / max.max(1)) as usize],
        })
        .collect()
}

fn activity(stats: &Stats) -> String {
    let (heading, unit, date) = if stats.monthly_bars {
        ("EVERY MONTH", "month", "%B %Y")
    } else {
        ("EVERY DAY", "day", "%a, %b %-d")
    };
    let best = stats
        .bars
        .iter()
        .filter(|b| b.listened_ms > 0)
        .max_by_key(|b| b.listened_ms)
        .map(|b| {
            format!(
                "Best {unit}: {} ({})",
                b.first_day.format(date),
                fmt_duration_words(b.listened_ms)
            )
        })
        .unwrap_or_default();
    // A chart of one bar says nothing.
    if stats.bars.len() < 2 {
        return format!("-# {heading}\n{best}");
    }
    format!("-# {heading}\n`{}` · {best}", sparkline(&stats.bars))
}

fn listener(hour: u32) -> &'static str {
    match hour {
        5..=10 => "Early bird",
        11..=16 => "Afternoon listener",
        17..=21 => "Evening listener",
        _ => "Night owl",
    }
}

fn peak_line(hour: u32) -> String {
    format!("{}: you listen most around {hour:02}:00.", listener(hour))
}

/// The stats card: the picture (or, without one, the same numbers as text)
/// and tabs for the other ranges. `picture` is the uploaded picture's address.
pub fn stats_card(icons: &Icons, stats: &Stats, picture: Option<&str>) -> Component {
    let mut parts = match picture {
        Some(url) if stats.plays > 0 => vec![cards::image(url, &alt_text(stats))],
        _ => {
            let mut parts = vec![summary(icons, stats)];
            if stats.plays > 0 {
                parts.extend(details(stats));
            }
            parts
        }
    };

    let mut tabs: Vec<Component> = Range::TABS
        .iter()
        .map(|&range| {
            let style = if range == stats.range {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            };
            Btn::label(&format!("loski:stats:{}", range.key()), range.label())
                .style(style)
                .build(icons)
        })
        .collect();
    tabs.push(
        Btn::icon(
            &format!("loski:stats:play:{}", stats.range.key()),
            Icon::Play,
        )
        .with_label("Play top songs")
        .disabled(stats.plays == 0)
        .build(icons),
    );
    parts.push(cards::divider());
    parts.push(cards::row(tabs));
    cards::card(Some(color::BRAND), parts)
}

/// The same numbers as a monthly recap, for your DMs (no buttons there).
pub fn recap_card(icons: &Icons, stats: &Stats, picture: Option<&str>) -> Component {
    let mut parts = match picture {
        Some(url) => vec![
            cards::text(format!(
                "{} **Your {} in music**",
                icons.md(Icon::Stats),
                stats.first_day.format("%B")
            )),
            cards::image(url, &alt_text(stats)),
        ],
        None => {
            let mut parts = vec![summary(icons, stats)];
            parts.extend(details(stats));
            parts
        }
    };
    parts.push(cards::divider());
    parts.push(cards::text(
        "-# Use `/stats` in your server to see this week or all time.",
    ));
    cards::card(Some(color::BRAND), parts)
}

fn summary(icons: &Icons, stats: &Stats) -> Component {
    let title = match stats.range {
        Range::WholeMonth { .. } => format!(
            "{} **Your {} in music**",
            icons.md(Icon::Stats),
            stats.first_day.format("%B")
        ),
        _ => format!("{} **Your listening**", icons.md(Icon::Stats)),
    };
    if stats.plays == 0 {
        let empty = match stats.range {
            Range::Week => "Nothing in the last 7 days yet.",
            Range::Month => "Nothing this month yet.",
            _ => "Nothing yet.",
        };
        return cards::text(format!(
            "{title}\n-# {}\n{empty} `/play` something and I'll keep count. Songs count once they've played for 30 seconds.",
            kicker(stats)
        ));
    }

    let mut facts = Vec::new();
    match stats.range {
        Range::WholeMonth { .. } if stats.best_streak > 1 => {
            facts.push(format!("🔥 {}-day streak at best", stats.best_streak));
        }
        Range::WholeMonth { .. } => {}
        _ if stats.streak > 1 => {
            let mut streak = format!("🔥 {}-day streak", stats.streak);
            if stats.best_streak > stats.streak {
                streak.push_str(&format!(" (best {})", stats.best_streak));
            }
            facts.push(streak);
        }
        _ => {}
    }
    facts.extend(compared(stats));

    let mut text = format!(
        "{title}\n-# {}\n## {}\n**{}** · {} · {}",
        kicker(stats),
        fmt_duration_words(stats.listened_ms),
        plural(stats.plays, "play", "plays"),
        plural(stats.songs as u64, "song", "songs"),
        plural(stats.artists as u64, "artist", "artists"),
    );
    if !facts.is_empty() {
        text.push('\n');
        text.push_str(&facts.join(" · "));
    }
    let cover = stats.top_songs.first().and_then(|s| s.cover.as_deref());
    cards::text_with_cover(text, cover)
}

fn details(stats: &Stats) -> Vec<Component> {
    let songs: Vec<String> = stats
        .top_songs
        .iter()
        .take(SHOWN)
        .enumerate()
        .map(|(i, s)| {
            format!(
                "`{}` [**{}**](https://open.spotify.com/track/{}) · {} · {}",
                i + 1,
                escape_md(&truncate(&s.title, 48)),
                s.track_id(),
                escape_md(&truncate(&s.artists, 32)),
                plural(s.plays, "play", "plays")
            )
        })
        .collect();
    let artists: Vec<String> = stats
        .top_artists
        .iter()
        .take(SHOWN)
        .enumerate()
        .map(|(i, a)| {
            format!(
                "`{}` **{}** · {} · {}",
                i + 1,
                escape_md(&truncate(&a.name, 48)),
                plural(a.plays, "play", "plays"),
                fmt_duration_words(a.listened_ms)
            )
        })
        .collect();

    let mut parts = vec![
        cards::text(activity(stats)),
        cards::divider(),
        cards::text(format!("-# TOP SONGS\n{}", songs.join("\n"))),
        cards::text(format!("-# TOP ARTISTS\n{}", artists.join("\n"))),
    ];
    if let Some(hour) = stats.peak_hour {
        parts.push(cards::text(format!("-# {}", peak_line(hour))));
    }
    parts
}

/// Checks every hour whether a month has ended, and DMs you its recap.
/// If Loski was off on the 1st, the recap arrives the next time it runs.
pub async fn recap_loop(app: Arc<App>) {
    loop {
        send_recap_if_due(&app).await;
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

async fn send_recap_if_due(app: &App) {
    let last = app.store.read_marker(RECAP_MARKER);
    let Some((year, month, key)) = stats::recap_due(now(app), last.as_deref()) else {
        return;
    };
    let stats = compute(app, Range::WholeMonth { year, month }).await;
    if stats.plays > 0 {
        if !send_recap(app, &stats).await {
            // Try again next hour.
            return;
        }
        tracing::info!("Sent the recap for {key}");
    }
    app.store.write_marker(RECAP_MARKER, &key);
}

async fn send_recap(app: &App, stats: &Stats) -> bool {
    let jpeg = picture(app, stats).await;
    let reference = format!("attachment://{}", picture::FILENAME);
    let card = recap_card(&app.icons, stats, jpeg.as_ref().map(|_| reference.as_str()));
    let files: Vec<Attachment> = jpeg
        .into_iter()
        .map(|bytes| Attachment::from_bytes(picture::FILENAME.to_string(), bytes, 1))
        .collect();
    let Ok(dm) = app.http.create_private_channel(app.owner_id()).await else {
        return false;
    };
    let Ok(dm) = dm.model().await else {
        return false;
    };
    app.http
        .create_message(dm.id)
        .flags(cards::V2)
        .attachments(&files)
        .components(&[card])
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_and_sparklines_read_well() {
        let d = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
        assert_eq!(days_span(d(10, 15), d(10, 21)), "Oct 15–21");
        assert_eq!(days_span(d(9, 28), d(10, 4)), "Sep 28 – Oct 4");
        assert_eq!(days_span(d(10, 21), d(10, 21)), "Oct 21");
        let bars: Vec<Bar> = [0, 10, 60, 30]
            .into_iter()
            .map(|ms| Bar {
                first_day: d(10, 1),
                listened_ms: ms,
            })
            .collect();
        assert_eq!(sparkline(&bars), "▁▃█▅");
        assert_eq!(peak_line(23), "Night owl: you listen most around 23:00.");
        assert_eq!(short_duration(112_320_000), "31h 12m");
        assert_eq!(short_duration(2_700_000), "45m");
        assert_eq!(short_duration(40_000), "40s");
        assert_eq!(peak_line(7), "Early bird: you listen most around 07:00.");
    }
}
