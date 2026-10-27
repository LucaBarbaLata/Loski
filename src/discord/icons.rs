//! Loski's icon set, uploaded as the bot's own (application) emojis.
//!
//! The PNGs are built into the binary. At startup Loski lists its emojis,
//! uploads any that are missing and replaces any whose art changed, keeping
//! a small record in `data/icons.json`. Until that finishes, or if it fails,
//! icons fall back to standard emoji so nothing breaks.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use twilight_http::Client;
use twilight_model::{
    channel::message::EmojiReactionType,
    id::{
        Id,
        marker::{ApplicationMarker, EmojiMarker},
    },
};

use crate::store::write_atomic;

macro_rules! icons {
    ($($variant:ident => $name:literal, $fallback:literal;)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Icon { $($variant),* }

        impl Icon {
            pub const ALL: &[Icon] = &[$(Icon::$variant),*];

            /// Emoji name on Discord, also the PNG's file name.
            pub fn name(self) -> &'static str {
                match self { $(Icon::$variant => $name),* }
            }

            /// Standard emoji used until the custom one is available.
            pub fn fallback(self) -> &'static str {
                match self { $(Icon::$variant => $fallback),* }
            }

            fn png(self) -> &'static [u8] {
                match self {
                    $(Icon::$variant => include_bytes!(concat!("../../assets/icons/", $name, ".png"))),*
                }
            }
        }
    };
}

icons! {
    Play => "play", "▶️";
    Pause => "pause", "⏸️";
    Prev => "prev", "⏮️";
    Next => "next", "⏭️";
    Shuffle => "shuffle", "🔀";
    Repeat => "repeat", "🔁";
    RepeatOne => "repeat_one", "🔂";
    VolDown => "vol_down", "🔉";
    VolUp => "vol_up", "🔊";
    Queue => "queue", "📜";
    Stop => "stop", "⏹️";
    Back15 => "back15", "⏪";
    Fwd15 => "fwd15", "⏩";
    Radio => "radio", "📻";
    Filters => "filters", "🎛️";
    Sleep => "sleep", "🌙";
    Liked => "liked", "💚";
    Connect => "connect", "🔈";
    Stats => "stats", "📊";
    History => "history", "🕘";
    Search => "search", "🔎";
    PlayNext => "play_next", "⤴️";
    Add => "add", "➕";
    Left => "left", "◀️";
    Right => "right", "▶️";
    Done => "done", "✅";
    Problem => "problem", "⚠️";
    Info => "info", "ℹ️";
    Music => "music", "🎵";
    Lyrics => "lyrics", "🎤";
    Settings => "settings", "⚙️";
    Undo => "undo", "↩️";
    Friends => "friends", "👥";
    BarStartOn => "bar_start_on", "▬";
    BarMidOn => "bar_mid_on", "▬";
    BarEndOn => "bar_end_on", "▬";
    BarStartOff => "bar_start_off", "▭";
    BarMidOff => "bar_mid_off", "▭";
    BarEndOff => "bar_end_off", "▭";
    BarKnob => "bar_knob", "🔘";
}

impl Icon {
    fn digest(self) -> String {
        let hash = Sha256::digest(self.png());
        hash.iter().take(8).map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Default)]
pub struct Icons {
    ids: RwLock<HashMap<Icon, Id<EmojiMarker>>>,
}

impl Icons {
    /// For buttons and select options.
    pub fn emoji(&self, icon: Icon) -> EmojiReactionType {
        match self.ids.read().get(&icon) {
            Some(id) => EmojiReactionType::Custom {
                animated: false,
                id: *id,
                name: Some(icon.name().to_string()),
            },
            None => EmojiReactionType::Unicode {
                name: icon.fallback().to_string(),
            },
        }
    }

    /// For text: `<:name:id>`.
    pub fn md(&self, icon: Icon) -> String {
        match self.ids.read().get(&icon) {
            Some(id) => format!("<:{}:{}>", icon.name(), id),
            None => icon.fallback().to_string(),
        }
    }

    pub fn is_ready(&self) -> bool {
        self.ids.read().len() == Icon::ALL.len()
    }

    /// Every icon with a made-up id, as if they were uploaded (for drawing
    /// the cards outside Discord).
    #[cfg(test)]
    pub fn uploaded() -> Self {
        let icons = Self::default();
        for (n, &icon) in Icon::ALL.iter().enumerate() {
            icons.ids.write().insert(icon, Id::new(n as u64 + 1));
        }
        icons
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Record {
    /// Emoji name -> digest of the art that was uploaded under it.
    uploaded: HashMap<String, String>,
}

/// Makes sure every icon exists as an application emoji with the current art.
pub async fn sync(
    icons: Arc<Icons>,
    http: &Client,
    app_id: Id<ApplicationMarker>,
    data_dir: PathBuf,
) -> Result<()> {
    let record_path = data_dir.join("icons.json");
    let mut record: Record = std::fs::read_to_string(&record_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let existing = http
        .get_application_emojis(app_id)
        .await
        .context("could not list Loski's emojis")?
        .model()
        .await?
        .items;
    let mut by_name: HashMap<String, Id<EmojiMarker>> =
        existing.into_iter().map(|e| (e.name, e.id)).collect();

    let mut uploaded = 0;
    for &icon in Icon::ALL {
        let digest = icon.digest();
        let current = by_name.get(icon.name()).copied();
        let stale = match (current, record.uploaded.get(icon.name())) {
            (Some(_), Some(known)) => known != &digest,
            // Uploaded by an earlier install without a record; keep it.
            (Some(_), None) => false,
            (None, _) => true,
        };

        let id = if stale {
            if let Some(old) = current {
                let _ = http.delete_application_emoji(app_id, old).await;
                by_name.remove(icon.name());
            }
            let image = format!("data:image/png;base64,{}", STANDARD.encode(icon.png()));
            let emoji = http
                .add_application_emoji(app_id, icon.name(), &image)
                .await
                .with_context(|| format!("could not upload the {} icon", icon.name()))?
                .model()
                .await?;
            uploaded += 1;
            emoji.id
        } else {
            current.expect("present when not stale")
        };

        record.uploaded.insert(icon.name().to_string(), digest);
        icons.ids.write().insert(icon, id);
    }

    if let Ok(json) = serde_json::to_string_pretty(&record) {
        let _ = write_atomic(&record_path, json.as_bytes());
    }
    if uploaded > 0 {
        tracing::info!("Uploaded {uploaded} icons as Loski's emojis");
    }
    Ok(())
}
