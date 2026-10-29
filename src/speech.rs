//! Voice commands (beta), the part that knows nothing about Discord: the
//! speech model, turning a few seconds of someone talking into text on this
//! computer (whisper.cpp), and what that text asks for.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::mpsc as std_mpsc,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, sync::oneshot};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// A speech model to pick from in /voice: one of whisper.cpp's English ones.
pub struct Model {
    pub key: &'static str,
    pub label: &'static str,
    /// What it's like, for the menu.
    pub about: &'static str,
    file: &'static str,
    sha256: &'static str,
    /// Its size, to say before it downloads.
    pub mb: u32,
}

/// The speech models, the usual one first.
pub static MODELS: [Model; 2] = [
    Model {
        key: "accurate",
        label: "Accurate",
        about: "The usual one, the surest of what it hears",
        file: "ggml-base.en-q5_1.bin",
        sha256: "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f",
        mb: 60,
    },
    Model {
        key: "quick",
        label: "Quick",
        about: "About 2× faster, for slower computers",
        file: "ggml-tiny.en-q5_1.bin",
        sha256: "c77c5766f1cef09b6b7d47f21b546cbddd4157886b3b5d6d4f709e91e66c7c2b",
        mb: 31,
    },
];

/// The model picked in /voice (by its key), or the usual one.
pub fn model(key: Option<&str>) -> &'static Model {
    MODELS
        .iter()
        .find(|m| Some(m.key) == key)
        .unwrap_or(&MODELS[0])
}

/// Samples a second, as the model hears them.
pub const RATE: usize = 16_000;

impl Model {
    pub fn path(&self, data_dir: &Path) -> PathBuf {
        data_dir.join("models").join(self.file)
    }

    /// Downloads it once, and checks it's the file it should be.
    pub async fn fetch(&self, http: &reqwest::Client, data_dir: &Path) -> Result<PathBuf> {
        let path = self.path(data_dir);
        if path.exists() {
            return Ok(path);
        }
        let dir = path.parent().context("no models folder")?;
        tokio::fs::create_dir_all(dir).await?;
        let partial = path.with_extension("part");
        let url = format!(
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
            self.file
        );
        let mut response = http
            .get(url)
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
        if format!("{:x}", hasher.finalize()) != self.sha256 {
            let _ = tokio::fs::remove_file(&partial).await;
            bail!("the speech model didn't arrive whole (its checksum is off); try again");
        }
        tokio::fs::rename(&partial, &path).await?;
        tracing::info!("Downloaded the speech model to {}", path.display());
        Ok(path)
    }
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
    /// How sure the model was of its words, on average (0 to 1). Humming
    /// or a cough written down as "Loski" comes out low.
    pub confidence: f32,
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
    /// Loads the model (a second or two) and starts its thread. It's tried
    /// on a moment of silence first, twice, since the first go is the
    /// slowest; how long the second took is about the least a command takes
    /// on this computer.
    pub fn start(model: &Path) -> Result<(Self, Duration)> {
        // whisper.cpp writes a lot to stderr; this keeps it quiet.
        whisper_rs::install_logging_hooks();
        let context = WhisperContext::new_with_params(model, WhisperContextParameters::default())
            .context("couldn't load the speech model")?;
        let mut state = context
            .create_state()
            .context("couldn't load the speech model")?;
        let quiet = vec![0.0; RATE];
        transcribe(&mut state, &quiet, "")?;
        let tried = transcribe(&mut state, &quiet, "")?.took;
        let (jobs, queue) = std_mpsc::sync_channel::<Job>(3);
        std::thread::Builder::new()
            .name("speech".into())
            .spawn(move || {
                for job in queue {
                    let heard = transcribe(&mut state, &job.samples, &job.prompt);
                    let _ = job.reply.send(heard);
                }
            })?;
        Ok((Self { jobs }, tried))
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

/// The most the model may write for one bit of speech.
const MAX_TOKENS: i32 = 32;
/// Samples in one 20 ms frame, as the call hands them over.
#[cfg(test)]
const FRAME: usize = RATE / 50;
/// A frame this loud or louder can be someone speaking.
const LOUD: f64 = 350.0;
/// How far over what's around them speech has to rise. On the bench, with
/// music 10 dB under the voice, 47 of 50 commands came through (none did
/// with a fixed level), and 41 with it only 5 dB under.
const OVER_BACKGROUND_DB: f64 = 4.0;
/// How slowly the background level may rise.
const BACKGROUND_RISE_DB_PER_SEC: f64 = 2.0;
/// Frames a level is averaged over.
const SMOOTH_FRAMES: usize = 3;
/// Quiet this long (in frames) ends a bit of speech.
const END_QUIET: u32 = 30;
/// Less speech than this (in frames) is a cough, not a command.
const MIN_VOICED: u32 = 12;
/// Longer than this is talk, not a command.
const MAX_SAMPLES: usize = 7 * RATE;
/// Frames kept from just before speech starts, for a soft first sound.
const BEFORE_FRAMES: usize = 10;

/// Cuts one person's voice, a frame at a time, into bits of speech short
/// enough to be commands. Speech is told apart from what's around them
/// (music from their speakers, someone across the room) by how far it rises
/// over it, so a command still ends when they stop, even with the music on.
#[derive(Default)]
pub struct Segmenter {
    /// The last few frames' levels.
    recent: VecDeque<f64>,
    /// How loud it is around them; 0 after a stretch of silence.
    background: f64,
    audio: Vec<i16>,
    /// The moment before speech starts, kept so a soft first sound comes along.
    before: Vec<i16>,
    /// Frames of quiet since they last spoke.
    quiet: u32,
    /// Frames of speech in `audio`.
    voiced: u32,
    /// Talking for longer than a command: ignored until they pause.
    too_long: bool,
}

impl Segmenter {
    /// Whether a frame at `level` is them speaking.
    fn is_speech(&mut self, level: f64) -> bool {
        // Averaged over a few frames, so a drum hit isn't a word.
        self.recent.push_back(level);
        if self.recent.len() > SMOOTH_FRAMES {
            self.recent.pop_front();
        }
        let level = self.recent.iter().sum::<f64>() / self.recent.len() as f64;
        let over = 10f64.powf(OVER_BACKGROUND_DB / 20.0);
        // The background drops at once to anything quieter and rises slowly,
        // so someone talking doesn't turn into background.
        let rise = 10f64.powf(BACKGROUND_RISE_DB_PER_SEC / 20.0 / 50.0);
        // It starts from whatever's heard first: with music on, that's the
        // music.
        self.background = if self.background == 0.0 || level < self.background {
            level
        } else {
            self.background * rise
        };
        level >= LOUD && level >= self.background * over
    }

    /// A frame of their voice.
    pub fn hear(&mut self, pcm: &[i16]) {
        let loud = self.is_speech(rms(pcm));
        if loud {
            self.quiet = 0;
        } else {
            self.quiet = self.quiet.saturating_add(1);
        }
        if self.too_long {
            return;
        }
        if loud {
            self.voiced += 1;
        }
        if loud && self.audio.is_empty() {
            self.audio.append(&mut self.before);
        }
        if loud || !self.audio.is_empty() {
            self.audio.extend_from_slice(pcm);
        } else {
            self.before.extend_from_slice(pcm);
            let keep = BEFORE_FRAMES * pcm.len();
            if self.before.len() > keep {
                self.before.drain(..self.before.len() - keep);
            }
        }
        if self.audio.len() > MAX_SAMPLES {
            self.forget();
            self.too_long = true;
        }
    }

    /// A frame where they sent nothing.
    pub fn silence(&mut self) {
        self.quiet = self.quiet.saturating_add(1);
        self.before.clear();
        self.recent.clear();
        self.background = 0.0;
    }

    /// The speech that just ended, if any (and long enough to be a command).
    pub fn finished(&mut self) -> Option<Vec<i16>> {
        if self.quiet < END_QUIET || (self.audio.is_empty() && !self.too_long) {
            return None;
        }
        let enough = self.voiced >= MIN_VOICED && !self.too_long;
        let audio = std::mem::take(&mut self.audio);
        self.forget();
        enough.then_some(audio)
    }

    /// Drops whatever was half-heard.
    pub fn forget(&mut self) {
        self.audio = Vec::new();
        self.voiced = 0;
        self.too_long = false;
    }
}

fn rms(pcm: &[i16]) -> f64 {
    if pcm.is_empty() {
        return 0.0;
    }
    let sum: f64 = pcm.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
    (sum / pcm.len() as f64).sqrt()
}

fn transcribe(
    state: &mut whisper_rs::WhisperState,
    samples: &[f32],
    prompt: &str,
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
    // A command is a handful of words. This stops the model going round in
    // circles ("Loski, pause. Loski, pause…"), which once took 15 seconds.
    params.set_max_tokens(MAX_TOKENS);
    params.set_audio_ctx(audio_ctx(samples.len() as f32 / RATE as f32));
    // Names and command words it might not expect.
    params.set_initial_prompt(prompt);
    state
        .full(params, samples)
        .context("the speech model failed")?;
    let mut text = Vec::new();
    let mut sure = Vec::new();
    for segment in state.as_iter() {
        if let Ok(words) = segment.to_str_lossy() {
            text.push(words.into_owned());
        }
        for i in 0..segment.n_tokens() {
            let Some(token) = segment.get_token(i) else {
                continue;
            };
            // Markers like [_BEG_] and <|endoftext|> aren't words.
            let marker = token
                .to_str_lossy()
                .is_ok_and(|t| t.starts_with("[_") || t.starts_with("<|"));
            if !marker {
                sure.push(token.token_probability());
            }
        }
    }
    let confidence = if sure.is_empty() {
        0.0
    } else {
        sure.iter().sum::<f32>() / sure.len() as f32
    };
    Ok(Heard {
        text: text.join(" ").trim().to_string(),
        took: started.elapsed(),
        confidence,
    })
}

/// How much of the model's 30-second window `seconds` of speech gets:
/// much quicker than all of it for a few words. Cut any closer and it
/// starts losing words: on the bench, half the commands came out as just
/// "L" with the speech's length plus a little, and the shortest ones
/// ("Loski, stop") still did until the window was at least 384.
fn audio_ctx(seconds: f32) -> i32 {
    ((seconds * 50.0) as i32 + 256).clamp(384, 1500)
}

/// Hints for the model: the bot's names, the words it listens for, and the
/// artists you play, so they're spelled the way you'd search for them.
pub fn prompt(names: &[String], artists: &[String]) -> String {
    let name = names.first().map_or("Loski", String::as_str);
    let mut hints = format!(
        "{name}, pause. {name}, skip. {name}, play Midnight City. {name}, volume up. {name}, like this."
    );
    for other in names.iter().skip(1) {
        hints.push_str(&format!(" {other}, skip."));
    }
    if !artists.is_empty() {
        hints.push_str(&format!(" Songs by {}.", artists.join(", ")));
    }
    hints
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
    /// What's playing, said out loud.
    WhatsPlaying,
    /// A song right after this one.
    PlayNext(String),
    /// A song at the end of the queue.
    Add(String),
    /// Your liked songs.
    Liked,
    /// One of your playlists, by name.
    Playlist(String),
    /// A radio from what's playing.
    Radio,
    Repeat(RepeatWhat),
    /// Forward this many seconds, or back when below 0.
    Seek(i32),
    /// Leave in this many minutes, or after this song.
    Sleep(Option<u32>),
    /// No sleep timer after all.
    SleepOff,
}

impl Spoken {
    /// Whether it names a song or a playlist, whose spelling matters for
    /// finding it.
    pub fn names_something(&self) -> bool {
        matches!(
            self,
            Spoken::Play(_) | Spoken::PlayNext(_) | Spoken::Add(_) | Spoken::Playlist(_)
        )
    }
}

/// What "repeat" is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatWhat {
    Off,
    Song,
    Queue,
}

/// What `text` asks the bot called `name` for, when a sentence in it starts
/// with the name (or something close, like "Lowski", or "hey Loski"); the
/// model writes down whoever else is talking nearby too. `None` is people
/// talking; `Some(Err(..))` is said to the bot but not a command it knows,
/// with what came after the name.
pub fn understand(text: &str, name: &str) -> Option<Result<Spoken, String>> {
    let target: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    if target.is_empty() {
        return None;
    }
    // The words, and where each sentence starts among them.
    let mut words = Vec::new();
    let mut starts = Vec::new();
    for sentence in text.split_inclusive(['.', '?', '!']) {
        starts.push(words.len());
        words.extend(words_of(sentence));
    }
    let mut not_a_command = None;
    for start in starts {
        // The name comes first, after a "hey" or "okay" at most, said as one
        // word or as two. Anywhere else it's people talking about the bot.
        let at = start
            + words[start.min(words.len())..]
                .iter()
                .take_while(|w| GREETINGS.contains(&w.as_str()))
                .count()
                .min(2);
        let Some(len) = [1, 2]
            .into_iter()
            .find(|&n| at + n <= words.len() && sounds_like(&words[at..at + n].concat(), &target))
        else {
            continue;
        };
        let rest = &words[at + len..];
        match command_in(rest) {
            Some(spoken) => return Some(Ok(spoken)),
            None => {
                not_a_command.get_or_insert_with(|| {
                    rest.iter()
                        .map(String::as_str)
                        .filter(|w| !FILLER.contains(w))
                        .collect::<Vec<_>>()
                        .join(" ")
                });
            }
        }
    }
    not_a_command.map(Err)
}

/// Like `understand`, for a bot answering to any of `names`.
pub fn understand_any(text: &str, names: &[String]) -> Option<Result<Spoken, String>> {
    let mut not_a_command = None;
    for name in names {
        match understand(text, name) {
            Some(Ok(spoken)) => return Some(Ok(spoken)),
            Some(Err(rest)) => {
                not_a_command.get_or_insert(Err(rest));
            }
            None => {}
        }
    }
    not_a_command
}

/// A command said without the name, right after someone said just the
/// name ("Loski." … "Skip.").
pub fn command(text: &str) -> Option<Spoken> {
    command_in(&words_of(text))
}

/// Words as the commands are matched: lowercase, without punctuation.
fn words_of(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '.' | '!' | '?' | ';' | ':'))
        .map(|w| {
            // "Loski's" is the name too, and "don't" is "dont".
            w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '%')
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Words that say nothing about what's asked.
const FILLER: [&str; 3] = ["please", "now", "thanks"];

/// The command in what came after the name.
fn command_in(words: &[String]) -> Option<Spoken> {
    let mut rest: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .filter(|w| !FILLER.contains(w))
        .collect();
    if let ["can" | "could" | "would" | "will", "you", ..] = rest.as_slice() {
        rest.drain(..2);
    }
    pattern(&rest)
        .or_else(|| phrase(&rest))
        .or_else(|| keyword(&rest))
}

/// Below this, a bare "Loski" or words that aren't a command are let go. On
/// the bench, humming came out 4 to 56% sure (once written as "Loski,"),
/// commands at least 73%, and someone talking to Loski 94% or more.
const SURE_NAME: f32 = 0.65;
/// Below this, even a command is let go: the model wasn't hearing words.
const SURE_COMMAND: f32 = 0.45;

/// Whether the model was sure enough of its words to act on `meaning`.
pub fn sure_enough(meaning: &Option<Result<Spoken, String>>, confidence: f32) -> bool {
    match meaning {
        Some(Ok(_)) => confidence >= SURE_COMMAND,
        Some(Err(_)) => confidence >= SURE_NAME,
        None => true,
    }
}

/// What may come before the name.
const GREETINGS: [&str; 10] = [
    "hey", "hi", "ok", "okay", "yo", "oh", "um", "uh", "so", "alright",
];

/// A command said just so.
fn phrase(rest: &[&str]) -> Option<Spoken> {
    Some(match rest.join(" ").as_str() {
        "pause" | "stop" | "pause it" | "pause the music" | "stop the music" | "hold on"
        | "wait" => Spoken::Pause,
        "resume" | "start" | "play" | "continue" | "unpause" | "keep going" | "go on"
        | "play the music" | "start the music" => Spoken::Resume,
        "skip" | "next" | "skip it" | "skip this" | "skip this song" | "skip the song"
        | "next song" | "play next" | "play the next song" | "play the next one" => Spoken::Skip,
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

/// Commands with something particular in them, tried first: a time and a
/// direction ("back 10 seconds"), a song and where it goes ("play Numb
/// next"), what to repeat, when to leave.
fn pattern(rest: &[&str]) -> Option<Spoken> {
    let text = rest.join(" ");
    let fixed = match text.as_str() {
        "whats playing"
        | "what is playing"
        | "whats playing now"
        | "what song is playing"
        | "whats this"
        | "what is this"
        | "whats this song"
        | "what is this song"
        | "what song is this"
        | "whats the song"
        | "who is this"
        | "whos this" => Some(Spoken::WhatsPlaying),
        "radio"
        | "start a radio"
        | "start radio"
        | "play a radio"
        | "more like this"
        | "play more like this"
        | "play something like this"
        | "something like this" => Some(Spoken::Radio),
        "play my liked songs"
        | "play my likes"
        | "play my liked"
        | "play my favorites"
        | "play my favourites"
        | "play liked songs"
        | "liked songs"
        | "my liked songs" => Some(Spoken::Liked),
        "cancel the sleep timer"
        | "cancel sleep timer"
        | "cancel sleep"
        | "no sleep timer"
        | "stop the sleep timer"
        | "turn off the sleep timer" => Some(Spoken::SleepOff),
        "stop after this song"
        | "stop after this one"
        | "sleep after this song"
        | "leave after this song"
        | "leave after this one" => Some(Spoken::Sleep(None)),
        _ => None,
    };
    if fixed.is_some() {
        return fixed;
    }
    match rest {
        // "Liked" is often heard as "like" or "likes".
        ["play", "my", word, "songs" | "song" | "music"] | ["play", "my", word]
            if word.starts_with("like") || word.starts_with("favo") =>
        {
            return Some(Spoken::Liked);
        }
        ["play", "my" | "the", "playlist", name @ ..] | ["play", "playlist", name @ ..]
            if !name.is_empty() =>
        {
            return Some(Spoken::Playlist(name.join(" ")));
        }
        ["play", "my", name @ .., "playlist"] if !name.is_empty() => {
            return Some(Spoken::Playlist(name.join(" ")));
        }
        ["play" | "add" | "queue" | "put", song @ .., "next"] if !song.is_empty() => {
            return Some(Spoken::PlayNext(song.join(" ")));
        }
        ["add" | "queue", song @ ..] if !song.is_empty() => {
            let song = match song {
                [song @ .., "to", "the", "queue" | "list"] | [song @ .., "to", "queue"] => song,
                song => song,
            };
            if !song.is_empty() {
                return Some(Spoken::Add(song.join(" ")));
            }
        }
        _ => {}
    }
    let has = |words: &[&str]| rest.iter().any(|w| words.contains(w));
    if has(&["repeat", "repeating", "loop", "looping"]) {
        return Some(Spoken::Repeat(
            if has(&["off", "stop", "dont", "no", "cancel", "not"]) {
                RepeatWhat::Off
            } else if has(&["queue", "cue", "all", "everything", "playlist", "list"]) {
                RepeatWhat::Queue
            } else {
                RepeatWhat::Song
            },
        ));
    }
    // A stretch of time: "30 seconds", "a minute", "half an hour".
    let unit = rest.iter().position(|w| {
        matches!(
            *w,
            "second"
                | "seconds"
                | "secs"
                | "sec"
                | "minute"
                | "minutes"
                | "mins"
                | "hour"
                | "hours"
        )
    });
    if let Some(at) = unit {
        let count = if rest[..at].contains(&"half") {
            None
        } else {
            rest[..at].iter().rev().find_map(|w| amount(w))
        };
        let forward = has(&["forward", "ahead", "skip"]);
        let back = has(&["back", "backward", "backwards", "rewind"]);
        let seconds = match rest[at] {
            "hour" | "hours" => count.map_or(1800, |n| n * 3600),
            "minute" | "minutes" | "mins" => count.map_or(30, |n| n * 60),
            _ => count.unwrap_or(15),
        };
        if forward != back {
            let seconds = seconds.min(3600) as i32;
            return Some(Spoken::Seek(if forward { seconds } else { -seconds }));
        }
        if has(&["sleep", "stop", "leave", "off"]) {
            return Some(Spoken::Sleep(Some((seconds / 60).clamp(1, 600))));
        }
    }
    None
}

/// A count, said as digits or a word: "30", "thirty", "a".
fn amount(word: &str) -> Option<u32> {
    word.parse().ok().or(match word {
        "a" | "an" | "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "fifteen" => Some(15),
        "twenty" => Some(20),
        "thirty" => Some(30),
        "forty" => Some(40),
        "fortyfive" => Some(45),
        "fifty" => Some(50),
        "sixty" => Some(60),
        "ninety" => Some(90),
        _ => None,
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
        let word = match *word {
            "started" => "start",
            "stopped" => "stop",
            "skipped" => "skip",
            "paused" => "pause",
            "resumed" => "resume",
            word => word,
        };
        Some(match word {
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
            // A like is "like this" or "I love it", not "I'd like to hear…"
            // or "what's the weather like".
            "like" | "love"
                if after.first() != Some(&"to")
                    && (i == 0 || matches!(rest[i - 1], "i" | "we" | "really" | "so")) =>
            {
                Spoken::Like
            }
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
mod bench;

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
        assert_eq!(hear("Loski started."), Some(Spoken::Resume));
        assert_eq!(hear("Okay Loski, pause"), Some(Spoken::Pause));
        // Talking about the bot isn't talking to it.
        assert_eq!(hear("I told Loski to stop yesterday"), None);
        assert_eq!(understand("I think Loski played this", "Loski"), None);
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

    #[test]
    fn finds_the_name_after_someone_else_talking() {
        let hear = |text: &str| understand(text, "Loski").and_then(Result::ok);
        assert_eq!(
            hear("Stop it. Ok Loski, pause. Can you play the game with us tonight?"),
            Some(Spoken::Pause)
        );
        assert_eq!(
            hear("That's so easy. Loski, skip. Can you play the game?"),
            Some(Spoken::Skip)
        );
        // Said as two sentences.
        assert_eq!(hear("Loski. Next song."), Some(Spoken::Skip));
        // Still not when the name isn't how a sentence starts.
        assert_eq!(hear("Stop it. I told Loski to stop."), None);
    }

    #[test]
    fn understands_the_newer_commands() {
        let hear = |text: &str| understand(text, "Loski").and_then(Result::ok);
        assert_eq!(hear("Loski, what's playing?"), Some(Spoken::WhatsPlaying));
        assert_eq!(
            hear("Loski, what song is this?"),
            Some(Spoken::WhatsPlaying)
        );
        assert_eq!(
            hear("Loski, play Numb next."),
            Some(Spoken::PlayNext("numb".into()))
        );
        assert_eq!(
            hear("Loski, add Bohemian Rhapsody to the queue."),
            Some(Spoken::Add("bohemian rhapsody".into()))
        );
        assert_eq!(hear("Loski, play my liked songs."), Some(Spoken::Liked));
        assert_eq!(hear("Loski, play my like songs."), Some(Spoken::Liked));
        assert_eq!(
            hear("Loski, repeat the cue."),
            Some(Spoken::Repeat(RepeatWhat::Queue))
        );
        assert_eq!(
            hear("Loski, play my playlist Chill Vibes."),
            Some(Spoken::Playlist("chill vibes".into()))
        );
        assert_eq!(
            hear("Loski, play my gym playlist"),
            Some(Spoken::Playlist("gym".into()))
        );
        assert_eq!(hear("Loski, more like this."), Some(Spoken::Radio));
        assert_eq!(
            hear("Loski, repeat this song."),
            Some(Spoken::Repeat(RepeatWhat::Song))
        );
        assert_eq!(
            hear("Loski, repeat the queue."),
            Some(Spoken::Repeat(RepeatWhat::Queue))
        );
        assert_eq!(
            hear("Loski, stop repeating."),
            Some(Spoken::Repeat(RepeatWhat::Off))
        );
        assert_eq!(hear("Loski, forward 30 seconds."), Some(Spoken::Seek(30)));
        assert_eq!(hear("Loski, skip ahead a minute."), Some(Spoken::Seek(60)));
        assert_eq!(hear("Loski, go back 10 seconds."), Some(Spoken::Seek(-10)));
        assert_eq!(hear("Loski, go back."), Some(Spoken::Previous));
        assert_eq!(
            hear("Loski, sleep in 30 minutes."),
            Some(Spoken::Sleep(Some(30)))
        );
        assert_eq!(
            hear("Loski, stop in half an hour."),
            Some(Spoken::Sleep(Some(30)))
        );
        assert_eq!(
            hear("Loski, stop in an hour."),
            Some(Spoken::Sleep(Some(60)))
        );
        assert_eq!(
            hear("Loski, stop after this song."),
            Some(Spoken::Sleep(None))
        );
        assert_eq!(
            hear("Loski, cancel the sleep timer."),
            Some(Spoken::SleepOff)
        );
        assert_eq!(hear("Loski, play next."), Some(Spoken::Skip));
        // The older ones still mean what they did.
        assert_eq!(hear("Loski, stop."), Some(Spoken::Pause));
        assert_eq!(
            hear("Loski, play Numb by Linkin Park."),
            Some(Spoken::Play("numb by linkin park".into()))
        );
    }

    #[test]
    fn answers_to_other_names() {
        let names = vec!["Loski".to_string(), "DJ".to_string()];
        assert_eq!(understand_any("DJ, skip.", &names), Some(Ok(Spoken::Skip)));
        assert_eq!(
            understand_any("Loski, pause.", &names),
            Some(Ok(Spoken::Pause))
        );
        assert_eq!(understand_any("Skip it.", &names), None);
        let hints = prompt(&names, &["Linkin Park".to_string()]);
        assert!(hints.contains("DJ, skip.") && hints.contains("Songs by Linkin Park."));
    }

    #[test]
    fn hears_a_command_on_its_own_after_the_name() {
        assert_eq!(command("Skip this song, please."), Some(Spoken::Skip));
        assert_eq!(command("Volume 40"), Some(Spoken::Volume(40)));
        assert_eq!(command("What's the weather like?"), None);
    }

    /// The voice commands bench, by hand (a few minutes):
    /// `LOSKI_SPEECH_MODEL=data/models/ggml-base.en-q5_1.bin
    /// LOSKI_BENCH=data/voice-bench cargo test voice_bench -- --ignored
    /// --nocapture`, with clips from `scripts/voice-bench.ps1`.
    #[test]
    #[ignore]
    fn voice_bench() {
        super::bench::run();
    }

    /// A frame of a square wave this loud.
    fn tone(level: i16) -> Vec<i16> {
        (0..FRAME)
            .map(|i| if i % 2 == 0 { level } else { -level })
            .collect()
    }

    /// The `i`th frame of someone talking: syllables, loud then soft.
    fn talking(i: usize) -> Vec<i16> {
        tone(if (i / 5) % 2 == 0 { 3000 } else { 500 })
    }

    #[test]
    fn cuts_speech_into_commands() {
        let mut cutter = Segmenter::default();
        // A second of speech, then a pause: one command.
        for i in 0..50 {
            cutter.hear(&talking(i));
            assert!(cutter.finished().is_none());
        }
        let mut heard = None;
        // The level is averaged over a few frames, so the pause shows a
        // little late.
        for _ in 0..END_QUIET as usize + SMOOTH_FRAMES {
            cutter.hear(&tone(0));
            heard = heard.or(cutter.finished());
        }
        assert!(heard.is_some_and(|audio| audio.len() >= 40 * FRAME));
        // A cough is too short.
        for _ in 0..5 {
            cutter.hear(&tone(3000));
        }
        for _ in 0..END_QUIET {
            cutter.silence();
            assert!(cutter.finished().is_none());
        }
        // Talking on and on isn't a command.
        for i in 0..(8 * 50) {
            cutter.hear(&talking(i));
        }
        for _ in 0..END_QUIET {
            cutter.silence();
            assert!(cutter.finished().is_none());
        }
    }

    #[test]
    fn hears_a_command_over_music() {
        // A steady sound around them (music, say) and their voice on top:
        // the command ends when they stop, though the music goes on.
        let mut cutter = Segmenter::default();
        let music = || tone(900);
        for _ in 0..50 {
            cutter.hear(&music());
            assert!(cutter.finished().is_none());
        }
        for i in 0..60 {
            let voice = talking(i);
            let mixed: Vec<i16> = voice.iter().zip(music()).map(|(v, m)| v / 2 + m).collect();
            cutter.hear(&mixed);
            assert!(cutter.finished().is_none());
        }
        let mut heard = None;
        for _ in 0..END_QUIET as usize + SMOOTH_FRAMES {
            cutter.hear(&music());
            heard = heard.or(cutter.finished());
        }
        assert!(heard.is_some(), "the command never ended under the music");
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
