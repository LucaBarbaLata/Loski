//! Saved mixes: `/mix save` keeps the queue under a name, `/mix load`
//! brings it back. They live in `data/mixes.json`.

use std::sync::Arc;

use anyhow::Result;
use librespot_core::{SpotifyId, SpotifyUri};
use rand::seq::SliceRandom;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::application::{
    command::CommandOptionChoice,
    interaction::application_command::{CommandData, CommandDataOption, CommandOptionValue},
};
use twilight_model::channel::message::{Component, component::ButtonStyle};

use super::{
    App, LONG_SECS, SHORT_SECS,
    cards::{self, Btn, color},
    commands::{self, Placement},
    icons::{Icon, Icons},
    interact::Ix,
    oops,
};
use crate::{
    spotify::{MAX_TRACKS_PER_REQUEST, auth::now_unix},
    store::{Mix, MixTrack},
    track::TrackInfo,
    util::{escape_md, fmt_duration_words, truncate},
};
/// Discord keeps names in buttons' ids, so they stay short.
const NAME_MAX: usize = 40;

/// As many as a menu or autocomplete list can show.
const MAX_MIXES: usize = 25;
/// With more mixes than this, the list drops the song previews to stay
/// inside Discord's text limit.
const PREVIEW_UP_TO: usize = 10;

#[derive(CommandModel, CreateCommand)]
#[command(name = "mix", desc = "Save the queue as a mix and play it again later")]
pub enum MixCmd {
    #[command(name = "save")]
    Save(MixSaveCmd),
    #[command(name = "load")]
    Load(MixLoadCmd),
    #[command(name = "list")]
    List(MixListCmd),
    #[command(name = "delete")]
    Delete(MixDeleteCmd),
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "save",
    desc = "Save every song in the queue (played ones too) under a name"
)]
pub struct MixSaveCmd {
    /// A name, like "late night" (saving under a name you used replaces it)
    #[command(min_length = 1, max_length = 40)]
    name: String,
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "load", desc = "Add one of your mixes to the queue")]
pub struct MixLoadCmd {
    /// Which mix
    #[command(autocomplete = true, max_length = 40)]
    name: String,
    /// Shuffle it
    shuffle: Option<bool>,
    /// Play it right after the current song
    next: Option<bool>,
}

#[derive(CommandModel, CreateCommand)]
#[command(name = "list", desc = "See your mixes")]
pub struct MixListCmd;

#[derive(CommandModel, CreateCommand)]
#[command(name = "delete", desc = "Delete one of your mixes")]
pub struct MixDeleteCmd {
    /// Which mix
    #[command(autocomplete = true, max_length = 40)]
    name: String,
}

pub async fn mix(ix: &Ix, cmd: MixCmd) -> Result<()> {
    match cmd {
        MixCmd::Save(cmd) => save(ix, cmd).await,
        MixCmd::Load(cmd) => {
            let placement = if cmd.next.unwrap_or(false) {
                Placement::Next
            } else {
                Placement::End
            };
            let mix = find(&ix.app, &cmd.name)?;
            let target = commands::require_voice(ix)?;
            ix.defer().await?;
            let card = load(
                &ix.app,
                target,
                &mix,
                cmd.shuffle.unwrap_or(false),
                placement,
            )
            .await?;
            ix.reply_temp(vec![card], SHORT_SECS).await
        }
        MixCmd::List(_) => {
            let mixes = ix.app.store.load_mixes();
            ix.reply_temp(vec![list_card(&ix.app.icons, &mixes, None)], LONG_SECS)
                .await
        }
        MixCmd::Delete(cmd) => {
            let mut mixes = ix.app.store.load_mixes();
            let gone = find_in(&mixes, &cmd.name)
                .ok_or_else(|| not_found(&cmd.name))?
                .name
                .clone();
            mixes.retain(|m| m.name != gone);
            ix.app.store.save_mixes(&mixes)?;
            let text = format!("Deleted the mix **{}**.", escape_md(&gone));
            ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
                .await
        }
    }
}

async fn save(ix: &Ix, cmd: MixSaveCmd) -> Result<()> {
    let app = &ix.app;
    let core = app.core()?;
    let text = save_tracks(app, &cmd.name, &core.engine.queue_tracks())?;
    ix.reply_temp(
        vec![cards::toast(&app.icons, &text, None, color::SUCCESS)],
        SHORT_SECS,
    )
    .await
}

/// Saves songs under `name`, replacing a mix with the same name, and
/// returns the confirmation to show.
pub fn save_tracks(app: &App, name: &str, chosen: &[Arc<TrackInfo>]) -> Result<String> {
    let name: String = name.trim().chars().take(NAME_MAX).collect();
    if name.is_empty() {
        return Err(oops("Give the mix a name."));
    }
    let tracks: Vec<MixTrack> = chosen
        .iter()
        .take(MAX_TRACKS_PER_REQUEST)
        .map(|t| MixTrack {
            id: t.base62(),
            title: t.title.clone(),
            artists: t.artists_joined(),
            duration_ms: t.duration_ms,
        })
        .collect();
    if tracks.is_empty() {
        return Err(oops(
            "The queue is empty. Queue some songs, then save them as a mix.",
        ));
    }

    let mut mixes = app.store.load_mixes();
    let replaced = mixes
        .iter()
        .position(|m| m.name.eq_ignore_ascii_case(&name))
        .map(|i| mixes.remove(i));
    if replaced.is_none() && mixes.len() >= MAX_MIXES {
        return Err(oops(format!(
            "You have {MAX_MIXES} mixes, the most I keep. Delete one with `/mix delete` first."
        )));
    }
    let mix = Mix {
        name,
        saved_at: now_unix(),
        tracks,
    };
    let text = format!(
        "{} **{}**: {} · {}.\n-# Play it again any time with `/mix load`.",
        if replaced.is_some() {
            "Updated the mix"
        } else {
            "Saved the mix"
        },
        escape_md(&mix.name),
        songs(mix.tracks.len()),
        fmt_duration_words(mix.duration_ms())
    );
    mixes.insert(0, mix);
    app.store.save_mixes(&mixes)?;
    Ok(text)
}

fn songs(n: usize) -> String {
    if n == 1 {
        "1 song".to_string()
    } else {
        format!("{n} songs")
    }
}

fn not_found(name: &str) -> anyhow::Error {
    oops(format!(
        "You don't have a mix called **{}**. `/mix list` shows them all.",
        escape_md(name)
    ))
}

/// An exact name first, then one that contains what you typed.
fn find_in<'a>(mixes: &'a [Mix], name: &str) -> Option<&'a Mix> {
    let wanted = name.trim().to_lowercase();
    mixes
        .iter()
        .find(|m| m.name.to_lowercase() == wanted)
        .or_else(|| {
            mixes
                .iter()
                .find(|m| m.name.to_lowercase().contains(&wanted))
        })
}

pub fn find(app: &App, name: &str) -> Result<Mix> {
    find_in(&app.store.load_mixes(), name)
        .cloned()
        .ok_or_else(|| not_found(name))
}

/// Looks the mix's songs up again (fresh covers and availability) and queues them.
pub async fn load(
    app: &App,
    target: commands::Target,
    mix: &Mix,
    shuffle: bool,
    placement: Placement,
) -> Result<Component> {
    let core = app.core()?;
    let uris: Vec<SpotifyUri> = mix
        .tracks
        .iter()
        .filter_map(|t| SpotifyId::from_base62(&t.id).ok())
        .map(|id| SpotifyUri::Track { id })
        .collect();
    let mut resolved = core
        .spotify
        .load_tracks(uris, format!("your mix **{}**", escape_md(&mix.name)))
        .await;
    if shuffle {
        resolved.tracks.shuffle(&mut rand::rng());
    }
    let missing = mix.tracks.len().saturating_sub(resolved.tracks.len());
    let note = (missing > 0).then(|| {
        format!(
            "{} couldn't be played anymore.",
            if missing == 1 {
                "1 song".to_string()
            } else {
                format!("{missing} songs")
            }
        )
    });
    commands::enqueue(
        app,
        &core,
        target,
        resolved.tracks,
        placement,
        resolved.source,
        note,
    )
    .await
}

/// Your mixes, with a menu to pick one; `selected` is the picked mix's name.
pub fn list_card(icons: &Icons, mixes: &[Mix], selected: Option<&str>) -> Component {
    let mut parts = vec![cards::text(format!(
        "{} **Your mixes**\n-# Save the queue with `/mix save` or from the queue card.",
        icons.md(Icon::Queue)
    ))];
    if mixes.is_empty() {
        parts.push(cards::text(
            "No mixes yet. Queue some songs, then `/mix save` them under a name.",
        ));
        return cards::card(Some(color::BRAND), parts);
    }

    let lines = mixes.iter().enumerate().map(|(i, mix)| {
        let mut line = format!(
            "`{:>2}` **{}** · {} · {} · saved <t:{}:R>",
            i + 1,
            escape_md(&mix.name),
            songs(mix.tracks.len()),
            fmt_duration_words(mix.duration_ms()),
            mix.saved_at,
        );
        if mixes.len() <= PREVIEW_UP_TO {
            let preview: Vec<String> = mix
                .tracks
                .iter()
                .take(3)
                .map(|t| escape_md(&truncate(&t.title, 30)))
                .collect();
            let more = if mix.tracks.len() > 3 { ", …" } else { "" };
            line.push_str(&format!("\n-# {}{more}", preview.join(", ")));
        }
        line
    });
    parts.push(cards::divider());
    parts.extend(cards::text_blocks(lines));
    parts.push(cards::row(vec![cards::select(
        "loski:mixsel",
        "Pick a mix to play, rename or delete",
        false,
        mixes
            .iter()
            .take(MAX_MIXES)
            .map(|m| {
                let mut option = cards::option(
                    truncate(&m.name, 100),
                    m.name.clone(),
                    Some(format!(
                        "{} · {}",
                        songs(m.tracks.len()),
                        fmt_duration_words(m.duration_ms())
                    )),
                    None,
                );
                option.default = selected == Some(m.name.as_str());
                option
            })
            .collect(),
    )]));

    match selected.and_then(|name| mixes.iter().find(|m| m.name == name)) {
        Some(mix) => {
            let name = &mix.name;
            parts.push(cards::text(format!(
                "-# SELECTED\n**{}** · {} · {}",
                escape_md(name),
                songs(mix.tracks.len()),
                fmt_duration_words(mix.duration_ms())
            )));
            parts.push(cards::row(vec![
                Btn::icon(&format!("loski:mixgo:end:{name}"), Icon::Add)
                    .with_label("Add to queue")
                    .style(ButtonStyle::Primary)
                    .build(icons),
                Btn::icon(&format!("loski:mixgo:next:{name}"), Icon::PlayNext)
                    .with_label("Play next")
                    .build(icons),
                Btn::label(&format!("loski:mixren:{name}"), "Rename").build(icons),
                Btn::label(&format!("loski:mixdel:{name}"), "Delete")
                    .style(ButtonStyle::Danger)
                    .build(icons),
                Btn::label("loski:mixback", "Back").build(icons),
            ]));
        }
        None => parts.push(cards::row(vec![
            Btn::label("loski:qclose", "Close").build(icons),
        ])),
    }
    cards::card(Some(color::BRAND), parts)
}

fn find_exact(app: &App, name: &str) -> Result<Mix> {
    app.store
        .load_mixes()
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| not_found(name))
}

/// The list card's menu (a mix was picked) or its Back button (`None`).
pub async fn select(ix: &Ix, name: Option<&str>) -> Result<()> {
    let mixes = ix.app.store.load_mixes();
    ix.update_message(vec![list_card(&ix.app.icons, &mixes, name)])
        .await
}

/// `how:name` from a picked mix's Add to queue / Play next buttons.
pub async fn play_from_card(ix: &Ix, rest: &str) -> Result<()> {
    let app = &ix.app;
    let (how, name) = rest.split_once(':').unwrap_or(("end", rest));
    let placement = if how == "next" {
        Placement::Next
    } else {
        Placement::End
    };
    let mix = find_exact(app, name)?;
    let target = commands::require_voice(ix)?;
    ix.ack().await?;
    let card = load(app, target, &mix, false, placement).await?;
    ix.followup_temp(vec![card], SHORT_SECS).await
}

pub async fn delete_from_card(ix: &Ix, name: &str) -> Result<()> {
    let app = &ix.app;
    let mut mixes = app.store.load_mixes();
    mixes.retain(|m| m.name != name);
    app.store.save_mixes(&mixes)?;
    ix.update_message(vec![list_card(&app.icons, &mixes, None)])
        .await
}

/// "Rename" on a picked mix: asks for the new name.
pub async fn ask_new_name(ix: &Ix, name: &str) -> Result<()> {
    find_exact(&ix.app, name)?;
    ix.modal(
        &format!("loski:m:mixren:{name}"),
        "Rename the mix",
        vec![cards::text_field(
            "name",
            "New name",
            None,
            "late night",
            Some(name.to_string()),
            NAME_MAX as u16,
        )],
    )
    .await
}

pub async fn rename(ix: &Ix, old: &str, new: &str) -> Result<()> {
    let app = &ix.app;
    let new: String = new.trim().chars().take(NAME_MAX).collect();
    if new.is_empty() {
        return Err(oops("Give the mix a name."));
    }
    let mut mixes = app.store.load_mixes();
    if mixes
        .iter()
        .any(|m| m.name != old && m.name.eq_ignore_ascii_case(&new))
    {
        return Err(oops(format!(
            "You already have a mix called **{}**.",
            escape_md(&new)
        )));
    }
    let mix = mixes
        .iter_mut()
        .find(|m| m.name == old)
        .ok_or_else(|| not_found(old))?;
    mix.name.clone_from(&new);
    app.store.save_mixes(&mixes)?;
    ix.update_message(vec![list_card(&app.icons, &mixes, Some(&new))])
        .await
}

/// What you've typed so far into a mix name, wherever it sits in the command.
fn focused(options: &[CommandDataOption]) -> Option<&str> {
    options.iter().find_map(|option| match &option.value {
        CommandOptionValue::Focused(value, _) => Some(value.as_str()),
        CommandOptionValue::SubCommand(inner) | CommandOptionValue::SubCommandGroup(inner) => {
            focused(inner)
        }
        _ => None,
    })
}

pub fn autocomplete(app: &App, data: &CommandData) -> Vec<CommandOptionChoice> {
    let wanted = focused(&data.options)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    app.store
        .load_mixes()
        .into_iter()
        .filter(|m| m.name.to_lowercase().contains(&wanted))
        .take(MAX_MIXES)
        .map(|m| {
            let label = format!(
                "{} · {} · {}",
                m.name,
                songs(m.tracks.len()),
                fmt_duration_words(m.duration_ms())
            );
            commands::choice(label, m.name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mix(name: &str) -> Mix {
        Mix {
            name: name.into(),
            saved_at: 1_760_000_000,
            tracks: (0..40)
                .map(|n| MixTrack {
                    id: "4uLU6hMCjMI75M1A2tKUQC".into(),
                    title: format!("A *long* song title number {n} (Live at Wembley)"),
                    artists: "Some Artist".into(),
                    duration_ms: 213_000,
                })
                .collect(),
        }
    }

    #[test]
    fn finds_mixes_by_exact_name_first() {
        let mixes = [mix("late night drive"), mix("Night")];
        assert_eq!(find_in(&mixes, "night").unwrap().name, "Night");
        assert_eq!(find_in(&mixes, "DRIVE").unwrap().name, "late night drive");
        assert!(find_in(&mixes, "gym").is_none());
    }
}
