//! The audio path between librespot and Discord.
//!
//! Each of the two librespot players ("decks") writes decoded PCM into its own
//! [`Pipe`]. Songbird pulls from a single [`MixReader`], which reads both pipes
//! and blends them during a crossfade. Pipes are bounded, so librespot is paced
//! by Discord's real-time consumption the same way a sound card would pace it.

use std::{
    collections::VecDeque,
    f32::consts::FRAC_PI_2,
    io::{self, Read, Seek, SeekFrom},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use librespot_playback::{
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};
use parking_lot::{Condvar, Mutex};
use symphonia_core::io::MediaSource;

use crate::dsp::{Dsp, Filter};

pub const SAMPLE_RATE: u32 = 44_100;
pub const CHANNELS: usize = 2;
/// The pipe the Spotify Connect player writes into (0 and 1 are the queue's decks).
pub const CONNECT_DECK: usize = 2;

/// How much decoded audio a deck may hold ahead of what Discord has played.
const PIPE_CAPACITY_FRAMES: usize = SAMPLE_RATE as usize / 2;
pub const PIPE_CAPACITY_MS: u32 = (PIPE_CAPACITY_FRAMES * 1000 / SAMPLE_RATE as usize) as u32;
/// Short ramp used around cuts (skip, seek) so they don't click.
const FADE_FRAMES: usize = SAMPLE_RATE as usize / 200;
/// Songbird asks for 20 ms at a time; serving it in 20 ms chunks keeps the
/// amount of audio that's out of our hands (and can't be flushed) tiny.
const READ_CHUNK_FRAMES: usize = SAMPLE_RATE as usize / 50;
/// If nobody drains a pipe for this long, drop audio instead of hanging the
/// librespot player thread (which would stop it from handling commands).
const WRITE_STALL_LIMIT: Duration = Duration::from_secs(3);
/// Quieter than this counts as silence (about -66 dBFS): digital silence
/// and the very end of a fade-out, never actual music.
const SILENCE: f32 = 0.0005;
/// At most this much silence is skipped at the start of a song.
const MAX_LEAD_TRIM_FRAMES: usize = SAMPLE_RATE as usize * 3;
/// How quickly the limiter lets go after a peak (about 150 ms).
const LIMIT_RELEASE: f32 = 1.0 / (0.15 * SAMPLE_RATE as f32);

fn is_silent(frame: &[f32]) -> bool {
    frame.iter().all(|s| s.abs() < SILENCE)
}

fn frames_to_ms(frames: usize) -> u32 {
    (frames as u64 * 1000 / SAMPLE_RATE as u64) as u32
}

struct PipeState {
    samples: VecDeque<f32>,
    /// Samples at the front that form a fade-out tail left by a flush.
    tail: usize,
    /// Frames of fade-in still to apply to fresh audio after a flush.
    fade_in: usize,
    /// librespot is blocked in `push` waiting for space.
    writer_waiting: bool,
    /// The packet that was waiting during a flush is stale; drop it.
    discard_waiting: bool,
    /// Frames of silence at the start of a song that may still be skipped.
    trim_budget: usize,
    /// Frames skipped that way since the engine last asked.
    trimmed: usize,
    /// How many frames in a row just played were silent.
    quiet: usize,
}

pub struct Pipe {
    state: Mutex<PipeState>,
    space: Condvar,
}

impl Pipe {
    fn new() -> Self {
        Self {
            state: Mutex::new(PipeState {
                samples: VecDeque::with_capacity(PIPE_CAPACITY_FRAMES * CHANNELS),
                tail: 0,
                fade_in: 0,
                writer_waiting: false,
                discard_waiting: false,
                trim_budget: 0,
                trimmed: 0,
                quiet: 0,
            }),
            space: Condvar::new(),
        }
    }

    /// Called from librespot's player thread. Blocks while the pipe is full.
    fn push(&self, data: &[f32]) {
        let capacity = PIPE_CAPACITY_FRAMES * CHANNELS;
        let started = Instant::now();
        let mut st = self.state.lock();
        let data = if st.trim_budget > 0 {
            let frames = data.len() / CHANNELS;
            let silent = data
                .chunks_exact(CHANNELS)
                .take(st.trim_budget)
                .take_while(|frame| is_silent(frame))
                .count();
            st.trimmed += silent;
            if silent == frames && silent < st.trim_budget {
                st.trim_budget -= silent;
                return;
            }
            // The music has started (or the budget ran out).
            st.trim_budget = 0;
            &data[silent * CHANNELS..]
        } else {
            data
        };
        while !st.samples.is_empty() && st.samples.len() + data.len() > capacity {
            if started.elapsed() > WRITE_STALL_LIMIT {
                tracing::debug!("audio output not draining, dropping {} samples", data.len());
                return;
            }
            st.writer_waiting = true;
            self.space.wait_for(&mut st, Duration::from_millis(50));
            st.writer_waiting = false;
            if st.discard_waiting {
                // This packet was decoded before a skip or seek.
                st.discard_waiting = false;
                return;
            }
        }
        st.samples.extend(data.iter().copied());
    }

    /// Throws away buffered audio, keeping a few milliseconds faded to silence
    /// so the cut is clean, and fades in whatever arrives next.
    pub fn flush(&self) {
        let mut st = self.state.lock();
        let available = st.samples.len() - st.samples.len() % CHANNELS;
        let keep = (FADE_FRAMES * CHANNELS).min(available);
        st.samples.truncate(keep);
        let frames = keep / CHANNELS;
        for f in 0..frames {
            let gain = 1.0 - (f + 1) as f32 / frames as f32;
            for c in 0..CHANNELS {
                st.samples[f * CHANNELS + c] *= gain;
            }
        }
        st.tail = keep;
        st.fade_in = FADE_FRAMES;
        st.discard_waiting = st.writer_waiting;
        st.quiet = 0;
        drop(st);
        self.space.notify_all();
    }

    pub fn buffered_ms(&self) -> u32 {
        frames_to_ms(self.state.lock().samples.len() / CHANNELS)
    }

    /// The next song starts here: skip the silence it opens with (up to
    /// three seconds), so it starts on the music.
    pub fn trim_leading_silence(&self) {
        let mut st = self.state.lock();
        st.trim_budget = MAX_LEAD_TRIM_FRAMES;
        st.trimmed = 0;
        st.quiet = 0;
    }

    /// Silence skipped at the start of the song since the last call.
    pub fn take_trimmed_ms(&self) -> u32 {
        frames_to_ms(std::mem::take(&mut self.state.lock().trimmed))
    }

    /// How long what just played has been silent.
    pub fn quiet_ms(&self) -> u32 {
        frames_to_ms(self.state.lock().quiet)
    }

    #[cfg(test)]
    pub fn pretend_quiet_for(&self, ms: u32) {
        self.state.lock().quiet = (ms as u64 * SAMPLE_RATE as u64 / 1000) as usize;
    }

    fn is_empty(&self) -> bool {
        self.state.lock().samples.is_empty()
    }

    /// Moves up to `out.len()` samples into `out` and returns how many.
    fn pop(&self, out: &mut [f32]) -> usize {
        let mut st = self.state.lock();
        let n = out.len().min(st.samples.len());
        let n = n - n % CHANNELS;
        for (o, s) in out.iter_mut().zip(st.samples.drain(..n)) {
            *o = s;
        }
        for frame in out[..n].chunks_exact_mut(CHANNELS) {
            if st.tail > 0 {
                st.tail -= CHANNELS;
            } else if st.fade_in > 0 {
                let gain = 1.0 - st.fade_in as f32 / FADE_FRAMES as f32;
                frame.iter_mut().for_each(|s| *s *= gain);
                st.fade_in -= 1;
            }
            st.quiet = if is_silent(frame) { st.quiet + 1 } else { 0 };
        }
        drop(st);
        self.space.notify_all();
        n
    }
}

#[derive(Debug, Clone, Copy)]
enum Mix {
    Solo(usize),
    Fade {
        from: usize,
        to: usize,
        total: usize,
        done: usize,
        /// The fade waits until the incoming deck has actually produced audio,
        /// so a slow track load doesn't fade the old song into silence.
        started: bool,
    },
}

/// What the output stage does to the mixed audio. The reader picks these up
/// every 20 ms, so changes apply almost immediately.
#[derive(Debug, Clone, Copy)]
struct Controls {
    filter: Filter,
    /// Volume as a linear gain; the reader glides towards it.
    volume: f32,
    /// 1.0 open, 0.0 silent. Used for soft pause and the sleep fade.
    gate_target: f32,
    /// How many frames a full 0 → 1 gate move takes.
    gate_ramp_frames: f32,
}

pub struct Mixer {
    pub pipes: [Pipe; 3],
    mix: Mutex<Mix>,
    fade_finished: AtomicBool,
    controls: Mutex<Controls>,
}

impl Mixer {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pipes: [Pipe::new(), Pipe::new(), Pipe::new()],
            mix: Mutex::new(Mix::Solo(0)),
            fade_finished: AtomicBool::new(false),
            controls: Mutex::new(Controls {
                filter: Filter::Off,
                volume: 1.0,
                gate_target: 1.0,
                gate_ramp_frames: 1.0,
            }),
        })
    }

    pub fn set_filter(&self, filter: Filter) {
        self.controls.lock().filter = filter;
    }

    pub fn filter(&self) -> Filter {
        self.controls.lock().filter
    }

    pub fn set_volume(&self, gain: f32) {
        self.controls.lock().volume = gain.clamp(0.0, 1.0);
    }

    /// Fades the output to `target` (0.0 silent, 1.0 full) over `duration`.
    pub fn fade_to(&self, target: f32, duration: Duration) {
        let mut controls = self.controls.lock();
        controls.gate_target = target.clamp(0.0, 1.0);
        controls.gate_ramp_frames = (duration.as_secs_f32() * SAMPLE_RATE as f32).max(1.0);
    }

    /// Only `deck` is audible. Cancels any crossfade in progress.
    pub fn solo(&self, deck: usize) {
        *self.mix.lock() = Mix::Solo(deck);
        self.fade_finished.store(false, Ordering::Release);
    }

    pub fn start_fade(&self, from: usize, to: usize, duration: Duration) {
        let total = ((duration.as_secs_f64() * SAMPLE_RATE as f64) as usize).max(1);
        *self.mix.lock() = Mix::Fade {
            from,
            to,
            total,
            done: 0,
            started: false,
        };
        self.fade_finished.store(false, Ordering::Release);
    }

    /// True once after a crossfade completes.
    pub fn take_fade_finished(&self) -> bool {
        self.fade_finished.swap(false, Ordering::AcqRel)
    }

    pub fn is_silent(&self) -> bool {
        self.pipes.iter().all(Pipe::is_empty)
    }

    fn render(&self, out: &mut [f32], scratch_a: &mut Vec<f32>, scratch_b: &mut Vec<f32>) {
        let mut mix = self.mix.lock();
        match *mix {
            Mix::Solo(deck) => {
                let n = self.pipes[deck].pop(out);
                out[n..].fill(0.0);
            }
            Mix::Fade {
                from,
                to,
                total,
                done,
                started,
            } => {
                let started = started || !self.pipes[to].is_empty();
                if !started {
                    let n = self.pipes[from].pop(out);
                    out[n..].fill(0.0);
                    return;
                }

                scratch_a.resize(out.len(), 0.0);
                scratch_b.resize(out.len(), 0.0);
                let na = self.pipes[from].pop(scratch_a);
                scratch_a[na..].fill(0.0);
                let nb = self.pipes[to].pop(scratch_b);
                scratch_b[nb..].fill(0.0);

                let frames = out.len() / CHANNELS;
                for f in 0..frames {
                    // Equal-power curve keeps loudness steady through the blend.
                    let t = ((done + f) as f32 / total as f32).min(1.0);
                    let (gain_out, gain_in) = ((t * FRAC_PI_2).cos(), (t * FRAC_PI_2).sin());
                    for c in 0..CHANNELS {
                        let i = f * CHANNELS + c;
                        out[i] = scratch_a[i] * gain_out + scratch_b[i] * gain_in;
                    }
                }

                let done = done + frames;
                *mix = if done >= total {
                    self.fade_finished.store(true, Ordering::Release);
                    Mix::Solo(to)
                } else {
                    Mix::Fade {
                        from,
                        to,
                        total,
                        done,
                        started: true,
                    }
                };
            }
        }
    }
}

/// The one continuous stream songbird plays: raw little-endian f32 stereo.
/// It never blocks and never ends; when there's nothing to play it's silence.
pub struct MixReader {
    mixer: Arc<Mixer>,
    frame: Vec<f32>,
    scratch_a: Vec<f32>,
    scratch_b: Vec<f32>,
    bytes: Vec<u8>,
    pos: usize,
    dsp: Dsp,
    /// Smoothed copies of the controls, moved a little every frame.
    volume: f32,
    gate: f32,
    /// The limiter's gain: 1.0 unless a peak needed taming.
    limit: f32,
}

impl MixReader {
    pub fn new(mixer: Arc<Mixer>) -> Self {
        let controls = *mixer.controls.lock();
        Self {
            mixer,
            frame: vec![0.0; READ_CHUNK_FRAMES * CHANNELS],
            scratch_a: Vec::new(),
            scratch_b: Vec::new(),
            bytes: Vec::with_capacity(READ_CHUNK_FRAMES * CHANNELS * 4),
            pos: 0,
            dsp: Dsp::new(controls.filter),
            volume: controls.volume,
            gate: controls.gate_target,
            limit: 1.0,
        }
    }

    /// Mixes the decks, runs the filter, applies volume and the gate, and
    /// keeps peaks under full scale.
    fn render_chunk(&mut self) {
        let controls = *self.mixer.controls.lock();
        if self.dsp.filter() != controls.filter {
            self.dsp = Dsp::new(controls.filter);
        }
        let Self {
            mixer,
            frame,
            scratch_a,
            scratch_b,
            dsp,
            ..
        } = self;
        dsp.render(frame, |buf| {
            buf.resize(READ_CHUNK_FRAMES * CHANNELS, 0.0);
            mixer.render(buf, scratch_a, scratch_b);
        });

        let gate_step = 1.0 / controls.gate_ramp_frames;
        for f in self.frame.chunks_exact_mut(CHANNELS) {
            // About 10 ms to settle: no zipper noise when the volume changes.
            self.volume += (controls.volume - self.volume) * 0.0025;
            self.gate = if self.gate < controls.gate_target {
                (self.gate + gate_step).min(controls.gate_target)
            } else {
                (self.gate - gate_step).max(controls.gate_target)
            };
            let gain = self.volume * self.gate * self.gate;
            f.iter_mut().for_each(|s| *s *= gain);

            // Discord's encoder would clip anything past full scale harshly.
            // A boosted filter or a crossfade can get there, so the limiter
            // dips just enough on a peak and eases back afterwards.
            let peak = f.iter().fold(0f32, |m, s| m.max(s.abs()));
            let target = if peak > 1.0 { 1.0 / peak } else { 1.0 };
            self.limit = if target < self.limit {
                target
            } else {
                self.limit + (target - self.limit) * LIMIT_RELEASE
            };
            if self.limit < 1.0 {
                f.iter_mut().for_each(|s| *s *= self.limit);
            }
        }
    }
}

impl Read for MixReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.pos >= self.bytes.len() {
            self.render_chunk();
            self.bytes.clear();
            for s in &self.frame {
                self.bytes.extend_from_slice(&s.to_le_bytes());
            }
            self.pos = 0;
        }
        let n = buf.len().min(self.bytes.len() - self.pos);
        buf[..n].copy_from_slice(&self.bytes[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Seek for MixReader {
    fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "live stream"))
    }
}

impl MediaSource for MixReader {
    fn is_seekable(&self) -> bool {
        false
    }

    fn byte_len(&self) -> Option<u64> {
        None
    }
}

/// librespot audio backend that writes into one deck's pipe.
pub struct PipeSink {
    mixer: Arc<Mixer>,
    deck: usize,
}

impl PipeSink {
    pub fn new(mixer: Arc<Mixer>, deck: usize) -> Self {
        Self { mixer, deck }
    }
}

impl Sink for PipeSink {
    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet
            .samples()
            .map_err(|e| SinkError::OnWrite(e.to_string()))?;
        let samples = converter.f64_to_f32(samples);
        self.mixer.pipes[self.deck].push(&samples);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_samples(reader: &mut MixReader, n_samples: usize) -> Vec<f32> {
        let mut bytes = vec![0u8; n_samples * 4];
        let mut filled = 0;
        while filled < bytes.len() {
            filled += reader.read(&mut bytes[filled..]).unwrap();
        }
        bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect()
    }

    /// Runs the stream through songbird's real probe and decoder, exactly as
    /// it happens in a voice call.
    #[test]
    fn songbird_decodes_the_stream() {
        use songbird::input::{
            Input, LiveInput, RawAdapter,
            codecs::{get_codec_registry, get_probe},
        };
        use symphonia_core::audio::SampleBuffer;

        let mixer = Mixer::new();
        mixer.pipes[0].push(&vec![0.25; READ_CHUNK_FRAMES * CHANNELS * 4]);
        let input: Input =
            RawAdapter::new(MixReader::new(mixer), SAMPLE_RATE, CHANNELS as u32).into();
        let Input::Live(live, _) = input else {
            panic!("expected a live input");
        };
        let LiveInput::Parsed(mut parsed) = live
            .promote(get_codec_registry(), get_probe())
            .expect("songbird must be able to open and decode the stream")
        else {
            panic!("expected a parsed input");
        };

        let packet = parsed.format.next_packet().unwrap();
        let decoded = parsed.decoder.decode(&packet).unwrap();
        assert_eq!(decoded.spec().rate, SAMPLE_RATE);
        assert_eq!(decoded.spec().channels.count(), CHANNELS);
        let mut samples = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        samples.copy_interleaved_ref(decoded);
        assert!(!samples.samples().is_empty());
        assert!(samples.samples().iter().all(|&s| s == 0.25));
    }

    #[test]
    fn soft_pause_fades_to_silence_and_back() {
        let mixer = Mixer::new();
        mixer.pipes[0].push(&vec![0.5; 44_100 * CHANNELS]);
        let mut reader = MixReader::new(mixer.clone());
        mixer.fade_to(0.0, Duration::from_millis(100));
        let out = read_samples(&mut reader, 8_820 * CHANNELS);
        assert!(out[0] > 0.4, "starts audible");
        assert!(
            out.windows(2).all(|w| w[1] <= w[0] + 1e-6),
            "only gets quieter"
        );
        assert_eq!(out[out.len() - 1], 0.0, "silent after the fade");

        mixer.fade_to(1.0, Duration::from_millis(100));
        let out = read_samples(&mut reader, 8_820 * CHANNELS);
        assert!(
            (out[out.len() - 1] - 0.5).abs() < 1e-3,
            "back to full level"
        );
    }

    #[test]
    fn volume_changes_glide_instead_of_jumping() {
        let mixer = Mixer::new();
        mixer.pipes[0].push(&vec![1.0; 44_100 * CHANNELS]);
        let mut reader = MixReader::new(mixer.clone());
        mixer.set_volume(0.25);
        let out = read_samples(&mut reader, 4_410 * CHANNELS);
        let biggest_step = out
            .windows(2)
            .map(|w| (w[0] - w[1]).abs())
            .fold(0f32, f32::max);
        assert!(biggest_step < 0.01, "volume jumped by {biggest_step}");
        assert!(
            (out[out.len() - 1] - 0.25).abs() < 0.01,
            "settles at the new volume"
        );
    }

    #[test]
    fn filters_apply_to_the_output() {
        let mixer = Mixer::new();
        let tone: Vec<f32> = (0..44_100)
            .flat_map(|i| {
                let s = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / SAMPLE_RATE as f32).sin()
                    * 0.5;
                [s, s]
            })
            .collect();
        mixer.pipes[0].push(&tone);
        mixer.set_filter(Filter::EightD);
        let mut reader = MixReader::new(mixer);
        let out = read_samples(&mut reader, 22_050 * CHANNELS);
        let (left, right): (Vec<f32>, Vec<f32>) = out.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
        assert_ne!(left, right, "8D makes the ears differ");
    }

    #[test]
    fn plays_audio_then_silence_without_blocking() {
        let mixer = Mixer::new();
        mixer.pipes[0].push(&[0.5; 100]);
        let mut reader = MixReader::new(mixer.clone());
        let out = read_samples(&mut reader, 200);
        assert!(out[..100].iter().all(|&s| s == 0.5));
        assert!(out[100..].iter().all(|&s| s == 0.0));
        assert!(mixer.is_silent());
    }

    #[test]
    fn flush_fades_out_then_fades_in() {
        let mixer = Mixer::new();
        mixer.pipes[0].push(&[1.0; 4000]);
        mixer.pipes[0].flush();
        assert_eq!(
            mixer.pipes[0].buffered_ms(),
            (FADE_FRAMES * 1000 / SAMPLE_RATE as usize) as u32
        );
        mixer.pipes[0].push(&[1.0; FADE_FRAMES * CHANNELS * 2]);

        let mut reader = MixReader::new(mixer);
        let out = read_samples(&mut reader, FADE_FRAMES * CHANNELS * 3);
        let tail = &out[..FADE_FRAMES * CHANNELS];
        assert!(
            tail.windows(2).all(|w| w[1] <= w[0]),
            "tail should fade out"
        );
        assert_eq!(tail[tail.len() - 1], 0.0);
        let fade_in = &out[FADE_FRAMES * CHANNELS..FADE_FRAMES * CHANNELS * 2];
        assert!(
            fade_in[0] < 0.01 && fade_in.windows(2).all(|w| w[1] >= w[0]),
            "should fade in"
        );
        assert_eq!(out[out.len() - 1], 1.0);
    }

    #[test]
    fn crossfade_waits_for_incoming_audio_then_hands_over() {
        let mixer = Mixer::new();
        let frames = 2_000;
        mixer.pipes[0].push(&vec![1.0; frames * CHANNELS * 2]);
        mixer.start_fade(
            0,
            1,
            Duration::from_secs_f64(frames as f64 / SAMPLE_RATE as f64),
        );

        let mut reader = MixReader::new(mixer.clone());
        // Deck 1 has nothing yet, so deck 0 plays untouched.
        let before = read_samples(&mut reader, READ_CHUNK_FRAMES * CHANNELS);
        assert!(before.iter().all(|&s| s == 1.0));
        assert!(!mixer.take_fade_finished());

        mixer.pipes[1].push(&vec![1.0; frames * CHANNELS * 2]);
        let during = read_samples(&mut reader, frames * CHANNELS);
        // An equal-power blend of two full-scale signals would reach sqrt(2);
        // the limiter holds it at full scale.
        assert!(during.iter().all(|&s| (0.7..=1.0).contains(&s)));
        assert!(mixer.take_fade_finished());
        assert!(matches!(*mixer.mix.lock(), Mix::Solo(1)));
    }

    #[test]
    fn songs_start_on_the_music_not_the_silence_before_it() {
        let mixer = Mixer::new();
        let pipe = &mixer.pipes[0];
        pipe.trim_leading_silence();
        pipe.push(&vec![0.0; 1_000 * CHANNELS]);
        pipe.push(&[0.0001; 10 * CHANNELS]);
        assert_eq!(pipe.buffered_ms(), 0);
        let mut first_music = vec![0.0; 200 * CHANNELS];
        first_music.extend(vec![0.5; 2_000 * CHANNELS]);
        // A packet that starts quiet keeps everything from the first sound.
        first_music[0] = 0.5;
        pipe.push(&first_music);
        assert_eq!(pipe.take_trimmed_ms(), frames_to_ms(1_010));
        assert_eq!(pipe.take_trimmed_ms(), 0);
        // Silence later in the song is left alone.
        pipe.push(&vec![0.0; 1_000 * CHANNELS]);
        assert_eq!(pipe.buffered_ms(), frames_to_ms(2_200 + 1_000));
    }

    #[test]
    fn a_silent_song_is_trimmed_for_three_seconds_at_most() {
        let mixer = Mixer::new();
        let pipe = &mixer.pipes[0];
        pipe.trim_leading_silence();
        for _ in 0..7 {
            pipe.push(&vec![0.0; SAMPLE_RATE as usize / 2 * CHANNELS]);
        }
        assert_eq!(pipe.take_trimmed_ms(), 3_000);
        assert_eq!(pipe.buffered_ms(), 500);
    }

    #[test]
    fn quiet_time_counts_what_just_played() {
        let mixer = Mixer::new();
        mixer.pipes[0].push(&vec![0.5; 100 * CHANNELS]);
        mixer.pipes[0].push(&vec![0.0; 20_000 * CHANNELS]);
        let mut reader = MixReader::new(mixer.clone());
        // The reader works in 20 ms chunks, so the first one ends in silence.
        read_samples(&mut reader, 100 * CHANNELS);
        assert!(mixer.pipes[0].quiet_ms() < 20);
        read_samples(&mut reader, 20_000 * CHANNELS);
        assert!(mixer.pipes[0].quiet_ms() >= 450);
        mixer.pipes[0].flush();
        assert_eq!(mixer.pipes[0].quiet_ms(), 0);
    }

    #[test]
    fn the_limiter_keeps_peaks_under_full_scale() {
        let mixer = Mixer::new();
        let hot: Vec<f32> = (0..20_000)
            .flat_map(|i| {
                let s = (i as f32 * 0.05).sin() * 1.6;
                [s, s]
            })
            .collect();
        mixer.pipes[0].push(&hot);
        let mut reader = MixReader::new(mixer);
        let out = read_samples(&mut reader, 20_000 * CHANNELS);
        assert!(out.iter().all(|s| s.abs() <= 1.0 + 1e-6));
        assert!(out.iter().any(|s| s.abs() > 0.9), "still loud");
    }

    #[test]
    fn full_pipe_drops_stale_packet_after_flush() {
        let mixer = Mixer::new();
        let capacity = PIPE_CAPACITY_FRAMES * CHANNELS;
        mixer.pipes[0].push(&vec![0.25; capacity]);
        let writer = {
            let mixer = mixer.clone();
            std::thread::spawn(move || mixer.pipes[0].push(&[0.9; 64]))
        };
        while !mixer.pipes[0].state.lock().writer_waiting {
            std::thread::yield_now();
        }
        mixer.pipes[0].flush();
        writer.join().unwrap();
        let st = mixer.pipes[0].state.lock();
        assert!(
            st.samples.iter().all(|&s| s != 0.9),
            "stale packet should be dropped"
        );
    }
}
