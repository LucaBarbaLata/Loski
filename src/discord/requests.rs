//! Song requests: anyone in the server can `/request` a song, and you pick
//! Play next, Add to queue or Decline on the card it makes. You stay the
//! only one who controls Loski.

use std::{
    collections::HashMap,
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
};

use super::{
    App, SHORT_SECS,
    cards::{self, Btn, color},
    commands::{self, Placement},
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

/// How long a request card can still be answered.
const TTL: Duration = Duration::from_secs(30 * 60);
/// Time between two requests from the same person.
const COOLDOWN: Duration = Duration::from_secs(20);
/// Unanswered requests one person can have at once.
const MAX_OPEN: usize = 5;

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

struct Request {
    track: TrackInfo,
    by: String,
    by_id: u64,
    at: Instant,
}

#[derive(Default)]
pub struct Requests {
    open: Mutex<HashMap<u64, Request>>,
    last: Mutex<HashMap<u64, Instant>>,
}

impl Requests {
    fn take(&self, nonce: u64) -> Option<Request> {
        let mut open = self.open.lock();
        open.retain(|_, r| r.at.elapsed() < TTL);
        open.remove(&nonce)
    }
}

/// Search suggestions while typing. Unlike `/play`, an empty box suggests
/// nothing: your recent plays stay yours.
pub async fn suggestions(app: &App, input: CommandInputData<'_>) -> Vec<CommandOptionChoice> {
    match RequestComplete::from_interaction(input) {
        Ok(RequestComplete {
            song: AutocompleteValue::Focused(partial),
        }) if !partial.trim().is_empty() => commands::song_suggestions(app, &partial).await,
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
    let by_id = ix.user_id.map_or(0, |u| u.get());
    {
        let requests = &app.requests;
        let open = requests
            .open
            .lock()
            .values()
            .filter(|r| r.by_id == by_id && r.at.elapsed() < TTL)
            .count();
        if open >= MAX_OPEN {
            return Err(oops(
                "You have a few requests waiting already. Give them a moment.",
            ));
        }
        if let Some(at) = requests.last.lock().get(&by_id)
            && at.elapsed() < COOLDOWN
        {
            return Err(oops("One at a time. Try again in a few seconds."));
        }
    }
    ix.defer().await?;
    let track = find_song(app, &cmd.song).await?;
    let nonce = ix.id.get();
    let by = ix.user_name.clone();
    let card = request_card(&app.icons, &track, &by, nonce);
    app.requests.last.lock().insert(by_id, Instant::now());
    app.requests.open.lock().insert(
        nonce,
        Request {
            track,
            by,
            by_id,
            at: Instant::now(),
        },
    );
    ix.reply(vec![card]).await
}

pub fn request_card(icons: &Icons, track: &TrackInfo, by: &str, nonce: u64) -> Component {
    let expires = now_unix() + TTL.as_secs() as i64;
    let text = format!(
        "-# SONG REQUEST\n**{}** would like to hear\n### [{}]({})\n{} · `{}`",
        escape_md(&truncate(by, 40)),
        escape_md(&truncate(&track.title, 80)),
        track.url(),
        escape_md(&truncate(&track.artists_joined(), 80)),
        fmt_ms(track.duration_ms as u64)
    );
    cards::card(
        Some(color::BRAND),
        vec![
            cards::text_with_cover(text, track.cover_thumb_url.as_deref()),
            cards::row(vec![
                Btn::icon(&format!("req:{nonce}:next"), Icon::PlayNext)
                    .with_label("Play next")
                    .style(ButtonStyle::Success)
                    .build(icons),
                Btn::icon(&format!("req:{nonce}:end"), Icon::Add)
                    .with_label("Add to queue")
                    .build(icons),
                Btn::label(&format!("req:{nonce}:no"), "Decline")
                    .style(ButtonStyle::Danger)
                    .build(icons),
            ]),
            cards::text(format!(
                "-# Only the owner can answer · expires <t:{expires}:R>"
            )),
        ],
    )
}

/// Play next / Add to queue / Decline on a request card (owner only; the
/// component handler has already checked).
pub async fn answer(ix: &Ix, rest: &str) -> Result<()> {
    let app = &ix.app;
    let (nonce, how) = rest.split_once(':').unwrap_or((rest, "no"));
    let Some(request) = app.requests.take(nonce.parse().unwrap_or_default()) else {
        ix.update_message(vec![cards::info(
            &app.icons,
            "This request expired. Ask for it again with `/request`.",
        )])
        .await?;
        ix.delete_later(SHORT_SECS);
        return Ok(());
    };
    let placement = match how {
        "next" => Placement::Next,
        "end" => Placement::End,
        _ => {
            let text = format!(
                "Not this time, {}. (**{}**)",
                escape_md(&request.by),
                escape_md(&request.track.title)
            );
            ix.update_message(vec![cards::info(&app.icons, &text)])
                .await?;
            ix.delete_later(SHORT_SECS);
            return Ok(());
        }
    };

    let core = app.core()?;
    let guild = match commands::require_voice(ix) {
        Ok(guild) => guild,
        Err(e) => {
            // Keep the request for when you're in voice.
            app.requests.open.lock().insert(
                nonce.parse().unwrap_or_default(),
                Request {
                    at: Instant::now(),
                    ..request
                },
            );
            return Err(e);
        }
    };
    ix.ack().await?;
    let from = format!("**{}**'s request", escape_md(&request.by));
    let card = commands::enqueue(
        app,
        &core,
        guild,
        vec![request.track],
        placement,
        Some(from),
        None,
    )
    .await?;
    app.http
        .interaction(app.app_id)
        .update_response(&ix.token)
        .flags(cards::V2)
        .components(Some(&[card]))
        .await?;
    ix.delete_later(SHORT_SECS);
    Ok(())
}
