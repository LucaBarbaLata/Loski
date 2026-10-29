//! Your Spotify library and stations: /radio, /liked, /playlist, and the
//! right-click "Play with Loski" / "Queue with Loski".

use std::{
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use anyhow::Result;
use librespot_core::{SpotifyId, SpotifyUri};
use parking_lot::Mutex;
use rand::seq::SliceRandom;
use twilight_interactions::command::{
    AutocompleteValue, CommandModel, CommandOption, CreateCommand, CreateOption,
};
use twilight_model::application::{
    command::{Command, CommandOptionChoice, CommandType},
    interaction::application_command::CommandData,
};
use twilight_util::builder::command::CommandBuilder;

use super::{
    App, Core, SHORT_SECS,
    cards::{self, color},
    commands::{self, Placement},
    friends,
    interact::Ix,
    oops,
};
use crate::{
    spotify::{Lookup, MAX_TRACKS_PER_REQUEST, Resolved, library::PlaylistRef, parse_lookup},
    store::Store,
    track::TrackInfo,
    util::escape_md,
};

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "radio",
    desc = "Start Spotify's radio for a song, or for the one playing"
)]
pub struct RadioCmd {
    /// A song to start from (leave empty for the one playing)
    #[command(autocomplete = true, max_length = 300)]
    song: Option<String>,
}

#[derive(CommandModel)]
#[command(autocomplete = true)]
pub struct RadioComplete {
    song: AutocompleteValue<String>,
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "liked", desc = "Play your Liked Songs")]
pub struct LikedCmd {
    /// Shuffle them (on unless you turn it off)
    shuffle: Option<bool>,
    /// Your own Liked Songs, or the songs friends liked on the panel
    from: Option<LikedFrom>,
}

#[derive(CommandOption, CreateOption)]
pub enum LikedFrom {
    #[option(name = "your Liked Songs on Spotify", value = "mine")]
    Mine,
    #[option(name = "songs your friends liked on the panel", value = "friends")]
    Friends,
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "playlist", desc = "Play one of your playlists")]
pub struct PlaylistCmd {
    /// Start typing a playlist name, or paste a link
    #[command(autocomplete = true, max_length = 300)]
    name: String,
    /// Shuffle it
    shuffle: Option<bool>,
}

#[derive(CommandModel)]
#[command(autocomplete = true)]
pub struct PlaylistComplete {
    name: AutocompleteValue<String>,
}

/// The right-click (Apps) entries on messages: "Play with Loski" and
/// "Queue with Loski", by the bot's name when Discord allows it there.
pub fn menu_names(bot: &str) -> (String, String) {
    let queue = format!("Queue with {bot}");
    // Entry names take letters, digits, spaces, - and _, 32 at most.
    let fits = bot
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        && queue.chars().count() <= 32;
    if fits {
        (format!("Play with {bot}"), queue)
    } else {
        ("Play now".to_string(), "Add to queue".to_string())
    }
}

/// Whether a right-click entry adds to the queue (rather than playing now).
pub fn is_queue_menu(command: &str) -> bool {
    command.starts_with("Queue with ") || command == "Add to queue"
}

pub fn message_commands(bot: &str) -> Vec<Command> {
    let (play, queue) = menu_names(bot);
    vec![
        CommandBuilder::new(play, "", CommandType::Message).build(),
        CommandBuilder::new(queue, "", CommandType::Message).build(),
    ]
}

/// Your playlists, cached briefly since autocomplete asks on every keystroke.
type CachedPlaylists = (Instant, Arc<Vec<PlaylistRef>>);
static PLAYLISTS: LazyLock<Mutex<Option<CachedPlaylists>>> = LazyLock::new(Default::default);
const PLAYLISTS_TTL: Duration = Duration::from_secs(300);

async fn my_playlists(core: &Core) -> Result<Arc<Vec<PlaylistRef>>> {
    if let Some((at, list)) = PLAYLISTS.lock().as_ref()
        && at.elapsed() < PLAYLISTS_TTL
    {
        return Ok(list.clone());
    }
    let list = Arc::new(core.spotify.my_playlists().await?);
    *PLAYLISTS.lock() = Some((Instant::now(), list.clone()));
    Ok(list)
}

fn find_playlist<'a>(list: &'a [PlaylistRef], name: &str) -> Option<&'a PlaylistRef> {
    let wanted = name.trim().to_lowercase();
    list.iter()
        .find(|p| p.name.to_lowercase() == wanted)
        .or_else(|| {
            list.iter()
                .find(|p| p.name.to_lowercase().contains(&wanted))
        })
}

pub async fn playlist_suggestions(app: &App, partial: &str) -> Vec<CommandOptionChoice> {
    let Some(core) = app.core.get() else {
        return Vec::new();
    };
    let Ok(list) = my_playlists(core).await else {
        return Vec::new();
    };
    let wanted = partial.trim().to_lowercase();
    list.iter()
        .filter(|p| wanted.is_empty() || p.name.to_lowercase().contains(&wanted))
        .take(25)
        .map(|p| {
            let name = match p.tracks {
                Some(1) => format!("{} · 1 song", p.name),
                Some(n) => format!("{} · {n} songs", p.name),
                None => p.name.clone(),
            };
            commands::choice(
                name,
                format!("spotify:playlist:{}", p.id.to_base62().unwrap_or_default()),
            )
        })
        .collect()
}

pub async fn autocomplete(app: &App, name: &str, data: CommandData) -> Vec<CommandOptionChoice> {
    let input = data.into();
    match name {
        "radio" => match RadioComplete::from_interaction(input) {
            Ok(RadioComplete {
                song: AutocompleteValue::Focused(partial),
            }) => commands::song_suggestions(app, &partial, true).await,
            _ => Vec::new(),
        },
        "playlist" => match PlaylistComplete::from_interaction(input) {
            Ok(PlaylistComplete {
                name: AutocompleteValue::Focused(partial),
            }) => playlist_suggestions(app, &partial).await,
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The song a /radio should start from.
async fn radio_seed(core: &Core, song: Option<&str>) -> Result<(TrackInfo, bool)> {
    let Some(song) = song.map(str::trim).filter(|s| !s.is_empty()) else {
        let current = core
            .engine
            .current_track()
            .ok_or_else(|| oops("Nothing's playing. Name a song: `/radio song:`"))?;
        return Ok(((*current).clone(), false));
    };
    let track = match parse_lookup(song) {
        Lookup::Text(query) => commands::search(core, &query)
            .await?
            .first()
            .cloned()
            .ok_or_else(|| oops("I couldn't find that song."))?,
        lookup @ (Lookup::Track(_) | Lookup::ShortLink(_)) => core
            .spotify
            .resolve(lookup)
            .await
            .map_err(|e| oops(format!("Hmm, {e}")))?
            .and_then(|r| r.tracks.into_iter().next())
            .ok_or_else(|| oops("I couldn't load that song."))?,
        _ => {
            return Err(oops(
                "Radio starts from a song. Pick one, or leave it empty for the one playing.",
            ));
        }
    };
    Ok((track, true))
}

pub async fn radio(ix: &Ix, cmd: RadioCmd) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    let target = commands::require_voice(ix)?;
    ix.defer().await?;
    if let Some(channel) = ix.channel_id {
        app.ui.asked_from(channel);
    }
    let (seed, play_seed) = radio_seed(&core, cmd.song.as_deref()).await?;
    let station = core
        .spotify
        .radio(&seed)
        .await
        .map_err(|e| oops(format!("Hmm, {e}")))?;
    commands::join_target(app, &core, target).await?;

    if play_seed {
        core.engine.play_now(vec![seed.clone()]);
    }
    let count = station.tracks.len();
    core.engine.replace_upcoming(station.tracks);
    let text = format!(
        "**{}** radio: {count} similar songs lined up after {}.",
        escape_md(&seed.title),
        if play_seed { "it" } else { "this one" }
    );
    let card = cards::toast(
        &app.icons,
        &text,
        seed.cover_thumb_url.as_deref(),
        color::SUCCESS,
    );
    ix.reply_temp(vec![card], SHORT_SECS).await
}

/// 24/7: a radio from the song that just played, or (after a restart) the
/// last one in your history.
pub async fn radio_to_keep_going(core: &Core, store: &Store) -> Result<usize> {
    let seed = match core.engine.current_track() {
        Some(track) => (*track).clone(),
        None => {
            let last = store
                .recent_history(1)
                .into_iter()
                .next()
                .ok_or_else(|| oops("Nothing in your history to start a radio from."))?;
            let id = librespot_core::SpotifyId::from_base62(last.track_id())
                .map_err(|_| oops("The last song in your history has a broken link."))?;
            core.spotify
                .resolve(Lookup::Track(id))
                .await?
                .and_then(|r| r.tracks.into_iter().next())
                .ok_or_else(|| oops("Spotify couldn't find the last song you played."))?
        }
    };
    let station = core.spotify.radio(&seed).await?;
    let count = station.tracks.len();
    core.engine.replace_upcoming(station.tracks);
    Ok(count)
}

/// Radio from the song that just played, for the panel's buttons.
pub async fn radio_from_current(core: &Core) -> Result<usize> {
    let seed = core
        .engine
        .current_track()
        .ok_or_else(|| oops("There's no song to start a radio from."))?;
    let station = core
        .spotify
        .radio(&seed)
        .await
        .map_err(|e| oops(format!("Hmm, {e}")))?;
    let count = station.tracks.len();
    core.engine.replace_upcoming(station.tracks);
    Ok(count)
}

pub async fn liked(ix: &Ix, cmd: LikedCmd) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    let target = commands::require_voice(ix)?;
    ix.defer().await?;
    if let Some(channel) = ix.channel_id {
        app.ui.asked_from(channel);
    }
    let shuffle = cmd.shuffle.unwrap_or(true);
    if matches!(cmd.from, Some(LikedFrom::Friends)) {
        return friends_liked(ix, &core, target, shuffle).await;
    }
    let liked = core
        .spotify
        .liked_songs(shuffle)
        .await
        .map_err(|e| oops(format!("Hmm, {e}")))?;
    let note = liked.truncated.then(|| {
        if shuffle {
            "A random 500 of your liked songs.".to_string()
        } else {
            "Your 500 most recent liked songs.".to_string()
        }
    });
    let card = commands::enqueue(
        app,
        &core,
        target,
        liked.tracks,
        Placement::End,
        liked.source,
        note,
    )
    .await?;
    ix.reply_temp(vec![card], SHORT_SECS).await
}

/// `/liked from:friends`: what your friends liked on the panel, most liked first.
async fn friends_liked(
    ix: &Ix,
    core: &Core,
    target: commands::Target,
    shuffle: bool,
) -> Result<()> {
    let app = &ix.app;
    let mut uris: Vec<SpotifyUri> = friends::friends_liked(app)
        .iter()
        .filter_map(|uri| SpotifyId::from_base62(uri.rsplit(':').next()?).ok())
        .map(|id| SpotifyUri::Track { id })
        .take(MAX_TRACKS_PER_REQUEST)
        .collect();
    if uris.is_empty() {
        return Err(oops(
            "Your friends haven't liked anything yet. Anyone in the call can press ♥ on the panel.",
        ));
    }
    if shuffle {
        uris.shuffle(&mut rand::rng());
    }
    let resolved = core
        .spotify
        .load_tracks(uris, "songs your **friends liked**".to_string())
        .await;
    let card = commands::enqueue(
        app,
        core,
        target,
        resolved.tracks,
        Placement::End,
        resolved.source,
        None,
    )
    .await?;
    ix.reply_temp(vec![card], SHORT_SECS).await
}

pub async fn playlist(ix: &Ix, cmd: PlaylistCmd) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    let target = commands::require_voice(ix)?;
    ix.defer().await?;
    if let Some(channel) = ix.channel_id {
        app.ui.asked_from(channel);
    }
    let lookup = match parse_lookup(&cmd.name) {
        Lookup::Text(name) => {
            let list = my_playlists(&core).await?;
            let found = find_playlist(&list, &name).ok_or_else(|| {
                oops(format!(
                    "You don't have a playlist called “{}”.",
                    escape_md(&name)
                ))
            })?;
            Lookup::Playlist(found.id)
        }
        other => other,
    };
    let mut resolved = core
        .spotify
        .resolve(lookup)
        .await
        .map_err(|e| oops(format!("Hmm, {e}")))?
        .ok_or_else(|| oops("I couldn't open that playlist."))?;
    if cmd.shuffle.unwrap_or(false) {
        resolved.tracks.shuffle(&mut rand::rng());
    }
    let note = resolved
        .truncated
        .then(|| "Only the first 500 songs, to keep things snappy.".to_string());
    let card = commands::enqueue(
        app,
        &core,
        target,
        resolved.tracks,
        Placement::End,
        resolved.source,
        note,
    )
    .await?;
    ix.reply_temp(vec![card], SHORT_SECS).await
}

/// One of your playlists by its name, and its songs: for "play my
/// playlist …".
pub(super) async fn my_playlist(core: &Core, name: &str) -> Result<(String, Resolved)> {
    let list = my_playlists(core).await?;
    let found = find_playlist(&list, name).ok_or_else(|| {
        oops(format!(
            "I couldn't find your playlist “{}”.",
            escape_md(name)
        ))
    })?;
    let resolved = core
        .spotify
        .resolve(Lookup::Playlist(found.id))
        .await
        .map_err(|e| oops(format!("Hmm, {e}")))?
        .ok_or_else(|| oops("I couldn't open that playlist."))?;
    Ok((found.name.clone(), resolved))
}

/// The first Spotify link or URI in some text.
pub fn find_spotify_link(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || "<>()[]\"'".contains(c))
        .map(|token| token.trim_end_matches(['.', ',', '!', '?', ';', ':']))
        .find(|token| {
            token.contains("open.spotify.com/")
                || token.contains("spotify.link/")
                || token.contains("spotify.app.link/")
                || token.starts_with("spotify:")
        })
        .map(str::to_string)
}

/// Right-click → Apps → Play with Loski / Queue with Loski.
pub async fn message_command(ix: &Ix, data: CommandData) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    let target = commands::require_voice(ix)?;
    let message = data
        .target_id
        .zip(data.resolved.as_ref())
        .and_then(|(target, resolved)| resolved.messages.get(&target.cast()))
        .ok_or_else(|| oops("I couldn't read that message."))?;

    let mut text = message.content.clone();
    for embed in &message.embeds {
        if let Some(url) = &embed.url {
            text.push(' ');
            text.push_str(url);
        }
    }
    let link = find_spotify_link(&text)
        .ok_or_else(|| oops("That message doesn't have a Spotify link."))?;

    ix.defer().await?;
    if let Some(channel) = ix.channel_id {
        app.ui.asked_from(channel);
    }
    let resolved = core
        .spotify
        .resolve(parse_lookup(&link))
        .await
        .map_err(|e| oops(format!("Hmm, {e}")))?
        .ok_or_else(|| oops("That link doesn't point to a song, album, playlist or artist."))?;
    let placement = if is_queue_menu(&data.name) {
        Placement::End
    } else {
        Placement::Now
    };
    let note = resolved
        .truncated
        .then(|| "Only the first 500 songs, to keep things snappy.".to_string());
    let card = commands::enqueue(
        app,
        &core,
        target,
        resolved.tracks,
        placement,
        resolved.source,
        note,
    )
    .await?;
    ix.reply_temp(vec![card], SHORT_SECS).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_spotify_links_in_chat_messages() {
        assert_eq!(
            find_spotify_link(
                "listen to this https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=abc!"
            )
            .as_deref(),
            Some("https://open.spotify.com/track/4uLU6hMCjMI75M1A2tKUQC?si=abc")
        );
        assert_eq!(
            find_spotify_link("<https://spotify.link/AbCd>").as_deref(),
            Some("https://spotify.link/AbCd")
        );
        assert_eq!(
            find_spotify_link("uri: spotify:album:4uLU6hMCjMI75M1A2tKUQC.").as_deref(),
            Some("spotify:album:4uLU6hMCjMI75M1A2tKUQC")
        );
        assert!(find_spotify_link("no music here https://youtube.com/watch").is_none());
    }

    #[test]
    fn finds_playlists_by_name() {
        let list = vec![
            PlaylistRef {
                id: librespot_core::SpotifyId::from_raw(&[1; 16]).unwrap(),
                name: "Linkin Park old - BEST OF".into(),
                tracks: Some(40),
            },
            PlaylistRef {
                id: librespot_core::SpotifyId::from_raw(&[2; 16]).unwrap(),
                name: "Old".into(),
                tracks: None,
            },
        ];
        assert_eq!(
            find_playlist(&list, "old").unwrap().name,
            "Old",
            "exact match wins"
        );
        assert_eq!(find_playlist(&list, "linkin").unwrap().tracks, Some(40));
        assert!(find_playlist(&list, "jazz").is_none());
    }
}
