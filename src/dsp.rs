//! Sound filters applied to the mixed stream before it goes to Discord:
//! EQ (biquads), speed changes (nightcore / slowed), 8D auto-panning and a
//! small Freeverb-style reverb. Everything works on interleaved stereo f32
//! at 44.1 kHz.

use std::{collections::VecDeque, f32::consts::PI};

use crate::audio::{CHANNELS, SAMPLE_RATE};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    Off,
    BassBoost,
    TrebleBoost,
    VocalBoost,
    Nightcore,
    Slowed,
    EightD,
    /// "My sound": your own EQ, in decibels per band (see `EQ_BANDS`).
    Mine([i8; 5]),
}

/// The bands of "My sound": name and centre frequency. The outer two are
/// shelves (everything below the bass, everything above the treble).
pub const EQ_BANDS: [(&str, f32); 5] = [
    ("Bass", 90.0),
    ("Low-mid", 300.0),
    ("Mid", 1_000.0),
    ("High-mid", 3_200.0),
    ("Treble", 9_000.0),
];
/// How far each band of "My sound" goes either way, in decibels.
pub const EQ_RANGE: i8 = 8;

impl Filter {
    pub const ALL: [Filter; 7] = [
        Filter::Off,
        Filter::BassBoost,
        Filter::TrebleBoost,
        Filter::VocalBoost,
        Filter::Nightcore,
        Filter::Slowed,
        Filter::EightD,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Filter::Off => "No filter",
            Filter::BassBoost => "Bass boost",
            Filter::TrebleBoost => "Treble boost",
            Filter::VocalBoost => "Vocal boost",
            Filter::Nightcore => "Nightcore",
            Filter::Slowed => "Slowed + reverb",
            Filter::EightD => "8D",
            Filter::Mine(_) => "My sound",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Filter::Off => "The song as Spotify sends it",
            Filter::BassBoost => "More low end",
            Filter::TrebleBoost => "Brighter highs",
            Filter::VocalBoost => "Voices forward, mud cut",
            Filter::Nightcore => "25% faster and higher",
            Filter::Slowed => "15% slower and lower, with a room",
            Filter::EightD => "Sound circles around your head (use headphones)",
            Filter::Mine(_) => "Your own EQ, set in /settings",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Filter::Off => "off",
            Filter::BassBoost => "bass",
            Filter::TrebleBoost => "treble",
            Filter::VocalBoost => "vocal",
            Filter::Nightcore => "nightcore",
            Filter::Slowed => "slowed",
            Filter::EightD => "8d",
            Filter::Mine(_) => "mine",
        }
    }

    /// The presets; "My sound" needs your EQ, so it isn't one of them.
    pub fn from_key(key: &str) -> Option<Filter> {
        Filter::ALL.into_iter().find(|f| f.key() == key)
    }

    /// Playback speed; also shifts pitch, like a record played faster.
    pub fn speed(self) -> f64 {
        match self {
            Filter::Nightcore => 1.25,
            Filter::Slowed => 0.85,
            _ => 1.0,
        }
    }
}

/// RBJ "audio EQ cookbook" biquad, transposed direct form II, one per channel.
#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: [f32; CHANNELS],
    z2: [f32; CHANNELS],
}

enum Shape {
    LowShelf,
    HighShelf,
    Peak,
    HighPass,
}

impl Biquad {
    fn new(shape: Shape, freq: f32, gain_db: f32, q: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / SAMPLE_RATE as f32;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let (b0, b1, b2, a0, a1, a2) = match shape {
            Shape::Peak => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            Shape::LowShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + k),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - k),
                    (a + 1.0) + (a - 1.0) * cos + k,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - k,
                )
            }
            Shape::HighShelf => {
                let k = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + k),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - k),
                    (a + 1.0) - (a - 1.0) * cos + k,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - k,
                )
            }
            Shape::HighPass => (
                (1.0 + cos) / 2.0,
                -(1.0 + cos),
                (1.0 + cos) / 2.0,
                1.0 + alpha,
                -2.0 * cos,
                1.0 - alpha,
            ),
        };
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            ..Default::default()
        }
    }

    fn process(&mut self, frame: &mut [f32]) {
        for (c, x) in frame.iter_mut().enumerate() {
            let y = self.b0 * *x + self.z1[c];
            self.z1[c] = self.b1 * *x - self.a1 * y + self.z2[c];
            self.z2[c] = self.b2 * *x - self.a2 * y;
            *x = y;
        }
    }
}

/// Feedback comb with a one-pole low-pass in the loop (Freeverb's comb).
struct Comb {
    buffer: Vec<f32>,
    index: usize,
    store: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len],
            index: 0,
            store: 0.0,
        }
    }

    fn process(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let out = self.buffer[self.index];
        self.store = out * (1.0 - damp) + self.store * damp;
        self.buffer[self.index] = input + self.store * feedback;
        self.index = (self.index + 1) % self.buffer.len();
        out
    }
}

struct Allpass {
    buffer: Vec<f32>,
    index: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self {
            buffer: vec![0.0; len],
            index: 0,
        }
    }

    fn process(&mut self, input: f32) -> f32 {
        let delayed = self.buffer[self.index];
        let out = delayed - input;
        self.buffer[self.index] = input + delayed * 0.5;
        self.index = (self.index + 1) % self.buffer.len();
        out
    }
}

/// A small room: Freeverb's tunings, with the right channel's delays
/// slightly longer for width.
struct Reverb {
    combs: [Vec<Comb>; CHANNELS],
    allpasses: [Vec<Allpass>; CHANNELS],
}

impl Reverb {
    fn new() -> Self {
        const COMBS: [usize; 4] = [1116, 1188, 1277, 1356];
        const ALLPASSES: [usize; 2] = [556, 441];
        const SPREAD: usize = 23;
        let channel = |offset: usize| {
            (
                COMBS
                    .iter()
                    .map(|&n| Comb::new(n + offset))
                    .collect::<Vec<_>>(),
                ALLPASSES
                    .iter()
                    .map(|&n| Allpass::new(n + offset))
                    .collect::<Vec<_>>(),
            )
        };
        let (left_combs, left_allpasses) = channel(0);
        let (right_combs, right_allpasses) = channel(SPREAD);
        Self {
            combs: [left_combs, right_combs],
            allpasses: [left_allpasses, right_allpasses],
        }
    }

    fn process(&mut self, frame: &mut [f32], wet: f32) {
        let input = (frame[0] + frame[1]) * 0.5 * 0.15;
        for (c, sample) in frame.iter_mut().enumerate() {
            let mut out: f32 = self.combs[c]
                .iter_mut()
                .map(|comb| comb.process(input, 0.82, 0.25))
                .sum();
            for allpass in &mut self.allpasses[c] {
                out = allpass.process(out);
            }
            *sample = *sample * (1.0 - wet * 0.5) + out * wet;
        }
    }
}

/// Changes speed (and with it pitch) by reading the input at `rate` input
/// frames per output frame, with Catmull-Rom interpolation.
struct Speed {
    input: VecDeque<[f32; CHANNELS]>,
    position: f64,
}

impl Speed {
    fn new() -> Self {
        Self {
            input: VecDeque::new(),
            position: 1.0,
        }
    }

    /// Fills `out` (interleaved stereo); `pull` supplies more input frames.
    fn process(&mut self, out: &mut [f32], rate: f64, mut pull: impl FnMut(&mut Vec<f32>)) {
        let frames = out.len() / CHANNELS;
        let needed = (self.position + frames as f64 * rate).ceil() as usize + 3;
        let mut chunk = Vec::new();
        while self.input.len() < needed {
            chunk.clear();
            pull(&mut chunk);
            if chunk.is_empty() {
                break;
            }
            self.input
                .extend(chunk.chunks_exact(CHANNELS).map(|f| [f[0], f[1]]));
        }
        for frame in out.chunks_exact_mut(CHANNELS) {
            let i = self.position.floor() as usize;
            let t = (self.position - i as f64) as f32;
            for (c, sample) in frame.iter_mut().enumerate() {
                let at = |k: usize| self.input.get(k).map_or(0.0, |f| f[c]);
                let (p0, p1, p2, p3) = (at(i.saturating_sub(1)), at(i), at(i + 1), at(i + 2));
                *sample = p1
                    + 0.5
                        * t
                        * (p2 - p0
                            + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3
                                + t * (3.0 * (p1 - p2) + p3 - p0)));
            }
            self.position += rate;
        }
        // Keep one frame of history for the next call's interpolation.
        let consumed = (self.position.floor() as usize)
            .saturating_sub(1)
            .min(self.input.len());
        self.input.drain(..consumed);
        self.position -= consumed as f64;
    }
}

/// Everything a filter needs, rebuilt when the filter changes.
pub struct Dsp {
    filter: Filter,
    eq: Vec<Biquad>,
    pre_gain: f32,
    speed: Speed,
    reverb: Option<(Reverb, f32)>,
    pan_phase: f32,
}

impl Dsp {
    pub fn new(filter: Filter) -> Self {
        let mut eq = Vec::new();
        let mut pre_gain = 1.0;
        let mut reverb = None;
        match filter {
            Filter::Off | Filter::EightD => {}
            Filter::BassBoost => {
                eq.push(Biquad::new(Shape::LowShelf, 110.0, 8.0, 0.7));
                pre_gain = 0.7;
            }
            Filter::TrebleBoost => {
                eq.push(Biquad::new(Shape::HighShelf, 5_000.0, 6.0, 0.7));
                pre_gain = 0.8;
            }
            Filter::VocalBoost => {
                eq.push(Biquad::new(Shape::HighPass, 110.0, 0.0, 0.7));
                eq.push(Biquad::new(Shape::Peak, 2_800.0, 5.0, 0.9));
                pre_gain = 0.8;
            }
            Filter::Nightcore => {
                eq.push(Biquad::new(Shape::HighShelf, 6_000.0, 2.0, 0.7));
                pre_gain = 0.9;
            }
            Filter::Slowed => reverb = Some((Reverb::new(), 0.32)),
            Filter::Mine(gains) => {
                for (i, (&(_, freq), &gain)) in EQ_BANDS.iter().zip(&gains).enumerate() {
                    let gain = gain.clamp(-EQ_RANGE, EQ_RANGE) as f32;
                    let shape = match i {
                        0 => Shape::LowShelf,
                        4 => Shape::HighShelf,
                        _ => Shape::Peak,
                    };
                    eq.push(Biquad::new(
                        shape,
                        freq,
                        gain,
                        if i % 4 == 0 { 0.7 } else { 1.0 },
                    ));
                }
                // Room for the loudest boost, so it doesn't lean on the limiter.
                let boost = gains.iter().copied().max().unwrap_or(0).max(0) as f32;
                pre_gain = 10f32.powf(-boost / 20.0);
            }
        }
        if filter == Filter::EightD {
            reverb = Some((Reverb::new(), 0.12));
        }
        Self {
            filter,
            eq,
            pre_gain,
            speed: Speed::new(),
            reverb,
            pan_phase: 0.0,
        }
    }

    pub fn filter(&self) -> Filter {
        self.filter
    }

    /// Fills `out`; `pull` renders more of the mixed stream on demand.
    pub fn render(&mut self, out: &mut [f32], pull: impl FnMut(&mut Vec<f32>)) {
        let rate = self.filter.speed();
        if rate == 1.0 {
            let mut pull = pull;
            let mut chunk = Vec::with_capacity(out.len());
            while chunk.len() < out.len() {
                let before = chunk.len();
                let mut more = Vec::new();
                pull(&mut more);
                chunk.extend(more);
                if chunk.len() == before {
                    break;
                }
            }
            chunk.resize(out.len(), 0.0);
            out.copy_from_slice(&chunk[..out.len()]);
        } else {
            self.speed.process(out, rate, pull);
        }

        if self.filter == Filter::Off {
            return;
        }
        let pan_step = 2.0 * PI * 0.12 / SAMPLE_RATE as f32;
        for frame in out.chunks_exact_mut(CHANNELS) {
            if self.pre_gain != 1.0 {
                frame.iter_mut().for_each(|s| *s *= self.pre_gain);
            }
            for biquad in &mut self.eq {
                biquad.process(frame);
            }
            if self.filter == Filter::EightD {
                // Equal-power pan swinging slowly between the ears.
                let pan = 0.5 + 0.42 * self.pan_phase.sin();
                let (l, r) = ((1.0 - pan).sqrt(), pan.sqrt());
                let mono = (frame[0] + frame[1]) * 0.5;
                let side = (frame[0] - frame[1]) * 0.5;
                frame[0] = (mono * l + side * 0.5) * std::f32::consts::SQRT_2;
                frame[1] = (mono * r - side * 0.5) * std::f32::consts::SQRT_2;
                self.pan_phase = (self.pan_phase + pan_step) % (2.0 * PI);
            }
            if let Some((reverb, wet)) = &mut self.reverb {
                reverb.process(frame, *wet);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s = (2.0 * PI * freq * i as f32 / SAMPLE_RATE as f32).sin() * 0.5;
                [s, s]
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Runs a whole buffer through a filter, feeding it as input.
    fn run(filter: Filter, input: &[f32], out_frames: usize) -> Vec<f32> {
        let mut dsp = Dsp::new(filter);
        let mut source = input.chunks(882 * CHANNELS).map(<[f32]>::to_vec);
        let mut out = vec![0.0; out_frames * CHANNELS];
        for chunk in out.chunks_mut(882 * CHANNELS) {
            dsp.render(chunk, |buf| {
                if let Some(next) = source.next() {
                    buf.extend(next);
                }
            });
        }
        out
    }

    #[test]
    fn off_passes_audio_through_untouched() {
        let input = sine(440.0, 44_100);
        assert_eq!(run(Filter::Off, &input, 44_100), input);
    }

    #[test]
    fn bass_boost_raises_lows_more_than_highs() {
        let low = rms(&run(Filter::BassBoost, &sine(60.0, 44_100), 44_100)[20_000..]);
        let high = rms(&run(Filter::BassBoost, &sine(4_000.0, 44_100), 44_100)[20_000..]);
        assert!(low > high * 1.8, "low {low} high {high}");
    }

    #[test]
    fn nightcore_plays_faster_and_slowed_slower() {
        // A tone at 440 Hz should come out near 550 Hz / 374 Hz: count zero crossings.
        let crossings = |s: &[f32]| {
            s.chunks_exact(2)
                .map(|f| f[0])
                .collect::<Vec<_>>()
                .windows(2)
                .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
                .count()
        };
        let input = sine(440.0, 3 * 44_100);
        let fast = crossings(&run(Filter::Nightcore, &input, 44_100)[4_000..]);
        let slow = crossings(&run(Filter::Slowed, &input, 44_100)[4_000..]);
        assert!((520..=580).contains(&fast), "nightcore pitch {fast}");
        assert!((350..=400).contains(&slow), "slowed pitch {slow}");
    }

    #[test]
    fn effects_stay_stable_and_in_range() {
        for filter in Filter::ALL {
            let out = run(filter, &sine(220.0, 3 * 44_100), 2 * 44_100);
            assert!(out.iter().all(|s| s.is_finite()), "{filter:?} produced NaN");
            let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
            assert!(peak < 1.2, "{filter:?} peaked at {peak}");
            if filter != Filter::Off {
                assert!(rms(&out) > 0.05, "{filter:?} went silent");
            }
        }
    }

    #[test]
    fn eight_d_moves_sound_between_ears() {
        let out = run(Filter::EightD, &sine(440.0, 5 * 44_100), 5 * 44_100);
        let left: Vec<f32> = out.chunks_exact(2).map(|f| f[0]).collect();
        let right: Vec<f32> = out.chunks_exact(2).map(|f| f[1]).collect();
        let window = 22_050;
        let balance: Vec<f32> = left
            .chunks(window)
            .zip(right.chunks(window))
            .map(|(l, r)| rms(l) / (rms(r) + 1e-6))
            .collect();
        let (min, max) = balance
            .iter()
            .fold((f32::MAX, 0f32), |(a, b), x| (a.min(*x), b.max(*x)));
        assert!(max / min > 2.0, "pan barely moved: {balance:?}");
    }

    #[test]
    fn my_sound_follows_its_bands() {
        let bassy = Filter::Mine([6, 0, 0, 0, -6]);
        let low = rms(&run(bassy, &sine(60.0, 44_100), 44_100)[20_000..]);
        let high = rms(&run(bassy, &sine(10_000.0, 44_100), 44_100)[20_000..]);
        assert!(low > high * 2.5, "low {low} high {high}");
        // Flat is (nearly) the song as it was.
        let input = sine(440.0, 44_100);
        let flat = run(Filter::Mine([0; 5]), &input, 44_100);
        assert!((rms(&flat[20_000..]) - rms(&input[20_000..])).abs() < 0.01);
        let out = run(Filter::Mine([8; 5]), &sine(220.0, 44_100), 44_100);
        let peak = out.iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak < 1.2, "boosted everything and peaked at {peak}");
    }

    #[test]
    fn filters_round_trip_through_keys() {
        for filter in Filter::ALL {
            assert_eq!(Filter::from_key(filter.key()), Some(filter));
        }
    }
}
