use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use librespot_core::SpotifyId;
use rand::seq::SliceRandom;

use crate::track::TrackInfo;

/// How long after a change its Undo button still works (it shows for less).
const UNDO_WINDOW: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    Track,
    Queue,
}

impl Repeat {
    pub fn cycle(self) -> Self {
        match self {
            Repeat::Off => Repeat::Queue,
            Repeat::Queue => Repeat::Track,
            Repeat::Track => Repeat::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::Track => "this song",
            Repeat::Queue => "the queue",
        }
    }
}

/// Played songs stay in the list (so "previous" works); everything after
/// `current` is upcoming. User-facing positions are 1-based into upcoming.
#[derive(Default)]
pub struct Queue {
    items: Vec<Arc<TrackInfo>>,
    current: Option<usize>,
    pub repeat: Repeat,
    undo: Option<Undo>,
    undo_seq: u64,
}

/// What was coming up before a clear, remove, dedupe or shuffle.
struct Undo {
    id: u64,
    /// The song playing then; undoing after it changed would be confusing.
    current: Option<SpotifyId>,
    upcoming: Vec<Arc<TrackInfo>>,
    at: Instant,
}

impl Queue {
    pub fn current(&self) -> Option<&Arc<TrackInfo>> {
        self.current.and_then(|i| self.items.get(i))
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[Arc<TrackInfo>] {
        &self.items
    }

    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub fn get(&self, index: usize) -> Option<&Arc<TrackInfo>> {
        self.items.get(index)
    }

    pub fn set_current(&mut self, index: usize) {
        if index < self.items.len() {
            self.current = Some(index);
        }
    }

    fn upcoming_start(&self) -> usize {
        self.current.map_or(0, |c| c + 1)
    }

    pub fn upcoming(&self) -> &[Arc<TrackInfo>] {
        &self.items[self.upcoming_start().min(self.items.len())..]
    }

    pub fn upcoming_duration_ms(&self) -> u64 {
        self.upcoming().iter().map(|t| t.duration_ms as u64).sum()
    }

    /// Index of the song that should play after the current one. A manual
    /// skip moves on even when repeating a single song.
    pub fn next_index(&self, manual: bool) -> Option<usize> {
        if let (Repeat::Track, Some(c), false) = (self.repeat, self.current, manual) {
            return Some(c);
        }
        let start = self.upcoming_start();
        if start < self.items.len() {
            Some(start)
        } else if self.repeat == Repeat::Queue && !self.items.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    pub fn previous_index(&self) -> Option<usize> {
        self.current.and_then(|c| c.checked_sub(1))
    }

    /// Index for 1-based upcoming position `pos`.
    pub fn index_of(&self, pos: usize) -> Option<usize> {
        let index = self.upcoming_start() + pos.checked_sub(1)?;
        (index < self.items.len()).then_some(index)
    }

    /// Adds tracks and returns the upcoming position of the first one.
    pub fn add(&mut self, tracks: Vec<Arc<TrackInfo>>, play_next: bool) -> usize {
        let start = self.upcoming_start().min(self.items.len());
        let at = if play_next { start } else { self.items.len() };
        self.items.splice(at..at, tracks);
        at - start + 1
    }

    pub fn remove(&mut self, pos: usize) -> Option<Arc<TrackInfo>> {
        let index = self.index_of(pos)?;
        Some(self.items.remove(index))
    }

    /// Removes by absolute index, keeping `current` pointing at the same song.
    pub fn remove_index(&mut self, index: usize) -> Option<Arc<TrackInfo>> {
        if index >= self.items.len() || Some(index) == self.current {
            return None;
        }
        if let Some(c) = self.current
            && index < c
        {
            self.current = Some(c - 1);
        }
        Some(self.items.remove(index))
    }

    /// Removes 1-based upcoming positions `from..=to` (in either order).
    pub fn remove_range(&mut self, from: usize, to: usize) -> usize {
        let (from, to) = (from.min(to).max(1), from.max(to));
        let Some(first) = self.index_of(from) else {
            return 0;
        };
        let last = self.index_of(to).unwrap_or(self.items.len() - 1);
        self.items.drain(first..=last).count()
    }

    /// Indexes of upcoming songs already playing or earlier in the queue.
    fn duplicate_indexes(&self) -> Vec<usize> {
        let start = self.upcoming_start().min(self.items.len());
        let mut seen: std::collections::HashSet<_> =
            self.current().map(|t| t.id).into_iter().collect();
        (start..self.items.len())
            .filter(|&i| !seen.insert(self.items[i].id))
            .collect()
    }

    pub fn duplicates(&self) -> usize {
        self.duplicate_indexes().len()
    }

    /// Keeps the first of each song coming up; returns how many went.
    pub fn dedupe(&mut self) -> usize {
        let doomed = self.duplicate_indexes();
        for &i in doomed.iter().rev() {
            self.items.remove(i);
        }
        doomed.len()
    }

    pub fn move_track(&mut self, from: usize, to: usize) -> Option<Arc<TrackInfo>> {
        let from = self.index_of(from)?;
        let to = self.index_of(to)?;
        let track = self.items.remove(from);
        self.items.insert(to, track.clone());
        Some(track)
    }

    pub fn clear_upcoming(&mut self) -> usize {
        let start = self.upcoming_start().min(self.items.len());
        let removed = self.items.len() - start;
        self.items.truncate(start);
        removed
    }

    /// Shuffles what's coming up, then spreads out songs by the same
    /// artist so they don't play back to back.
    pub fn shuffle_upcoming(&mut self) -> usize {
        let start = self.upcoming_start().min(self.items.len());
        self.items[start..].shuffle(&mut rand::rng());
        spread_artists(&mut self.items[start..]);
        self.items.len() - start
    }

    /// Remembers what's coming up, so the next change can be undone.
    pub fn save_undo(&mut self) -> u64 {
        self.undo_seq += 1;
        self.undo = Some(Undo {
            id: self.undo_seq,
            current: self.current().map(|t| t.id),
            upcoming: self.upcoming().to_vec(),
            at: Instant::now(),
        });
        self.undo_seq
    }

    /// Puts back what was coming up when undo point `id` was saved, and
    /// returns how many songs that is.
    pub fn undo(&mut self, id: u64) -> Result<usize, &'static str> {
        if self.undo.as_ref().is_none_or(|u| u.id != id) {
            return Err("That can't be undone anymore.");
        }
        let undo = self.undo.take().expect("checked above");
        if undo.at.elapsed() > UNDO_WINDOW {
            return Err("Too late to undo that one.");
        }
        if self.current().map(|t| t.id) != undo.current {
            return Err("The song changed since, so I left the queue as it is.");
        }
        let start = self.upcoming_start().min(self.items.len());
        self.items.truncate(start);
        self.items.extend(undo.upcoming);
        Ok(self.items.len() - start)
    }

    /// Marks everything as played, so the queue counts as finished.
    pub fn finish(&mut self) {
        self.current = self.items.len().checked_sub(1);
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.current = None;
    }
}

/// The artist a song counts as, for spreading them out.
fn lead_artist(track: &TrackInfo) -> String {
    track
        .artists
        .first()
        .map(|a| a.to_lowercase())
        .unwrap_or_default()
}

/// Reorders shuffled songs so the same artist doesn't play twice in a row,
/// wherever the rest of the list allows it. Each slot takes the next song
/// (in shuffled order) by someone other than the last artist, unless an
/// artist has so many songs left that they have to go now to fit at all.
fn spread_artists(songs: &mut [Arc<TrackInfo>]) {
    let mut left: Vec<(String, Arc<TrackInfo>)> =
        songs.iter().map(|t| (lead_artist(t), t.clone())).collect();
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (artist, _) in &left {
        *counts.entry(artist.clone()).or_default() += 1;
    }
    let mut last: Option<String> = None;
    for slot in songs.iter_mut() {
        let remaining = left.len();
        let urgent = counts
            .iter()
            .find(|(artist, count)| Some(*artist) != last.as_ref() && **count * 2 > remaining)
            .map(|(artist, _)| artist.clone());
        let index = match &urgent {
            Some(artist) => left.iter().position(|(a, _)| a == artist),
            None => left.iter().position(|(a, _)| Some(a) != last.as_ref()),
        }
        .unwrap_or(0);
        let (artist, song) = left.remove(index);
        if let Some(count) = counts.get_mut(&artist) {
            *count -= 1;
        }
        *slot = song;
        last = Some(artist);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(n: u8) -> Arc<TrackInfo> {
        Arc::new(TrackInfo {
            id: SpotifyId::from_raw(&[n; 16]).unwrap(),
            title: format!("t{n}"),
            artists: vec![],
            album: String::new(),
            duration_ms: 1000,
            cover_url: None,
            cover_thumb_url: None,
            explicit: false,
            year: None,
            from: None,
            requested_by: None,
        })
    }

    fn titles(q: &Queue) -> Vec<String> {
        q.upcoming().iter().map(|t| t.title.clone()).collect()
    }

    #[test]
    fn add_next_and_order() {
        let mut q = Queue::default();
        assert_eq!(q.add(vec![track(1), track(2)], false), 1);
        assert_eq!(q.next_index(false), Some(0));
        q.set_current(0);
        assert_eq!(q.add(vec![track(3)], false), 2);
        assert_eq!(q.add(vec![track(4)], true), 1);
        assert_eq!(titles(&q), ["t4", "t2", "t3"]);
    }

    #[test]
    fn repeat_modes() {
        let mut q = Queue::default();
        q.add(vec![track(1), track(2)], false);
        q.set_current(1);
        assert_eq!(q.next_index(false), None);
        q.repeat = Repeat::Queue;
        assert_eq!(q.next_index(false), Some(0));
        q.repeat = Repeat::Track;
        assert_eq!(q.next_index(false), Some(1));
        // A manual skip on the last song still ends the queue.
        assert_eq!(q.next_index(true), None);
        q.set_current(0);
        assert_eq!(q.next_index(true), Some(1));
    }

    #[test]
    fn remove_move_clear() {
        let mut q = Queue::default();
        q.add(vec![track(1), track(2), track(3), track(4)], false);
        q.set_current(0);
        assert_eq!(q.remove(2).unwrap().title, "t3");
        assert_eq!(titles(&q), ["t2", "t4"]);
        q.move_track(2, 1);
        assert_eq!(titles(&q), ["t4", "t2"]);
        assert!(q.remove(5).is_none());
        assert_eq!(q.clear_upcoming(), 2);
        assert_eq!(q.next_index(false), None);
    }

    #[test]
    fn remove_range_and_dedupe() {
        let mut q = Queue::default();
        q.add(
            [1, 2, 3, 2, 4, 1, 5, 3].into_iter().map(track).collect(),
            false,
        );
        q.set_current(0);
        assert_eq!(titles(&q), ["t2", "t3", "t2", "t4", "t1", "t5", "t3"]);
        // t1 is playing, so its later copy goes too.
        assert_eq!(q.duplicates(), 3);
        assert_eq!(q.dedupe(), 3);
        assert_eq!(titles(&q), ["t2", "t3", "t4", "t5"]);
        assert_eq!(q.duplicates(), 0);

        assert_eq!(q.remove_range(3, 2), 2);
        assert_eq!(titles(&q), ["t2", "t5"]);
        // A range running past the end stops there.
        assert_eq!(q.remove_range(2, 40), 1);
        assert_eq!(q.remove_range(5, 9), 0);
        assert_eq!(titles(&q), ["t2"]);
        assert_eq!(q.current().unwrap().title, "t1");
    }

    fn by(n: u8, artist: &str) -> Arc<TrackInfo> {
        Arc::new(TrackInfo {
            artists: vec![artist.into()],
            ..(*track(n)).clone()
        })
    }

    #[test]
    fn shuffle_spreads_out_artists() {
        for _ in 0..50 {
            let mut q = Queue::default();
            let songs = (0..12)
                .map(|n| by(n, ["A", "A", "A", "B", "B", "C"][n as usize % 6]))
                .collect();
            q.add(songs, false);
            assert_eq!(q.shuffle_upcoming(), 12);
            let artists: Vec<String> = q.upcoming().iter().map(|t| lead_artist(t)).collect();
            let repeats = artists.windows(2).filter(|w| w[0] == w[1]).count();
            assert_eq!(repeats, 0, "{artists:?}");
        }
        // When one artist is most of the queue, the rest still spreads them.
        let mut songs: Vec<_> = (0..5).map(|n| by(n, "A")).chain([by(9, "B")]).collect();
        spread_artists(&mut songs);
        assert_eq!(lead_artist(&songs[1]), "b");
    }

    #[test]
    fn undo_puts_the_queue_back() {
        let mut q = Queue::default();
        q.add([1, 2, 3, 4].into_iter().map(track).collect(), false);
        q.set_current(0);
        let id = q.save_undo();
        q.clear_upcoming();
        assert_eq!(q.undo(id), Ok(3));
        assert_eq!(titles(&q), ["t2", "t3", "t4"]);
        // Once only.
        assert!(q.undo(id).is_err());

        let id = q.save_undo();
        q.shuffle_upcoming();
        q.set_current(1);
        assert!(q.undo(id).is_err(), "the song changed, so no undo");
        let old = q.save_undo();
        let newer = q.save_undo();
        assert!(q.undo(old).is_err(), "only the latest change can be undone");
        assert!(q.undo(newer).is_ok());
    }

    #[test]
    fn add_after_finish_plays_new_track() {
        let mut q = Queue::default();
        q.add(vec![track(1)], false);
        q.set_current(0);
        q.finish();
        q.add(vec![track(2)], false);
        assert_eq!(q.next_index(false), Some(1));
    }
}
