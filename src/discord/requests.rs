//! Song requests: anyone in the server can `/request` a song. Waiting
//! requests gather on one card, where you (or a co-DJ) play them next, add
//! them to the queue or decline them. A co-DJ's own requests go straight in.
//!
//! Party mode: everyone in the call votes on the waiting requests, and the
//! one with the most votes goes up next, one at a time, by itself.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use parking_lot::Mutex;
use twilight_interactions::command::{
    AutocompleteValue, CommandInputData, CommandModel, CreateCommand,
};
use twilight_model::{
    application::command::CommandOptionChoice,
    channel::message::{Component, component::ButtonStyle},
    id::{
        Id,
        marker::{ChannelMarker, MessageMarker},
    },
};

use super::{
    App, SHORT_SECS,
    cards::{self, Btn, color},
    commands::{self, Placement},
    friends::{self, Access},
    icons::{Icon, Icons},
    interact::Ix,
    oops,
};
use crate::{
    personality,
    spotify::{Lookup, auth::now_unix, parse_lookup},
    track::TrackInfo,
    util::{escape_md, fmt_ms, truncate},
};

/// How long a request waits for an answer.
const TTL: Duration = Duration::from_secs(30 * 60);
/// Time between two requests from the same person.
const COOLDOWN: Duration = Duration::from_secs(20);
/// Unanswered requests one person can have at once.
const MAX_OPEN: usize = 5;
/// Requests listed on the card with their covers; the rest are in its menu.
const SHOWN: usize = 6;
/// What a select menu holds.
const MENU_MAX: usize = 25;

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "request",
    desc = "Ask for a song to be played (whoever runs Loski decides)"
)]
pub struct RequestCmd {
    /// Song name or Spotify link
    #[command(autocomplete = true, max_length = 300)]
    song: String,
}

#[derive(CommandModel)]
#[command(autocomplete = true)]
pub struct RequestComplete {
    song: AutocompleteValue<String>,
}

/// A song someone asked for.
pub struct Request {
    pub nonce: u64,
    pub track: TrackInfo,
    pub by: String,
    pub by_id: u64,
    /// When it was asked, as a Unix timestamp (for the card).
    pub asked_at: i64,
    pub at: Instant,
    /// Who voted for it in party mode, starting with whoever asked.
    pub votes: HashSet<u64>,
}

#[derive(Default)]
pub struct Requests {
    state: Mutex<State>,
    /// One card update at a time, so two quick requests don't post two cards.
    drawing: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct State {
    open: Vec<Request>,
    last: HashMap<u64, Instant>,
    /// The card listing them.
    card: Option<(Id<ChannelMarker>, Id<MessageMarker>)>,
    /// The request picked in the card's menu.
    picked: Option<u64>,
}

impl State {
    fn tidy(&mut self) {
        self.open.retain(|r| r.at.elapsed() < TTL);
        if self
            .picked
            .is_some_and(|n| !self.open.iter().any(|r| r.nonce == n))
        {
            self.picked = None;
        }
    }

    fn take(&mut self, nonce: u64) -> Option<Request> {
        let index = self.open.iter().position(|r| r.nonce == nonce)?;
        Some(self.open.remove(index))
    }

    /// The request with the most votes; on a tie, the one asked first.
    fn top(&self) -> Option<u64> {
        party_order(&self.open).first().map(|r| r.nonce)
    }
}

/// Requests as party mode ranks them: most votes first, then oldest.
pub(super) fn party_order(open: &[Request]) -> Vec<&Request> {
    let mut ranked: Vec<&Request> = open.iter().collect();
    // A stable sort keeps the order they were asked in among equals.
    ranked.sort_by_key(|r| std::cmp::Reverse(r.votes.len()));
    ranked
}

/// Search suggestions while typing. Unlike `/play`, an empty box suggests
/// nothing: your recent plays stay yours.
pub async fn suggestions(app: &App, input: CommandInputData<'_>) -> Vec<CommandOptionChoice> {
    match RequestComplete::from_interaction(input) {
        Ok(RequestComplete {
            song: AutocompleteValue::Focused(partial),
        }) if !partial.trim().is_empty() => commands::song_suggestions(app, &partial, false).await,
        _ => Vec::new(),
    }
}

async fn find_song(app: &App, song: &str) -> Result<TrackInfo> {
    let core = app.core()?;
    match parse_lookup(song) {
        Lookup::Text(query) => commands::search(&core, &query)
            .await
            .map_err(|e| oops(format!("Search failed: {e}")))?
            .first()
            .cloned()
            .ok_or_else(|| oops(personality::nothing_found())),
        Lookup::Track(id) => core
            .spotify
            .resolve(Lookup::Track(id))
            .await
            .map_err(|e| oops(format!("Hmm, {e}")))?
            .and_then(|r| r.tracks.into_iter().next())
            .ok_or_else(|| oops(personality::nothing_found())),
        _ => Err(oops(
            "Requests are one song at a time. Paste a song link or type its name.",
        )),
    }
}

pub async fn request(ix: &Ix, cmd: RequestCmd) -> Result<()> {
    let app = &ix.app;
    if ix.access() >= Access::Queue {
        // You and co-DJs don't need to ask.
        let target = commands::require_voice(ix)?;
        ix.defer().await?;
        let track = find_song(app, &cmd.song).await?;
        let core = app.core()?;
        let card =
            commands::enqueue(app, &core, target, vec![track], Placement::End, None, None).await?;
        return ix.reply_temp(vec![card], SHORT_SECS).await;
    }
    if app.prefs().requests_off {
        return Err(oops("Song requests are off right now."));
    }
    let by_id = ix.user_id.map_or(0, |u| u.get());
    {
        let mut st = app.requests.state.lock();
        st.tidy();
        if st.open.iter().filter(|r| r.by_id == by_id).count() >= MAX_OPEN {
            return Err(oops(
                "You have a few requests waiting already. Give them a moment.",
            ));
        }
        if let Some(at) = st.last.get(&by_id)
            && at.elapsed() < COOLDOWN
        {
            return Err(oops("One at a time. Try again in a few seconds."));
        }
    }
    ix.defer().await?;
    let track = find_song(app, &cmd.song).await?;
    let party = app.prefs().party;
    let text = format!(
        "Asked for **{}** by {}. {}",
        escape_md(&truncate(&track.title, 80)),
        escape_md(&truncate(&track.artists_joined(), 60)),
        if party {
            "It's on the requests card now: the call votes on what plays next."
        } else {
            "It's on the requests card now."
        }
    );
    let cover = track.cover_thumb_url.clone();
    {
        let mut st = app.requests.state.lock();
        st.last.insert(by_id, Instant::now());
        st.open.push(Request {
            nonce: ix.id.get(),
            track,
            by: ix.user_name.clone(),
            by_id,
            asked_at: now_unix(),
            at: Instant::now(),
            votes: HashSet::from([by_id]),
        });
    }
    let card = cards::toast(&app.icons, &text, cover.as_deref(), color::SUCCESS);
    ix.reply_temp(vec![card], SHORT_SECS).await?;
    redraw(app, ix.channel_id).await;
    if party {
        party_turn(app).await;
    }
    Ok(())
}

/// Every waiting request on one card. `picked` shows that one's buttons.
/// In `party` mode each has a vote button, most votes first.
pub fn requests_card(
    icons: &Icons,
    open: &[Request],
    picked: Option<u64>,
    party: bool,
) -> Component {
    let waiting = match open.len() {
        1 => "1 WAITING".to_string(),
        n => format!("{n} WAITING"),
    };
    let mut parts = if party {
        vec![cards::text(format!(
            "-# SONG REQUESTS · PARTY MODE · {waiting}\nVote for what you'd like to hear: the request with the most votes plays next."
        ))]
    } else {
        vec![cards::text(format!("-# SONG REQUESTS · {waiting}"))]
    };
    let shown: Vec<&Request> = if party {
        party_order(open)
    } else {
        open.iter().collect()
    };
    for request in shown.into_iter().take(SHOWN) {
        let track = &request.track;
        let text = format!(
            "[**{}**]({}) · {} · `{}`\n-# from **{}** <t:{}:R>",
            escape_md(&truncate(&track.title, 70)),
            track.url(),
            escape_md(&truncate(&track.artists_joined(), 50)),
            fmt_ms(track.duration_ms as u64),
            escape_md(&truncate(&request.by, 32)),
            request.asked_at
        );
        if party {
            let votes = request.votes.len().to_string();
            let vote = Btn::icon(&format!("req:vote:{}", request.nonce), Icon::MoveUp)
                .with_label(&votes)
                .build(icons);
            parts.push(cards::section(text, vote));
        } else {
            parts.push(cards::text_with_cover(
                text,
                track.cover_thumb_url.as_deref(),
            ));
        }
    }
    if open.len() > SHOWN {
        parts.push(cards::text(format!(
            "-# and {} more in the menu",
            open.len() - SHOWN
        )));
    }
    let options = open
        .iter()
        .take(MENU_MAX)
        .map(|r| {
            let mut option = cards::option(
                truncate(
                    &format!("{} — {}", r.track.title, r.track.artists_joined()),
                    100,
                ),
                r.nonce.to_string(),
                Some(truncate(&format!("from {}", r.by), 100)),
                None,
            );
            option.default = picked == Some(r.nonce);
            option
        })
        .collect();
    parts.push(cards::row(vec![cards::select(
        "req:pick",
        "Pick a request",
        false,
        options,
    )]));

    match picked.and_then(|n| open.iter().find(|r| r.nonce == n)) {
        Some(request) => {
            let nonce = request.nonce;
            parts.push(cards::text(format!(
                "-# PICKED\n**{}** from **{}**",
                escape_md(&truncate(&request.track.title, 70)),
                escape_md(&truncate(&request.by, 32))
            )));
            parts.push(cards::row(vec![
                Btn::icon(&format!("req:one:{nonce}:next"), Icon::PlayNext)
                    .with_label("Play next")
                    .style(ButtonStyle::Success)
                    .build(icons),
                Btn::icon(&format!("req:one:{nonce}:end"), Icon::Add)
                    .with_label("Add to queue")
                    .build(icons),
                Btn::label(&format!("req:one:{nonce}:no"), "Decline")
                    .style(ButtonStyle::Danger)
                    .build(icons),
                Btn::label("req:back", "Back").build(icons),
            ]));
        }
        None => parts.push(cards::row(vec![
            Btn::icon("req:all:next", Icon::PlayNext)
                .with_label(if open.len() == 1 {
                    "Play next"
                } else {
                    "Play all next"
                })
                .style(ButtonStyle::Success)
                .build(icons),
            Btn::icon("req:all:end", Icon::Add)
                .with_label(if open.len() == 1 {
                    "Add to queue"
                } else {
                    "Add all to the end"
                })
                .build(icons),
            Btn::label(
                "req:all:no",
                if open.len() == 1 {
                    "Decline"
                } else {
                    "Decline all"
                },
            )
            .style(ButtonStyle::Danger)
            .build(icons),
        ])),
    }
    parts.push(cards::text(if party {
        "-# Join the call to vote · you and co-DJs can still answer by hand · requests expire after 30 minutes"
    } else {
        "-# You and co-DJs can answer · requests expire after 30 minutes"
    }));
    cards::card(Some(color::BRAND), parts)
}

/// Party mode: while I'm in a call, the waiting request with the most votes
/// goes up next, one at a time. Runs as each song starts, when the queue
/// runs out, when a request comes in, and when party mode goes on.
pub async fn party_turn(app: &Arc<App>) {
    let prefs = app.prefs();
    if !prefs.party || prefs.requests_off {
        return;
    }
    let (Some(guild), Some(channel)) = (app.voice.guild(), app.voice.channel()) else {
        return;
    };
    let Some(core) = app.core.get().cloned() else {
        return;
    };
    // The Spotify app (or follow mode) is in charge: that's not ours to change.
    let view = core.engine.queue_view();
    if view.following || view.connect {
        return;
    }
    // One request up next at a time; the votes pick the one after it.
    let snap = core.engine.snapshot();
    if snap
        .up_next
        .first()
        .is_some_and(|t| t.requested_by.is_some())
    {
        return;
    }
    let request = {
        let mut st = app.requests.state.lock();
        st.tidy();
        st.top().and_then(|nonce| st.take(nonce))
    };
    let Some(request) = request else {
        return;
    };
    let votes = request.votes.len();
    let source = format!("**{}**'s request", escape_md(&request.by));
    let track = TrackInfo {
        from: Some(source.clone().into()),
        requested_by: Some(request.by_id),
        ..request.track
    };
    let note = (votes > 1).then(|| format!("The top request, with {votes} votes."));
    let queued = commands::enqueue(
        app,
        &core,
        (guild, channel),
        vec![track],
        Placement::Next,
        Some(source),
        note,
    )
    .await;
    match queued {
        Ok(card) => {
            if let Some(home) = app.ui.home() {
                app.announce(home, card, SHORT_SECS).await;
            }
        }
        Err(e) => tracing::warn!("party mode couldn't queue a request: {e:#}"),
    }
    redraw(app, None).await;
}

/// The requests card after a change of mode, and party mode's next pick.
pub async fn refresh(app: &Arc<App>) {
    redraw(app, None).await;
    party_turn(app).await;
}

/// A vote in party mode; pressing again takes it back. Only people in the
/// call vote.
async fn vote(ix: &Ix, nonce: u64) -> Result<()> {
    let app = &ix.app;
    let voter = ix.user_id.map_or(0, |u| u.get());
    if !friends::listeners(app).iter().any(|l| l.id == voter) {
        return Err(oops("Join the call to vote on requests."));
    }
    {
        let mut st = app.requests.state.lock();
        st.tidy();
        if let Some(request) = st.open.iter_mut().find(|r| r.nonce == nonce)
            && !request.votes.remove(&voter)
        {
            request.votes.insert(voter);
        }
    }
    redraw_here(ix).await
}

/// Puts the requests card up to date: edited where it is, posted if it
/// isn't up (in the panel's channel, or where the request came from), and
/// removed once nothing's waiting.
async fn redraw(app: &App, fallback: Option<Id<ChannelMarker>>) {
    let _drawing = app.requests.drawing.lock().await;
    let (card, existing) = {
        let mut st = app.requests.state.lock();
        st.tidy();
        let card = (!st.open.is_empty())
            .then(|| requests_card(&app.icons, &st.open, st.picked, app.prefs().party));
        (card, st.card)
    };
    let Some(card) = card else {
        let old = app.requests.state.lock().card.take();
        if let Some((channel, message)) = old {
            let _ = app.http.delete_message(channel, message).await;
        }
        return;
    };
    let components = [card];
    if let Some((channel, message)) = existing {
        let edited = app
            .http
            .update_message(channel, message)
            .flags(cards::V2)
            .components(Some(&components))
            .await;
        if edited.is_ok() {
            return;
        }
    }
    let Some(channel) = app.ui.home().or(fallback) else {
        return;
    };
    let posted = app
        .http
        .create_message(channel)
        .flags(cards::V2)
        .components(&components)
        .await;
    match posted {
        Ok(response) => {
            if let Ok(message) = response.model().await {
                app.requests.state.lock().card = Some((channel, message.id));
            }
        }
        Err(e) => tracing::warn!("could not post the requests card: {e}"),
    }
}

/// A menu or button on the requests card (the component handler has
/// checked it's you or a co-DJ). `rest` is what follows `req:`.
pub async fn answer(ix: &Ix, rest: &str, value: Option<&str>) -> Result<()> {
    let app = &ix.app;
    let mut parts = rest.split(':');
    let (what, how) = match (parts.next(), parts.next(), parts.next()) {
        (Some("pick"), _, _) => {
            app.requests.state.lock().picked = value.and_then(|v| v.parse().ok());
            return redraw_here(ix).await;
        }
        (Some("back"), _, _) => {
            app.requests.state.lock().picked = None;
            return redraw_here(ix).await;
        }
        (Some("vote"), Some(nonce), _) => {
            return vote(ix, nonce.parse().unwrap_or_default()).await;
        }
        (Some("one"), Some(nonce), Some(how)) => (nonce.parse::<u64>().ok(), how),
        (Some("all"), Some(how), _) => (None, how),
        _ => return ix.ack().await,
    };
    let placement = match how {
        "next" => Some(Placement::Next),
        "end" => Some(Placement::End),
        _ => None,
    };
    // Before taking the requests, so they stay if you aren't in voice.
    let target = match placement {
        Some(_) => Some(commands::require_voice(ix)?),
        None => None,
    };
    let taken: Vec<Request> = {
        let mut st = app.requests.state.lock();
        st.tidy();
        match what {
            Some(nonce) => st.take(nonce).into_iter().collect(),
            None => std::mem::take(&mut st.open),
        }
    };
    ix.ack().await?;
    if taken.is_empty() {
        redraw_after(ix).await;
        let card = cards::info(&app.icons, "That request was answered already, or expired.");
        return ix.followup_temp(vec![card], SHORT_SECS).await;
    }

    let outcome = match (placement, target) {
        (Some(placement), Some(target)) => {
            let core = app.core()?;
            let source = match taken.as_slice() {
                [one] => format!("**{}**'s request", escape_md(&one.by)),
                _ => "the requests".to_string(),
            };
            let tracks = taken
                .into_iter()
                .map(|r| TrackInfo {
                    from: Some(format!("**{}**'s request", escape_md(&r.by)).into()),
                    requested_by: Some(r.by_id),
                    ..r.track
                })
                .collect();
            commands::enqueue(app, &core, target, tracks, placement, Some(source), None).await
        }
        _ => {
            let text = match taken.as_slice() {
                [one] => format!(
                    "Not this time, {}. (**{}**)",
                    escape_md(&one.by),
                    escape_md(&one.track.title)
                ),
                many => format!("Declined {} requests.", many.len()),
            };
            Ok(cards::info(&app.icons, &text))
        }
    };
    redraw_after(ix).await;
    ix.followup_temp(vec![outcome?], SHORT_SECS).await
}

/// The card the menu was on, redrawn in place (or removed when empty).
async fn redraw_here(ix: &Ix) -> Result<()> {
    let app = &ix.app;
    let card = {
        let mut st = app.requests.state.lock();
        st.tidy();
        (!st.open.is_empty())
            .then(|| requests_card(&app.icons, &st.open, st.picked, app.prefs().party))
    };
    match card {
        Some(card) => ix.update_message(vec![card]).await,
        None => {
            ix.ack().await?;
            ix.delete_reply().await;
            app.requests.state.lock().card = None;
            Ok(())
        }
    }
}

/// After an answer (already acknowledged): the card shows what's left.
async fn redraw_after(ix: &Ix) {
    let app = &ix.app;
    let card = {
        let mut st = app.requests.state.lock();
        st.tidy();
        (!st.open.is_empty())
            .then(|| requests_card(&app.icons, &st.open, st.picked, app.prefs().party))
    };
    match card {
        Some(card) => {
            let _ = app
                .http
                .interaction(app.app_id)
                .update_response(&ix.token)
                .flags(cards::V2)
                .components(Some(&[card]))
                .await;
        }
        None => {
            ix.delete_reply().await;
            app.requests.state.lock().card = None;
        }
    }
}
