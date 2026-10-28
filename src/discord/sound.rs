//! Sound commands: /filter, /sleep and /normalize, and the "My sound" EQ card.

use std::time::Duration;

use anyhow::Result;
use twilight_interactions::command::{CommandModel, CommandOption, CreateCommand, CreateOption};
use twilight_model::channel::message::{Component, component::ButtonStyle};

use super::{
    App, SHORT_SECS,
    cards::{self, Btn, color},
    icons::{Icon, Icons},
    interact::Ix,
    oops, settings,
};
use crate::{
    dsp::{EQ_BANDS, EQ_RANGE, Filter},
    personality,
};

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
    #[option(name = "My sound (your own EQ, from /settings)", value = "mine")]
    Mine,
}

impl FilterChoice {
    fn filter(&self, eq: [i8; 5]) -> Filter {
        match self {
            FilterChoice::Off => Filter::Off,
            FilterChoice::Bass => Filter::BassBoost,
            FilterChoice::Treble => Filter::TrebleBoost,
            FilterChoice::Vocal => Filter::VocalBoost,
            FilterChoice::Nightcore => Filter::Nightcore,
            FilterChoice::Slowed => Filter::Slowed,
            FilterChoice::EightD => Filter::EightD,
            FilterChoice::Mine => Filter::Mine(eq),
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
    let filter = cmd.sound.filter(ix.app.prefs().eq);
    core.engine.set_filter(filter).map_err(oops)?;
    let text = match filter {
        Filter::Off => "Filters off. Back to the original sound.".to_string(),
        Filter::Mine(_) => "**My sound** is on. Change it from `/settings` → My sound.".to_string(),
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
                    "Volume levelling is **{state}**. Remembered for next time.\n-# Spotify Connect picks it up after I restart."
                )
            } else {
                format!("Volume levelling was already **{state}**.")
            }
        }
    };
    ix.reply_temp(vec![cards::info(&ix.app.icons, &text)], SHORT_SECS)
        .await
}

/// What the "My sound" card shows.
pub struct EqView {
    pub gains: [i8; 5],
    /// "My sound" is the filter playing now.
    pub on: bool,
}

impl EqView {
    pub fn read(app: &App) -> Self {
        Self {
            gains: app.prefs().eq,
            on: app
                .core
                .get()
                .is_some_and(|core| matches!(core.engine.mixer().filter(), Filter::Mine(_))),
        }
    }
}

fn gain_words(name: &str, gain: i8) -> String {
    match gain {
        0 => format!("{name}: flat"),
        g if g > 0 => format!("{name}: +{g} dB"),
        g => format!("{name}: {g} dB"),
    }
}

/// Your own EQ: a menu per band. `from_settings` puts a Back button on it
/// instead of Close.
pub fn eq_card(icons: &Icons, view: &EqView, from_settings: bool) -> Component {
    let origin = if from_settings { "s" } else { "p" };
    let status = if view.on {
        "It's on, so you hear each change right away."
    } else {
        "Press **Use My sound** to hear it."
    };
    let mut parts = vec![cards::text(format!(
        "{} **My sound**\nYour own EQ: turn each part of the sound up or down. {status}",
        icons.md(Icon::Filters)
    ))];
    for (band, (&(name, _), &gain)) in EQ_BANDS.iter().zip(&view.gains).enumerate() {
        let options = (-EQ_RANGE..=EQ_RANGE)
            .rev()
            .step_by(2)
            .map(|g| {
                let mut option = cards::option(gain_words(name, g), g.to_string(), None, None);
                option.default = g == gain;
                option
            })
            .collect();
        parts.push(cards::row(vec![cards::select(
            &format!("loski:eq:{origin}:{band}"),
            name,
            false,
            options,
        )]));
    }
    let toggle = if view.on {
        Btn::label(&format!("loski:eq:{origin}:off"), "Turn it off").build(icons)
    } else {
        Btn::icon(&format!("loski:eq:{origin}:on"), Icon::Filters)
            .with_label("Use My sound")
            .style(ButtonStyle::Primary)
            .build(icons)
    };
    let flat_id = format!("loski:eq:{origin}:flat");
    let leave_id = format!("loski:eq:{origin}:leave");
    parts.push(cards::row(vec![
        toggle,
        Btn::label(&flat_id, "Flat")
            .disabled(view.gains == [0; 5])
            .build(icons),
        Btn::label(&leave_id, if from_settings { "Back" } else { "Close" }).build(icons),
    ]));
    cards::card(Some(color::BRAND), parts)
}

/// A menu or button on the "My sound" card: `rest` is `origin:action`.
pub async fn eq_action(ix: &Ix, rest: &str, value: Option<&str>) -> Result<()> {
    let app = &ix.app;
    let (origin, action) = rest.split_once(':').unwrap_or(("p", rest));
    let from_settings = origin == "s";
    let mut playing = EqView::read(app).on;
    match action {
        "on" => playing = true,
        "off" => {
            app.core()?.engine.set_filter(Filter::Off).map_err(oops)?;
            playing = false;
        }
        "flat" => app.update_prefs(|p| p.eq = [0; 5]),
        "leave" if from_settings => return settings::show(ix).await,
        "leave" => {
            ix.ack().await?;
            ix.delete_reply().await;
            return Ok(());
        }
        band => {
            let gain = value.and_then(|v| v.parse::<i8>().ok());
            if let (Ok(band), Some(gain)) = (band.parse::<usize>(), gain)
                && band < EQ_BANDS.len()
            {
                app.update_prefs(|p| p.eq[band] = gain.clamp(-EQ_RANGE, EQ_RANGE));
            }
        }
    }
    if playing {
        let eq = app.prefs().eq;
        app.core()?
            .engine
            .set_filter(Filter::Mine(eq))
            .map_err(oops)?;
    }
    ix.update_message(vec![eq_card(&app.icons, &EqView::read(app), from_settings)])
        .await
}
