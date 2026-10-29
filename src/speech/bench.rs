//! The voice commands bench, run by hand (see `voice_bench`): commands and
//! everyday talk, said clean, over music and over someone else talking,
//! cut up and heard just as in a call. It reports how many commands work,
//! how often talk sets something off, and how long answers take.

use std::{
    f32::consts::TAU,
    path::{Path, PathBuf},
};

use whisper_rs::{WhisperContext, WhisperContextParameters, WhisperState};

use super::{
    FRAME, RATE, Segmenter, Spoken, edit_distance, prompt, sure_enough, transcribe, understand,
};

/// A recorded clip: what's said, and what should happen.
struct Clip {
    /// Who says it (the recording's voice), so talk underneath is someone else.
    voice: String,
    samples: Vec<f32>,
    said: String,
    /// The command it should give; none for talk, where nothing should happen.
    expected: Option<String>,
}

/// What's going on around the person talking.
#[derive(Clone, Copy)]
enum Around {
    /// Nothing: the call only sends their voice.
    Quiet,
    /// Music from their speakers, this many dB under their voice.
    Music(f32),
    /// Someone else talking near their microphone, this many dB under.
    Talk(f32),
}

impl Around {
    fn label(self) -> String {
        match self {
            Around::Quiet => "clean".into(),
            Around::Music(db) => format!("music {db} dB under"),
            Around::Talk(db) => format!("talk {db} dB under"),
        }
    }
}

const AROUND: [Around; 5] = [
    Around::Quiet,
    Around::Music(20.0),
    Around::Music(10.0),
    Around::Music(5.0),
    Around::Talk(10.0),
];

/// What one clip came to.
#[derive(Default)]
struct Outcome {
    action: Option<Spoken>,
    /// Heard the name, but not a command.
    name_only: bool,
    /// Bits of speech the cutter handed over.
    bits: usize,
    /// What the model made of each.
    heard: Vec<String>,
    took_ms: Vec<u128>,
    /// From the end of speech to the action.
    answer_ms: Option<u128>,
    /// How sure the model was of each bit.
    sure: Vec<f32>,
}

#[derive(Default)]
struct Report {
    commands: usize,
    right: usize,
    wrong: usize,
    missed: usize,
    /// Commands the cutter never handed over (lost in the background).
    never_cut: usize,
    talk: usize,
    set_off: usize,
    name_only: usize,
    took_ms: Vec<u128>,
    answer_ms: Vec<u128>,
    notes: Vec<String>,
    /// How sure the model was, for commands that came out right.
    sure_right: Vec<f32>,
}

pub fn run() {
    let model = std::env::var("LOSKI_SPEECH_MODEL").expect("LOSKI_SPEECH_MODEL: the speech model");
    let dir = PathBuf::from(std::env::var("LOSKI_BENCH").expect("LOSKI_BENCH: the clips' folder"));
    // Part of a label, to run just those: "clean", "music", "talk".
    let only = std::env::var("LOSKI_BENCH_ONLY").unwrap_or_default();
    whisper_rs::install_logging_hooks();
    let context =
        WhisperContext::new_with_params(&model, WhisperContextParameters::default()).unwrap();
    let mut state = context.create_state().unwrap();
    let commands = load(&dir.join("commands"), true);
    let talk = load(&dir.join("talk"), false);
    // The kind of artists someone who plays these songs has in their history.
    let artists: Vec<String> = if std::env::var("LOSKI_BENCH_ARTISTS").as_deref() == Ok("none") {
        Vec::new()
    } else {
        ["Linkin Park", "The Weeknd", "Queen", "Breaking Benjamin"]
            .map(String::from)
            .to_vec()
    };
    let names = ["Loski".to_string()];
    let prompts = (prompt(&names, &[]), prompt(&names, &artists));
    let song = music(60 * RATE);
    println!(
        "{} commands and {} bits of everyday talk",
        commands.len(),
        talk.len()
    );
    for around in AROUND {
        if !only.split(',').any(|o| around.label().contains(o)) {
            continue;
        }
        let mut report = Report::default();
        for (i, clip) in commands.iter().chain(&talk).enumerate() {
            let length = RATE + clip.samples.len() + 2 * RATE;
            let background = match around {
                Around::Quiet => None,
                Around::Music(db) => {
                    let start = (i * 27_000) % (song.len() - length);
                    Some(under(&song[start..start + length], &clip.samples, db))
                }
                Around::Talk(db) => {
                    let others: Vec<f32> = talk
                        .iter()
                        .filter(|t| t.voice != clip.voice)
                        .flat_map(|t| t.samples.iter().copied())
                        .collect();
                    let start = (i * 13_000) % others.len().max(1);
                    let looped: Vec<f32> = others
                        .iter()
                        .cycle()
                        .skip(start)
                        .take(length)
                        .copied()
                        .collect();
                    Some(under(&looped, &clip.samples, db))
                }
            };
            let outcome = hear(&mut state, &prompts, &clip.samples, background.as_deref());
            report.add(clip, outcome);
        }
        report.print(&around.label());
    }
    if only.is_empty() || only.contains("hum") {
        println!("\n== humming ==");
        for (n, (base, seconds)) in [120.0, 180.0, 240.0]
            .into_iter()
            .flat_map(|base| [1.0, 1.5, 2.5, 4.0].map(|s| (base, s)))
            .enumerate()
        {
            let tune = hum(base, seconds, n as u32);
            let outcome = hear(&mut state, &prompts, &tune, None);
            let meant = outcome
                .heard
                .iter()
                .zip(&outcome.sure)
                .map(|(h, sure)| {
                    let meaning = Some(understand(h, "Loski")).filter(|m| sure_enough(m, *sure));
                    format!("{h:?} -> {:?}", meaning.flatten())
                })
                .collect::<Vec<_>>()
                .join(" + ");
            println!(
                "  {base:.0} Hz, {seconds} s: {} sure {:?}",
                if meant.is_empty() { "nothing" } else { &meant },
                outcome
                    .sure
                    .iter()
                    .map(|s| (s * 100.0).round() / 100.0)
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// Someone humming a tune at around `base` Hz, at 16 kHz: notes that move
/// every 0.4 s, a little vibrato, and a nasal "mmm" tone.
fn hum(base: f32, seconds: f32, seed: u32) -> Vec<f32> {
    const STEPS: [f32; 8] = [0.0, 2.0, 4.0, 5.0, 7.0, 5.0, 4.0, 2.0];
    let length = (seconds * RATE as f32) as usize;
    let mut phase = 0.0f32;
    (0..length)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let note = STEPS[((t / 0.4) as usize + seed as usize) % STEPS.len()];
            let f0 = base * 2f32.powf(note / 12.0) * (1.0 + 0.015 * (TAU * 5.0 * t).sin());
            phase += TAU * f0 / RATE as f32;
            // Strong low harmonics, weak high ones: humming through the nose.
            let tone: f32 = (1..10)
                .map(|k| (phase * k as f32).sin() / (k * k) as f32)
                .sum();
            let edge = (t / 0.08).min(1.0) * ((seconds - t) / 0.15).clamp(0.0, 1.0);
            0.25 * tone * edge
        })
        .collect()
}

/// The clip as one person's microphone would send it: a second of what's
/// around them, the clip, then two more seconds.
/// `prompts` as Loski uses them: the first listen without artists, and a
/// second with them for a song asked for.
fn hear(
    state: &mut WhisperState,
    prompts: &(String, String),
    voice: &[f32],
    background: Option<&[f32]>,
) -> Outcome {
    let lead = RATE;
    let mut mic = vec![0.0; lead + voice.len() + 2 * RATE];
    for (i, sample) in voice.iter().enumerate() {
        mic[lead + i] = *sample;
    }
    if let Some(background) = background {
        for (m, b) in mic.iter_mut().zip(background) {
            *m += b;
        }
    }
    let spoken_until = lead + voice.iter().rposition(|s| s.abs() > 0.02).unwrap_or(0);
    let mut cutter = Segmenter::default();
    let mut outcome = Outcome::default();
    for (i, frame) in mic.chunks(FRAME).enumerate() {
        let pcm: Vec<i16> = frame
            .iter()
            .map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .collect();
        // With nothing around them, Discord sends nothing between words.
        if background.is_none() && pcm.iter().all(|s| s.unsigned_abs() < 64) {
            cutter.silence();
        } else {
            cutter.hear(&pcm);
        }
        let Some(audio) = cutter.finished() else {
            continue;
        };
        outcome.bits += 1;
        let samples: Vec<f32> = audio.iter().map(|&s| f32::from(s) / 32768.0).collect();
        let mut heard = transcribe(state, &samples, &prompts.0).unwrap();
        let first = understand(&heard.text, "Loski");
        if matches!(&first, Some(Ok(asked)) if asked.names_something()) && prompts.0 != prompts.1 {
            let again = transcribe(state, &samples, &prompts.1).unwrap();
            if matches!(understand(&again.text, "Loski"), Some(Ok(better)) if better.names_something())
            {
                heard = super::Heard {
                    took: heard.took + again.took,
                    ..again
                };
            }
        }
        let took = heard.took.as_millis();
        outcome.took_ms.push(took);
        outcome.sure.push(heard.confidence);
        let after_speech =
            ((i + 1) * FRAME).saturating_sub(spoken_until) as u128 * 1000 / RATE as u128;
        let mut meaning = understand(&heard.text, "Loski");
        if !sure_enough(&meaning, heard.confidence) {
            meaning = None;
        }
        match meaning {
            Some(Ok(command)) if outcome.action.is_none() => {
                outcome.action = Some(command);
                outcome.answer_ms = Some(after_speech + took);
            }
            Some(Err(_)) => outcome.name_only = true,
            _ => {}
        }
        outcome.heard.push(heard.text);
    }
    outcome
}

impl Report {
    fn add(&mut self, clip: &Clip, outcome: Outcome) {
        self.took_ms.extend(&outcome.took_ms);
        let heard = if outcome.heard.is_empty() {
            "nothing".to_string()
        } else {
            outcome
                .heard
                .iter()
                .map(|h| format!("{h:?}"))
                .collect::<Vec<_>>()
                .join(" + ")
        };
        match &clip.expected {
            Some(expected) => {
                self.commands += 1;
                match &outcome.action {
                    Some(action) if matches(action, expected) => {
                        self.right += 1;
                        self.answer_ms.extend(outcome.answer_ms);
                        self.sure_right
                            .extend(outcome.sure.iter().copied().reduce(f32::max));
                    }
                    Some(action) => {
                        self.wrong += 1;
                        self.notes.push(format!(
                            "WRONG  {:?} heard as {heard} -> {action:?}",
                            clip.said
                        ));
                    }
                    None => {
                        self.missed += 1;
                        if outcome.bits == 0 {
                            self.never_cut += 1;
                        }
                        self.notes
                            .push(format!("MISSED {:?} heard as {heard}", clip.said));
                    }
                }
            }
            None => {
                self.talk += 1;
                if let Some(action) = &outcome.action {
                    self.set_off += 1;
                    self.notes.push(format!(
                        "SET OFF {:?} heard as {heard} -> {action:?}",
                        clip.said
                    ));
                } else if outcome.name_only {
                    self.name_only += 1;
                    self.notes.push(format!(
                        "NAME   {:?} heard as {heard}, sure {:.2}",
                        clip.said,
                        outcome.sure.iter().copied().fold(0.0, f32::max)
                    ));
                }
            }
        }
    }

    fn print(&self, label: &str) {
        let mut took = self.took_ms.clone();
        took.sort_unstable();
        let mut answer = self.answer_ms.clone();
        answer.sort_unstable();
        let average = |v: &[u128]| v.iter().sum::<u128>() / v.len().max(1) as u128;
        let at = |v: &[u128], share: f32| {
            v.get(((v.len() as f32 * share) as usize).min(v.len().saturating_sub(1)))
                .copied()
                .unwrap_or(0)
        };
        println!("\n== {label} ==");
        println!(
            "commands  {}/{} right, {} wrong, {} missed ({} never cut out of the background)",
            self.right, self.commands, self.wrong, self.missed, self.never_cut
        );
        println!(
            "talk      {}/{} set something off, {} only got the name",
            self.set_off, self.talk, self.name_only
        );
        println!(
            "model     {} ms on average, 95% under {} ms, slowest {} ms ({} bits)",
            average(&took),
            at(&took, 0.95),
            took.last().copied().unwrap_or(0),
            took.len()
        );
        println!(
            "answer    {} ms on average after they stop talking, slowest {} ms",
            average(&answer),
            answer.last().copied().unwrap_or(0)
        );
        let mut sure = self.sure_right.clone();
        sure.sort_by(f32::total_cmp);
        if let (Some(least), Some(most)) = (sure.first(), sure.last()) {
            println!(
                "sure      commands right: least {least:.2}, 1 in 10 under {:.2}, most {most:.2}",
                sure[sure.len() / 10]
            );
        }
        for note in &self.notes {
            println!("  {note}");
        }
    }
}

/// Whether `got` is the command `expected` names: "Skip", "Volume(30)", or
/// "Play:numb by linkin park" for a song (a few letters off is fine; Spotify's
/// search copes).
fn matches(got: &Spoken, expected: &str) -> bool {
    let named = match got {
        Spoken::Play(query) => Some(("Play", query)),
        Spoken::PlayNext(query) => Some(("PlayNext", query)),
        Spoken::Add(query) => Some(("Add", query)),
        Spoken::Playlist(query) => Some(("Playlist", query)),
        _ => None,
    };
    match (named, expected.split_once(':')) {
        (Some((kind, query)), Some((want_kind, want))) if kind == want_kind => {
            let squash = |s: &str| {
                s.split_whitespace()
                    .filter(|w| *w != "by")
                    .collect::<String>()
                    .to_lowercase()
                    .chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
            };
            let (got, want) = (squash(query), squash(want));
            edit_distance(&got, &want) * 3 <= want.chars().count()
        }
        _ => format!("{got:?}") == expected,
    }
}

/// The clips in `dir`: `name.wav` (16 kHz, mono, 16-bit) with `name.txt`
/// saying what's said, and for commands, a tab and what it should do.
fn load(dir: &Path, commands: bool) -> Vec<Clip> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "wav"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(path.with_extension("txt")).unwrap();
            let text = text.trim().trim_start_matches('\u{feff}');
            let (said, expected) = match text.split_once('\t') {
                Some((said, expected)) if commands => (said, Some(expected.trim().to_string())),
                _ => (text, None),
            };
            let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
            Clip {
                voice: stem.rsplit('-').next().unwrap_or_default().to_string(),
                samples: read_wav(&path),
                said: said.to_string(),
                expected,
            }
        })
        .collect()
}

fn read_wav(path: &Path) -> Vec<f32> {
    let bytes = std::fs::read(path).unwrap();
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        match &bytes[at..at + 4] {
            b"fmt " => {
                let channels = u16::from_le_bytes([body[2], body[3]]);
                let rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let bits = u16::from_le_bytes([body[14], body[15]]);
                assert_eq!(
                    (channels, rate, bits),
                    (1, RATE as u32, 16),
                    "{}: not 16 kHz mono 16-bit",
                    path.display()
                );
            }
            b"data" => {
                return body
                    .chunks_exact(2)
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                    .collect();
            }
            _ => {}
        }
        at += 8 + size + (size & 1);
    }
    panic!("{}: no audio in it", path.display());
}

/// `background` turned down to `db` under the speech in `voice`.
fn under(background: &[f32], voice: &[f32], db: f32) -> Vec<f32> {
    let level = |samples: &[f32]| {
        // The level while something's going on, not counting pauses.
        let loud: Vec<f32> = samples
            .chunks(FRAME)
            .map(|f| (f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).sqrt())
            .filter(|rms| *rms > 0.01)
            .collect();
        loud.iter().sum::<f32>() / loud.len().max(1) as f32
    };
    let gain = level(voice) / level(background).max(1e-6) / 10f32.powf(db / 20.0);
    background.iter().map(|s| s * gain).collect()
}

/// A stand-in for a song at 16 kHz: chords, a bass line, a kick on every
/// beat and hi-hats in between. Easier than a real one (nobody sings).
fn music(length: usize) -> Vec<f32> {
    const CHORDS: [[f32; 3]; 4] = [
        [220.0, 261.63, 329.63],
        [174.61, 220.0, 261.63],
        [196.0, 246.94, 293.66],
        [164.81, 207.65, 246.94],
    ];
    let mut seed: u32 = 1;
    (0..length)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let chord = CHORDS[(t / 2.0) as usize % CHORDS.len()];
            let pad = chord
                .iter()
                .map(|f| {
                    (TAU * f * t).sin()
                        + 0.4 * (TAU * 2.0 * f * t).sin()
                        + 0.2 * (TAU * 3.0 * f * t).sin()
                })
                .sum::<f32>()
                / 5.0;
            let bass = (TAU * chord[0] / 2.0 * t).sin();
            let beat = t % 0.5;
            let kick = (TAU * 55.0 * beat).sin() * (-beat * 25.0).exp();
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = (seed >> 9) as f32 / (1u32 << 23) as f32 * 2.0 - 1.0;
            let hat = noise * (-(t % 0.25) * 60.0).exp();
            0.4 * pad + 0.3 * bass + 0.6 * kick + 0.2 * hat
        })
        .collect()
}
