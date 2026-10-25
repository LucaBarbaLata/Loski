//! Sound commands: /filter, /sleep and /normalize.

use std::time::Duration;

use anyhow::Result;
use twilight_interactions::command::{CommandModel, CommandOption, CreateCommand, CreateOption};

use super::{SHORT_SECS, cards, interact::Ix, oops};
use crate::{dsp::Filter, personality};

#[derive(CommandOption, CreateOption)]
pub enum FilterChoice {
    #[option(name = "No filter", value = "off")]
    Off,
    #[option(name = "Bass boost", value = "bass")]
    Bass,
    #[option(name = "Treble boost", value = "treble")]
    Treble,
    #[option(name = "Vocal boost", value = "vocal")]
    Vocal,
    #[option(name = "Nightcore (faster, higher)", value = "nightcore")]
    Nightcore,
    #[option(name = "Slowed + reverb", value = "slowed")]
    Slowed,
    #[option(name = "8D (use headphones)", value = "8d")]
    EightD,
}

impl FilterChoice {
    fn filter(&self) -> Filter {
        match self {
            FilterChoice::Off => Filter::Off,
            FilterChoice::Bass => Filter::BassBoost,
            FilterChoice::Treble => Filter::TrebleBoost,
            FilterChoice::Vocal => Filter::VocalBoost,
            FilterChoice::Nightcore => Filter::Nightcore,
            FilterChoice::Slowed => Filter::Slowed,
            FilterChoice::EightD => Filter::EightD,
        }
    }
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "filter",
    desc = "Change how the music sounds: bass boost, nightcore, 8D and more"
)]
pub struct FilterCmd {
    /// The sound you want
    sound: FilterChoice,
}

#[derive(CommandOption, CreateOption)]
pub enum SleepChoice {
    #[option(name = "in 15 minutes", value = "15")]
    In15,
    #[option(name = "in 30 minutes", value = "30")]
    In30,
    #[option(name = "in 45 minutes", value = "45")]
    In45,
    #[option(name = "in 1 hour", value = "60")]
    In60,
    #[option(name = "in 90 minutes", value = "90")]
    In90,
    #[option(name = "in 2 hours", value = "120")]
    In120,
    #[option(name = "after this song", value = "song")]
    AfterSong,
    #[option(name = "cancel the timer", value = "off")]
    Cancel,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "sleep",
    desc = "Fade out and leave after a while, or after this song"
)]
pub struct SleepCmd {
    /// When to stop
    when: SleepChoice,
}

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "normalize",
    desc = "Turn volume levelling between songs on or off"
)]
pub struct NormalizeCmd {
    /// Level songs to a similar loudness, like Spotify's own setting
    on: Option<bool>,
}

pub async fn filter(ix: &Ix, cmd: FilterCmd) -> Result<()> {
    let core = ix.app.core()?;
    let filter = cmd.sound.filter();
    core.engine.set_filter(filter).map_err(oops)?;
    let text = match filter {
        Filter::Off => "Filters off. Back to the original sound.".to_string(),
        other => format!(
            "**{}**: {}.",
            other.label(),
            other.description().to_lowercase()
        ),
    };
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}

pub async fn sleep(ix: &Ix, cmd: SleepCmd) -> Result<()> {
    let core = ix.app.core()?;
    let engine = &core.engine;
    let minutes = |m: u64| Duration::from_secs(m * 60);
    let text = match cmd.when {
        SleepChoice::Cancel => {
            if !engine.cancel_sleep() {
                return Err(oops("There's no sleep timer to cancel."));
            }
            "Sleep timer cancelled. Keep the music going.".to_string()
        }
        SleepChoice::AfterSong => {
            if !engine.sleep_after_song() {
                return Err(oops(personality::nothing_playing()));
            }
            "🌙 I'll leave when this song ends.".to_string()
        }
        choice => {
            let mins = match choice {
                SleepChoice::In15 => 15,
                SleepChoice::In30 => 30,
                SleepChoice::In45 => 45,
                SleepChoice::In60 => 60,
                SleepChoice::In90 => 90,
                _ => 120,
            };
            engine.sleep_in(minutes(mins));
            let end = crate::spotify::auth::now_unix() + mins as i64 * 60;
            format!("🌙 Sleep timer set. I'll fade out and leave <t:{end}:R>.")
        }
    };
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}

pub async fn normalize(ix: &Ix, cmd: NormalizeCmd) -> Result<()> {
    let core = ix.app.core()?;
    let engine = &core.engine;
    let text = match cmd.on {
        None => format!(
            "Volume levelling is **{}**.",
            if engine.normalise() { "on" } else { "off" }
        ),
        Some(on) => {
            let changed = engine.set_normalise(on);
            let state = if on { "on" } else { "off" };
            if changed {
                format!(
                    "Volume levelling is **{state}**. Remembered for next time.\n-# Spotify Connect picks it up after Loski restarts."
                )
            } else {
                format!("Volume levelling was already **{state}**.")
            }
        }
    };
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}
