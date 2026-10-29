//! Talking back (voice commands, beta): Loski's replies, spoken in the call.
//! The most natural voice is Chatterbox, on an NVIDIA graphics card, in a
//! container of its own next to Loski (`voice/`); anywhere else there's
//! Piper, which comes with Loski's image, with a voice (about 60 MB)
//! downloaded the first time it's picked in /voice. Lines said once are
//! kept, and the usual ones are made ahead, so most replies come at once.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc as std_mpsc,
    },
    time::Duration,
};

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, sync::oneshot};

/// A voice Loski can speak with.
pub struct Voice {
    pub key: &'static str,
    pub label: &'static str,
    pub about: &'static str,
    /// Where it is among Piper's voices on Hugging Face, without ".onnx".
    path: &'static str,
    sha256: &'static str,
    config_sha256: &'static str,
}

pub const VOICES: [Voice; 3] = [
    Voice {
        key: "lessac",
        label: "Lessac",
        about: "American English, clear and bright",
        path: "en/en_US/lessac/medium/en_US-lessac-medium",
        sha256: "5efe09e69902187827af646e1a6e9d269dee769f9877d17b16b1b46eeaaf019f",
        config_sha256: "efe19c417bed055f2d69908248c6ba650fa135bc868b0e6abb3da181dab690a0",
    },
    Voice {
        key: "ryan",
        label: "Ryan",
        about: "American English, deeper",
        path: "en/en_US/ryan/medium/en_US-ryan-medium",
        sha256: "abf4c274862564ed647ba0d2c47f8ee7c9b717d27bdad9219100eb310db4047a",
        config_sha256: "44034c056cb15681b2ad494307c7f3f2e4499d1253c700c711fa0a4607ffe78d",
    },
    Voice {
        key: "alan",
        label: "Alan",
        about: "British English",
        path: "en/en_GB/alan/medium/en_GB-alan-medium",
        sha256: "0a309668932205e762801f1efc2736cd4b0120329622adf62be09e56339d3330",
        config_sha256: "c0f0d124e5895c00e7c03b35dcc8287f319a6998a365b182deb5c8e752ee8c1e",
    },
];

/// Each voice's size, to say before it downloads.
pub const VOICE_MB: u32 = 60;
/// What /voice calls the graphics-card voice.
pub const SERVER_VOICE: &str = "chatterbox";

pub fn voice(key: &str) -> Option<&'static Voice> {
    VOICES.iter().find(|v| v.key == key)
}

/// Piper itself, where Loski's image puts it (`PIPER` says otherwise).
pub fn piper() -> PathBuf {
    std::env::var_os("PIPER").map_or_else(|| PathBuf::from("/opt/piper/piper"), PathBuf::from)
}

/// Whether Piper is here to speak with: it comes with Loski's image, not
/// with a build of your own.
pub fn available() -> bool {
    piper().is_file()
}

fn voice_path(data_dir: &Path, voice: &Voice) -> PathBuf {
    let name = voice.path.rsplit('/').next().unwrap_or(voice.key);
    data_dir.join("voices").join(format!("{name}.onnx"))
}

/// Downloads a voice once (the model and its settings), and checks both are
/// the files they should be.
pub async fn fetch_voice(
    http: &reqwest::Client,
    data_dir: &Path,
    voice: &Voice,
) -> Result<PathBuf> {
    let model = voice_path(data_dir, voice);
    let config = model.with_extension("onnx.json");
    if model.exists() && config.exists() {
        return Ok(model);
    }
    tokio::fs::create_dir_all(model.parent().context("no voices folder")?).await?;
    let base = format!(
        "https://huggingface.co/rhasspy/piper-voices/resolve/main/{}",
        voice.path
    );
    download(
        http,
        &format!("{base}.onnx.json"),
        &config,
        voice.config_sha256,
    )
    .await?;
    download(http, &format!("{base}.onnx"), &model, voice.sha256).await?;
    tracing::info!(
        "Downloaded the {} voice to {}",
        voice.label,
        model.display()
    );
    Ok(model)
}

async fn download(http: &reqwest::Client, url: &str, path: &Path, sha256: &str) -> Result<()> {
    let partial = path.with_extension("part");
    let mut response = http
        .get(url)
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await
        .context("couldn't reach Hugging Face for the voice")?
        .error_for_status()?;
    let mut file = tokio::fs::File::create(&partial).await?;
    let mut hasher = Sha256::new();
    while let Some(chunk) = response.chunk().await? {
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);
    if format!("{:x}", hasher.finalize()) != sha256 {
        let _ = tokio::fs::remove_file(&partial).await;
        bail!("the voice didn't arrive whole (its checksum is off); try again");
    }
    tokio::fs::rename(&partial, path).await?;
    Ok(())
}

/// A line, spoken.
#[derive(Clone)]
pub struct Speech {
    pub samples: Arc<Vec<f32>>,
    /// Samples a second.
    pub rate: u32,
}

/// Lines kept once said; past this many, new ones aren't kept.
const KEPT: usize = 400;
/// Lines that may wait to be said at once; more and the newest is let go.
const WAITING: usize = 2;

/// Loski's voice: one of the two engines, and the lines it has said.
pub struct Talker {
    engine: Engine,
    said: Mutex<HashMap<String, Speech>>,
    waiting: AtomicUsize,
}

enum Engine {
    Piper(PiperTalker),
    Server { http: reqwest::Client, url: String },
}

impl Talker {
    /// Piper with `model`, on this computer.
    pub fn piper(model: &Path, scratch: &Path) -> Result<Self> {
        Ok(Self::new(Engine::Piper(PiperTalker::start(
            model, scratch,
        )?)))
    }

    /// The graphics-card voice, at `url`.
    pub fn server(http: reqwest::Client, url: String) -> Self {
        Self::new(Engine::Server { http, url })
    }

    fn new(engine: Engine) -> Self {
        Self {
            engine,
            said: Mutex::new(HashMap::new()),
            waiting: AtomicUsize::new(0),
        }
    }

    /// `text` spoken. None when lines are already waiting to be said (by
    /// then this one would come too late).
    pub async fn say(&self, text: &str) -> Option<Result<Speech>> {
        if let Some(speech) = self.said.lock().get(text) {
            return Some(Ok(speech.clone()));
        }
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= WAITING {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        let spoken = self.speak(text).await;
        self.waiting.fetch_sub(1, Ordering::SeqCst);
        if let Ok(speech) = &spoken {
            let mut said = self.said.lock();
            if said.len() < KEPT {
                said.insert(text.to_string(), speech.clone());
            }
        }
        Some(spoken)
    }

    /// Makes `lines` ahead, one at a time, so they're said at once later.
    pub async fn prepare(&self, lines: &[String]) -> usize {
        let mut made = 0;
        for line in lines {
            if self.said.lock().contains_key(line) {
                continue;
            }
            match self.speak(line).await {
                Ok(speech) => {
                    self.said.lock().insert(line.clone(), speech);
                    made += 1;
                }
                Err(e) => {
                    tracing::debug!("couldn't make \"{line}\" ahead: {e:#}");
                    break;
                }
            }
        }
        made
    }

    async fn speak(&self, text: &str) -> Result<Speech> {
        match &self.engine {
            Engine::Piper(piper) => piper.say(text).await,
            Engine::Server { http, url } => {
                let response = http
                    .post(format!("{url}/say"))
                    .json(&serde_json::json!({ "text": text }))
                    .timeout(Duration::from_secs(30))
                    .send()
                    .await
                    .context("the graphics-card voice didn't answer")?
                    .error_for_status()
                    .context("the graphics-card voice couldn't say it")?;
                let bytes = response.bytes().await?;
                wav(&bytes).context("the graphics-card voice's reply couldn't be read")
            }
        }
    }
}

/// Where the graphics-card voice is: next to Loski in Docker Compose,
/// unless VOICE_SERVER says otherwise.
pub fn server_url() -> String {
    std::env::var("VOICE_SERVER")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| "http://voice:8800".to_string())
        .trim_end_matches('/')
        .to_string()
}

/// Whether the graphics-card voice is up, with its model loaded.
pub async fn server_ready(http: &reqwest::Client) -> bool {
    http.get(format!("{}/health", server_url()))
        .timeout(Duration::from_secs(2))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

struct Job {
    text: String,
    reply: oneshot::Sender<Result<Speech>>,
}

/// Piper, kept running with a voice loaded, on a thread of its own; lines
/// to say queue there.
struct PiperTalker {
    jobs: std_mpsc::SyncSender<Job>,
}

impl PiperTalker {
    /// Starts Piper with `model`. Replies are written to files in
    /// `scratch` and read back as they're done.
    fn start(model: &Path, scratch: &Path) -> Result<Self> {
        std::fs::create_dir_all(scratch)?;
        let mut piper = Piper::start(model)?;
        let (jobs, queue) = std_mpsc::sync_channel::<Job>(WAITING);
        let (model, scratch) = (model.to_path_buf(), scratch.to_path_buf());
        std::thread::Builder::new()
            .name("talk".into())
            .spawn(move || {
                for (n, job) in queue.into_iter().enumerate() {
                    let out = scratch.join(format!("{}.wav", n % 4));
                    let mut said = piper.say(&job.text, &out);
                    if said.is_err() {
                        // Piper stopped; one fresh start, then a second try.
                        if let Ok(fresh) = Piper::start(&model) {
                            piper = fresh;
                            said = piper.say(&job.text, &out);
                        }
                    }
                    let _ = job.reply.send(said);
                }
            })?;
        Ok(Self { jobs })
    }

    async fn say(&self, text: &str) -> Result<Speech> {
        let (reply, answer) = oneshot::channel();
        self.jobs
            .try_send(Job {
                text: text.to_string(),
                reply,
            })
            .ok()
            .context("Piper is busy")?;
        answer.await.context("Piper stopped")?
    }
}

/// A running Piper: a line of JSON in, the path of the finished WAV out.
struct Piper {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Piper {
    fn start(model: &Path) -> Result<Self> {
        let mut child = Command::new(piper())
            .arg("--model")
            .arg(model)
            .args(["--json-input", "--quiet", "--sentence_silence", "0"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("couldn't start Piper")?;
        let stdin = child.stdin.take().context("no way to talk to Piper")?;
        let stdout = BufReader::new(child.stdout.take().context("no way to hear Piper")?);
        Ok(Self {
            child,
            stdin,
            stdout,
        })
    }

    fn say(&mut self, text: &str, out: &Path) -> Result<Speech> {
        let line = serde_json::json!({ "text": text, "output_file": out });
        writeln!(self.stdin, "{line}").context("Piper stopped")?;
        self.stdin.flush().context("Piper stopped")?;
        let mut done = String::new();
        if self.stdout.read_line(&mut done)? == 0 {
            bail!("Piper stopped");
        }
        let bytes = std::fs::read(out).context("Piper's reply went missing")?;
        let _ = std::fs::remove_file(out);
        wav(&bytes).context("Piper's reply couldn't be read")
    }
}

impl Drop for Piper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A 16-bit mono WAV, as both voices write them.
fn wav(bytes: &[u8]) -> Option<Speech> {
    let mut rate = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().ok()?) as usize;
        let body = bytes.get(at + 8..(at + 8 + size).min(bytes.len()))?;
        match &bytes[at..at + 4] {
            b"fmt " => rate = Some(u32::from_le_bytes(body.get(4..8)?.try_into().ok()?)),
            b"data" => {
                let samples = body
                    .chunks_exact(2)
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0)
                    .collect();
                return Some(Speech {
                    samples: Arc::new(samples),
                    rate: rate?,
                });
            }
            _ => {}
        }
        at += 8 + size + (size & 1);
    }
    None
}

/// A song's title as it's said: without "(feat. …)", " - Remastered 2011"
/// and the like, which sound clumsy read out.
pub fn spoken_title(title: &str) -> &str {
    let cut = [" (", " - ", " [", " feat."]
        .iter()
        .filter_map(|marker| title.find(marker))
        .filter(|&at| at > 0)
        .min()
        .unwrap_or(title.len());
    title[..cut].trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_wav() {
        let rate = 24_000u32;
        let mut bytes = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        bytes.extend(16u32.to_le_bytes());
        bytes.extend([1, 0, 1, 0]);
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * 2).to_le_bytes());
        bytes.extend([2, 0, 16, 0]);
        bytes.extend(b"data");
        bytes.extend(4u32.to_le_bytes());
        bytes.extend(16384i16.to_le_bytes());
        bytes.extend((-32768i16).to_le_bytes());
        let speech = wav(&bytes).unwrap();
        assert_eq!((*speech.samples).clone(), vec![0.5, -1.0]);
        assert_eq!(speech.rate, rate);
        assert!(wav(b"RIFF").is_none());
    }

    #[test]
    fn finds_the_voice_server() {
        // Next to Loski in Docker Compose, unless told otherwise.
        if std::env::var_os("VOICE_SERVER").is_none() {
            assert_eq!(server_url(), "http://voice:8800");
        }
    }

    #[test]
    fn says_titles_without_the_extras() {
        assert_eq!(spoken_title("Numb"), "Numb");
        assert_eq!(
            spoken_title("Bohemian Rhapsody - Remastered 2011"),
            "Bohemian Rhapsody"
        );
        assert_eq!(spoken_title("Señorita (feat. Someone)"), "Señorita");
        assert_eq!(
            spoken_title("(What's The Story) Morning Glory?"),
            "(What's The Story) Morning Glory?"
        );
    }

    #[test]
    fn voices_are_known_by_key() {
        assert!(VOICES.iter().all(|v| voice(v.key).is_some()));
        assert!(voice("nobody").is_none());
    }
}
