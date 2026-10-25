//! Buttons and menus: the now playing panel, the queue card and the search card.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use twilight_model::{
    application::interaction::{Interaction, InteractionData, modal::ModalInteractionComponent},
    id::{Id, marker::MessageMarker},
};

use super::{
    App, Core, ERROR_SECS, LONG_SECS, SHORT_SECS, cards,
    commands::{self, Placement},
    interact::Ix,
    library, mixes, oops, panel, recap, requests, stats, voice,
};
use crate::{
    dsp::Filter,
    personality,
    spotify::{
        Lookup,
        web::{SearchItem, SearchKind, Target},
    },
    track::TrackInfo,
};

/// How long a search card stays answerable.
const PICK_TTL: Duration = Duration::from_secs(60);
const JUMP_MS: u32 = 15_000;

pub async fn handle(app: Arc<App>, interaction: Interaction) {
    let message_id = interaction.message.as_ref().map(|m| m.id);
    let ix = Ix::new(app, &interaction);
    let Some(InteractionData::MessageComponent(data)) = interaction.data else {
        return;
    };
    if !ix.is_owner() {
        let _ = ix.refuse().await;
        return;
    }
    let id = data.custom_id.as_str();
    let value = data.values.first().map(String::as_str);
    let result = if let Some(nonce) = id.strip_prefix("pick:") {
        choose_result(&ix, nonce, value).await
    } else if let Some(rest) = id.strip_prefix("pickgo:") {
        play_result(&ix, rest).await
    } else if let Some(rest) = id.strip_prefix("picktab:") {
        switch_tab(&ix, rest).await
    } else if let Some(nonce) = id.strip_prefix("pickcancel:") {
        cancel_pick(&ix, nonce).await
    } else if let Some(rest) = id.strip_prefix("req:") {
        requests::answer(&ix, rest).await
    } else if id.starts_with("loski:") {
        match ix.app.core.get().cloned() {
            Some(core) => loski(&ix, &core, id, value, message_id).await,
            None => ix.ack().await,
        }
    } else {
        ix.ack().await
    };
    if let Err(e) = result {
        ix.fail(e).await;
    }
}

fn parse_nonce(nonce: &str) -> u64 {
    nonce.parse().unwrap_or_default()
}

fn expired() -> anyhow::Error {
    oops("That search expired. Run `/play` again.")
}

/// A different result was chosen in the search card's menu.
async fn choose_result(ix: &Ix, nonce: &str, value: Option<&str>) -> Result<()> {
    let nonce = parse_nonce(nonce);
    let card = {
        let mut picks = ix.app.picks.lock();
        picks
            .get_mut(&nonce)
            .filter(|p| p.created.elapsed() < PICK_TTL)
            .map(|pick| {
                pick.selected = value
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0)
                    .min(pick.items.len().saturating_sub(1));
                commands::picker_card(&ix.app.icons, pick, nonce)
            })
    };
    match card {
        Some(card) => ix.update_message(vec![card]).await,
        None => {
            ix.delete_reply().await;
            Err(expired())
        }
    }
}

/// Songs / Albums / Playlists / Artists on the search card.
async fn switch_tab(ix: &Ix, rest: &str) -> Result<()> {
    let app = &ix.app;
    let (nonce, key) = rest.split_once(':').unwrap_or((rest, ""));
    let nonce = parse_nonce(nonce);
    let kind = SearchKind::from_key(key).unwrap_or(SearchKind::Songs);
    let query = app
        .picks
        .lock()
        .get(&nonce)
        .filter(|p| p.created.elapsed() < PICK_TTL)
        .map(|p| p.query.clone());
    let Some(query) = query else {
        ix.delete_reply().await;
        return Err(expired());
    };
    let core = app.core()?;
    // Searching takes a moment; acknowledge first.
    ix.ack().await?;
    let items = commands::search_items(&core, &query, kind).await?;
    let card = {
        let mut picks = app.picks.lock();
        let Some(pick) = picks.get_mut(&nonce) else {
            return Ok(());
        };
        pick.kind = kind;
        pick.items = items.to_vec();
        pick.selected = 0;
        commands::picker_card(&app.icons, pick, nonce)
    };
    app.http
        .interaction(app.app_id)
        .update_response(&ix.token)
        .flags(cards::V2)
        .components(Some(&[card]))
        .await?;
    Ok(())
}

/// Loads what a search result plays: a song directly, anything else
/// through Spotify.
async fn result_tracks(
    core: &Core,
    item: SearchItem,
) -> Result<(Vec<TrackInfo>, Option<String>, Option<String>)> {
    let lookup = match item.target {
        Target::Track(track) => return Ok((vec![*track], None, None)),
        Target::Album(id) => Lookup::Album(id),
        Target::Playlist(id) => Lookup::Playlist(id),
        Target::Artist(id) => Lookup::Artist(id),
    };
    let resolved = core
        .spotify
        .resolve(lookup)
        .await
        .map_err(|e| oops(format!("Hmm, {e}")))?
        .ok_or_else(|| oops("I couldn't open that."))?;
    let note = resolved
        .truncated
        .then(|| "Only the first 500 songs, to keep things snappy.".to_string());
    Ok((resolved.tracks, resolved.source, note))
}

/// Play now / Play next / Add to queue on the search card.
async fn play_result(ix: &Ix, rest: &str) -> Result<()> {
    let app = &ix.app;
    let (nonce, how) = rest.split_once(':').unwrap_or((rest, "end"));
    let placement = match how {
        "now" => Placement::Now,
        "next" => Placement::Next,
        _ => Placement::End,
    };
    let pick = app.picks.lock().remove(&parse_nonce(nonce));
    let Some(pick) = pick.filter(|p| p.created.elapsed() < PICK_TTL) else {
        ix.delete_reply().await;
        return Err(expired());
    };
    let Some(item) = pick.items.into_iter().nth(pick.selected) else {
        return ix.ack().await;
    };

    let core = app.core()?;
    let guild = ix
        .guild_id
        .ok_or_else(|| oops("This only works in a server."))?;
    // Loading and joining voice can take a moment; acknowledge first.
    ix.ack().await?;
    let outcome = match result_tracks(&core, item).await {
        Ok((tracks, source, note)) => {
            commands::enqueue(app, &core, guild, tracks, placement, source, note).await
        }
        Err(e) => Err(e),
    };
    // Either way the search card turns into the outcome and then goes away.
    let (card, secs) = match outcome {
        Ok(card) => (card, SHORT_SECS),
        Err(e) => {
            let message = match e.downcast_ref::<super::Oops>() {
                Some(oops) => oops.0.clone(),
                None => {
                    tracing::error!("couldn't queue the chosen result: {e:#}");
                    format!("Something went wrong: {e}")
                }
            };
            (cards::error(&app.icons, &message), ERROR_SECS)
        }
    };
    app.http
        .interaction(app.app_id)
        .update_response(&ix.token)
        .flags(cards::V2)
        .components(Some(&[card]))
        .await?;
    ix.delete_later(secs);
    Ok(())
}

async fn cancel_pick(ix: &Ix, nonce: &str) -> Result<()> {
    ix.app.picks.lock().remove(&parse_nonce(nonce));
    ix.ack().await?;
    ix.delete_reply().await;
    Ok(())
}

async fn loski(
    ix: &Ix,
    core: &Core,
    id: &str,
    value: Option<&str>,
    message: Option<Id<MessageMarker>>,
) -> Result<()> {
    let app = &ix.app;
    let engine = &core.engine;
    let queue_card = |page: usize, selected: Option<usize>| {
        panel::queue_card(
            &app.icons,
            app.ui.color(),
            &engine.queue_view(),
            page,
            selected,
        )
    };

    match id {
        "loski:toggle" => {
            engine.toggle_pause();
        }
        "loski:prev" => {
            engine.previous();
        }
        "loski:skip" => {
            engine.skip(1);
            // The panel gets reposted for the new song.
            return ix.ack().await;
        }
        "loski:shuffle" => {
            engine.shuffle();
        }
        "loski:filter" => {
            let filter = value.and_then(Filter::from_key).unwrap_or(Filter::Off);
            if let Err(reason) = engine.set_filter(filter) {
                // Put the menu back to what's actually playing, then explain.
                ix.update_message(vec![panel::current_card(app, &engine.snapshot())])
                    .await?;
                return Err(oops(reason));
            }
        }
        "loski:repeat" => {
            engine.cycle_repeat();
        }
        "loski:voldown" => {
            engine.set_volume(engine.volume().saturating_sub(10));
        }
        "loski:volup" => {
            engine.set_volume((engine.volume() + 10).min(100));
        }
        "loski:replay" => {
            engine.replay();
            return ix.ack().await;
        }
        "loski:radio" => {
            ix.ack().await?;
            library::radio_from_current(core).await?;
            return Ok(());
        }
        "loski:more" => match value {
            Some("back15") => {
                let now = engine.position_ms().unwrap_or(0);
                engine.seek(now.saturating_sub(JUMP_MS));
            }
            Some("fwd15") => {
                let now = engine.position_ms().unwrap_or(0);
                engine.seek(now.saturating_add(JUMP_MS));
            }
            Some("restart") => {
                engine.seek(0);
            }
            Some("seek") => {
                return ix
                    .modal(
                        "loski:m:seek",
                        "Jump to a time",
                        vec![cards::text_field(
                            "time",
                            "Time",
                            Some("Like 1:23, or +30 / -10 to jump from where it is now"),
                            "1:23",
                            None,
                            12,
                        )],
                    )
                    .await;
            }
            Some("radio") => {
                ix.ack().await?;
                library::radio_from_current(core).await?;
                return Ok(());
            }
            Some("repost") => {
                ix.ack().await?;
                panel::repost(app, core).await;
                return Ok(());
            }
            _ => {}
        },
        "loski:stop" => {
            ix.ack().await?;
            voice::leave(app, Some(personality::goodbye())).await;
            return Ok(());
        }
        "loski:queue" => {
            ix.reply(vec![queue_card(0, None)]).await?;
            ix.delete_later(LONG_SECS);
            return Ok(());
        }
        "loski:qclose" => {
            ix.ack().await?;
            ix.delete_reply().await;
            return Ok(());
        }
        "loski:hsel" => {
            return ix.update_message(vec![history_card(app, value)]).await;
        }
        "loski:hback" => {
            return ix.update_message(vec![history_card(app, None)]).await;
        }
        "loski:qsavemix" => {
            return ix
                .modal(
                    "loski:m:qsave",
                    "Save the queue as a mix",
                    vec![cards::text_field(
                        "name",
                        "Name",
                        Some("Every song in the queue, played ones too"),
                        "late night",
                        None,
                        40,
                    )],
                )
                .await;
        }
        "loski:recap:save" => return recap::ask_mix_name(ix).await,
        "loski:recap:stats" => return recap::show_week(ix).await,
        "loski:mixsel" => return mixes::select(ix, value).await,
        "loski:mixback" => return mixes::select(ix, None).await,
        other => {
            if let Some(page) = other.strip_prefix("loski:qpage:") {
                return ix
                    .update_message(vec![queue_card(page.parse().unwrap_or(0), None)])
                    .await;
            }
            if let Some(page) = other.strip_prefix("loski:qshuffle:") {
                engine.shuffle();
                return ix
                    .update_message(vec![queue_card(page.parse().unwrap_or(0), None)])
                    .await;
            }
            if let Some(page) = other.strip_prefix("loski:qsel:") {
                let page = page.parse().unwrap_or(0);
                let position = value
                    .and_then(|v| v.split(':').next())
                    .and_then(|p| p.parse().ok());
                return ix.update_message(vec![queue_card(page, position)]).await;
            }
            if let Some(rest) = other.strip_prefix("loski:qact:") {
                return queue_action(ix, core, rest).await;
            }
            if let Some(page) = other.strip_prefix("loski:qdedupe:") {
                engine.dedupe();
                return ix
                    .update_message(vec![queue_card(page.parse().unwrap_or(0), None)])
                    .await;
            }
            if let Some(rest) = other.strip_prefix("loski:hgo:") {
                return replay_from_history(ix, core, rest).await;
            }
            if let Some(rest) = other.strip_prefix("loski:mixgo:") {
                return mixes::play_from_card(ix, rest).await;
            }
            if let Some(name) = other.strip_prefix("loski:mixren:") {
                return mixes::ask_new_name(ix, name).await;
            }
            if let Some(name) = other.strip_prefix("loski:mixdel:") {
                return mixes::delete_from_card(ix, name).await;
            }
            if let Some(rest) = other.strip_prefix("loski:stats:") {
                return match rest.strip_prefix("play:") {
                    Some(key) => stats::play_top(ix, key).await,
                    None => stats::switch_tab(ix, rest).await,
                };
            }
            return ix.ack().await;
        }
    }

    // Everything that falls through changed the panel it was used on.
    ix.update_message(vec![panel::current_card(app, &engine.snapshot())])
        .await?;
    if let Some(message) = message {
        app.ui.mark_fresh(message);
    }
    Ok(())
}

/// A pop-up form came back filled in.
pub async fn modal(app: Arc<App>, interaction: Interaction) {
    let message = interaction.message.as_ref().map(|m| m.id);
    let ix = Ix::new(app, &interaction);
    let Some(InteractionData::ModalSubmit(data)) = interaction.data else {
        return;
    };
    if !ix.is_owner() {
        let _ = ix.refuse().await;
        return;
    }
    let text = |id: &str| field(&data.components, id).unwrap_or_default();
    let result = match data.custom_id.as_str() {
        "loski:m:seek" => seek_form(&ix, &text("time"), message).await,
        "loski:m:qsave" => save_queue_form(&ix, &text("name")).await,
        "loski:m:recapsave" => recap::save_mix(&ix, &text("name")).await,
        id => match id.strip_prefix("loski:m:mixren:") {
            Some(old) => mixes::rename(&ix, old, &text("name")).await,
            None => ix.ack().await,
        },
    };
    if let Err(e) = result {
        ix.fail(e).await;
    }
}

/// The text typed into a form's field, wherever the field sits.
fn field(components: &[ModalInteractionComponent], id: &str) -> Option<String> {
    components.iter().find_map(|component| match component {
        ModalInteractionComponent::TextInput(input) if input.custom_id == id => {
            Some(input.value.trim().to_string())
        }
        ModalInteractionComponent::Label(label) => {
            field(std::slice::from_ref(&label.component), id)
        }
        ModalInteractionComponent::ActionRow(row) => field(&row.components, id),
        _ => None,
    })
}

/// "Jump to a time…" from the panel's More menu.
async fn seek_form(ix: &Ix, input: &str, message: Option<Id<MessageMarker>>) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    commands::seek_to(&core, input)?;
    ix.update_message(vec![panel::current_card(app, &core.engine.snapshot())])
        .await?;
    if let Some(message) = message {
        app.ui.mark_fresh(message);
    }
    Ok(())
}

/// "Save as mix" on the queue card.
async fn save_queue_form(ix: &Ix, name: &str) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    let text = mixes::save_tracks(app, name, &core.engine.queue_tracks())?;
    ix.reply_temp(
        vec![cards::toast(&app.icons, &text, None, cards::color::SUCCESS)],
        SHORT_SECS,
    )
    .await
}

fn history_card(app: &App, selected: Option<&str>) -> twilight_model::channel::message::Component {
    let entries = app.store.recent_history(panel::HISTORY_SIZE);
    panel::history_card(&app.icons, app.ui.color(), &entries, selected)
}

/// `how:track-id` from the history card: play a past song again.
async fn replay_from_history(ix: &Ix, core: &Core, rest: &str) -> Result<()> {
    let app = &ix.app;
    let (how, track_id) = rest.split_once(':').unwrap_or(("end", rest));
    let placement = match how {
        "now" => Placement::Now,
        "next" => Placement::Next,
        _ => Placement::End,
    };
    let id = librespot_core::SpotifyId::from_base62(track_id)
        .map_err(|_| oops("I couldn't find that song again."))?;
    let guild = commands::require_voice(ix)?;
    ix.ack().await?;
    let resolved = core
        .spotify
        .resolve(Lookup::Track(id))
        .await
        .map_err(|e| oops(format!("Hmm, {e}")))?
        .ok_or_else(|| oops("I couldn't find that song again."))?;
    let card = commands::enqueue(app, core, guild, resolved.tracks, placement, None, None).await?;
    // The history card goes back to its list, ready for another pick.
    app.http
        .interaction(app.app_id)
        .update_response(&ix.token)
        .flags(cards::V2)
        .components(Some(&[history_card(app, None)]))
        .await?;
    ix.followup_temp(vec![card], SHORT_SECS).await
}

/// `action:position:track-id:page` from the queue card's selected song.
async fn queue_action(ix: &Ix, core: &Core, rest: &str) -> Result<()> {
    let app = &ix.app;
    let engine = &core.engine;
    let mut parts = rest.split(':');
    let (Some(action), Some(position), Some(track_id), Some(page)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return ix.ack().await;
    };
    let position: usize = position.parse().unwrap_or(0);
    let page: usize = page.parse().unwrap_or(0);

    // The queue may have changed since the card was drawn; if so, the
    // refreshed card shows where things are now and nothing else happens.
    let still_there = engine
        .track_at(position)
        .is_some_and(|t| t.base62() == track_id);
    if still_there {
        match action {
            "play" => {
                engine.skip_to(position);
            }
            "top" => {
                engine.move_track(position, 1);
            }
            "del" => {
                engine.remove(position);
            }
            _ => {}
        }
    }
    let card = panel::queue_card(&app.icons, app.ui.color(), &engine.queue_view(), page, None);
    ix.update_message(vec![card]).await
}
