//! Small on-disk state: remembered settings, listening history, saved
//! mixes, and what your friends liked.

use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub volume: u8,
    pub crossfade_secs: u8,
    /// Set with /normalize; falls back to NORMALISE in .env.
    pub normalise: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: 70,
            crossfade_secs: 0,
            normalise: None,
        }
    }
}

/// What you change in `/settings`. Anything left unset falls back to `.env`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// The channel the panel always goes to, instead of wherever `/play` was used.
    pub panel_channel: Option<u64>,
    /// The slim panel, without the artwork.
    pub compact: bool,
    /// How long Loski waits after you leave the call.
    pub leave_after_secs: Option<u64>,
    /// How long Loski waits after the music ends.
    pub idle_leave_secs: Option<u64>,
    /// In kbps; 0 means the best the server allows.
    pub bitrate_kbps: Option<u32>,
    pub timezone: Option<String>,
    pub requests_off: bool,
    /// "My sound": decibels for bass, low-mid, mid, high-mid and treble.
    pub eq: [i8; 5],
    /// 24/7: stay in voice and keep the music going.
    pub always_on: bool,
    /// Where 24/7 keeps Loski (guild, channel), so it can come back after a
    /// restart. Cleared by `/stop` until the next `/play`.
    pub stay_in: Option<(u64, u64)>,
    /// Co-DJs picked by name (Discord user ids).
    pub co_djs: Vec<u64>,
    /// Everyone with this role is a co-DJ too.
    pub dj_role: Option<u64>,
    /// Co-DJs can add songs, but not control playback.
    pub djs_queue_only: bool,
    pub vote_skip_off: bool,
}

/// One song you listened to. Entries from before listening time was
/// tracked only have the first four fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// When the song started, as a Unix timestamp in seconds.
    pub played_at: i64,
    pub uri: String,
    pub title: String,
    pub artists: String,
    /// Each artist on their own, so "Tyler, The Creator" stays one name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artist_list: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    /// How long you actually listened (pauses don't count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listened_ms: Option<u64>,
    /// Friends in the call while it played (Discord user ids).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub with: Vec<u64>,
    /// The friend who asked for it with /request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<u64>,
}

impl HistoryEntry {
    /// The Spotify id from `uri`.
    pub fn track_id(&self) -> &str {
        self.uri.rsplit(':').next().unwrap_or_default()
    }

    pub fn artist_names(&self) -> Vec<&str> {
        if self.artist_list.is_empty() {
            self.artists.split(", ").filter(|a| !a.is_empty()).collect()
        } else {
            self.artist_list.iter().map(String::as_str).collect()
        }
    }
}

/// Someone pressed the heart on the panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Like {
    pub at: i64,
    pub uri: String,
    pub title: String,
    pub artists: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    /// Who liked it (a Discord user id).
    pub by: u64,
}

/// A queue saved with `/mix save`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mix {
    pub name: String,
    pub saved_at: i64,
    pub tracks: Vec<MixTrack>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixTrack {
    /// Spotify id (base62).
    pub id: String,
    pub title: String,
    pub artists: String,
    pub duration_ms: u32,
}

impl Mix {
    pub fn duration_ms(&self) -> u64 {
        self.tracks.iter().map(|t| t.duration_ms as u64).sum()
    }
}

pub struct Store {
    dir: PathBuf,
    write_lock: Mutex<()>,
}

/// History is trimmed back to this many entries once it grows past twice that.
const HISTORY_KEEP: usize = 5_000;

impl Store {
    pub fn new(dir: PathBuf) -> std::io::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            write_lock: Mutex::new(()),
        })
    }

    pub fn load_settings(&self) -> Settings {
        fs::read_to_string(self.dir.join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &Settings) {
        let _guard = self.write_lock.lock();
        let json = serde_json::to_string_pretty(settings).expect("settings serialize");
        if let Err(e) = write_atomic(&self.dir.join("settings.json"), json.as_bytes()) {
            tracing::warn!("could not save settings: {e}");
        }
    }

    pub fn load_prefs(&self) -> Prefs {
        fs::read_to_string(self.dir.join("preferences.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_prefs(&self, prefs: &Prefs) {
        let _guard = self.write_lock.lock();
        let json = serde_json::to_string_pretty(prefs).expect("prefs serialize");
        if let Err(e) = write_atomic(&self.dir.join("preferences.json"), json.as_bytes()) {
            tracing::warn!("could not save your settings: {e}");
        }
    }

    pub fn append_history(&self, entry: &HistoryEntry) {
        let _guard = self.write_lock.lock();
        let path = self.dir.join("history.jsonl");
        let line = serde_json::to_string(entry).expect("history serialize");
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = result {
            tracing::warn!("could not write history: {e}");
            return;
        }

        // Keep the file from growing forever.
        if let Ok(text) = fs::read_to_string(&path) {
            let lines: Vec<&str> = text.lines().collect();
            if lines.len() > HISTORY_KEEP * 2 {
                let kept = lines[lines.len() - HISTORY_KEEP..].join("\n") + "\n";
                let _ = write_atomic(&path, kept.as_bytes());
            }
        }
    }

    /// Most recent first.
    pub fn recent_history(&self, limit: usize) -> Vec<HistoryEntry> {
        let Ok(text) = fs::read_to_string(self.dir.join("history.jsonl")) else {
            return Vec::new();
        };
        text.lines()
            .rev()
            .filter_map(|l| serde_json::from_str(l).ok())
            .take(limit)
            .collect()
    }

    /// Everything, oldest first.
    pub fn all_history(&self) -> Vec<HistoryEntry> {
        let Ok(text) = fs::read_to_string(self.dir.join("history.jsonl")) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// Newest first.
    pub fn load_mixes(&self) -> Vec<Mix> {
        fs::read_to_string(self.dir.join("mixes.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_mixes(&self, mixes: &[Mix]) -> std::io::Result<()> {
        let _guard = self.write_lock.lock();
        let json = serde_json::to_string_pretty(mixes).expect("mixes serialize");
        write_atomic(&self.dir.join("mixes.json"), json.as_bytes())
    }

    pub fn append_likes(&self, likes: &[Like]) {
        let _guard = self.write_lock.lock();
        let mut text = String::new();
        for like in likes {
            text.push_str(&serde_json::to_string(like).expect("like serialize"));
            text.push('\n');
        }
        let result = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("likes.jsonl"))
            .and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = result {
            tracing::warn!("could not save likes: {e}");
        }
    }

    /// Everything anyone liked, oldest first.
    pub fn all_likes(&self) -> Vec<Like> {
        let Ok(text) = fs::read_to_string(self.dir.join("likes.jsonl")) else {
            return Vec::new();
        };
        text.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    /// The names your friends had when Loski last saw them, by user id.
    pub fn load_names(&self) -> HashMap<u64, String> {
        fs::read_to_string(self.dir.join("friends.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_names(&self, names: &HashMap<u64, String>) {
        let _guard = self.write_lock.lock();
        let json = serde_json::to_string_pretty(names).expect("names serialize");
        if let Err(e) = write_atomic(&self.dir.join("friends.json"), json.as_bytes()) {
            tracing::warn!("could not save friends' names: {e}");
        }
    }

    /// A small note-to-self file, like which monthly recap was last sent.
    pub fn read_marker(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.dir.join(name))
            .ok()
            .map(|s| s.trim().to_string())
    }

    pub fn write_marker(&self, name: &str, value: &str) {
        if let Err(e) = write_atomic(&self.dir.join(name), value.as_bytes()) {
            tracing::warn!("could not write {name}: {e}");
        }
    }
}

pub fn write_atomic(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, contents)?;
    fs::rename(tmp, path)
}
