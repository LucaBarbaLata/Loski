use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, LazyLock},
    time::{Duration, Instant},
};

use anyhow::Result;
use parking_lot::Mutex;
use twilight_interactions::command::{
    AutocompleteValue, CommandInputData, CommandModel, CommandOption, CreateCommand, CreateOption,
    ResolvedUser,
};
use twilight_model::{
    application::{
        command::{Command, CommandOptionChoice, CommandOptionChoiceValue, CommandType},
        interaction::{Interaction, InteractionData},
    },
    channel::message::{Component, component::ButtonStyle},
    id::{
        Id,
        marker::{ChannelMarker, GuildMarker},
    },
};

use super::{
    App, Core, LONG_SECS, Pick, SHORT_SECS,
    cards::{self, Btn, color},
    components, friends,
    icons::{Icon, Icons},
    interact::Ix,
    library, lyrics, mixes, oops, panel, requests, settings, sound, stats, status, voice,
};
use crate::{
    engine::Repeat,
    personality,
    spotify::{
        Lookup, MAX_TRACKS_PER_REQUEST, parse_lookup,
        web::{SearchItem, SearchKind},
    },
    track::TrackInfo,
    util::{SeekTarget, escape_md, fmt_ms, parse_seek, truncate},
};

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "play",
    desc = "Play a song, album, playlist or artist from Spotify"
)]
pub struct PlayCmd {
    /// Song name, or a Spotify link to a song, album, playlist or artist
    #[command(autocomplete = true, max_length = 300)]
    song: String,
    /// Play it right after the current song
    next: Option<bool>,
}

#[derive(CommandModel)]
#[command(autocomplete = true)]
pub struct PlayComplete {
    song: AutocompleteValue<String>,
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "pause", desc = "Pause the music")]
pub struct PauseCmd;

#[derive(CommandModel, CreateCommand)]
#[command(name = "resume", desc = "Resume the music")]
pub struct ResumeCmd;

#[derive(CommandModel, CreateCommand)]
#[command(name = "skip", desc = "Skip the current song, or several")]
pub struct SkipCmd {
    /// How many songs to skip
    #[command(min_value = 1, max_value = 500)]
    count: Option<i64>,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "previous",
    desc = "Go back to the previous song, or restart this one"
)]
pub struct PreviousCmd;

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "seek",
    desc = "Jump to a point in the song, like 1:23, +15 or -10"
)]
pub struct SeekCmd {
    /// A time like 1:23, or +15 / -10 to jump forward or back
    #[command(max_length = 12)]
    position: String,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "nowplaying",
    desc = "Bring the now playing panel back to the bottom of the chat"
)]
pub struct NowPlayingCmd;

#[derive(CommandModel, CreateCommand)]
#[command(name = "queue", desc = "See what's coming up")]
pub struct QueueCmd {
    /// Page number
    #[command(min_value = 1)]
    page: Option<i64>,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "skipto",
    desc = "Jump to a song in the queue, skipping everything before it"
)]
pub struct SkipToCmd {
    /// Position in the queue (see /queue)
    #[command(min_value = 1)]
    position: i64,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "remove",
    desc = "Remove a song from the queue, or a run of songs"
)]
pub struct RemoveCmd {
    /// Position in the queue (see /queue)
    #[command(min_value = 1)]
    position: i64,
    /// Remove everything up to this position too
    #[command(min_value = 1)]
    to: Option<i64>,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "dedupe",
    desc = "Remove songs that are in the queue more than once"
)]
pub struct DedupeCmd;

#[derive(CommandModel, CreateCommand)]
#[command(name = "move", desc = "Move a song to a different spot in the queue")]
pub struct MoveCmd {
    /// Current position (see /queue)
    #[command(min_value = 1)]
    from: i64,
    /// New position (leave it out to play the song next)
    #[command(min_value = 1)]
    to: Option<i64>,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "clear",
    desc = "Clear everything coming up (the current song keeps playing)"
)]
pub struct ClearCmd;

#[derive(CommandModel, CreateCommand)]
#[command(name = "shuffle", desc = "Shuffle what's coming up")]
pub struct ShuffleCmd;

#[derive(CommandOption, CreateOption)]
pub enum RepeatChoice {
    #[option(name = "off", value = "off")]
    Off,
    #[option(name = "this song", value = "track")]
    Track,
    #[option(name = "the queue", value = "queue")]
    Queue,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "repeat",
    desc = "Repeat this song, the whole queue, or nothing"
)]
pub struct RepeatCmd {
    /// What to repeat
    mode: RepeatChoice,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "volume",
    desc = "Check or set the volume (remembered between sessions)"
)]
pub struct VolumeCmd {
    /// 0 to 100
    #[command(min_value = 0, max_value = 100)]
    level: Option<i64>,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "crossfade",
    desc = "Blend songs into each other, like Spotify's crossfade (0 turns it off)"
)]
pub struct CrossfadeCmd {
    /// Seconds of overlap, 0 to 12
    #[command(min_value = 0, max_value = 12)]
    seconds: Option<i64>,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "follow",
    desc = "Play whatever you're listening to on Spotify, in sync"
)]
pub struct FollowCmd;

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "history",
    desc = "What you've listened to: recent plays, or search them"
)]
pub struct HistoryCmd {
    /// Words from a song title or artist
    #[command(max_length = 100)]
    search: Option<String>,
    /// How far back to look
    when: Option<HistoryWhen>,
    /// Songs you played with this friend in the call, or that they asked for
    with: Option<ResolvedUser>,
    /// Only songs friends asked for with /request
    requests: Option<bool>,
}

#[derive(CommandOption, CreateOption)]
pub enum HistoryWhen {
    #[option(name = "today", value = "today")]
    Today,
    #[option(name = "the last 7 days", value = "week")]
    Week,
    #[option(name = "the last 30 days", value = "month")]
    Month,
    #[option(name = "the last year", value = "year")]
    Year,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "stop",
    desc = "Stop the music, clear the queue and leave the voice channel"
)]
pub struct StopCmd;

#[derive(CommandModel, CreateCommand)]
#[command(name = "help", desc = "Everything Loski can do")]
pub struct HelpCmd;

pub(super) fn definitions() -> Vec<Command> {
    [
        PlayCmd::create_command().into(),
        PauseCmd::create_command().into(),
        ResumeCmd::create_command().into(),
        SkipCmd::create_command().into(),
        PreviousCmd::create_command().into(),
        SeekCmd::create_command().into(),
        NowPlayingCmd::create_command().into(),
        QueueCmd::create_command().into(),
        SkipToCmd::create_command().into(),
        RemoveCmd::create_command().into(),
        DedupeCmd::create_command().into(),
        MoveCmd::create_command().into(),
        ClearCmd::create_command().into(),
        ShuffleCmd::create_command().into(),
        RepeatCmd::create_command().into(),
        VolumeCmd::create_command().into(),
        CrossfadeCmd::create_command().into(),
        FollowCmd::create_command().into(),
        HistoryCmd::create_command().into(),
        StopCmd::create_command().into(),
        HelpCmd::create_command().into(),
        library::RadioCmd::create_command().into(),
        library::LikedCmd::create_command().into(),
        library::PlaylistCmd::create_command().into(),
        sound::FilterCmd::create_command().into(),
        sound::SleepCmd::create_command().into(),
        sound::NormalizeCmd::create_command().into(),
        stats::StatsCmd::create_command().into(),
        mixes::MixCmd::create_command().into(),
        status::StatusCmd::create_command().into(),
        requests::RequestCmd::create_command().into(),
        lyrics::LyricsCmd::create_command().into(),
        settings::SettingsCmd::create_command().into(),
        stats::WrappedCmd::create_command().into(),
    ]
    .into_iter()
    .chain(library::message_commands())
    .collect()
}

/// Commands are registered per server so changes show up immediately.
pub async fn register(app: &App, guild: Id<GuildMarker>) -> Result<()> {
    app.http
        .interaction(app.app_id)
        .set_guild_commands(guild, &definitions())
        .await?;
    Ok(())
}

pub async fn run(app: Arc<App>, interaction: Interaction) {
    let ix = Ix::new(app, &interaction);
    let Some(InteractionData::ApplicationCommand(data)) = interaction.data else {
        return;
    };
    // Anyone can ask for a song; co-DJs get what you allowed them.
    if ix.access() < friends::command_needs(&data.name) {
        let _ = ix.refuse().await;
        return;
    }
    if data.kind == CommandType::Message {
        if let Err(e) = library::message_command(&ix, *data).await {
            ix.fail(e).await;
        }
        return;
    }
    let name = data.name.clone();
    let input = CommandInputData::from(*data);

    macro_rules! dispatch {
        ($($name:literal => $model:ty => $handler:ident),* $(,)?) => {
            match name.as_str() {
                $($name => match <$model>::from_interaction(input) {
                    Ok(cmd) => $handler(&ix, cmd).await,
                    Err(e) => Err(anyhow::anyhow!("couldn't read the command: {e}")),
                },)*
                other => Err(anyhow::anyhow!("unknown command /{other}")),
            }
        };
    }

    let result = dispatch! {
        "play" => PlayCmd => play,
        "pause" => PauseCmd => pause,
        "resume" => ResumeCmd => resume,
        "skip" => SkipCmd => skip,
        "previous" => PreviousCmd => previous,
        "seek" => SeekCmd => seek,
        "nowplaying" => NowPlayingCmd => now_playing,
        "queue" => QueueCmd => queue,
        "skipto" => SkipToCmd => skip_to,
        "remove" => RemoveCmd => remove,
        "dedupe" => DedupeCmd => dedupe,
        "move" => MoveCmd => move_track,
        "clear" => ClearCmd => clear,
        "shuffle" => ShuffleCmd => shuffle,
        "repeat" => RepeatCmd => repeat,
        "volume" => VolumeCmd => volume,
        "crossfade" => CrossfadeCmd => crossfade,
        "follow" => FollowCmd => follow,
        "history" => HistoryCmd => history,
        "stop" => StopCmd => stop,
        "help" => HelpCmd => help,
        "radio" => library::RadioCmd => radio,
        "liked" => library::LikedCmd => liked,
        "playlist" => library::PlaylistCmd => playlist,
        "filter" => sound::FilterCmd => filter,
        "sleep" => sound::SleepCmd => sleep,
        "normalize" => sound::NormalizeCmd => normalize,
        "stats" => stats::StatsCmd => stats,
        "mix" => mixes::MixCmd => mix,
        "status" => status::StatusCmd => status,
        "request" => requests::RequestCmd => request,
        "lyrics" => lyrics::LyricsCmd => lyrics_cmd,
        "settings" => settings::SettingsCmd => settings_cmd,
        "wrapped" => stats::WrappedCmd => wrapped,
    };
    if let Err(e) = result {
        ix.fail(e).await;
    }
}

type SearchHits = Arc<Vec<TrackInfo>>;

/// Search results are cached briefly; autocomplete fires on every keystroke.
static SEARCH_CACHE: LazyLock<Mutex<HashMap<String, (Instant, SearchHits)>>> =
    LazyLock::new(Default::default);
const SEARCH_CACHE_TTL: Duration = Duration::from_secs(120);

pub(super) async fn search(core: &Core, query: &str) -> Result<SearchHits> {
    let key = query.trim().to_lowercase();
    if let Some((at, hits)) = SEARCH_CACHE.lock().get(&key)
        && at.elapsed() < SEARCH_CACHE_TTL
    {
        return Ok(hits.clone());
    }
    let hits = Arc::new(core.spotify.web.search_tracks(query, 10).await?);
    let mut cache = SEARCH_CACHE.lock();
    if cache.len() > 200 {
        cache.retain(|_, (at, _)| at.elapsed() < SEARCH_CACHE_TTL);
    }
    cache.insert(key, (Instant::now(), hits.clone()));
    Ok(hits)
}

pub(super) fn choice(name: String, value: String) -> CommandOptionChoice {
    CommandOptionChoice {
        name: truncate(&name, 100),
        name_localizations: None,
        value: CommandOptionChoiceValue::String(value),
    }
}

pub async fn autocomplete(app: Arc<App>, interaction: Interaction) {
    let ix = Ix::new(app, &interaction);
    let Some(InteractionData::ApplicationCommand(data)) = interaction.data else {
        return;
    };
    // Your recent plays are suggested to you and co-DJs only.
    let recent = ix.access() >= friends::Access::Queue;
    let choices = if ix.access() < friends::command_needs(&data.name) {
        Vec::new()
    } else if data.name == "request" {
        requests::suggestions(&ix.app, CommandInputData::from(*data)).await
    } else if data.name == "mix" {
        mixes::autocomplete(&ix.app, &data)
    } else if data.name == "lyrics" {
        lyrics::suggestions(&ix.app, CommandInputData::from(*data), recent).await
    } else if data.name == "play" {
        match PlayComplete::from_interaction(CommandInputData::from(*data)) {
            Ok(PlayComplete {
                song: AutocompleteValue::Focused(partial),
            }) => song_suggestions(&ix.app, &partial, recent).await,
            _ => Vec::new(),
        }
    } else {
        let name = data.name.clone();
        library::autocomplete(&ix.app, &name, *data).await
    };
    let _ = ix.autocomplete(choices).await;
}

/// Songs matching what's typed so far; with nothing typed, your recent
/// plays when `recent` allows.
pub(super) async fn song_suggestions(
    app: &App,
    partial: &str,
    recent: bool,
) -> Vec<CommandOptionChoice> {
    let Some(core) = app.core.get() else {
        return vec![choice(
            "Log in to Spotify first (check your DMs)".into(),
            "login".into(),
        )];
    };
    let partial = partial.trim();

    if partial.is_empty() {
        if !recent {
            return Vec::new();
        }
        // Nothing typed yet: offer recent plays.
        let mut seen = HashSet::new();
        return app
            .store
            .recent_history(50)
            .into_iter()
            .filter(|e| seen.insert(e.uri.clone()))
            .take(10)
            .map(|e| choice(format!("↺ {} — {}", e.title, e.artists), e.uri))
            .collect();
    }

    if !matches!(parse_lookup(partial), Lookup::Text(_)) {
        // A link: offer it back without the tracking junk (values max 100 chars).
        let clean = partial.split('?').next().unwrap_or(partial);
        return vec![choice(format!("🔗 {clean}"), truncate(clean, 100))];
    }

    match search(core, partial).await {
        Ok(hits) => hits
            .iter()
            .map(|t| {
                choice(
                    format!(
                        "{} — {} · {}",
                        t.title,
                        t.artists_joined(),
                        fmt_ms(t.duration_ms as u64)
                    ),
                    t.spotify_uri(),
                )
            })
            .collect(),
        Err(e) => {
            tracing::debug!("autocomplete search failed: {e:#}");
            Vec::new()
        }
    }
}

pub(super) fn guild(ix: &Ix) -> Result<Id<GuildMarker>> {
    ix.guild_id
        .ok_or_else(|| oops("This only works in a server."))
}

/// Where Loski goes to play: a voice channel in a server.
pub type Target = (Id<GuildMarker>, Id<ChannelMarker>);

/// Where Loski should play for this person: your voice channel; for a
/// co-DJ, the channel Loski's already in, or theirs.
pub(super) fn require_voice(ix: &Ix) -> Result<Target> {
    let app = &ix.app;
    let guild = guild(ix)?;
    let channel = if ix.is_owner() {
        voice::user_channel(app, guild, app.owner_id())
    } else {
        let here = app
            .voice
            .guild()
            .filter(|g| *g == guild)
            .and(app.voice.channel());
        here.or_else(|| ix.user_id.and_then(|u| voice::user_channel(app, guild, u)))
    };
    let channel = channel.ok_or_else(|| oops(personality::join_voice_first()))?;
    Ok((guild, channel))
}

async fn play(ix: &Ix, cmd: PlayCmd) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    let target = require_voice(ix)?;
    ix.defer().await?;
    if let Some(channel) = ix.channel_id {
        app.ui.set_home(channel);
    }
    let placement = if cmd.next.unwrap_or(false) {
        Placement::Next
    } else {
        Placement::End
    };

    let (tracks, source, truncated) = match parse_lookup(&cmd.song) {
        Lookup::Text(query) => {
            let results = search(&core, &query)
                .await
                .map_err(|e| oops(format!("Search failed: {e}")))?;
            if results.is_empty() {
                return Err(oops(personality::nothing_found()));
            }
            let results = results.iter().take(PICKER_RESULTS).cloned().collect();
            return show_picker(ix, &query, results).await;
        }
        lookup => {
            let resolved = core
                .spotify
                .resolve(lookup)
                .await
                .map_err(|e| oops(format!("Hmm, {e}")))?
                .ok_or_else(|| oops(personality::nothing_found()))?;
            (resolved.tracks, resolved.source, resolved.truncated)
        }
    };

    let note = truncated
        .then(|| format!("Only the first {MAX_TRACKS_PER_REQUEST} songs, to keep things snappy."));
    let card = enqueue(app, &core, target, tracks, placement, source, note).await?;
    ix.reply_temp(vec![card], SHORT_SECS).await
}

/// How many results the search card shows (with covers).
pub(super) const PICKER_RESULTS: usize = 5;

async fn show_picker(ix: &Ix, query: &str, results: Vec<TrackInfo>) -> Result<()> {
    let nonce = ix.id.get();
    let pick = Pick {
        query: query.to_string(),
        kind: SearchKind::Songs,
        items: results.into_iter().map(SearchItem::from_track).collect(),
        selected: 0,
        created: Instant::now(),
    };
    let picker = picker_card(&ix.app.icons, &pick, nonce);
    ix.app.picks.lock().insert(nonce, pick);
    ix.reply(vec![picker]).await?;

    // Unanswered search cards clean themselves up.
    let app = ix.app.clone();
    let token = ix.token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(90)).await;
        if app.picks.lock().remove(&nonce).is_some() {
            let _ = app
                .http
                .interaction(app.app_id)
                .delete_response(&token)
                .await;
        }
    });
    Ok(())
}

type ItemHits = Arc<Vec<SearchItem>>;

/// Album, playlist and artist searches for the search card's tabs.
type ItemKey = (String, &'static str);
static ITEM_CACHE: LazyLock<Mutex<HashMap<ItemKey, (Instant, ItemHits)>>> =
    LazyLock::new(Default::default);

pub(super) async fn search_items(core: &Core, query: &str, kind: SearchKind) -> Result<ItemHits> {
    let key = (query.trim().to_lowercase(), kind.key());
    if let Some((at, hits)) = ITEM_CACHE.lock().get(&key)
        && at.elapsed() < SEARCH_CACHE_TTL
    {
        return Ok(hits.clone());
    }
    let hits = Arc::new(
        core.spotify
            .web
            .search(query, kind, PICKER_RESULTS as u8)
            .await?,
    );
    let mut cache = ITEM_CACHE.lock();
    if cache.len() > 200 {
        cache.retain(|_, (at, _)| at.elapsed() < SEARCH_CACHE_TTL);
    }
    cache.insert(key, (Instant::now(), hits.clone()));
    Ok(hits)
}

/// The search card: tabs for what to search, each result with its cover, a
/// menu to choose one, and buttons for how to play it.
pub(super) fn picker_card(icons: &Icons, pick: &Pick, nonce: u64) -> Component {
    let what = match pick.kind {
        SearchKind::Songs => "song",
        SearchKind::Albums => "album",
        SearchKind::Playlists => "playlist",
        SearchKind::Artists => "artist",
    };
    let mut components = vec![
        cards::text(format!(
            "{} **Results for “{}”**\n-# {}",
            icons.md(Icon::Search),
            escape_md(&truncate(&pick.query, 80)),
            personality::pick_prompt()
        )),
        cards::row(
            SearchKind::ALL
                .iter()
                .map(|&kind| {
                    let style = if kind == pick.kind {
                        ButtonStyle::Primary
                    } else {
                        ButtonStyle::Secondary
                    };
                    Btn::label(&format!("picktab:{nonce}:{}", kind.key()), kind.label())
                        .style(style)
                        .build(icons)
                })
                .collect(),
        ),
    ];

    if pick.items.is_empty() {
        components.push(cards::text(format!(
            "No {what}s found for that. Try another tab or different words."
        )));
    }
    for (i, item) in pick.items.iter().enumerate() {
        let marker = if i == pick.selected { "▸ " } else { "" };
        let explicit = if item.explicit { "  `E`" } else { "" };
        let line = format!(
            "{marker}`{}` **{}**{explicit}\n{}",
            i + 1,
            escape_md(&truncate(&item.title, 70)),
            escape_md(&truncate(&item.subtitle, 110)),
        );
        components.push(cards::text_with_cover(line, item.cover.as_deref()));
    }

    if !pick.items.is_empty() {
        let options = pick
            .items
            .iter()
            .enumerate()
            .map(|(i, item)| {
                let mut option = cards::option(
                    truncate(&format!("{}. {}", i + 1, item.title), 100),
                    i.to_string(),
                    Some(truncate(&item.subtitle, 100)),
                    None,
                );
                option.default = i == pick.selected;
                option
            })
            .collect();
        components.push(cards::divider());
        components.push(cards::row(vec![cards::select(
            &format!("pick:{nonce}"),
            &format!(
                "Choose a{} {what}",
                if what == "album" || what == "artist" {
                    "n"
                } else {
                    ""
                }
            ),
            false,
            options,
        )]));
    }
    let none = pick.items.is_empty();
    components.push(cards::row(vec![
        Btn::icon(&format!("pickgo:{nonce}:now"), Icon::Play)
            .with_label("Play now")
            .style(ButtonStyle::Primary)
            .disabled(none)
            .build(icons),
        Btn::icon(&format!("pickgo:{nonce}:next"), Icon::PlayNext)
            .with_label("Play next")
            .disabled(none)
            .build(icons),
        Btn::icon(&format!("pickgo:{nonce}:end"), Icon::Add)
            .with_label("Add to queue")
            .disabled(none)
            .build(icons),
        Btn::label(&format!("pickcancel:{nonce}"), "Cancel").build(icons),
    ]));
    cards::card(Some(color::BRAND), components)
}

/// Where new songs go.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Start playing straight away.
    Now,
    /// Right after the current song.
    Next,
    /// At the end of the queue.
    End,
}

/// Joins the voice channel you're in (or moves there).
pub async fn join_target(app: &App, core: &Core, (guild, channel): Target) -> Result<()> {
    voice::join(app, core, guild, channel).await
}

/// Joins the channel, queues the songs and returns the confirmation card.
pub async fn enqueue(
    app: &App,
    core: &Core,
    target: Target,
    tracks: Vec<TrackInfo>,
    placement: Placement,
    source: Option<String>,
    note: Option<String>,
) -> Result<Component> {
    if tracks.is_empty() {
        return Err(oops("None of those songs can be played here, sorry."));
    }
    join_target(app, core, target).await?;

    let mut tracks = tracks;
    if let Some(from) = source.as_deref() {
        let from: Arc<str> = Arc::from(from);
        for track in &mut tracks {
            // A song may say already where it came from (whose request it was).
            track.from.get_or_insert_with(|| from.clone());
        }
    }
    let count = tracks.len();
    let first = tracks[0].clone();
    let result = match placement {
        Placement::Now => core.engine.play_now(tracks),
        Placement::Next => core.engine.enqueue(tracks, true),
        Placement::End => core.engine.enqueue(tracks, false),
    };

    let mut text = if count == 1 {
        let song = format!(
            "**[{}]({})** by {}",
            escape_md(&first.title),
            first.url(),
            escape_md(&first.artists_joined())
        );
        if result.started {
            format!("{} {song}", personality::now_starting())
        } else {
            let place = if result.position == 1 {
                "up next".to_string()
            } else {
                format!("#{} in the queue", result.position)
            };
            format!("{} {song} · {place}", personality::queued())
        }
    } else {
        format!(
            "{} Added **{count} songs** from {}.",
            personality::queued(),
            source.as_deref().unwrap_or("that link")
        )
    };
    if let Some(note) = note {
        text.push_str(&format!("\n-# {note}"));
    }
    if let Some(mode) = result.left_mode {
        text.push_str(&format!(
            "\n-# {mode} is off now; this is the Discord queue."
        ));
    }
    let cover = if count == 1 {
        first.cover_thumb_url.as_deref()
    } else {
        None
    };
    Ok(cards::toast(&app.icons, &text, cover, color::SUCCESS))
}

async fn pause(ix: &Ix, _: PauseCmd) -> Result<()> {
    let core = ix.app.core()?;
    if !core.engine.pause() {
        return Err(oops(personality::nothing_playing()));
    }
    ix.reply_temp(vec![cards::info(&ix.app.icons, "Paused.")], SHORT_SECS)
        .await
}

async fn resume(ix: &Ix, _: ResumeCmd) -> Result<()> {
    let core = ix.app.core()?;
    if !core.engine.resume() {
        return Err(oops("Nothing's paused."));
    }
    ix.reply_temp(vec![cards::info(&ix.app.icons, "Back on.")], SHORT_SECS)
        .await
}

async fn skip(ix: &Ix, cmd: SkipCmd) -> Result<()> {
    let core = ix.app.core()?;
    if core.engine.is_following() {
        return Err(oops(
            "I'm following your Spotify. Skip there, or turn it off with `/follow`.",
        ));
    }
    let count = cmd.count.unwrap_or(1).max(1) as usize;
    let skipped = core
        .engine
        .skip(count)
        .ok_or_else(|| oops(personality::nothing_playing()))?;
    let text = if count > 1 {
        format!("Skipped {count} songs.")
    } else {
        format!(
            "{} (**{}**)",
            personality::skipped(),
            escape_md(&skipped.title)
        )
    };
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}

async fn previous(ix: &Ix, _: PreviousCmd) -> Result<()> {
    let core = ix.app.core()?;
    if !core.engine.previous() {
        return Err(oops("There's nothing to go back to."));
    }
    ix.reply_temp(vec![cards::info(&ix.app.icons, "Rewound.")], SHORT_SECS)
        .await
}

/// Jumps to a time like "1:23", "+15" or "-10" and returns where that is.
pub(super) fn seek_to(core: &Core, input: &str) -> Result<u32> {
    let target = parse_seek(input)
        .ok_or_else(|| oops("Try something like `1:23`, `95`, `+15` or `-10`."))?;
    let now = core
        .engine
        .position_ms()
        .ok_or_else(|| oops(personality::nothing_playing()))?;
    let to = match target {
        SeekTarget::Absolute(ms) => ms,
        SeekTarget::Forward(ms) => now.saturating_add(ms),
        SeekTarget::Back(ms) => now.saturating_sub(ms),
    };
    if !core.engine.seek(to) {
        return Err(oops(personality::nothing_playing()));
    }
    Ok(to)
}

async fn seek(ix: &Ix, cmd: SeekCmd) -> Result<()> {
    let core = ix.app.core()?;
    let to = seek_to(&core, &cmd.position)?;
    let text = format!("Jumped to `{}`.", fmt_ms(to as u64));
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}

async fn now_playing(ix: &Ix, _: NowPlayingCmd) -> Result<()> {
    let core = ix.app.core()?;
    ix.defer_ephemeral().await?;
    if let Some(channel) = ix.channel_id {
        ix.app.ui.set_home(channel);
    }
    panel::repost(&ix.app, &core).await;
    ix.reply(vec![cards::info(&ix.app.icons, "Here's the panel 👇")])
        .await?;
    ix.delete_later(5);
    Ok(())
}

async fn queue(ix: &Ix, cmd: QueueCmd) -> Result<()> {
    let core = ix.app.core()?;
    let view = core.engine.queue_view();
    if view.following || view.connect {
        return Err(oops(
            "Your Spotify app is in charge right now, so the queue is there. `/play` something to switch back.",
        ));
    }
    let page = cmd.page.unwrap_or(1).saturating_sub(1).max(0) as usize;
    let card = panel::queue_card(&ix.app.icons, ix.app.ui.color(), &view, page, None);
    ix.reply_temp(vec![card], LONG_SECS).await
}

async fn skip_to(ix: &Ix, cmd: SkipToCmd) -> Result<()> {
    let core = ix.app.core()?;
    let position = cmd.position.max(1) as usize;
    let track = core
        .engine
        .skip_to(position)
        .ok_or_else(|| oops(format!("There's no song #{position} in the queue.")))?;
    let text = format!("Jumping to **{}**.", escape_md(&track.title));
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}

async fn remove(ix: &Ix, cmd: RemoveCmd) -> Result<()> {
    let core = ix.app.core()?;
    let undo = core.engine.save_undo();
    let position = cmd.position.max(1) as usize;
    let text = match cmd.to.map(|to| to.max(1) as usize) {
        Some(to) if to != position => {
            let (first, last) = (position.min(to), position.max(to));
            match core.engine.remove_range(first, last) {
                0 => return Err(oops(format!("There's no song #{first} in the queue."))),
                1 => "Removed 1 song.".to_string(),
                n => format!("Removed {n} songs (#{first} to #{}).", first + n - 1),
            }
        }
        _ => {
            let track = core
                .engine
                .remove(position)
                .ok_or_else(|| oops(format!("There's no song #{position} in the queue.")))?;
            format!("Removed **{}**.", escape_md(&track.title))
        }
    };
    reply_undoable(ix, &text, undo).await
}

async fn dedupe(ix: &Ix, _: DedupeCmd) -> Result<()> {
    let core = ix.app.core()?;
    let undo = core.engine.save_undo();
    let text = match core.engine.dedupe() {
        0 => {
            let card = cards::info(&ix.app.icons, "No repeats in the queue.");
            return ix.reply_temp(vec![card], SHORT_SECS).await;
        }
        1 => "Removed 1 song that was already in the queue.".to_string(),
        n => format!("Removed {n} songs that were already in the queue."),
    };
    reply_undoable(ix, &text, undo).await
}

async fn move_track(ix: &Ix, cmd: MoveCmd) -> Result<()> {
    let core = ix.app.core()?;
    let from = cmd.from.max(1) as usize;
    let to = cmd.to.map_or(1, |to| to.max(1) as usize);
    let track = core
        .engine
        .move_track(from, to)
        .ok_or_else(|| oops("Those positions aren't both in the queue."))?;
    let text = if to == 1 {
        format!("**{}** plays next.", escape_md(&track.title))
    } else {
        format!("Moved **{}** to #{to}.", escape_md(&track.title))
    };
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}

async fn clear(ix: &Ix, _: ClearCmd) -> Result<()> {
    let core = ix.app.core()?;
    let undo = core.engine.save_undo();
    let text = match core.engine.clear() {
        0 => {
            let card = cards::info(&ix.app.icons, "The queue was already empty.");
            return ix.reply_temp(vec![card], SHORT_SECS).await;
        }
        1 => "Cleared 1 song.".to_string(),
        n => format!("Cleared {n} songs."),
    };
    reply_undoable(ix, &text, undo).await
}

async fn shuffle(ix: &Ix, _: ShuffleCmd) -> Result<()> {
    let core = ix.app.core()?;
    let undo = core.engine.save_undo();
    let count = core.engine.shuffle();
    if count < 2 {
        return Err(oops("Need at least two songs in the queue to shuffle."));
    }
    let text = format!("{} ({count} songs)", personality::shuffled());
    reply_undoable(ix, &text, undo).await
}

async fn repeat(ix: &Ix, cmd: RepeatCmd) -> Result<()> {
    let core = ix.app.core()?;
    let mode = match cmd.mode {
        RepeatChoice::Off => Repeat::Off,
        RepeatChoice::Track => Repeat::Track,
        RepeatChoice::Queue => Repeat::Queue,
    };
    core.engine.set_repeat(mode);
    let text = match mode {
        Repeat::Off => "Repeat is off.",
        Repeat::Track => "Repeating this song.",
        Repeat::Queue => "Repeating the queue.",
    };
    ix.reply_temp(vec![cards::info(&ix.app.icons, text)], SHORT_SECS)
        .await
}

async fn volume(ix: &Ix, cmd: VolumeCmd) -> Result<()> {
    let core = ix.app.core()?;
    let text = match cmd.level {
        Some(level) => {
            let level = core.engine.set_volume(level.clamp(0, 100) as u8);
            format!("Volume set to **{level}%**.")
        }
        None => format!("Volume is **{}%**.", core.engine.volume()),
    };
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}

async fn crossfade(ix: &Ix, cmd: CrossfadeCmd) -> Result<()> {
    let core = ix.app.core()?;
    let text = match cmd.seconds {
        Some(0) => {
            core.engine.set_crossfade(0);
            "Crossfade is off. Songs play gapless, back to back.".to_string()
        }
        Some(secs) => {
            let secs = secs.clamp(1, 12) as u8;
            core.engine.set_crossfade(secs);
            format!("Crossfading songs over **{secs}s**.")
        }
        None => match core.engine.crossfade() {
            0 => "Crossfade is off. Turn it on with `/crossfade seconds:6`.".to_string(),
            secs => format!("Crossfade is **{secs}s**."),
        },
    };
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}

async fn follow(ix: &Ix, _: FollowCmd) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    if core.engine.is_following() {
        core.engine.set_following(false);
        let card = cards::info(&app.icons, "Stopped following your Spotify.");
        return ix.reply_temp(vec![card], SHORT_SECS).await;
    }

    let (guild, channel) = require_voice(ix)?;
    ix.defer().await?;
    if let Some(home) = ix.channel_id {
        app.ui.set_home(home);
    }
    voice::join(app, &core, guild, channel).await?;
    core.engine.set_following(true);
    panel::repost(app, &core).await;
    let card = cards::toast(
        &app.icons,
        "**Following your Spotify.** Whatever you play there plays here, in sync.\n\
         -# Mute Spotify on your device so you don't hear it twice. `/follow` again to stop.",
        None,
        color::SUCCESS,
    );
    ix.reply_temp(vec![card], super::ERROR_SECS).await
}

async fn history(ix: &Ix, cmd: HistoryCmd) -> Result<()> {
    let app = &ix.app;
    let now = stats::now(app);
    let (since, when) = match cmd.when {
        Some(HistoryWhen::Today) => (
            now.date_naive()
                .and_hms_opt(0, 0, 0)
                .and_then(|midnight| midnight.and_local_timezone(now.timezone()).earliest())
                .map(|t| t.timestamp()),
            Some("today"),
        ),
        Some(HistoryWhen::Week) => (Some(now.timestamp() - 7 * 86_400), Some("the last 7 days")),
        Some(HistoryWhen::Month) => (
            Some(now.timestamp() - 30 * 86_400),
            Some("the last 30 days"),
        ),
        Some(HistoryWhen::Year) => (Some(now.timestamp() - 365 * 86_400), Some("the last year")),
        None => (None, None),
    };
    let search = cmd
        .search
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut about = Vec::new();
    if let Some(words) = &search {
        about.push(format!("“{}”", escape_md(&truncate(words, 40))));
    }
    about.extend(when.map(str::to_string));
    if let Some(friend) = &cmd.with {
        let name = friend
            .member
            .as_ref()
            .and_then(|m| m.nick.clone())
            .or_else(|| friend.resolved.global_name.clone())
            .unwrap_or_else(|| friend.resolved.name.clone());
        about.push(format!("with **{}**", escape_md(&truncate(&name, 32))));
    }
    if cmd.requests == Some(true) {
        about.push("friends' requests".to_string());
    }
    let filter = crate::stats::HistoryFilter {
        search,
        since,
        with: cmd.with.map(|f| f.resolved.id.get()),
        requests: cmd.requests == Some(true),
    };
    *app.history_view.lock() = (filter, about.join(" · "));
    let card = components::history_card(app, None);
    ix.reply_temp(vec![card], LONG_SECS * 2).await
}

async fn stop(ix: &Ix, _: StopCmd) -> Result<()> {
    if ix.app.voice.guild().is_none() {
        return Err(oops("I'm not in a voice channel."));
    }
    voice::send_away(&ix.app, None).await;
    let card = cards::info(&ix.app.icons, personality::goodbye());
    ix.reply_temp(vec![card], SHORT_SECS).await
}

async fn help(ix: &Ix, _: HelpCmd) -> Result<()> {
    ix.reply_temp(vec![help_card(&ix.app.icons)], LONG_SECS)
        .await
}

/// Every command, grouped by what you're trying to do. Also the welcome DM.
pub(super) fn help_card(icons: &Icons) -> Component {
    let group = |title: &str, lines: &[&str]| {
        format!(
            "-# {title}
{}",
            lines.join(
                "
"
            )
        )
    };
    cards::card(
        Some(color::BRAND),
        vec![
            cards::text(format!(
                "{} **Loski**
Your private Spotify DJ. You control me, along with any co-DJs you pick in `/settings`; everyone in the call can listen, like songs and vote to skip.",
                icons.md(Icon::Music)
            )),
            cards::divider(),
            cards::text(group(
                "PLAY",
                &[
                    "`/play` a song, album, playlist or artist. Start typing for suggestions, or leave it empty for recent plays.",
                    "`/liked` your Liked Songs · `/playlist` one of your playlists · `/radio` similar songs",
                    "Right-click a message with a Spotify link → Apps → **Play with Loski**.",
                ],
            )),
            cards::text(group(
                "FROM YOUR PHONE",
                &[
                    "Pick **Loski** in the Spotify app's device list and your phone becomes the remote.",
                    "`/follow` mirrors whatever you're playing on Spotify instead.",
                ],
            )),
            cards::text(group(
                "CONTROL",
                &[
                    "`/pause` `/resume` `/skip` `/previous` `/seek` `/volume`",
                    "The panel's buttons do the same, and its More menu jumps 15 seconds or to a time you type.",
                ],
            )),
            cards::text(group(
                "QUEUE",
                &[
                    "`/queue` see what's next, then pick a song to play, move or remove.",
                    "`/skipto` `/move` `/remove` (one song or a run) `/dedupe` `/clear` `/shuffle` `/repeat`",
                    "`/mix save` keeps the queue under a name; `/mix load` brings it back.",
                ],
            )),
            cards::text(group(
                "SOUND",
                &[
                    "`/filter` bass boost, nightcore, slowed, 8D… (also the panel's Sound menu)",
                    "`/crossfade` blend songs; albums stay gapless · `/normalize` level loudness",
                    "`/sleep` fade out and leave later, or after this song",
                ],
            )),
            cards::text(group(
                "YOU",
                &[
                    "`/stats` listening time, top songs, artists and genres, streaks, and who you listen with",
                    "`/history` recent plays, or search them by words, time or friend; play any again",
                    "`/wrapped` your year in pictures (it also arrives on January 1st)",
                    "Every Monday your **Blend** mix mixes your top songs with what friends liked: `/mix load name:Blend`",
                ],
            )),
            cards::text(group(
                "FRIENDS",
                &[
                    "Anyone in the server can `/request` a song; requests wait on one card for you or a co-DJ.",
                    "Everyone in the call can press ♥ on the panel, and Skip to vote. `/liked from:friends` plays what they liked.",
                    "Co-DJs, by name or with a DJ role: `/settings` → Friends.",
                ],
            )),
            cards::text(group(
                "MORE",
                &[
                    "`/lyrics` sing along, line by line · `/nowplaying` bring the panel back down",
                    "`/settings` the panel, leaving, 24/7, sound and requests, in one card",
                    "`/status` check on me · `/stop` leave the call",
                ],
            )),
            cards::divider(),
            cards::text(
                "-# I leave a minute after you do, and a few minutes after the queue runs out (or stay with 24/7, in `/settings`); the panel then sums up the session. On the 1st of each month I DM you a recap.",
            ),
        ],
    )
}

async fn radio(ix: &Ix, cmd: library::RadioCmd) -> Result<()> {
    library::radio(ix, cmd).await
}

async fn liked(ix: &Ix, cmd: library::LikedCmd) -> Result<()> {
    library::liked(ix, cmd).await
}

async fn playlist(ix: &Ix, cmd: library::PlaylistCmd) -> Result<()> {
    library::playlist(ix, cmd).await
}

async fn filter(ix: &Ix, cmd: sound::FilterCmd) -> Result<()> {
    sound::filter(ix, cmd).await
}

async fn sleep(ix: &Ix, cmd: sound::SleepCmd) -> Result<()> {
    sound::sleep(ix, cmd).await
}

async fn normalize(ix: &Ix, cmd: sound::NormalizeCmd) -> Result<()> {
    sound::normalize(ix, cmd).await
}

async fn stats(ix: &Ix, cmd: stats::StatsCmd) -> Result<()> {
    stats::stats(ix, cmd).await
}

async fn mix(ix: &Ix, cmd: mixes::MixCmd) -> Result<()> {
    mixes::mix(ix, cmd).await
}

async fn status(ix: &Ix, cmd: status::StatusCmd) -> Result<()> {
    status::status(ix, cmd).await
}

async fn request(ix: &Ix, cmd: requests::RequestCmd) -> Result<()> {
    requests::request(ix, cmd).await
}

async fn lyrics_cmd(ix: &Ix, cmd: lyrics::LyricsCmd) -> Result<()> {
    lyrics::lyrics(ix, cmd).await
}

async fn settings_cmd(ix: &Ix, cmd: settings::SettingsCmd) -> Result<()> {
    settings::settings(ix, cmd).await
}

async fn wrapped(ix: &Ix, cmd: stats::WrappedCmd) -> Result<()> {
    stats::wrapped(ix, cmd).await
}

/// How long a queue change's Undo button stays.
const UNDO_SECS: u64 = 15;

/// The confirmation for a change to the queue, with an Undo button while it lasts.
async fn reply_undoable(ix: &Ix, text: &str, undo: u64) -> Result<()> {
    let icons = &ix.app.icons;
    let card = cards::card(
        None,
        vec![
            cards::text(format!("{} {text}", icons.md(Icon::Info))),
            cards::row(vec![
                Btn::icon(&format!("loski:undo:{undo}"), Icon::Undo)
                    .with_label("Undo")
                    .build(icons),
            ]),
        ],
    );
    ix.reply_temp(vec![card], UNDO_SECS).await
}
