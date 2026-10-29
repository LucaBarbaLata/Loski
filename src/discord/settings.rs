//! /settings: everything that isn't a password, changed from Discord and
//! saved in `data/preferences.json`. Anything you never changed there
//! still comes from `.env`.

use std::time::Duration;

use anyhow::Result;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::{
    channel::{
        ChannelType,
        message::{
            Component,
            component::{SelectDefaultValue, SelectMenu, SelectMenuOption, SelectMenuType},
        },
    },
    id::{Id, marker::ChannelMarker},
};

use super::{
    App,
    cards::{self, Btn, color},
    friends,
    icons::{Icon, Icons},
    interact::Ix,
    oops, panel, sound, voice,
};
use crate::{banner::Style, config::AudioBitrate, store::Prefs};

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "settings",
    desc = "Change how Loski works: the panel, leaving, 24/7, sound and requests"
)]
pub struct SettingsCmd;

impl App {
    pub fn prefs(&self) -> Prefs {
        self.prefs.lock().clone()
    }

    /// Changes your settings and saves them straight away.
    pub fn update_prefs(&self, change: impl FnOnce(&mut Prefs)) {
        let mut prefs = self.prefs.lock();
        change(&mut prefs);
        self.store.save_prefs(&prefs);
    }

    /// How long Loski stays after you leave the call.
    pub fn leave_after(&self) -> Duration {
        self.prefs
            .lock()
            .leave_after_secs
            .map_or(self.cfg.leave_after, Duration::from_secs)
    }

    /// How long Loski stays after the music ends.
    pub fn idle_leave(&self) -> Duration {
        self.prefs
            .lock()
            .idle_leave_secs
            .map_or(self.cfg.idle_leave, Duration::from_secs)
    }

    /// Where "today" and "this month" are, for /stats and the monthly recap.
    pub fn timezone(&self) -> chrono_tz::Tz {
        self.prefs
            .lock()
            .timezone
            .as_deref()
            .and_then(|name| name.parse().ok())
            .unwrap_or(self.cfg.timezone)
    }

    pub fn bitrate(&self) -> AudioBitrate {
        match self.prefs.lock().bitrate_kbps {
            None => self.cfg.bitrate,
            Some(0) => AudioBitrate::Auto,
            Some(kbps) => AudioBitrate::Fixed(kbps as i32 * 1000),
        }
    }

    /// 24/7 mode is on.
    pub fn always_on(&self) -> bool {
        self.prefs.lock().always_on
    }

    pub fn look(&self) -> panel::Look {
        let prefs = self.prefs.lock();
        panel::Look {
            compact: prefs.compact,
            style: prefs.style,
            staying: prefs.always_on,
        }
    }
}

/// Everything the settings card shows.
pub struct SettingsView {
    pub panel_channel: Option<Id<ChannelMarker>>,
    pub compact: bool,
    pub style: Style,
    pub always_on: bool,
    /// The channel 24/7 keeps Loski in right now.
    pub staying_in: Option<Id<ChannelMarker>>,
    pub leave_after: Duration,
    pub idle_leave: Duration,
    pub crossfade_secs: u8,
    pub normalise: bool,
    pub bitrate: AudioBitrate,
    pub timezone: String,
}

impl SettingsView {
    pub fn read(app: &App) -> Self {
        let prefs = app.prefs();
        let core = app.core.get();
        Self {
            panel_channel: prefs.panel_channel.and_then(Id::new_checked),
            compact: prefs.compact,
            style: prefs.style,
            always_on: prefs.always_on,
            staying_in: prefs
                .stay_in
                .and_then(|(_, channel)| Id::new_checked(channel)),
            leave_after: app.leave_after(),
            idle_leave: app.idle_leave(),
            crossfade_secs: core.map_or(0, |c| c.engine.crossfade()),
            normalise: core.map_or(app.cfg.normalise, |c| c.engine.normalise()),
            bitrate: app.bitrate(),
            timezone: app.timezone().name().to_string(),
        }
    }
}

const LEAVE_AFTER: [u64; 5] = [0, 60, 300, 900, 1800];
const IDLE_LEAVE: [u64; 5] = [60, 300, 900, 1800, 3600];
const CROSSFADES: [u8; 7] = [0, 2, 4, 6, 8, 10, 12];
/// kbps, and the server boost level each needs.
const BITRATES: [(u32, u8); 5] = [(64, 0), (96, 0), (128, 1), (256, 2), (384, 3)];

/// "1 minute", "90 seconds", "1 hour".
fn wait_words(secs: u64) -> String {
    match secs {
        s if s % 3600 == 0 && s > 0 => match s / 3600 {
            1 => "1 hour".to_string(),
            h => format!("{h} hours"),
        },
        s if s % 60 == 0 && s > 0 => match s / 60 {
            1 => "1 minute".to_string(),
            m => format!("{m} minutes"),
        },
        1 => "1 second".to_string(),
        s => format!("{s} seconds"),
    }
}

/// The listed choices plus the current one, if `.env` set something else.
fn with_current<T: Copy + Ord>(listed: &[T], current: T) -> Vec<T> {
    let mut all = listed.to_vec();
    if !all.contains(&current) {
        all.push(current);
        all.sort();
    }
    all
}

/// A row with one menu of fixed choices, the current one picked.
fn menu(
    id: &str,
    placeholder: &str,
    disabled: bool,
    options: Vec<SelectMenuOption>,
    current: &str,
) -> Component {
    let options = options
        .into_iter()
        .map(|mut option| {
            option.default = option.value == current;
            option
        })
        .collect();
    cards::row(vec![cards::select(id, placeholder, disabled, options)])
}

fn option(label: impl Into<String>, value: impl Into<String>) -> SelectMenuOption {
    cards::option(label, value, None, None)
}

fn described(
    label: impl Into<String>,
    value: impl Into<String>,
    description: &str,
) -> SelectMenuOption {
    cards::option(label, value, Some(description.to_string()), None)
}

fn channel_menu(view: &SettingsView) -> Component {
    cards::row(vec![Component::SelectMenu(SelectMenu {
        id: None,
        channel_types: Some(vec![ChannelType::GuildText, ChannelType::GuildVoice]),
        custom_id: "loski:set:panel".to_string(),
        default_values: view
            .panel_channel
            .map(|channel| vec![SelectDefaultValue::Channel(channel)]),
        disabled: false,
        kind: SelectMenuType::Channel,
        max_values: Some(1),
        // Clearing it sends the panel back to the voice channel's chat.
        min_values: Some(0),
        options: None,
        placeholder: Some("Panel channel: the voice channel's chat".to_string()),
        required: None,
    })])
}

pub fn settings_card(icons: &Icons, view: &SettingsView) -> Component {
    let mut parts = vec![
        cards::text(format!(
            "{} **Settings**\nSaved as you go, and used right away.",
            icons.md(Icon::Settings)
        )),
        cards::divider(),
        cards::text(match view.panel_channel {
            Some(channel) => format!("-# PANEL\nThe panel always goes to <#{channel}>."),
            None => "-# PANEL\nThe panel lives in the chat of the voice channel I'm in. `/panel` brings it somewhere else until I leave the call.".to_string(),
        }),
        channel_menu(view),
        menu(
            "loski:set:look",
            "Panel look",
            false,
            vec![
                described(
                    "Panel: glass",
                    "glass",
                    "The cover blurred behind a frosted panel",
                ),
                described(
                    "Panel: vinyl",
                    "vinyl",
                    "The cover as a sleeve, the record sliding out",
                ),
                described(
                    "Panel: minimal",
                    "minimal",
                    "The cover beside a flat colour from it",
                ),
                described(
                    "Panel: compact, without artwork",
                    "compact",
                    "One row of buttons, for busy chats and phones",
                ),
            ],
            if view.compact {
                "compact"
            } else {
                view.style.key()
            },
        ),
    ];

    let voice_text = match (view.always_on, view.staying_in) {
        (false, _) => "-# VOICE".to_string(),
        (true, Some(channel)) => format!(
            "-# VOICE\n24/7 keeps me in <#{channel}>, playing even when nobody's there. When the queue runs out I keep going with a radio. `/stop` sends me away until the next `/play`."
        ),
        (true, None) => "-# VOICE\n24/7 is on: I'll stay in the next channel you `/join` or `/play` in, and keep the music going there.".to_string(),
    };
    let leave_options = with_current(&LEAVE_AFTER, view.leave_after.as_secs())
        .into_iter()
        .map(|secs| match secs {
            0 => option("Leave as soon as you leave the call", "0"),
            s => option(
                format!("Leave {} after you leave the call", wait_words(s)),
                s.to_string(),
            ),
        })
        .collect();
    let idle_options = with_current(&IDLE_LEAVE, view.idle_leave.as_secs())
        .into_iter()
        .map(|secs| {
            option(
                format!("Leave {} after the music ends", wait_words(secs)),
                secs.to_string(),
            )
        })
        .collect();
    parts.extend([
        cards::text(voice_text),
        menu(
            "loski:set:247",
            "24/7",
            false,
            vec![
                described(
                    "24/7 off",
                    "off",
                    "Leave when you go, or a while after the music ends",
                ),
                described(
                    "24/7 on: stay and keep the music going",
                    "on",
                    "Radio when the queue runs out; back after a restart",
                ),
            ],
            if view.always_on { "on" } else { "off" },
        ),
        menu(
            "loski:set:leave",
            "When you leave the call",
            view.always_on,
            leave_options,
            &view.leave_after.as_secs().to_string(),
        ),
        menu(
            "loski:set:idle",
            "When the music ends",
            view.always_on,
            idle_options,
            &view.idle_leave.as_secs().to_string(),
        ),
    ]);

    let crossfade_options = with_current(&CROSSFADES, view.crossfade_secs)
        .into_iter()
        .map(|secs| match secs {
            0 => described(
                "Crossfade off",
                "0",
                "Songs play back to back, without a gap",
            ),
            s => described(
                format!("Crossfade {s} seconds"),
                s.to_string(),
                "Songs from the same album still play gapless",
            ),
        })
        .collect();
    let current_kbps = match view.bitrate {
        AudioBitrate::Auto => 0,
        AudioBitrate::Fixed(bits) => (bits / 1000) as u32,
    };
    let mut quality_options = vec![described(
        "Audio quality: the best this server allows",
        "0",
        "96 kbps, more on boosted servers (up to 384)",
    )];
    let listed = BITRATES.map(|(kbps, _)| kbps);
    let kbps_choices = match current_kbps {
        0 => listed.to_vec(),
        current => with_current(&listed, current),
    };
    for kbps in kbps_choices {
        let needs = BITRATES
            .iter()
            .find(|(k, _)| *k == kbps)
            .map_or(0, |(_, level)| *level);
        quality_options.push(match needs {
            0 => option(format!("Audio quality: {kbps} kbps"), kbps.to_string()),
            level => described(
                format!("Audio quality: {kbps} kbps"),
                kbps.to_string(),
                &format!("Needs server boost level {level}"),
            ),
        });
    }
    parts.extend([
        cards::text("-# SOUND"),
        menu(
            "loski:set:xfade",
            "Crossfade",
            false,
            crossfade_options,
            &view.crossfade_secs.to_string(),
        ),
        menu(
            "loski:set:level",
            "Volume levelling",
            false,
            vec![
                described(
                    "Volume levelling on",
                    "on",
                    "Songs at a similar loudness, like Spotify's setting",
                ),
                described(
                    "Volume levelling off",
                    "off",
                    "Each song as loud as it was mastered",
                ),
            ],
            if view.normalise { "on" } else { "off" },
        ),
        menu(
            "loski:set:quality",
            "Audio quality",
            false,
            quality_options,
            &current_kbps.to_string(),
        ),
        cards::text(format!(
            "-# OTHER\nDays and months in `/stats` and the recap start at midnight in **{}**.",
            view.timezone
        )),
        cards::row(vec![
            Btn::icon("loski:set:friends", Icon::Friends)
                .with_label("Friends")
                .build(icons),
            Btn::icon("loski:set:eq", Icon::Filters)
                .with_label("My sound")
                .build(icons),
            Btn::label("loski:set:tz", "Change timezone").build(icons),
            Btn::label("loski:set:close", "Close").build(icons),
        ]),
    ]);
    cards::card(Some(color::BRAND), parts)
}

pub async fn settings(ix: &Ix, _: SettingsCmd) -> Result<()> {
    let card = settings_card(&ix.app.icons, &SettingsView::read(&ix.app));
    ix.reply_private(vec![card]).await
}

/// A menu or button on the settings card; `key` is what follows `loski:set:`.
pub async fn change(ix: &Ix, key: &str, value: Option<&str>) -> Result<()> {
    let app = &ix.app;
    let number = || value.and_then(|v| v.parse::<u64>().ok());
    // Some changes move the panel, which takes longer than Discord waits
    // for an answer; those happen after the card is updated.
    let mut repost = false;
    let mut always_on_changed = false;
    match key {
        "panel" => {
            let channel = value
                .and_then(|v| v.parse().ok())
                .and_then(Id::<ChannelMarker>::new_checked);
            app.update_prefs(|p| p.panel_channel = channel.map(Id::get));
            app.ui.set_fixed(channel);
            repost = app.ui.has_panel();
        }
        "look" => {
            let style = value.and_then(Style::from_key);
            app.update_prefs(|p| {
                p.compact = style.is_none();
                if let Some(style) = style {
                    p.style = style;
                }
            });
            repost = app.ui.has_panel();
        }
        "247" => {
            set_always_on(app, value == Some("on"));
            // The panel says whether Loski is about to leave.
            app.ui.mark_dirty();
            always_on_changed = true;
        }
        "leave" => {
            if let Some(secs) = number() {
                app.update_prefs(|p| p.leave_after_secs = Some(secs));
            }
        }
        "idle" => {
            if let Some(secs) = number() {
                app.update_prefs(|p| p.idle_leave_secs = Some(secs));
            }
        }
        "xfade" => {
            let core = app.core()?;
            if let Some(secs) = number() {
                core.engine.set_crossfade(secs.min(12) as u8);
            }
        }
        "level" => {
            let core = app.core()?;
            core.engine.set_normalise(value == Some("on"));
        }
        "quality" => {
            if let Some(kbps) = number() {
                app.update_prefs(|p| p.bitrate_kbps = Some(kbps.min(512) as u32));
                voice::apply_bitrate(app).await;
            }
        }
        "friends" => return friends::show(ix).await,
        "tz" => {
            return ix
                .modal(
                    "loski:m:tz",
                    "Timezone",
                    vec![cards::text_field(
                        "tz",
                        "Timezone",
                        Some("A name like Europe/Bucharest, America/New_York or UTC"),
                        "Europe/Bucharest",
                        Some(app.timezone().name().to_string()),
                        64,
                    )],
                )
                .await;
        }
        "eq" => {
            let card = sound::eq_card(&app.icons, &sound::EqView::read(app), true);
            return ix.update_message(vec![card]).await;
        }
        "close" => {
            ix.ack().await?;
            ix.delete_reply().await;
            return Ok(());
        }
        _ => {}
    }
    show(ix).await?;
    if repost && let Some(core) = app.core.get() {
        panel::repost(app, core).await;
    }
    // Starting a radio for 24/7 can take a moment too.
    if always_on_changed {
        voice::always_on_changed(app).await;
    }
    Ok(())
}

/// Puts the settings card back up to date.
pub async fn show(ix: &Ix) -> Result<()> {
    let card = settings_card(&ix.app.icons, &SettingsView::read(&ix.app));
    ix.update_message(vec![card]).await
}

/// The timezone form came back.
pub async fn set_timezone(ix: &Ix, input: &str) -> Result<()> {
    let name = input.trim();
    let tz: chrono_tz::Tz = name.parse().map_err(|_| {
        oops(format!(
            "I don't know the timezone \"{name}\". Use a name like Europe/Bucharest, America/New_York or UTC."
        ))
    })?;
    ix.app
        .update_prefs(|p| p.timezone = Some(tz.name().to_string()));
    show(ix).await
}

/// Turns 24/7 on or off, remembering the channel Loski is in now.
pub(super) fn set_always_on(app: &App, on: bool) {
    let here = app.voice.guild().zip(app.voice.channel());
    app.update_prefs(|p| {
        p.always_on = on;
        p.stay_in = match (on, here) {
            (true, Some((guild, channel))) => Some((guild.get(), channel.get())),
            _ => None,
        };
    });
}
