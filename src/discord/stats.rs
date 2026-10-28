//! /stats, its tabs (Together included) and "play your top songs", the
//! monthly recap DM, and Loski Wrapped: /wrapped, and a DM on January 1st.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
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
    genres,
    picture::{self, StatsPicture, Tile, WrappedPicture},
    stats::{self, Bar, Range, Stats, Together},
    util::{escape_md, fmt_duration_words, truncate},
};

/// Shown on the card; "play your top songs" queues more than this.
const SHOWN: usize = 5;
const PLAY_TOP: usize = 50;
/// Remembers which month the last recap covered.
const RECAP_MARKER: &str = "recap_sent";
/// Remembers which year the last Wrapped covered.
const WRAPPED_MARKER: &str = "wrapped_sent";
/// Friends listed on the Together tab.
const FRIENDS_SHOWN: usize = 6;

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

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "wrapped",
    desc = "Loski Wrapped: your year in music, in pictures"
)]
pub struct WrappedCmd {
    /// Which year (this one so far if you leave it out)
    #[command(min_value = 2020, max_value = 2100)]
    year: Option<i64>,
}

pub(super) fn now(app: &App) -> DateTime<Tz> {
    Utc::now().with_timezone(&app.timezone())
}

pub(super) async fn compute(app: &App, range: Range) -> Stats {
    let store = app.store.clone();
    let now = now(app);
    let known = app.genres.known();
    let stats = tokio::task::spawn_blocking(move || {
        stats::compute(&store.all_history(), range, now, &known)
    })
    .await
    .expect("stats task");
    // The genres of the artists that matter here are looked up for next time.
    app.genres
        .want(stats.top_artists.iter().take(25).map(|a| a.name.as_str()));
    stats
}

/// A friend's name, as Loski last saw it.
fn friend_name(app: &App, id: u64) -> String {
    app.room
        .name_of(id)
        .unwrap_or_else(|| "a friend".to_string())
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
    if key == "together" {
        let app = &ix.app;
        let stats = compute(app, Range::AllTime).await;
        let card = together_card(&app.icons, &stats.together, |id| friend_name(app, id));
        app.http
            .interaction(app.app_id)
            .update_response(&ix.token)
            .flags(cards::V2)
            .keep_attachment_ids(&[])
            .components(Some(&[card]))
            .await?;
        return Ok(());
    }
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
        Range::Year(_) => first.format("%Y").to_string(),
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
        Range::WholeMonth { .. } | Range::Year(_) if stats.best_streak > 1 => {
            chips.push(format!("{}-day best streak", stats.best_streak));
        }
        Range::WholeMonth { .. } | Range::Year(_) => {}
        _ if stats.streak > 1 => chips.push(format!("{}-day streak", stats.streak)),
        _ => {}
    }
    chips.extend(compared_short(stats));
    if let Some((genre, _)) = stats.top_genres.first() {
        chips.push(format!("Mostly {genre}"));
    }
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
        mark: picture::signature(&app.name()),
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
    let target = commands::require_voice(ix)?;
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
        target,
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
        Range::Year(year) => format!("YOUR {year} IN MUSIC"),
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
        Range::Year(_) => first.format("%Y").to_string(),
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

/// The tabs under the stats and Together cards, `active` highlighted, and
/// "Play top songs" for a stats tab (disabled when there's nothing).
fn tabs(icons: &Icons, active: &str, play: Option<bool>) -> Component {
    let mut buttons: Vec<Component> = Range::TABS
        .iter()
        .map(|range| (range.key(), range.label()))
        .chain([("together", "Together")])
        .map(|(key, label)| {
            let style = if key == active {
                ButtonStyle::Primary
            } else {
                ButtonStyle::Secondary
            };
            Btn::label(&format!("loski:stats:{key}"), label)
                .style(style)
                .build(icons)
        })
        .collect();
    if let Some(enabled) = play {
        buttons.push(
            Btn::icon(&format!("loski:stats:play:{active}"), Icon::Play)
                .with_label("Play top songs")
                .disabled(!enabled)
                .build(icons),
        );
    }
    cards::row(buttons)
}

/// "-# TOP GENRES" and the first few, or None while none are known.
fn genres_text(stats: &Stats) -> Option<String> {
    if stats.top_genres.is_empty() {
        return None;
    }
    let names: Vec<String> = stats
        .top_genres
        .iter()
        .take(4)
        .map(|(genre, _)| genres::display(genre))
        .collect();
    Some(format!("-# TOP GENRES\n{}", names.join(" · ")))
}

/// The stats card: the picture (or, without one, the same numbers as text)
/// and tabs for the other ranges. `picture` is the uploaded picture's address.
pub fn stats_card(icons: &Icons, stats: &Stats, picture: Option<&str>) -> Component {
    let mut parts = match picture {
        Some(url) if stats.plays > 0 => {
            let mut parts = vec![cards::image(url, &alt_text(stats))];
            parts.extend(genres_text(stats).map(cards::text));
            parts
        }
        _ => {
            let mut parts = vec![summary(icons, stats)];
            if stats.plays > 0 {
                parts.extend(details(stats));
            }
            parts
        }
    };
    parts.push(cards::divider());
    parts.push(tabs(icons, stats.range.key(), Some(stats.plays > 0)));
    cards::card(Some(color::BRAND), parts)
}

/// The Together tab: time and songs with each friend who was in the call.
pub fn together_card(
    icons: &Icons,
    together: &[Together],
    name: impl Fn(u64) -> String,
) -> Component {
    let mut parts = vec![cards::text(format!(
        "{} **Listening together**\n-# ALL TIME",
        icons.md(Icon::Friends)
    ))];
    if together.is_empty() {
        parts.push(cards::text(
            "Nobody's listened along yet. Friends in the call while your music plays show up here.",
        ));
    }
    for friend in together.iter().take(FRIENDS_SHOWN) {
        let songs: Vec<String> = friend
            .top_songs
            .iter()
            .map(|s| {
                format!(
                    "[{}](https://open.spotify.com/track/{})",
                    escape_md(&truncate(&s.title, 40)),
                    s.track_id()
                )
            })
            .collect();
        let mut text = format!(
            "**{}** · {} · {}",
            escape_md(&truncate(&name(friend.friend), 40)),
            fmt_duration_words(friend.listened_ms),
            plural(friend.plays, "song", "songs")
        );
        if !songs.is_empty() {
            text.push_str(&format!("\n-# Most played together: {}", songs.join(" · ")));
        }
        let cover = friend.top_songs.first().and_then(|s| s.cover.as_deref());
        parts.push(cards::text_with_cover(text, cover));
    }
    if together.len() > FRIENDS_SHOWN {
        parts.push(cards::text(format!(
            "-# and {} more",
            plural((together.len() - FRIENDS_SHOWN) as u64, "friend", "friends")
        )));
    }
    parts.push(cards::divider());
    parts.push(tabs(icons, "together", None));
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

/// "41,230".
pub(super) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// The tiles of Wrapped's highlights picture, from the year's numbers.
fn wrapped_tiles(stats: &Stats, name: impl Fn(u64) -> String) -> Vec<Tile> {
    let tile = |label: &str, value: String, detail: String| Tile {
        label: label.to_string(),
        value,
        detail,
    };
    let mut tiles = Vec::new();
    if let Some(song) = stats.top_songs.first() {
        tiles.push(tile(
            "TOP SONG",
            song.title.clone(),
            format!("{} · {}", song.artists, plural(song.plays, "play", "plays")),
        ));
    }
    if let Some(artist) = stats.top_artists.first() {
        tiles.push(tile(
            "TOP ARTIST",
            artist.name.clone(),
            format!(
                "{} · {}",
                plural(artist.plays, "play", "plays"),
                fmt_duration_words(artist.listened_ms)
            ),
        ));
    }
    if let Some((day, ms)) = stats.best_day {
        tiles.push(tile(
            "BIGGEST DAY",
            day.format("%A, %B %-d").to_string(),
            fmt_duration_words(ms),
        ));
    }
    if stats.best_streak > 1 {
        tiles.push(tile(
            "LONGEST STREAK",
            format!("{} days", stats.best_streak),
            "in a row with music".to_string(),
        ));
    }
    if let Some(friend) = stats.together.first() {
        tiles.push(tile(
            "LISTENED WITH MOST",
            name(friend.friend),
            format!("{} together", fmt_duration_words(friend.listened_ms)),
        ));
    }
    if let Some((genre, _)) = stats.top_genres.first() {
        let more: Vec<&str> = stats
            .top_genres
            .iter()
            .skip(1)
            .take(2)
            .map(|(g, _)| g.as_str())
            .collect();
        let detail = match more.as_slice() {
            [] => "your sound this year".to_string(),
            [one] => format!("then {one}"),
            [one, two, ..] => format!("then {one} and {two}"),
        };
        tiles.push(tile("TOP GENRE", genres::display(genre), detail));
    }
    // Fill the grid when friends or genres are missing.
    let busiest = stats
        .bars
        .iter()
        .filter(|b| b.listened_ms > 0)
        .max_by_key(|b| b.listened_ms);
    if tiles.len() < 6
        && let Some(month) = busiest
    {
        tiles.push(tile(
            "BIGGEST MONTH",
            month.first_day.format("%B").to_string(),
            fmt_duration_words(month.listened_ms),
        ));
    }
    if tiles.len() < 6 && stats.artists > 0 {
        tiles.push(tile(
            "DISCOVERED",
            plural(stats.artists as u64, "artist", "artists"),
            format!("across {}", plural(stats.songs as u64, "song", "songs")),
        ));
    }
    tiles.truncate(6);
    tiles
}

/// Draws Wrapped's highlights picture.
async fn wrapped_picture(app: &App, stats: &Stats, year: i32) -> Option<Vec<u8>> {
    let cover = fetch_cover(
        &app.web,
        stats.top_songs.first().and_then(|s| s.cover.clone()),
    )
    .await;
    let picture = WrappedPicture {
        kicker: format!("{} WRAPPED · {year}", picture::signature(&app.name())),
        mark: picture::signature(&app.name()),
        headline: format!("{} minutes", thousands(stats.listened_ms / 60_000)),
        subline: format!(
            "of music · {} · {} · {}",
            plural(stats.plays, "play", "plays"),
            plural(stats.songs as u64, "song", "songs"),
            plural(stats.artists as u64, "artist", "artists"),
        ),
        tiles: wrapped_tiles(stats, |id| friend_name(app, id)),
        cover,
    };
    tokio::task::spawn_blocking(move || picture::render_wrapped(&picture))
        .await
        .ok()
}

/// Loski Wrapped as a card: the highlights and the year's stats picture,
/// or the same in words without them. `pictures` are the uploaded files'
/// addresses (highlights, then the year).
pub fn wrapped_card(
    icons: &Icons,
    bot: &str,
    stats: &Stats,
    year: i32,
    so_far: bool,
    pictures: &[&str],
    name: impl Fn(u64) -> String,
) -> Component {
    let mut parts = vec![cards::text(format!(
        "{} **{} Wrapped {year}**\nYour year in music{}.",
        icons.md(Icon::Stats),
        escape_md(bot),
        if so_far {
            " so far"
        } else {
            ", start to finish"
        }
    ))];
    if pictures.is_empty() {
        let mut lines = vec![format!(
            "## {} minutes\n{} · {} · {}",
            thousands(stats.listened_ms / 60_000),
            plural(stats.plays, "play", "plays"),
            plural(stats.songs as u64, "song", "songs"),
            plural(stats.artists as u64, "artist", "artists"),
        )];
        for tile in wrapped_tiles(stats, name) {
            lines.push(format!(
                "-# {}\n**{}** · {}",
                tile.label,
                escape_md(&truncate(&tile.value, 80)),
                escape_md(&truncate(&tile.detail, 80))
            ));
        }
        parts.push(cards::text(lines.join("\n")));
    } else {
        let alt = alt_text(stats);
        let items: Vec<(&str, &str)> = pictures.iter().map(|url| (*url, alt.as_str())).collect();
        parts.push(cards::gallery(&items));
    }
    parts.push(cards::text(
        "-# `/wrapped year:` shows another year. `/stats` has this week and month.",
    ));
    cards::card(Some(color::BRAND), parts)
}

/// Wrapped for `year`, ready to send: the card and its pictures. None when
/// nothing played that year.
async fn wrapped_message(app: &App, year: i32) -> Option<(Component, Vec<Attachment>)> {
    let stats = compute(app, Range::Year(year)).await;
    if stats.plays == 0 {
        return None;
    }
    let (highlights, year_picture) =
        futures::join!(wrapped_picture(app, &stats, year), picture(app, &stats));
    let mut files = Vec::new();
    let mut urls = Vec::new();
    for (bytes, name) in [
        (highlights, picture::WRAPPED_FILENAME),
        (year_picture, picture::FILENAME),
    ] {
        if let Some(bytes) = bytes {
            files.push(Attachment::from_bytes(
                name.to_string(),
                bytes,
                files.len() as u64 + 1,
            ));
            urls.push(format!("attachment://{name}"));
        }
    }
    let urls: Vec<&str> = urls.iter().map(String::as_str).collect();
    let so_far = year >= now(app).year();
    let card = wrapped_card(&app.icons, &app.name(), &stats, year, so_far, &urls, |id| {
        friend_name(app, id)
    });
    Some((card, files))
}

pub async fn wrapped(ix: &Ix, cmd: WrappedCmd) -> Result<()> {
    let app = &ix.app;
    // Drawing two pictures takes a moment.
    ix.defer().await?;
    let year = cmd.year.map_or_else(|| now(app).year(), |y| y as i32);
    let Some((card, files)) = wrapped_message(app, year).await else {
        return Err(oops(format!("Nothing played in {year} yet.")));
    };
    app.http
        .interaction(app.app_id)
        .update_response(&ix.token)
        .flags(cards::V2)
        .attachments(&files)
        .components(Some(&[card]))
        .await?;
    ix.delete_later(LONG_SECS * 3);
    Ok(())
}

/// On January 1st (or the first time Loski runs after it), Wrapped for the
/// year that just ended arrives in your DMs.
async fn send_wrapped_if_due(app: &App) {
    let last = app.store.read_marker(WRAPPED_MARKER);
    let Some(year) = stats::wrapped_due(now(app), last.as_deref()) else {
        return;
    };
    if let Some((card, files)) = wrapped_message(app, year).await {
        if !dm_with_files(app, card, &files).await {
            // Try again next hour.
            return;
        }
        tracing::info!("Sent Loski Wrapped for {year}");
    }
    app.store.write_marker(WRAPPED_MARKER, &year.to_string());
}

/// A card with pictures in your DMs. Returns whether it arrived.
async fn dm_with_files(app: &App, card: Component, files: &[Attachment]) -> bool {
    let Ok(dm) = app.http.create_private_channel(app.owner_id()).await else {
        return false;
    };
    let Ok(dm) = dm.model().await else {
        return false;
    };
    app.http
        .create_message(dm.id)
        .flags(cards::V2)
        .attachments(files)
        .components(&[card])
        .await
        .is_ok()
}

fn summary(icons: &Icons, stats: &Stats) -> Component {
    let title = match stats.range {
        Range::WholeMonth { .. } => format!(
            "{} **Your {} in music**",
            icons.md(Icon::Stats),
            stats.first_day.format("%B")
        ),
        Range::Year(year) => format!("{} **Your {year} in music**", icons.md(Icon::Stats)),
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
        Range::WholeMonth { .. } | Range::Year(_) if stats.best_streak > 1 => {
            facts.push(format!("🔥 {}-day streak at best", stats.best_streak));
        }
        Range::WholeMonth { .. } | Range::Year(_) => {}
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
    parts.extend(genres_text(stats).map(cards::text));
    if let Some(hour) = stats.peak_hour {
        parts.push(cards::text(format!("-# {}", peak_line(hour))));
    }
    parts
}

/// Checks every hour whether a month (or a year) has ended, and DMs you its
/// recap (or Wrapped). If Loski was off on the 1st, it arrives the next time
/// it runs.
pub async fn recap_loop(app: Arc<App>) {
    loop {
        send_recap_if_due(&app).await;
        send_wrapped_if_due(&app).await;
        // Keeps the genres of this year's artists looked up ahead of Wrapped.
        compute(&app, Range::Year(now(&app).year())).await;
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
        assert_eq!(thousands(41_230), "41,230");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }
}
