//! Voice commands (beta), the part that knows nothing about Discord: the
//! speech model, turning a few seconds of someone talking into text on this
//! computer (whisper.cpp), and what that text asks for.

use std::{
    path::{Path, PathBuf},
    sync::mpsc as std_mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, sync::oneshot};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// A small English model: quick for a few words on most computers.
pub const MODEL: &str = "ggml-base.en-q5_1.bin";
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en-q5_1.bin";
const MODEL_SHA256: &str = "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f";
/// Its size, to say before it downloads.
pub const MODEL_MB: u32 = 60;
/// Samples a second, as the model hears them.
pub const RATE: usize = 16_000;

pub fn model_path(data_dir: &Path) -> PathBuf {
    data_dir.join("models").join(MODEL)
}

/// Downloads the model once, and checks it's the file it should be.
pub async fn fetch_model(http: &reqwest::Client, data_dir: &Path) -> Result<PathBuf> {
    let path = model_path(data_dir);
    if path.exists() {
        return Ok(path);
    }
    let dir = path.parent().context("no models folder")?;
    tokio::fs::create_dir_all(dir).await?;
    let partial = path.with_extension("part");
    let mut response = http
        .get(MODEL_URL)
        .timeout(Duration::from_secs(600))
        .send()
        .await
        .context("couldn't reach Hugging Face for the speech model")?
        .error_for_status()?;
    let mut file = tokio::fs::File::create(&partial).await?;
    let mut hasher = Sha256::new();
    while let Some(chunk) = response.chunk().await? {
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    if format!("{:x}", hasher.finalize()) != MODEL_SHA256 {
        let _ = tokio::fs::remove_file(&partial).await;
        bail!("the speech model didn't arrive whole (its checksum is off); try again");
    }
    tokio::fs::rename(&partial, &path).await?;
    tracing::info!("Downloaded the speech model to {}", path.display());
    Ok(path)
}

/// Whether this processor runs the model as Loski's image builds it: with
/// AVX2 on Intel and AMD (any of the last ten years or so), on any 64-bit ARM.
pub fn cpu_supported() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        std::is_x86_feature_detected!("avx")
            && std::is_x86_feature_detected!("avx2")
            && std::is_x86_feature_detected!("fma")
            && std::is_x86_feature_detected!("f16c")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        true
    }
}

/// What the model made of one bit of speech, and how long it took.
pub struct Heard {
    pub text: String,
    pub took: Duration,
}

struct Job {
    samples: Vec<f32>,
    prompt: String,
    reply: oneshot::Sender<Result<Heard>>,
}

/// The model, loaded on a thread of its own; bits of speech queue there.
pub struct Recognizer {
    jobs: std_mpsc::SyncSender<Job>,
}

impl Recognizer {
    /// Loads the model (a second or two) and starts its thread.
    pub fn start(model: &Path) -> Result<Self> {
        // whisper.cpp writes a lot to stderr; this keeps it quiet.
        whisper_rs::install_logging_hooks();
        let context = WhisperContext::new_with_params(model, WhisperContextParameters::default())
            .context("couldn't load the speech model")?;
        let mut state = context
            .create_state()
            .context("couldn't load the speech model")?;
        let (jobs, queue) = std_mpsc::sync_channel::<Job>(3);
        std::thread::Builder::new()
            .name("speech".into())
            .spawn(move || {
                for job in queue {
                    let heard = transcribe(&mut state, &job.samples, &job.prompt);
                    let _ = job.reply.send(heard);
                }
            })?;
        Ok(Self { jobs })
    }

    /// The text for `samples` (mono, 16 kHz). None when a few are queued
    /// already (people talking over each other): by then it'd be too late
    /// to act on.
    pub async fn hear(&self, samples: Vec<f32>, prompt: String) -> Option<Result<Heard>> {
        let (reply, answer) = oneshot::channel();
        self.jobs
            .try_send(Job {
                samples,
                prompt,
                reply,
            })
            .ok()?;
        answer.await.ok()
    }
}

fn transcribe(
    state: &mut whisper_rs::WhisperState,
    samples: &[f32],
    prompt: &str,
) -> Result<Heard> {
    transcribe_with(state, samples, prompt, audio_ctx)
}

/// How much of the model's 30-second window `seconds` of speech gets:
/// much quicker than all of it for a few words. Cut any closer and it
/// starts losing words (on recorded commands, half came out as just "L").
fn audio_ctx(seconds: f32) -> i32 {
    ((seconds * 50.0) as i32 + 256).min(1500)
}

fn transcribe_with(
    state: &mut whisper_rs::WhisperState,
    samples: &[f32],
    prompt: &str,
    window: impl Fn(f32) -> i32,
) -> Result<Heard> {
    let started = Instant::now();
    // The model wants a second at least; short commands get some silence.
    let mut padded;
    let samples = if samples.len() < RATE {
        padded = samples.to_vec();
        padded.resize(RATE + RATE / 4, 0.0);
        &padded[..]
    } else {
        samples
    };
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some("en"));
    params.set_n_threads(
        std::thread::available_parallelism().map_or(2, |n| n.get().clamp(1, 4)) as i32,
    );
    params.set_no_context(true);
    params.set_single_segment(true);
    params.set_no_timestamps(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_nst(true);
    // No slower second tries: a command misheard is just said again.
    params.set_temperature_inc(0.0);
    let seconds = samples.len() as f32 / RATE as f32;
    params.set_audio_ctx(window(seconds));
    // Names and command words it might not expect.
    params.set_initial_prompt(prompt);
    state
        .full(params, samples)
        .context("the speech model failed")?;
    let text: Vec<String> = state
        .as_iter()
        .filter_map(|segment| segment.to_str_lossy().ok().map(|t| t.into_owned()))
        .collect();
    Ok(Heard {
        text: text.join(" ").trim().to_string(),
        took: started.elapsed(),
    })
}

/// Hints for the model: the bot's name and the words it listens for.
pub fn prompt(name: &str) -> String {
    format!(
        "{name}, pause. {name}, skip. {name}, play Midnight City. {name}, volume up. {name}, like this."
    )
}

/// What someone asked for out loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spoken {
    Pause,
    Resume,
    Skip,
    Previous,
    Leave,
    Louder,
    Quieter,
    Volume(u8),
    Shuffle,
    Like,
    Play(String),
}

/// What `text` asks the bot called `name` for, when it starts with the name
/// (or something close, like "Lowski", or "hey Loski"). `None` is people
/// talking; `Some(Err(..))` is said to the bot but not a command it knows,
/// with what came after the name.
pub fn understand(text: &str, name: &str) -> Option<Result<Spoken, String>> {
    let words: Vec<String> = text
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | '!' | '?' | ';' | ':'))
        .map(|w| {
            // "Loski's" is the name too, and "don't" is "dont".
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '%')
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    let target: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    if target.is_empty() {
        return None;
    }
    // The name in the first few words, said as one word or as two.
    let (at, len) = (0..words.len().min(3)).find_map(|i| {
        [1, 2]
            .into_iter()
            .find(|&n| i + n <= words.len() && sounds_like(&words[i..i + n].concat(), &target))
            .map(|n| (i, n))
    })?;
    let mut rest: Vec<&str> = words[at + len..]
        .iter()
        .map(String::as_str)
        .filter(|w| !matches!(*w, "please" | "now" | "thanks"))
        .collect();
    if let ["can" | "could" | "would" | "will", "you", ..] = rest.as_slice() {
        rest.drain(..2);
    }
    Some(
        phrase(&rest)
            .or_else(|| keyword(&rest))
            .ok_or_else(|| rest.join(" ")),
    )
}

/// A command said just so.
fn phrase(rest: &[&str]) -> Option<Spoken> {
    Some(match rest.join(" ").as_str() {
        "pause" | "stop" | "pause it" | "pause the music" | "stop the music" | "hold on"
        | "wait" => Spoken::Pause,
        "resume" | "start" | "play" | "continue" | "unpause" | "keep going" | "go on"
        | "play the music" | "start the music" => Spoken::Resume,
        "skip" | "next" | "skip it" | "skip this" | "skip this song" | "skip the song"
        | "next song" => Spoken::Skip,
        "previous" | "back" | "go back" | "previous song" | "last song" => Spoken::Previous,
        "leave" | "go away" | "disconnect" | "bye" | "goodbye" | "get out" => Spoken::Leave,
        "louder" | "volume up" | "turn it up" | "turn up" | "turn up the volume" => Spoken::Louder,
        "quieter"
        | "softer"
        | "volume down"
        | "turn it down"
        | "turn down"
        | "turn down the volume" => Spoken::Quieter,
        "shuffle" | "shuffle the queue" => Spoken::Shuffle,
        "like" | "like it" | "like this" | "like this song" | "i like this"
        | "i like this song" | "love this" | "i love this" | "i love this song" => Spoken::Like,
        _ => match rest {
            ["volume" | "set", .., amount] => Spoken::Volume(number(amount)?),
            ["play", song @ ..] if !song.is_empty() => Spoken::Play(song.join(" ")),
            _ => return None,
        },
    })
}

/// Failing that, the first command word in what was said: "Loski, um,
/// skip it" skips, "Loski, set the volume to 30" sets it.
fn keyword(rest: &[&str]) -> Option<Spoken> {
    rest.iter().enumerate().find_map(|(i, word)| {
        // "Don't stop" isn't "stop".
        if i > 0 && matches!(rest[i - 1], "dont" | "not" | "never") {
            return None;
        }
        let after = &rest[i + 1..];
        Some(match *word {
            "play" => match after {
                [] | ["it"] | ["music"] | ["the", "music"] | ["again"] => Spoken::Resume,
                song => Spoken::Play(song.join(" ")),
            },
            "pause" | "stop" | "wait" => Spoken::Pause,
            "resume" | "start" | "continue" | "unpause" => Spoken::Resume,
            "skip" | "next" => Spoken::Skip,
            "previous" | "back" => Spoken::Previous,
            "leave" | "disconnect" | "bye" | "goodbye" => Spoken::Leave,
            "louder" => Spoken::Louder,
            "quieter" | "softer" => Spoken::Quieter,
            "shuffle" => Spoken::Shuffle,
            // "I'd like to hear…" is a song coming, not a like.
            "like" | "love" if after.first() != Some(&"to") => Spoken::Like,
            "volume" | "turn" => after.iter().find_map(|w| match *w {
                "up" => Some(Spoken::Louder),
                "down" => Some(Spoken::Quieter),
                w => number(w).map(Spoken::Volume),
            })?,
            _ => return None,
        })
    })
}

/// "50", "50%" or "fifty", up to a hundred.
fn number(word: &str) -> Option<u8> {
    let word = word.trim_end_matches('%');
    let value = word.parse::<u32>().ok().or(match word {
        "zero" => Some(0),
        "ten" => Some(10),
        "twenty" => Some(20),
        "thirty" => Some(30),
        "forty" => Some(40),
        "fifty" => Some(50),
        "sixty" => Some(60),
        "seventy" => Some(70),
        "eighty" => Some(80),
        "ninety" => Some(90),
        "hundred" => Some(100),
        _ => None,
    })?;
    Some(value.min(100) as u8)
}

/// Close enough to the bot's name for a model that's never heard it: a
/// letter off for short names, two for long ones.
fn sounds_like(word: &str, name: &str) -> bool {
    let allowed = match name.chars().count() {
        0..=3 => 0,
        4..=6 => 1,
        _ => 2,
    };
    edit_distance(word, name) <= allowed
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous + usize::from(ca != cb);
            previous = row[j + 1];
            row[j + 1] = substitute.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn understands_commands_after_the_name() {
        let hear = |text: &str| understand(text, "Loski").and_then(Result::ok);
        assert_eq!(hear("Loski, skip."), Some(Spoken::Skip));
        assert_eq!(hear("Hey Lowski, pause the music"), Some(Spoken::Pause));
        assert_eq!(hear("Losky next song please"), Some(Spoken::Skip));
        assert_eq!(hear("Loski, can you turn it up?"), Some(Spoken::Louder));
        assert_eq!(hear("Loski volume 40%"), Some(Spoken::Volume(40)));
        assert_eq!(
            hear("Loski, set the volume to fifty."),
            Some(Spoken::Volume(50))
        );
        assert_eq!(
            hear("Loski, play The Diary of Jane."),
            Some(Spoken::Play("the diary of jane".into()))
        );
        assert_eq!(hear("Loski play"), Some(Spoken::Resume));
        assert_eq!(hear("Loski, I love this song!"), Some(Spoken::Like));
        // Not said just so.
        assert_eq!(hear("Loski, um, skip it"), Some(Spoken::Skip));
        assert_eq!(hear("Loski's, stop."), Some(Spoken::Pause));
        assert_eq!(hear("Loski, stop it now"), Some(Spoken::Pause));
        assert_eq!(hear("Loski, could you start again"), Some(Spoken::Resume));
        assert_eq!(
            hear("Loski, set the volume to 30"),
            Some(Spoken::Volume(30))
        );
        assert_eq!(hear("Loski, turn the volume down"), Some(Spoken::Quieter));
        assert_eq!(hear("Loski, I'd like to hear Numb"), None);
        assert_eq!(hear("Loski, don't stop the music"), None);
        // Talk that isn't for the bot.
        assert_eq!(hear("skip this one, it's boring"), None);
        assert_eq!(hear("lots of people skip it"), None);
        assert_eq!(hear("Loski, what's the weather"), None);
        assert_eq!(hear(""), None);
    }

    #[test]
    fn says_when_it_was_meant_for_the_bot() {
        assert_eq!(
            understand("Loski, look at this.", "Loski"),
            Some(Err("look at this".into()))
        );
        assert_eq!(understand("Loski.", "Loski"), Some(Err(String::new())));
        assert_eq!(understand("look at this", "Loski"), None);
    }

    /// By hand: LOSKI_SPEECH_MODEL=model.bin LOSKI_SPEECH_CLIPS=dir (16 kHz
    /// mono WAVs, each with a .wav.txt of what's said).
    #[test]
    #[ignore]
    fn hears_recorded_commands() {
        let model = std::env::var("LOSKI_SPEECH_MODEL").unwrap();
        let dir = std::env::var("LOSKI_SPEECH_CLIPS").unwrap();
        whisper_rs::install_logging_hooks();
        let context =
            WhisperContext::new_with_params(&model, WhisperContextParameters::default()).unwrap();
        let mut state = context.create_state().unwrap();
        let mut clips: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "wav"))
            .collect();
        clips.sort();
        let mut right = 0;
        let mut took = 0;
        for clip in &clips {
            let bytes = std::fs::read(clip).unwrap();
            let data = bytes.windows(4).position(|w| w == b"data").unwrap() + 8;
            let samples: Vec<f32> = bytes[data..]
                .chunks_exact(2)
                .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                .collect();
            let said = std::fs::read_to_string(clip.with_extension("wav.txt")).unwrap();
            let said = said.trim().trim_start_matches('\u{feff}');
            let heard = transcribe(&mut state, &samples, &prompt("Loski")).unwrap();
            let got = understand(&heard.text, "Loski");
            let ok = got == understand(said, "Loski");
            right += usize::from(ok);
            took += heard.took.as_millis();
            println!(
                "{} {:>4} ms {said:?} heard as {:?} -> {got:?}",
                if ok { "ok " } else { "BAD" },
                heard.took.as_millis(),
                heard.text
            );
        }
        println!(
            "{right}/{} right, {} ms on average",
            clips.len(),
            took / clips.len().max(1) as u128
        );
    }

    #[test]
    fn hears_names_of_any_length() {
        let hear = |text: &str, name: &str| understand(text, name).and_then(Result::ok);
        assert_eq!(hear("DJ Bob, skip", "DJ Bob"), Some(Spoken::Skip));
        assert_eq!(hear("deejay bob skip", "DJ Bob"), None);
        assert_eq!(hear("Max, pause", "Max"), Some(Spoken::Pause));
        assert_eq!(hear("Mac, pause", "Max"), None);
        assert_eq!(
            hear("Bartholomew, shuffle", "Bartholomeo"),
            Some(Spoken::Shuffle)
        );
    }
}
