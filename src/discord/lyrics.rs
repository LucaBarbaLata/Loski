//! Lyrics: a card that follows the song line by line, for singing along in
//! the call, plus `/lyrics` for any song. Spotify's own lyrics come through
//! Loski's existing connection; LRCLIB, a free lyrics database, fills in when
//! Spotify has none. Nothing is stored beyond a small in-memory cache.

use std::{
    collections::VecDeque,
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use anyhow::Result;
use librespot_core::SpotifyId;
use parking_lot::Mutex;
use serde::Deserialize;
use twilight_interactions::command::{AutocompleteValue, CommandModel, CreateCommand};
use twilight_model::{
    channel::message::Component,
    id::{
        Id,
        marker::{ChannelMarker, MessageMarker},
    },
};

use super::{
    App, Core, LONG_SECS,
    cards::{self, Btn, color},
    commands,
    icons::{Icon, Icons},
    interact::Ix,
    oops,
};
use crate::{
    engine::Status,
    spotify::{Lookup, parse_lookup},
    track::TrackInfo,
    util::{escape_md, truncate},
};

/// Show a line this early, to make up for the time Discord takes to show an edit.
const LEAD_MS: u32 = 600;
/// Edits of the live card are at least this far apart.
const MIN_EDIT_GAP: Duration = Duration::from_millis(1_200);
const TICK: Duration = Duration::from_millis(250);
/// Characters of lyrics per page of the full text.
const PAGE_CHARS: usize = 1_700;
const CACHE_SIZE: usize = 40;

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "lyrics",
    desc = "Lyrics that follow the song, or the lyrics of any song"
)]
pub struct LyricsCmd {
    /// A song (leave it out for the one playing, line by line)
    #[command(autocomplete = true, max_length = 300)]
    song: Option<String>,
}

#[derive(CommandModel)]
#[command(autocomplete = true)]
pub struct LyricsComplete {
    song: AutocompleteValue<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// When the line starts, for synced lyrics.
    pub start_ms: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lyrics {
    pub lines: Vec<Line>,
    pub synced: bool,
    pub instrumental: bool,
    /// Who the lyrics come from, as the card credits them.
    pub source: String,
    /// The background colour Spotify uses for this song's lyrics.
    pub color: Option<u32>,
}

impl Lyrics {
    /// The line being sung at `position_ms` (None before the first one).
    pub fn line_at(&self, position_ms: u32) -> Option<usize> {
        if !self.synced {
            return None;
        }
        self.lines
            .iter()
            .rposition(|l| l.start_ms.is_some_and(|start| start <= position_ms))
    }

    fn full_text(&self) -> String {
        self.lines
            .iter()
            .map(|l| {
                if l.text.trim().is_empty() {
                    String::new()
                } else {
                    escape_md(&l.text)
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The full text split into pages that fit a card, at line breaks.
    pub fn pages(&self) -> Vec<String> {
        let mut pages = vec![String::new()];
        for line in self.full_text().split('\n') {
            let page = pages.last_mut().expect("a page");
            if page.chars().count() + line.chars().count() + 1 > PAGE_CHARS && !page.is_empty() {
                pages.push(String::new());
            }
            let page = pages.last_mut().expect("a page");
            if !page.is_empty() {
                page.push('\n');
            }
            page.push_str(line);
        }
        pages
            .into_iter()
            .map(|p| p.trim_matches('\n').to_string())
            .filter(|p| !p.is_empty())
            .collect()
    }
}

// Spotify's reply, read leniently: a field it leaves out shouldn't cost the lyrics.
#[derive(Deserialize)]
struct SpotifyLyrics {
    lyrics: SpotifyInner,
    #[serde(default)]
    colors: Option<SpotifyColors>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpotifyInner {
    #[serde(default)]
    lines: Vec<SpotifyLine>,
    #[serde(default)]
    sync_type: String,
    #[serde(default)]
    provider_display_name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpotifyLine {
    #[serde(default)]
    start_time_ms: String,
    #[serde(default)]
    words: String,
}

#[derive(Deserialize)]
struct SpotifyColors {
    background: i64,
}

fn from_spotify(bytes: &[u8]) -> Option<Lyrics> {
    let parsed: SpotifyLyrics = serde_json::from_slice(bytes).ok()?;
    let synced = parsed.lyrics.sync_type == "LINE_SYNCED";
    let lines: Vec<Line> = parsed
        .lyrics
        .lines
        .into_iter()
        .map(|l| Line {
            start_ms: if synced {
                l.start_time_ms.parse().ok()
            } else {
                None
            },
            text: l.words,
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    let source = match parsed.lyrics.provider_display_name.as_str() {
        "" => "Spotify".to_string(),
        name => name.to_string(),
    };
    Some(Lyrics {
        lines,
        synced,
        instrumental: false,
        source,
        // Spotify sends the colour as a signed ARGB number.
        color: parsed.colors.map(|c| (c.background as u32) & 0xFF_FF_FF),
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrclibLyrics {
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    plain_lyrics: Option<String>,
}

/// "[01:23.45] words" lines; a line may carry several timestamps.
fn parse_lrc(text: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut starts = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some((stamp, after)) = stripped.split_once(']') else {
                break;
            };
            let Some((minutes, seconds)) = stamp.split_once(':') else {
                break;
            };
            let (Ok(minutes), Ok(seconds)) = (minutes.parse::<u32>(), seconds.parse::<f64>())
            else {
                // A tag like [ar:Artist], not a time.
                break;
            };
            starts.push(minutes * 60_000 + (seconds * 1000.0).round() as u32);
            rest = after;
        }
        for start in starts {
            lines.push(Line {
                start_ms: Some(start),
                text: rest.trim().to_string(),
            });
        }
    }
    lines.sort_by_key(|l| l.start_ms);
    lines
}

fn from_lrclib(parsed: LrclibLyrics) -> Option<Lyrics> {
    let source = "LRCLIB".to_string();
    if parsed.instrumental {
        return Some(Lyrics {
            lines: Vec::new(),
            synced: false,
            instrumental: true,
            source,
            color: None,
        });
    }
    if let Some(synced) = parsed.synced_lyrics.as_deref() {
        let lines = parse_lrc(synced);
        if !lines.is_empty() {
            return Some(Lyrics {
                lines,
                synced: true,
                instrumental: false,
                source,
                color: None,
            });
        }
    }
    let plain = parsed.plain_lyrics?;
    let lines: Vec<Line> = plain
        .lines()
        .map(|l| Line {
            start_ms: None,
            text: l.to_string(),
        })
        .collect();
    (!lines.is_empty()).then_some(Lyrics {
        lines,
        synced: false,
        instrumental: false,
        source,
        color: None,
    })
}

async fn lrclib(app: &App, track: &TrackInfo) -> Option<Lyrics> {
    let duration = (track.duration_ms / 1000).to_string();
    let artist = track.artists.first().cloned().unwrap_or_default();
    let response = app
        .web
        .get("https://lrclib.net/api/get")
        .query(&[
            ("track_name", track.title.as_str()),
            ("artist_name", artist.as_str()),
            ("album_name", track.album.as_str()),
            ("duration", duration.as_str()),
        ])
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    from_lrclib(response.json().await.ok()?)
}

type Cached = (SpotifyId, Option<Arc<Lyrics>>);
static CACHE: LazyLock<Mutex<VecDeque<Cached>>> = LazyLock::new(Default::default);

/// The song's lyrics, from Spotify or else LRCLIB. None means neither has them.
pub async fn fetch(app: &App, core: &Core, track: &TrackInfo) -> Option<Arc<Lyrics>> {
    if let Some((_, lyrics)) = CACHE.lock().iter().find(|(id, _)| *id == track.id) {
        return lyrics.clone();
    }
    let session = core.spotify.session();
    let spotify = match session.spclient().get_lyrics(&track.id).await {
        Ok(bytes) => from_spotify(&bytes),
        Err(e) => {
            tracing::debug!("no Spotify lyrics for {}: {e}", track.title);
            None
        }
    };
    let lyrics = match spotify {
        Some(lyrics) => Some(lyrics),
        None => lrclib(app, track).await,
    }
    .map(Arc::new);
    let mut cache = CACHE.lock();
    cache.push_front((track.id, lyrics.clone()));
    cache.truncate(CACHE_SIZE);
    lyrics
}

fn cached(id: SpotifyId) -> Option<Arc<Lyrics>> {
    CACHE
        .lock()
        .iter()
        .find(|(cached, _)| *cached == id)
        .and_then(|(_, lyrics)| lyrics.clone())
}

// ------------------------------------------------------------ the live card

struct Live {
    channel: Id<ChannelMarker>,
    message: Id<MessageMarker>,
    /// The song the card shows, and its lyrics once fetched.
    track: Option<SpotifyId>,
    lyrics: Option<Arc<Lyrics>>,
    /// The line shown, and whether the card is up to date with it.
    line: Option<usize>,
    shown_line: Option<Option<usize>>,
    last_edit: Instant,
    waiting: bool,
}

static LIVE: LazyLock<Mutex<Option<Live>>> = LazyLock::new(Default::default);

fn header(track: &TrackInfo) -> String {
    format!(
        "-# LYRICS · **{}** · {}",
        escape_md(&truncate(&track.title, 60)),
        escape_md(&truncate(&track.artists_joined(), 40))
    )
}

fn line_text(line: &Line) -> String {
    if line.text.trim().is_empty() {
        "♪".to_string()
    } else {
        escape_md(&truncate(&line.text, 300))
    }
}

/// The card that follows the song: the line being sung large, the one
/// before and the next two smaller.
pub fn live_card(
    icons: &Icons,
    track: Option<&TrackInfo>,
    lyrics: Option<&Lyrics>,
    line: Option<usize>,
) -> Component {
    let accent = lyrics.and_then(|l| l.color).unwrap_or(color::BRAND);
    let buttons = cards::row(vec![
        Btn::icon("loski:lyricsall", Icon::Lyrics)
            .with_label("All lyrics")
            .disabled(lyrics.is_none_or(|l| l.lines.is_empty()))
            .build(icons),
        Btn::label("loski:lyricsclose", "Close").build(icons),
    ]);
    let Some(track) = track else {
        return cards::card(
            Some(accent),
            vec![
                cards::text("-# LYRICS\nWaiting for the next song."),
                buttons,
            ],
        );
    };
    let mut parts = vec![cards::text(header(track))];
    match lyrics {
        None => parts.push(cards::text(
            "No lyrics for this one. I'll pick up again with the next song.",
        )),
        Some(l) if l.instrumental => parts.push(cards::text("### ♪ Instrumental")),
        Some(l) if !l.synced => {
            // No timing: show the start and offer the rest.
            let first = l.pages().into_iter().next().unwrap_or_default();
            parts.push(cards::text(truncate(&first, 900)));
            parts.push(cards::text(
                "-# These lyrics have no timing, so they can't follow along. **All lyrics** shows the rest.",
            ));
        }
        Some(l) => {
            let get = |i: Option<usize>| i.and_then(|i| l.lines.get(i)).map(line_text);
            let (before, now, next, after) = match line {
                None => (None, "♪".to_string(), get(Some(0)), get(Some(1))),
                Some(i) => (
                    i.checked_sub(1).and_then(|p| get(Some(p))),
                    get(Some(i)).unwrap_or_else(|| "♪".to_string()),
                    get(Some(i + 1)),
                    get(Some(i + 2)),
                ),
            };
            let mut text = String::new();
            if let Some(before) = before {
                text.push_str(&format!("-# {before}\n"));
            }
            text.push_str(&format!("### {now}"));
            if let Some(next) = next {
                text.push_str(&format!("\n{next}"));
            }
            if let Some(after) = after {
                text.push_str(&format!("\n-# {after}"));
            }
            parts.push(cards::text(text));
        }
    }
    if let Some(l) = lyrics {
        parts.push(cards::text(format!("-# Lyrics from {}", l.source)));
    }
    parts.push(buttons);
    cards::card(Some(accent), parts)
}

/// The Lyrics button, or `/lyrics` without a song: puts a live card at the
/// bottom of the channel (replacing an earlier one).
pub async fn open(ix: &Ix, core: &Core) -> Result<()> {
    let app = &ix.app;
    let channel = ix
        .channel_id
        .ok_or_else(|| oops("This only works in a server channel."))?;
    if ix.is_component() {
        ix.ack().await?;
    } else {
        ix.defer_ephemeral().await?;
    }
    close(app).await;

    let snap = core.engine.snapshot();
    let track = snap.track.clone().filter(|_| snap.status != Status::Idle);
    let lyrics = match &track {
        Some(track) => fetch(app, core, track).await,
        None => None,
    };
    let line = lyrics
        .as_ref()
        .and_then(|l| l.line_at(snap.position_ms + LEAD_MS));
    let card = live_card(&app.icons, track.as_deref(), lyrics.as_deref(), line);
    let message = app
        .http
        .create_message(channel)
        .flags(cards::V2)
        .components(&[card])
        .await?
        .model()
        .await?;
    *LIVE.lock() = Some(Live {
        channel,
        message: message.id,
        track: track.as_ref().map(|t| t.id),
        lyrics,
        line,
        shown_line: Some(line),
        last_edit: Instant::now(),
        waiting: track.is_none(),
    });
    if !ix.is_component() {
        ix.delete_reply().await;
    }
    Ok(())
}

/// Removes the live card (Close, or Loski leaving).
pub async fn close(app: &App) {
    let live = LIVE.lock().take();
    if let Some(live) = live {
        let _ = app.http.delete_message(live.channel, live.message).await;
    }
}

/// Keeps the live card in step with the song. Runs for as long as Loski does
/// and only does work while a card is open.
pub async fn follow_loop(app: Arc<App>, core: Arc<Core>) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if LIVE.lock().is_none() {
            continue;
        }
        let snap = core.engine.snapshot();
        let track = snap.track.clone().filter(|_| snap.status != Status::Idle);

        // A new song (or none): fetch its lyrics and redraw.
        let current = LIVE.lock().as_ref().map(|l| l.track);
        let track_id = track.as_ref().map(|t| t.id);
        if current.is_some_and(|c| c != track_id) {
            let lyrics = match &track {
                Some(track) => fetch(&app, &core, track).await,
                None => None,
            };
            let mut guard = LIVE.lock();
            if let Some(live) = guard.as_mut() {
                live.track = track_id;
                live.lyrics = lyrics;
                live.line = None;
                live.shown_line = None;
                live.waiting = track.is_none();
            }
        }

        // Which line is being sung, and whether the card needs an edit.
        let edit = {
            let mut guard = LIVE.lock();
            let Some(live) = guard.as_mut() else { continue };
            if snap.status == Status::Playing
                && let Some(lyrics) = &live.lyrics
            {
                live.line = lyrics.line_at(snap.position_ms + LEAD_MS);
            }
            let due =
                live.shown_line != Some(live.line) && live.last_edit.elapsed() >= MIN_EDIT_GAP;
            if due {
                live.shown_line = Some(live.line);
                live.last_edit = Instant::now();
                let card = live_card(
                    &app.icons,
                    track.as_deref(),
                    live.lyrics.as_deref(),
                    live.line,
                );
                Some((live.channel, live.message, card))
            } else {
                None
            }
        };
        if let Some((channel, message, card)) = edit {
            let result = app
                .http
                .update_message(channel, message)
                .flags(cards::V2)
                .components(Some(&[card]))
                .await;
            if result.is_err() {
                // Deleted by someone, or the channel is gone: stop following.
                let mut guard = LIVE.lock();
                if guard.as_ref().is_some_and(|l| l.message == message) {
                    *guard = None;
                }
            }
        }
    }
}

// ------------------------------------------------------- the full text

/// One page of a song's lyrics, with buttons to turn the page.
pub fn page_card(icons: &Icons, track: &TrackInfo, lyrics: &Lyrics, page: usize) -> Component {
    let pages = lyrics.pages();
    let total = pages.len().max(1);
    let page = page.min(total - 1);
    let id = track.base62();
    let mut parts = vec![cards::text_with_cover(
        format!(
            "-# LYRICS\n**{}**\n{}",
            escape_md(&truncate(&track.title, 80)),
            escape_md(&truncate(&track.artists_joined(), 60))
        ),
        track.cover_thumb_url.as_deref(),
    )];
    if lyrics.instrumental {
        parts.push(cards::text("### ♪ Instrumental"));
    } else {
        parts.extend(cards::text_blocks(
            pages
                .get(page)
                .cloned()
                .unwrap_or_default()
                .split('\n')
                .map(str::to_string),
        ));
    }
    parts.push(cards::text(format!(
        "-# Page {} of {total} · lyrics from {}",
        page + 1,
        lyrics.source
    )));
    let mut buttons = Vec::new();
    if total > 1 {
        buttons.push(
            Btn::icon(
                &format!("loski:lyricspage:{id}:{}", page.saturating_sub(1)),
                Icon::Left,
            )
            .disabled(page == 0)
            .build(icons),
        );
        buttons.push(
            Btn::icon(&format!("loski:lyricspage:{id}:{}", page + 1), Icon::Right)
                .disabled(page + 1 >= total)
                .build(icons),
        );
    }
    buttons.push(Btn::label("loski:qclose", "Close").build(icons));
    parts.push(cards::row(buttons));
    cards::card(Some(lyrics.color.unwrap_or(color::BRAND)), parts)
}

fn no_lyrics(track: &TrackInfo) -> anyhow::Error {
    oops(format!(
        "I couldn't find lyrics for **{}**.",
        escape_md(&track.title)
    ))
}

/// "All lyrics" on the live card: the whole text, only for you.
pub async fn show_all(ix: &Ix, core: &Core) -> Result<()> {
    let app = &ix.app;
    let track = core
        .engine
        .current_track()
        .ok_or_else(|| oops("Nothing's playing."))?;
    let lyrics = match cached(track.id) {
        Some(lyrics) => lyrics,
        None => fetch(app, core, &track)
            .await
            .ok_or_else(|| no_lyrics(&track))?,
    };
    ix.reply_private(vec![page_card(&app.icons, &track, &lyrics, 0)])
        .await
}

/// `track-id:page` from the page buttons.
pub async fn turn_page(ix: &Ix, core: &Core, rest: &str) -> Result<()> {
    let app = &ix.app;
    let (id, page) = rest.rsplit_once(':').unwrap_or((rest, "0"));
    let id = SpotifyId::from_base62(id).map_err(|_| oops("I lost track of that song."))?;
    let track = resolve(core, Lookup::Track(id)).await?;
    let lyrics = match cached(id) {
        Some(lyrics) => lyrics,
        None => fetch(app, core, &track)
            .await
            .ok_or_else(|| no_lyrics(&track))?,
    };
    ix.update_message(vec![page_card(
        &app.icons,
        &track,
        &lyrics,
        page.parse().unwrap_or(0),
    )])
    .await
}

async fn resolve(core: &Core, lookup: Lookup) -> Result<TrackInfo> {
    core.spotify
        .resolve(lookup)
        .await
        .map_err(|e| oops(format!("Hmm, {e}")))?
        .and_then(|r| r.tracks.into_iter().next())
        .ok_or_else(|| oops("I couldn't find that song."))
}

/// `/lyrics`: the live card, or the lyrics of a song you name.
pub async fn lyrics(ix: &Ix, cmd: LyricsCmd) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    let Some(song) = cmd.song.filter(|s| !s.trim().is_empty()) else {
        return open(ix, &core).await;
    };
    ix.defer().await?;
    let track = match parse_lookup(&song) {
        Lookup::Text(query) => commands::search(&core, &query)
            .await
            .map_err(|e| oops(format!("Search failed: {e}")))?
            .first()
            .cloned()
            .ok_or_else(|| oops("I couldn't find that song."))?,
        Lookup::Track(id) => resolve(&core, Lookup::Track(id)).await?,
        _ => {
            return Err(oops(
                "Lyrics are for one song. Paste a song link or type its name.",
            ));
        }
    };
    let lyrics = fetch(app, &core, &track)
        .await
        .ok_or_else(|| no_lyrics(&track))?;
    ix.reply_temp(
        vec![page_card(&app.icons, &track, &lyrics, 0)],
        LONG_SECS * 2,
    )
    .await
}

pub async fn suggestions(
    app: &App,
    input: twilight_interactions::command::CommandInputData<'_>,
    recent: bool,
) -> Vec<twilight_model::application::command::CommandOptionChoice> {
    match LyricsComplete::from_interaction(input) {
        Ok(LyricsComplete {
            song: AutocompleteValue::Focused(partial),
        }) => commands::song_suggestions(app, &partial, recent).await,
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_lrc_timestamps() {
        let lines = parse_lrc(
            "[ar:Someone]\n[00:12.50] First line\n[00:15.00][01:15.00] Chorus\n[00:20.04]\n",
        );
        let starts: Vec<_> = lines.iter().map(|l| l.start_ms.unwrap()).collect();
        assert_eq!(starts, [12_500, 15_000, 20_040, 75_000]);
        assert_eq!(lines[1].text, "Chorus");
        assert_eq!(lines[2].text, "");
    }

    #[test]
    fn finds_the_line_being_sung() {
        let lyrics = Lyrics {
            lines: [1_000, 4_000, 9_000]
                .iter()
                .map(|&ms| Line {
                    start_ms: Some(ms),
                    text: format!("line at {ms}"),
                })
                .collect(),
            synced: true,
            instrumental: false,
            source: "test".into(),
            color: None,
        };
        assert_eq!(lyrics.line_at(500), None);
        assert_eq!(lyrics.line_at(1_000), Some(0));
        assert_eq!(lyrics.line_at(8_999), Some(1));
        assert_eq!(lyrics.line_at(60_000), Some(2));
    }

    #[test]
    fn reads_spotify_lyrics_and_their_colour() {
        let json = br#"{"lyrics":{"syncType":"LINE_SYNCED","lines":[{"startTimeMs":"1200","words":"Hello","syllables":[],"endTimeMs":"0"},{"startTimeMs":"3400","words":"","syllables":[],"endTimeMs":"0"}],"provider":"MusixMatch","providerDisplayName":"Musixmatch"},"colors":{"background":-9013642,"text":-16777216,"highlightText":-1},"hasVocalRemoval":false}"#;
        let lyrics = from_spotify(json).unwrap();
        assert!(lyrics.synced);
        assert_eq!(lyrics.lines[0].start_ms, Some(1_200));
        assert_eq!(lyrics.source, "Musixmatch");
        assert_eq!(lyrics.color, Some(0x76_76_76));
        assert!(from_spotify(b"{}").is_none());
    }

    #[test]
    fn splits_long_lyrics_into_pages() {
        let lyrics = Lyrics {
            lines: (0..300)
                .map(|i| Line {
                    start_ms: None,
                    text: format!("A fairly ordinary line of words, number {i}"),
                })
                .collect(),
            synced: false,
            instrumental: false,
            source: "test".into(),
            color: None,
        };
        let pages = lyrics.pages();
        assert!(pages.len() > 5);
        assert!(pages.iter().all(|p| p.chars().count() <= PAGE_CHARS));
    }
}
