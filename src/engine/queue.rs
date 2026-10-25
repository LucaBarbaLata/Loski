use std::sync::Arc;

use rand::seq::SliceRandom;

use crate::track::TrackInfo;

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

    pub fn shuffle_upcoming(&mut self) -> usize {
        let start = self.upcoming_start().min(self.items.len());
        self.items[start..].shuffle(&mut rand::rng());
        self.items.len() - start
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

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::SpotifyId;

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
