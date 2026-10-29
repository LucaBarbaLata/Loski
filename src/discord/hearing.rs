//! Voice commands (beta): "Loski, skip" in the call. With voice commands on
//! (you turn them on in /voice) I stop deafening myself in calls, but I only
//! listen to people who said yes in /voice. Short bits of what they say are
//! turned into text on this computer, acted on if they start with my name,
//! and forgotten. While this is a beta, what was said to me goes in the log,
//! so the words I listen for can be tuned; other talk is only counted. With
//! talking back on, I answer out loud too (see `talk.rs`).

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use parking_lot::Mutex;
use songbird::{
    Call, CoreEvent, Event, EventContext, EventHandler,
    driver::{Channels, DecodeConfig, DecodeMode, SampleRate},
    events::context_data::VoiceTick,
};
use tokio::sync::mpsc;
use twilight_interactions::command::{CommandModel, CreateCommand};
use twilight_model::{
    channel::message::{Component, component::ButtonStyle},
    id::Id,
};

use super::{
    App, Core, LONG_SECS, SHORT_SECS,
    cards::{self, Btn, color},
    commands::{self, Placement},
    friends::{self, Access},
    icons::{Icon, Icons},
    interact::Ix,
    library, oops, requests, voice,
};
use crate::{
    engine::Repeat,
    personality,
    speech::{self, RATE, Recognizer, RepeatWhat, Segmenter, Spoken},
    spotify::auth::now_unix,
    talk::{self, Talker},
    util::{escape_md, truncate},
};

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "voice",
    desc = "Voice commands (beta): say \"Loski, skip\" in the call"
)]
pub struct VoiceCmd;

/// What I can hear: the model, who said yes, and each person's speech as
/// it comes in.
pub struct Ears {
    recognizer: Mutex<Option<Arc<Recognizer>>>,
    /// People who said yes in /voice (Discord user ids).
    people: Mutex<HashSet<u64>>,
    /// Each voice stream in the call (Discord numbers them), and whose it is.
    streams: Mutex<HashMap<u32, Stream>>,
    heard: mpsc::Sender<Utterance>,
    /// Voice commands are being turned on: the model is on its way.
    pub busy: AtomicBool,
    /// What came through from the call lately, for the log during the beta.
    check: Mutex<Check>,
    /// What I heard lately, newest first, for "Last heard" on /voice.
    notes: Mutex<VecDeque<Note>>,
    /// Talking back: Piper with a voice loaded, when it's on.
    talker: Mutex<Option<Arc<Talker>>>,
    /// A voice for talking back is on its way.
    pub talk_busy: AtomicBool,
    /// The graphics-card voice is up (checked every so often).
    server_up: AtomicBool,
    /// The artists you play most, for the speech model, and when they were
    /// counted.
    artists: Mutex<Option<(Instant, Vec<String>)>>,
    /// How long the model took for the last few bits of speech (ms), newest
    /// first: how quick it is on this computer.
    speed: Mutex<VecDeque<u64>>,
}

/// Something I heard, kept in memory for "Last heard" on /voice.
#[derive(Clone)]
pub struct Note {
    /// When (Unix seconds).
    pub at: i64,
    pub user: u64,
    pub who: String,
    pub text: String,
    /// What came of it: "skipped.", "not for me"…
    pub outcome: String,
    pub took_ms: u64,
    /// Said to me (my name first). Other talk is only kept a short while,
    /// and only shown to whoever said it.
    pub for_me: bool,
}

/// How many notes are kept.
const NOTES_KEPT: usize = 20;
/// How long talk that wasn't for me is kept, for its speaker to check.
const TALK_KEPT_SECS: i64 = 120;
/// How many notes the card shows.
const NOTES_SHOWN: usize = 5;
/// How many of the model's times go into how quick it is here.
const SPEED_KEPT: usize = 20;
/// Slower than this, a command keeps you waiting: /voice suggests the Quick
/// speech model.
const SLOW_MS: u64 = 1500;

/// Counts of what songbird handed over since the last report.
#[derive(Default)]
struct Check {
    since: Option<Instant>,
    /// A report went out since the call was set up.
    reported: bool,
    ticks: u32,
    /// 20 ms frames of someone's voice.
    frames: u32,
    /// Of those, frames songbird could turn into sound.
    decoded: u32,
    /// Of those, frames from a voice Discord hasn't said is whose.
    unplaced: u32,
}

/// How often the beta's report goes in the log.
const CHECK_EVERY: Duration = Duration::from_secs(15);

/// A finished bit of speech from someone who said yes.
pub struct Utterance {
    user: u64,
    audio: Vec<i16>,
}

/// One voice in the call: whose it is, and their speech being cut up.
#[derive(Default)]
struct Stream {
    user: Option<u64>,
    speech: Segmenter,
    /// Their voice has come through at least once (said in the log).
    came_through: bool,
}

impl Ears {
    pub fn new(people: &[u64]) -> (Self, mpsc::Receiver<Utterance>) {
        let (heard, receiver) = mpsc::channel(8);
        (
            Self {
                recognizer: Mutex::new(None),
                people: Mutex::new(people.iter().copied().collect()),
                streams: Mutex::new(HashMap::new()),
                heard,
                busy: AtomicBool::new(false),
                check: Mutex::new(Check::default()),
                notes: Mutex::new(VecDeque::new()),
                talker: Mutex::new(None),
                talk_busy: AtomicBool::new(false),
                server_up: AtomicBool::new(false),
                artists: Mutex::new(None),
                speed: Mutex::new(VecDeque::new()),
            },
            receiver,
        )
    }

    /// Voice commands are on and the model is ready.
    pub fn listening(&self) -> bool {
        self.recognizer.lock().is_some()
    }

    fn recognizer(&self) -> Option<Arc<Recognizer>> {
        self.recognizer.lock().clone()
    }

    fn talker(&self) -> Option<Arc<Talker>> {
        self.talker.lock().clone()
    }

    /// Listening with `recognizer` from now on; `tried` is how long its
    /// first try took.
    fn use_recognizer(&self, recognizer: Recognizer, tried: Duration) {
        *self.recognizer.lock() = Some(Arc::new(recognizer));
        self.speed.lock().clear();
        self.timed(tried);
    }

    /// How long the model took for a bit of speech.
    fn timed(&self, took: Duration) {
        let mut speed = self.speed.lock();
        speed.push_front(took.as_millis() as u64);
        speed.truncate(SPEED_KEPT);
    }

    /// How long understanding a command usually takes here: the middle of
    /// the last few times.
    fn typical_ms(&self) -> Option<u64> {
        let mut times: Vec<u64> = self.speed.lock().iter().copied().collect();
        times.sort_unstable();
        times.get(times.len() / 2).copied()
    }

    fn set_people(&self, people: &[u64]) {
        *self.people.lock() = people.iter().copied().collect();
        // Anything half-heard from someone who just said no goes.
        self.streams
            .lock()
            .values_mut()
            .for_each(|s| s.speech.forget());
    }

    /// One 20 ms tick of the call: each person's audio, or their silence.
    fn tick(&self, tick: &VoiceTick) {
        self.count(tick);
        if !self.listening() {
            return;
        }
        let people = self.people.lock().clone();
        let mut finished = Vec::new();
        {
            let mut streams = self.streams.lock();
            for (ssrc, data) in &tick.speaking {
                let Some(stream) = streams.get_mut(ssrc) else {
                    continue;
                };
                match (stream.user, &data.decoded_voice) {
                    (Some(user), Some(pcm)) if people.contains(&user) => {
                        if !stream.came_through {
                            stream.came_through = true;
                            tracing::info!("Voice (beta): I can hear {user}");
                        }
                        stream.speech.hear(pcm);
                    }
                    // Never keep anything from someone who didn't say yes.
                    _ => stream.speech.forget(),
                }
            }
            for ssrc in &tick.silent {
                if let Some(stream) = streams.get_mut(ssrc) {
                    stream.speech.silence();
                }
            }
            for stream in streams.values_mut() {
                if let (Some(user), Some(audio)) = (stream.user, stream.speech.finished()) {
                    finished.push(Utterance { user, audio });
                }
            }
        }
        for utterance in finished {
            // Full means people are talking over each other: let it go.
            let _ = self.heard.try_send(utterance);
        }
    }

    fn note(&self, note: Note) {
        let mut notes = self.notes.lock();
        notes.push_front(note);
        let now = now_unix();
        notes.retain(|n| n.for_me || now - n.at <= TALK_KEPT_SECS);
        notes.truncate(NOTES_KEPT);
    }

    /// What `viewer` may see of what I heard lately: what was said to me
    /// (everyone's, for the owner), and their own other talk, briefly.
    pub fn last_heard(&self, viewer: u64, owner: bool) -> Vec<Note> {
        let now = now_unix();
        self.notes
            .lock()
            .iter()
            .filter(|n| {
                if n.for_me {
                    owner || n.user == viewer
                } else {
                    n.user == viewer && now - n.at <= TALK_KEPT_SECS
                }
            })
            .take(NOTES_SHOWN)
            .cloned()
            .collect()
    }

    fn who_is(&self, ssrc: u32, user: u64) {
        let mut streams = self.streams.lock();
        let stream = streams.entry(ssrc).or_default();
        if stream.user != Some(user) {
            tracing::info!("Voice (beta): voice {ssrc} is {user}");
        }
        stream.user = Some(user);
    }

    /// Beta: says in the log what the call hands over, once after joining
    /// and again whenever there are voices it can't tell whose they are, to
    /// see where a command gets lost.
    fn count(&self, tick: &VoiceTick) {
        let placed: HashSet<u32> = self
            .streams
            .lock()
            .iter()
            .filter(|(_, s)| s.user.is_some())
            .map(|(ssrc, _)| *ssrc)
            .collect();
        let mut check = self.check.lock();
        let now = Instant::now();
        let since = *check.since.get_or_insert(now);
        check.ticks += 1;
        for (ssrc, data) in &tick.speaking {
            check.frames += 1;
            check.decoded += u32::from(data.decoded_voice.is_some());
            check.unplaced += u32::from(!placed.contains(ssrc));
        }
        if now - since < CHECK_EVERY {
            return;
        }
        if !check.reported || check.unplaced > 0 {
            tracing::info!(
                "Voice (beta) check, last {} s: {} ticks, {} frames of voice ({} decoded, {} from voices I can't place), {} voices placed",
                (now - since).as_secs(),
                check.ticks,
                check.frames,
                check.decoded,
                check.unplaced,
                placed.len(),
            );
        }
        *check = Check {
            since: Some(now),
            reported: true,
            ..Check::default()
        };
    }
}

/// songbird's side: who each voice stream belongs to, and each tick's audio.
struct Listening {
    ears: Arc<Ears>,
}

#[async_trait::async_trait]
impl EventHandler for Listening {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(speaking) => {
                if let Some(user) = speaking.user_id {
                    self.ears.who_is(speaking.ssrc, user.0);
                }
            }
            EventContext::VoiceTick(tick) => self.ears.tick(tick),
            _ => {}
        }
        None
    }
}

/// How every call is joined: decoding people's voices at the model's rate.
/// songbird only switches decoding on as a call connects, never later, so
/// it's always on; while Loski is deafened nothing arrives to decode.
pub fn call_config() -> songbird::Config {
    let decode = DecodeConfig::new(Channels::Mono, SampleRate::Hz16000);
    songbird::Config::default().decode_mode(DecodeMode::Decode(decode))
}

/// The call's ears: whose voice is whose, noted whether or not voice
/// commands are on, and with them on, what people say. Discord says whose
/// voice is whose once, the moment the call connects, so these go in
/// before joining (with the call's other events cleared).
pub fn add_events(call: &mut Call, ears: &Arc<Ears>) {
    call.add_global_event(
        Event::Core(CoreEvent::SpeakingStateUpdate),
        Listening { ears: ears.clone() },
    );
    if ears.listening() {
        call.add_global_event(
            Event::Core(CoreEvent::VoiceTick),
            Listening { ears: ears.clone() },
        );
    }
}

/// Undeafened while voice commands are on, deafened otherwise. Once the
/// call is connected.
pub async fn set_deaf(call: &mut Call, ears: &Ears) {
    let listening = ears.listening();
    if call.is_deaf() == listening {
        let _ = call.deafen(!listening).await;
    }
    *ears.check.lock() = Check::default();
    if listening {
        tracing::info!(
            "Voice (beta): listening in the call ({} deafened)",
            if call.is_deaf() { "still" } else { "not" }
        );
    }
}

/// Loads the model when voice commands were on before a restart.
pub async fn wake(app: Arc<App>) {
    if !app.prefs().voice || !speech::cpu_supported() {
        return;
    }
    let model = speech::model(app.prefs().voice_model.as_deref());
    let path = model.path(&app.cfg.data_dir);
    if !path.exists() {
        return;
    }
    match tokio::task::spawn_blocking(move || Recognizer::start(&path)).await {
        Ok(Ok((recognizer, tried))) => {
            app.ears.use_recognizer(recognizer, tried);
            tracing::info!(
                "Voice commands (beta) are on, with the {} speech model ({} ms to try)",
                model.label,
                tried.as_millis()
            );
            voice::hearing_changed(&app).await;
        }
        Ok(Err(e)) => tracing::warn!("Voice commands are off: {e:#}"),
        Err(e) => tracing::warn!("Voice commands are off: {e}"),
    }
    let prefs = app.prefs();
    let Some(voice) = prefs.talk_voice.as_deref().and_then(talk::voice) else {
        return;
    };
    if !prefs.talk || !talk::available() {
        return;
    }
    match talk::fetch_voice(&app.web, &app.cfg.data_dir, voice).await {
        Ok(model) => {
            match tokio::task::spawn_blocking(move || Talker::piper(&model, &scratch())).await {
                Ok(Ok(talker)) => {
                    use_talker(&app, talker);
                    tracing::info!("Talking back (beta) is on, as {}", voice.label);
                }
                Ok(Err(e)) => tracing::warn!("Talking back is off: {e:#}"),
                Err(e) => tracing::warn!("Talking back is off: {e}"),
            }
        }
        Err(e) => tracing::warn!("Talking back is off: {e:#}"),
    }
}

/// Keeps an eye on the graphics-card voice: it takes a minute or two to
/// load, and comes and goes with its container. When it's picked for
/// talking back, it's put to use as soon as it's there.
pub async fn keep_talking(app: Arc<App>) {
    loop {
        let up = talk::server_ready(&app.web).await;
        if up != app.ears.server_up.swap(up, Ordering::Relaxed) {
            if up {
                tracing::info!("The graphics-card voice is ready");
            } else {
                tracing::info!("The graphics-card voice went away");
            }
        }
        let prefs = app.prefs();
        if up
            && prefs.talk
            && prefs.talk_voice.as_deref() == Some(talk::SERVER_VOICE)
            && app.ears.talker().is_none()
        {
            use_talker(&app, Talker::server(app.web.clone(), talk::server_url()));
            tracing::info!("Talking back (beta) is on, with the graphics-card voice");
        }
        tokio::time::sleep(Duration::from_secs(if up { 60 } else { 15 })).await;
    }
}

/// Talking back with `talker` from now on, the usual lines made ahead in
/// the background so they're said at once.
fn use_talker(app: &App, talker: Talker) {
    let talker = Arc::new(talker);
    *app.ears.talker.lock() = Some(talker.clone());
    let lines = personality::spoken_lines(&app.name());
    tokio::spawn(async move {
        let started = Instant::now();
        let made = talker.prepare(&lines).await;
        tracing::info!(
            "Talking back: {made} lines made ahead, in {:.0} s",
            started.elapsed().as_secs_f32()
        );
    });
}

/// Where Piper's replies are written on their way into the call.
fn scratch() -> std::path::PathBuf {
    std::env::temp_dir().join("loski-talk")
}

/// Says `line` in the call when talking back is on, and how long that takes.
async fn speak(app: &App, line: &str) -> Option<Duration> {
    let talker = app.ears.talker()?;
    let core = app.core.get()?;
    match talker.say(line).await? {
        Ok(speech) => Some(core.engine.say(&speech.samples, speech.rate)),
        Err(e) => {
            tracing::warn!("Talking back: {e:#}");
            None
        }
    }
}

/// Says `line` without holding up what I hear next.
fn speak_soon(app: &Arc<App>, line: String) {
    let app = app.clone();
    tokio::spawn(async move {
        speak(&app, &line).await;
    });
}

/// The names I answer to: my own first, then any others picked in /voice.
fn names(app: &App) -> Vec<String> {
    let mut names = vec![app.name()];
    names.extend(app.prefs().voice_names);
    names
}

/// How often the artists you play most are counted again.
const ARTISTS_EVERY: Duration = Duration::from_secs(600);
/// Artists named in the hints: enough for the ones you play most.
const HINT_ARTISTS: usize = 12;

/// Hints for the speech model: my names, and with `artists`, the artists
/// you play most, so "Linkin Park" comes out the way Spotify spells it.
/// Artists are only hinted at for a second listen to a song asked for: on
/// the bench they cost short commands ("Loski, stop") more than they gave.
fn hints(app: &App, artists: bool) -> String {
    let names = names(app);
    if !artists {
        return speech::prompt(&names, &[]);
    }
    if let Some((at, artists)) = app.ears.artists.lock().as_ref()
        && at.elapsed() < ARTISTS_EVERY
    {
        return speech::prompt(&names, artists);
    }
    let mut plays: HashMap<String, usize> = HashMap::new();
    for entry in app.store.recent_history(500) {
        if let Some(artist) = entry.artist_list.into_iter().next() {
            *plays.entry(artist).or_default() += 1;
        }
    }
    let mut artists: Vec<(String, usize)> = plays.into_iter().collect();
    artists.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let artists: Vec<String> = artists
        .into_iter()
        .take(HINT_ARTISTS)
        .map(|(artist, _)| artist)
        .collect();
    let hints = speech::prompt(&names, &artists);
    *app.ears.artists.lock() = Some((Instant::now(), artists));
    hints
}

/// How long after someone says just my name the next thing they say counts
/// as a command, without the name.
const FOLLOW_UP: Duration = Duration::from_secs(6);
/// A song asked for gets a second listen (see `run`) only when the first
/// was quicker than this: on a slow computer it would double the wait.
const SECOND_LISTEN_UNDER: Duration = Duration::from_secs(1);

/// Turns what people said into text, and acts on what's for me.
pub async fn run(app: Arc<App>, mut heard: mpsc::Receiver<Utterance>) {
    // Who said just my name, and when: the next thing they say is for me.
    let mut called: HashMap<u64, Instant> = HashMap::new();
    while let Some(Utterance { user, audio }) = heard.recv().await {
        let Some(recognizer) = app.ears.recognizer() else {
            continue;
        };
        let names = names(&app);
        let name = names[0].clone();
        let seconds = audio.len() as f32 / RATE as f32;
        let samples: Vec<f32> = audio.iter().map(|&s| f32::from(s) / 32768.0).collect();
        let mut heard = match recognizer.hear(samples.clone(), hints(&app, false)).await {
            Some(Ok(heard)) => heard,
            Some(Err(e)) => {
                tracing::warn!("Voice commands: {e:#}");
                continue;
            }
            None => {
                tracing::debug!("Voice commands: too much at once, let a bit go");
                continue;
            }
        };
        app.ears.timed(heard.took);
        let who = friends::listeners(&app)
            .into_iter()
            .find(|l| l.id == user)
            .map(|l| l.name)
            .or_else(|| app.room.name_of(user))
            .unwrap_or_else(|| "Someone".to_string());
        let mut meaning = speech::understand_any(&heard.text, &names);
        // A song or playlist asked for: a second listen, knowing your
        // artists, so their names are spelled the way Spotify has them.
        if let Some(Ok(asked)) = &meaning
            && asked.names_something()
            && heard.took < SECOND_LISTEN_UNDER
            && let Some(Ok(again)) = recognizer.hear(samples, hints(&app, true)).await
            && let Some(Ok(better)) = speech::understand_any(&again.text, &names)
            && better.names_something()
        {
            meaning = Some(Ok(better));
            heard = speech::Heard {
                took: heard.took + again.took,
                ..again
            };
        }
        let following_up = called
            .remove(&user)
            .is_some_and(|at| at.elapsed() <= FOLLOW_UP + Duration::from_secs_f32(seconds));
        if following_up && meaning.is_none() {
            meaning = Some(speech::command(&heard.text).ok_or_else(|| heard.text.clone()));
        }
        // Humming or a cough written down as my name comes out unsure: only
        // what was clearly said to me gets an answer.
        let unsure = !speech::sure_enough(&meaning, heard.confidence);
        if unsure {
            meaning = None;
        }
        let sure = heard.confidence;
        // While this is a beta: what was said to me, how quickly I got it,
        // and what it meant. Other talk isn't written down.
        let took = heard.took.as_millis();
        match &meaning {
            None if unsure => tracing::info!(
                "Voice (beta): {who}, {seconds:.1} s, {took} ms, sure {sure:.2} → too unsure to answer"
            ),
            None => tracing::info!("Voice (beta): {who}, {seconds:.1} s, {took} ms → not for me"),
            Some(result) => tracing::info!(
                "Voice (beta): {who}, {seconds:.1} s, {took} ms, sure {sure:.2}: \"{}\" → {}",
                heard.text,
                match result {
                    Ok(command) => format!("{command:?}"),
                    Err(rest) if rest.is_empty() => "just my name".to_string(),
                    Err(_) => "not a command I know".to_string(),
                }
            ),
        }
        let for_me = meaning.is_some();
        // A song played from what was heard can be swapped for another.
        let mut pick = None;
        let (outcome, said) = match meaning {
            None => ("not for me".to_string(), None),
            Some(Ok(command)) => match act(&app, user, &who, command).await {
                Ok(reply) => {
                    let said = format!(
                        "{} **{}**: {}",
                        app.icons.md(Icon::Music),
                        escape_md(&who),
                        reply.chat
                    );
                    if let Some(line) = reply.say {
                        speak_soon(&app, line);
                    }
                    pick = reply.pick;
                    (reply.chat, Some(said))
                }
                Err(e) => {
                    let said = format!(
                        "{} **{}**: {e}",
                        app.icons.md(Icon::Problem),
                        escape_md(&who)
                    );
                    speak_soon(&app, personality::say_failed(&e.to_string()).to_string());
                    (e.to_string(), Some(said))
                }
            },
            // Just my name: I'm listening for the rest.
            Some(Err(rest)) if rest.is_empty() && !following_up => {
                called.insert(user, Instant::now());
                speak_soon(&app, personality::say_yes().to_string());
                let said = format!(
                    "{} **{}**: yes? Say what you'd like: skip, pause, play a song…",
                    app.icons.md(Icon::Info),
                    escape_md(&who),
                );
                ("listening for the rest".to_string(), Some(said))
            }
            // Talking about me at length ("Loski is honestly the best bot"):
            // nothing to answer, though it's in "Last heard".
            Some(Err(rest)) if rest.split_whitespace().count() > 4 => {
                ("talking about me".to_string(), None)
            }
            // Said to me, but not something I know: say what I heard, so
            // it's clear what to say instead.
            Some(Err(rest)) => {
                let heard = if rest.is_empty() {
                    "my name".to_string()
                } else {
                    format!("\"{}\"", escape_md(&truncate(&rest, 80)))
                };
                let said = format!(
                    "{} **{}**: I heard {heard}, but that's not a command I know. Try \"{name}, pause\", \"{name}, skip\" or \"{name}, play\" and a song.",
                    app.icons.md(Icon::Info),
                    escape_md(&who),
                    name = escape_md(&name),
                );
                speak_soon(&app, personality::say_missed().to_string());
                ("not a command I know".to_string(), Some(said))
            }
        };
        app.ears.note(Note {
            at: now_unix(),
            user,
            who: who.clone(),
            text: heard.text.clone(),
            outcome,
            took_ms: took as u64,
            for_me,
        });
        let Some(said) = said else {
            continue;
        };
        let mut parts = vec![cards::text(said)];
        if let Some(query) = &pick {
            let query: String = query.chars().take(80).collect();
            parts.push(cards::row(vec![
                Btn::icon(&format!("loski:vpick:{query}"), Icon::Search)
                    .with_label("Not this one?")
                    .build(&app.icons),
            ]));
        }
        // With a button on it, it stays long enough to be pressed.
        let secs = if pick.is_some() {
            LONG_SECS
        } else {
            SHORT_SECS
        };
        if let Some(home) = app.ui.home() {
            app.announce(home, cards::card(None, parts), secs).await;
        }
    }
}

/// What someone in the call may do, as with buttons.
fn access_of(app: &App, user: u64) -> Access {
    if user == app.owner_id().get() {
        return Access::Owner;
    }
    let prefs = app.prefs();
    let roles = match (app.voice.guild(), Id::new_checked(user)) {
        (Some(guild), Some(id)) => app
            .cache
            .member(guild, id)
            .map(|m| m.roles().to_vec())
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    if friends::is_co_dj(&prefs, user, &roles) {
        friends::co_dj_access(&prefs)
    } else {
        Access::Listener
    }
}

/// What came of a command: a line for the chat, one to say out loud when
/// talking back is on (`None` once it's been said), and for a song picked
/// from what was heard, what was searched for.
struct Reply {
    chat: String,
    say: Option<String>,
    pick: Option<String>,
}

impl Reply {
    fn new(chat: impl Into<String>, say: impl Into<String>) -> Self {
        Self {
            chat: chat.into(),
            say: Some(say.into()),
            pick: None,
        }
    }
}

/// The call I'm in, for anything that queues songs.
fn in_call(app: &App) -> Result<commands::Target> {
    match (app.voice.guild(), app.voice.channel()) {
        (Some(guild), Some(channel)) => Ok((guild, channel)),
        _ => Err(oops("I'm not in a call.")),
    }
}

/// A song as it's said: its name without the extras, and who it's by.
fn spoken_song(track: &crate::track::TrackInfo) -> String {
    format!(
        "{}, by {}",
        talk::spoken_title(&track.title),
        track.artists.first().map_or("", String::as_str)
    )
}

/// "30 seconds", "a minute", "2 minutes".
fn stretch(seconds: u32) -> String {
    match seconds {
        60 => "a minute".to_string(),
        s if s % 60 == 0 => format!("{} minutes", s / 60),
        1 => "a second".to_string(),
        s => format!("{s} seconds"),
    }
}

/// Does what was asked, and says what happened.
async fn act(app: &Arc<App>, user: u64, who: &str, command: Spoken) -> Result<Reply> {
    let core = app.core()?;
    let engine = &core.engine;
    let access = access_of(app, user);
    let needs = match command {
        // Anyone in the call likes songs, votes to skip, asks what's on, and
        // asks for songs (as requests, for someone who may only listen).
        Spoken::Like
        | Spoken::Skip
        | Spoken::WhatsPlaying
        | Spoken::Play(_)
        | Spoken::PlayNext(_)
        | Spoken::Add(_) => Access::Listener,
        // Your own Spotify library.
        Spoken::Liked | Spoken::Playlist(_) => Access::Owner,
        _ => Access::Full,
    };
    if access < needs {
        return Err(oops("that one's for whoever runs me."));
    }
    Ok(match command {
        Spoken::Pause => {
            engine.pause();
            Reply::new("paused.", personality::say_paused())
        }
        Spoken::Resume => {
            engine.resume();
            Reply::new("carrying on.", personality::say_resumed())
        }
        Spoken::Skip if access >= Access::Full => {
            engine.skip(1);
            Reply::new("skipped.", personality::say_skipped())
        }
        Spoken::Skip => {
            let track = engine
                .current_track()
                .ok_or_else(|| oops("nothing's playing."))?;
            let (votes, needed) = friends::vote_skip(app, &track, user)?;
            if votes >= needed {
                engine.skip(1);
                Reply::new(
                    format!("the call voted to skip ({votes} of {needed})."),
                    personality::VOTED_SKIP,
                )
            } else {
                Reply::new(
                    format!("a vote to skip ({votes} of {needed})."),
                    format!("Vote counted: {votes} of {needed}."),
                )
            }
        }
        Spoken::Previous => {
            engine.previous();
            Reply::new("back one.", personality::say_back())
        }
        Spoken::Leave => {
            // Goodbye first, while I'm still in the call to say it.
            if let Some(took) = speak(app, personality::say_bye()).await {
                tokio::time::sleep(took).await;
            }
            voice::send_away(app, None).await;
            Reply {
                chat: "bye!".into(),
                say: None,
                pick: None,
            }
        }
        Spoken::Louder | Spoken::Quieter | Spoken::Volume(_) => {
            let level = match command {
                Spoken::Louder => (engine.volume() + 10).min(100),
                Spoken::Quieter => engine.volume().saturating_sub(10),
                Spoken::Volume(level) => level,
                _ => engine.volume(),
            };
            engine.set_volume(level);
            Reply::new(format!("volume {level}."), personality::say_volume(level))
        }
        Spoken::Shuffle => {
            engine.shuffle();
            Reply::new("shuffled.", personality::say_shuffled())
        }
        Spoken::Like => {
            let track = engine
                .current_track()
                .ok_or_else(|| oops("nothing's playing."))?;
            let name = app.room.name_of(user).unwrap_or_default();
            if friends::toggle_like(app, &track, user, &name)? {
                Reply::new(
                    format!("♥ {}.", escape_md(&track.title)),
                    personality::say_liked(),
                )
            } else {
                Reply::new("like taken back.", personality::UNLIKED)
            }
        }
        Spoken::WhatsPlaying => {
            let track = engine
                .current_track()
                .ok_or_else(|| oops("nothing's playing."))?;
            Reply::new(
                format!(
                    "it's **{}** by {}.",
                    escape_md(&track.title),
                    escape_md(&track.artists_joined())
                ),
                format!("This is {}.", spoken_song(&track)),
            )
        }
        song @ (Spoken::Play(_) | Spoken::PlayNext(_) | Spoken::Add(_)) => {
            play(app, &core, access, user, who, song).await?
        }
        Spoken::Liked => {
            let target = in_call(app)?;
            let liked = core
                .spotify
                .liked_songs(true)
                .await
                .map_err(|e| oops(format!("Hmm, {e}")))?;
            commands::enqueue(
                app,
                &core,
                target,
                liked.tracks,
                Placement::Now,
                liked.source,
                None,
            )
            .await?;
            Reply::new(
                "playing your liked songs, shuffled.",
                personality::LIKED_SONGS,
            )
        }
        Spoken::Playlist(name) => {
            let target = in_call(app)?;
            let (name, playlist) = library::my_playlist(&core, &name).await?;
            commands::enqueue(
                app,
                &core,
                target,
                playlist.tracks,
                Placement::Now,
                playlist.source,
                None,
            )
            .await?;
            Reply::new(
                format!("playing your playlist **{}**.", escape_md(&name)),
                format!("Playing {name}."),
            )
        }
        Spoken::Radio => {
            let count = library::radio_from_current(&core).await?;
            Reply::new(
                format!("a radio from this song: {count} songs coming up."),
                personality::RADIO,
            )
        }
        Spoken::Repeat(what) => {
            let (repeat, chat, say) = match what {
                RepeatWhat::Song => (
                    Repeat::Track,
                    "repeating this song.",
                    personality::REPEAT_SONG,
                ),
                RepeatWhat::Queue => (
                    Repeat::Queue,
                    "repeating the queue.",
                    personality::REPEAT_QUEUE,
                ),
                RepeatWhat::Off => (Repeat::Off, "repeat off.", personality::REPEAT_OFF),
            };
            engine.set_repeat(repeat);
            Reply::new(chat, say)
        }
        Spoken::Seek(seconds) => {
            let track = engine
                .current_track()
                .ok_or_else(|| oops("nothing's playing."))?;
            let at = i64::from(engine.position_ms().unwrap_or(0));
            let last = i64::from(track.duration_ms.saturating_sub(1_000));
            engine.seek((at + i64::from(seconds) * 1_000).clamp(0, last) as u32);
            let how_far = stretch(seconds.unsigned_abs());
            if seconds >= 0 {
                Reply::new(format!("forward {how_far}."), format!("Forward {how_far}."))
            } else {
                Reply::new(format!("back {how_far}."), format!("Back {how_far}."))
            }
        }
        Spoken::Sleep(Some(minutes)) => {
            engine.sleep_in(Duration::from_secs(u64::from(minutes) * 60));
            let end = now_unix() + i64::from(minutes) * 60;
            Reply::new(
                format!("🌙 I'll fade out and leave <t:{end}:R>."),
                format!("Okay. I'll stop in {}.", stretch(minutes * 60)),
            )
        }
        Spoken::Sleep(None) => {
            if !engine.sleep_after_song() {
                return Err(oops("nothing's playing."));
            }
            Reply::new(
                "🌙 I'll leave when this song ends.",
                personality::AFTER_SONG,
            )
        }
        Spoken::SleepOff => {
            if !engine.cancel_sleep() {
                return Err(oops("there's no sleep timer."));
            }
            Reply::new("sleep timer off.", personality::SLEEP_OFF)
        }
    })
}

/// "Play …", "play … next", "add …": the best match on Spotify, where it was
/// asked for. Playing it now or next needs full control; someone who may
/// only add songs adds it at the end; someone who may only listen asks for
/// it, on the requests card.
async fn play(
    app: &Arc<App>,
    core: &Core,
    access: Access,
    user: u64,
    who: &str,
    command: Spoken,
) -> Result<Reply> {
    let (query, wanted) = match command {
        Spoken::PlayNext(query) => (query, Placement::Next),
        Spoken::Add(query) => (query, Placement::End),
        Spoken::Play(query) => (query, Placement::Now),
        _ => return Err(oops("that isn't a song.")),
    };
    let placement = if access >= Access::Full {
        wanted
    } else {
        Placement::End
    };
    if access < Access::Queue {
        requests::may_ask(app, user)?;
    }
    let track = commands::search(core, &query)
        .await
        .map_err(|e| oops(format!("search failed: {e}")))?
        .first()
        .cloned()
        .ok_or_else(|| oops(format!("I couldn't find \"{query}\".")))?;
    let title = format!(
        "**{}** by {}",
        escape_md(&track.title),
        escape_md(&track.artists_joined())
    );
    let spoken = spoken_song(&track);
    if access < Access::Queue {
        let asked = requests::ask(
            app,
            user,
            who.to_string(),
            track,
            rand::random(),
            app.ui.home(),
        )
        .await;
        return Ok(Reply::new(
            asked,
            format!("{spoken} is on the requests list."),
        ));
    }
    commands::enqueue(app, core, in_call(app)?, vec![track], placement, None, None).await?;
    let (chat, say) = match placement {
        Placement::Now => (format!("playing {title}."), format!("Playing {spoken}.")),
        Placement::Next => (
            format!("{title}, up next."),
            format!("{spoken} is up next."),
        ),
        Placement::End => (
            format!("{title}, added to the queue."),
            format!("Added {spoken}."),
        ),
    };
    Ok(Reply {
        chat,
        say: Some(say),
        pick: Some(query),
    })
}

/// "Not this one?" under a song played from what I heard: the search card,
/// to pick the right one.
pub async fn pick_again(ix: &Ix, query: &str) -> Result<()> {
    let core = ix.app.core()?;
    let results = commands::search(&core, query)
        .await
        .map_err(|e| oops(format!("Search failed: {e}")))?;
    if results.is_empty() {
        return Err(oops(personality::nothing_found()));
    }
    let results = results
        .iter()
        .take(commands::PICKER_RESULTS)
        .cloned()
        .collect();
    commands::show_picker(ix, query, results).await
}

/// What the /voice card shows.
pub struct VoiceView {
    pub bot: String,
    pub owner: bool,
    pub on: bool,
    pub busy: bool,
    pub cpu_ok: bool,
    /// This person said yes.
    pub me: bool,
    /// Who said yes, of the people in the call.
    pub heard_here: Vec<String>,
    /// What I heard lately that this person may see.
    pub last_heard: Vec<Note>,
    /// Talking back: the voice I answer in, when it's on.
    pub talk: Option<String>,
    /// A voice for talking back is on its way.
    pub talk_busy: bool,
    /// Piper is here to talk with (it comes with Loski's image).
    pub talk_ok: bool,
    /// The graphics-card voice is up.
    pub server_ok: bool,
    /// Other names I answer to.
    pub names: Vec<String>,
    /// I'm in a call.
    pub in_call: bool,
    /// The speech model picked.
    pub model: &'static speech::Model,
    /// How long understanding a command usually takes here (ms).
    pub typical_ms: Option<u64>,
}

impl VoiceView {
    fn read(app: &App, ix: &Ix) -> Self {
        let prefs = app.prefs();
        let me = ix.user_id.map_or(0, |u| u.get());
        let owner = ix.is_owner();
        Self {
            bot: app.name(),
            owner,
            on: app.ears.listening(),
            busy: app.ears.busy.load(Ordering::Relaxed),
            cpu_ok: speech::cpu_supported(),
            me: prefs.voice_people.contains(&me),
            heard_here: friends::listeners(app)
                .into_iter()
                .filter(|l| prefs.voice_people.contains(&l.id))
                .map(|l| l.name)
                .collect(),
            last_heard: app.ears.last_heard(me, owner),
            talk: app
                .ears
                .talker()
                .and(prefs.talk_voice.clone())
                .filter(|_| prefs.talk),
            talk_busy: app.ears.talk_busy.load(Ordering::Relaxed),
            talk_ok: talk::available(),
            server_ok: app.ears.server_up.load(Ordering::Relaxed),
            names: prefs.voice_names.clone(),
            in_call: app.voice.channel().is_some(),
            model: speech::model(prefs.voice_model.as_deref()),
            typical_ms: app.ears.typical_ms(),
        }
    }
}

/// The owner's part of the card for talking back: a menu of voices, or why
/// there's none.
fn talk_parts(view: &VoiceView) -> Vec<Component> {
    if view.talk_busy {
        return vec![cards::text(format!(
            "-# TALKING BACK\nGetting the voice ready ({} MB, once)…",
            talk::VOICE_MB
        ))];
    }
    let mut options = vec![cards::option(
        "Talking back: off",
        "off",
        Some("Answers in the chat only".to_string()),
        None,
    )];
    if view.server_ok {
        options.push(cards::option(
            "Talking back: Chatterbox",
            talk::SERVER_VOICE,
            Some("The most natural voice, on your graphics card".to_string()),
            None,
        ));
    }
    if view.talk_ok {
        options.extend(talk::VOICES.iter().map(|v| {
            cards::option(
                format!("Talking back: {}", v.label),
                v.key,
                Some(format!("{}; simpler, runs anywhere", v.about)),
                None,
            )
        }));
    }
    if options.len() == 1 {
        return vec![cards::text(
            "-# TALKING BACK\nTalking back needs the graphics-card voice (see INSTALLATION.md) or Piper, which comes with Loski's Docker image.",
        )];
    }
    let current = view.talk.as_deref().unwrap_or("off");
    for option in &mut options {
        option.default = option.value == current;
    }
    let about = if view.server_ok {
        "I answer out loud in the call, and the music fades down while I talk."
    } else {
        "I answer out loud in the call, and the music fades down while I talk. The most natural voice, Chatterbox, needs an NVIDIA graphics card: see INSTALLATION.md."
    };
    vec![
        cards::text(format!("-# TALKING BACK\n{about}")),
        cards::row(vec![cards::select(
            "loski:voice:talk",
            "Talking back",
            false,
            options,
        )]),
    ]
}

/// The owner's part of the card for the speech model: how quick it is
/// here, and a menu to pick another.
fn model_parts(view: &VoiceView) -> Vec<Component> {
    let mut text = "-# SPEECH MODEL".to_string();
    if let Some(ms) = view.typical_ms.filter(|_| view.on) {
        text.push_str(&format!(
            "\nUnderstanding a command takes about {:.1} s on this computer.",
            ms as f32 / 1000.0
        ));
        if ms > SLOW_MS {
            text.push_str(if view.model.key == speech::MODELS[0].key {
                " That's slow: **Quick** is about 2× faster, and a little less sure of what it hears."
            } else {
                " That's slow even for this model, so expect to wait a moment after each command."
            });
        }
    } else if !view.on {
        text.push_str("\n**Quick** is for slower computers, like a Raspberry Pi or an old laptop.");
    }
    let options = speech::MODELS
        .iter()
        .map(|m| {
            let mut option = cards::option(
                format!("Speech model: {}", m.label),
                m.key,
                Some(format!("{} MB · {}", m.mb, m.about)),
                None,
            );
            option.default = m.key == view.model.key;
            option
        })
        .collect();
    vec![
        cards::text(text),
        cards::row(vec![cards::select(
            "loski:voice:model",
            "Speech model",
            false,
            options,
        )]),
    ]
}

/// "Last heard" lines: when, who, what I made of it and what happened.
fn last_heard_lines(notes: &[Note]) -> String {
    let lines: Vec<String> = notes
        .iter()
        .map(|n| {
            format!(
                "<t:{}:R> **{}** \"{}\" → {} · {:.1} s",
                n.at,
                escape_md(&truncate(&n.who, 32)),
                escape_md(&truncate(&n.text, 80)),
                escape_md(&truncate(n.outcome.trim_end_matches('.'), 80)),
                n.took_ms as f32 / 1000.0
            )
        })
        .collect();
    format!("-# LAST HEARD · only you see this\n{}", lines.join("\n"))
}

pub fn voice_card(icons: &Icons, view: &VoiceView) -> Component {
    let bot = escape_md(&view.bot);
    let status = if !view.cpu_ok {
        "Can't run on this computer".to_string()
    } else if view.busy {
        "Getting ready…".to_string()
    } else if !view.on {
        "Off".to_string()
    } else if !view.in_call {
        "On · I'm not in a call right now".to_string()
    } else if view.heard_here.is_empty() {
        "On · nobody in my call said yes yet".to_string()
    } else {
        format!(
            "On · in my call I listen to {}",
            friends::names_list(&view.heard_here, 6)
        )
    };
    let mut parts = vec![cards::text(format!(
        "{} **Voice commands** · beta\n-# {status}",
        icons.md(Icon::Lyrics)
    ))];
    let close = Btn::label("loski:voice:close", "Close").build(icons);
    if !view.cpu_ok {
        parts.push(cards::text(
            "This computer's processor can't run the speech model: it needs AVX2, which Intel and AMD processors from the last ten years or so have.",
        ));
        parts.push(cards::row(vec![close]));
        return cards::card(Some(color::BRAND), parts);
    }
    if view.busy {
        parts.push(cards::text(format!(
            "Getting the {} speech model ready ({} MB, once)…",
            view.model.label, view.model.mb
        )));
        return cards::card(Some(color::BRAND), parts);
    }
    if !view.on {
        if view.owner {
            parts.push(cards::text(format!(
                "Say **{bot}, skip** in the call, or pause, play *a song*, volume up…\n\nTurning them on downloads a {} MB speech model to this computer, once, and I stop deafening myself in calls so I can hear. I only listen to people who say yes here. What they say is turned into text on this computer and never saved or sent anywhere, though while this is a beta, what's said to me goes in my log so the commands can be tuned. Commands are in English.",
                view.model.mb
            )));
            parts.extend(model_parts(view));
            parts.push(cards::row(vec![
                Btn::label("loski:voice:on", "Turn on")
                    .style(ButtonStyle::Success)
                    .build(icons),
                close,
            ]));
        } else {
            parts.push(cards::text(
                "They're off right now; whoever runs me can turn them on.",
            ));
            parts.push(cards::row(vec![close]));
        }
        return cards::card(Some(color::BRAND), parts);
    }

    parts.push(cards::divider());
    parts.push(if view.me {
        cards::section(
            "**I'm listening to you.**\n-# Short bits of what you say are turned into text on this computer and forgotten (while this is a beta, what's said to me goes in my log). A mic by your name on the panel shows it.",
            Btn::label("loski:voice:notme", "Stop listening").build(icons),
        )
    } else {
        cards::section(
            "**I'm not listening to you.**\n-# Say yes, and I'll hear commands from you in the call.",
            Btn::icon("loski:voice:me", Icon::Done)
                .with_label("Listen to me")
                .style(ButtonStyle::Success)
                .build(icons),
        )
    });
    parts.push(cards::divider());
    let mut tips = format!(
        "-# TRY SAYING \"{bot}, …\"\n\
        **Playing** pause · resume · skip · go back · stop after this song\n\
        **Volume** volume 50 · louder · quieter\n\
        **Songs** play *a song* · play *a song* next · add *a song* · what's playing\n\
        **Your music** play my liked songs · play my playlist *name* · more like this · like this\n\
        **Modes** shuffle · repeat this song · forward 30 seconds · sleep in 30 minutes · leave\n\
        -# Or say just \"{bot}\", then the command after my \"yes?\"."
    );
    if !view.names.is_empty() {
        tips.push_str(&format!(
            " I answer to {} too.",
            friends::names_list(&view.names, 5)
        ));
    }
    parts.push(cards::text(tips));
    if !view.last_heard.is_empty() {
        parts.push(cards::text(last_heard_lines(&view.last_heard)));
    }
    if view.owner {
        parts.push(cards::divider());
        parts.extend(talk_parts(view));
        parts.extend(model_parts(view));
        parts.push(cards::row(vec![
            Btn::label("loski:voice:names", "Other names").build(icons),
            Btn::label("loski:voice:off", "Turn off for everyone")
                .style(ButtonStyle::Danger)
                .build(icons),
        ]));
    } else if view.talk.is_some() {
        parts.push(cards::text("-# I answer out loud in the call."));
    }
    parts.push(cards::row(vec![
        Btn::icon("loski:voice:refresh", Icon::Repeat)
            .with_label("Refresh")
            .build(icons),
        close,
    ]));
    cards::card(Some(color::BRAND), parts)
}

pub async fn voice_cmd(ix: &Ix, _: VoiceCmd) -> Result<()> {
    let view = VoiceView::read(&ix.app, ix);
    ix.reply_private(vec![voice_card(&ix.app.icons, &view)])
        .await
}

/// A button on the /voice card; `key` is what follows `loski:voice:`.
pub async fn change(ix: &Ix, key: &str, value: Option<&str>) -> Result<()> {
    let app = &ix.app;
    let me = ix.user_id.map_or(0, |u| u.get());
    match key {
        "me" | "notme" => {
            app.update_prefs(|p| {
                p.voice_people.retain(|&id| id != me);
                if key == "me" {
                    p.voice_people.push(me);
                }
            });
            app.ears.set_people(&app.prefs().voice_people);
            // The mic by their name on the panel.
            app.ui.mark_dirty();
        }
        "on" if ix.is_owner() => return load(ix, true).await,
        "model" if ix.is_owner() => {
            let model = speech::model(value);
            app.update_prefs(|p| p.voice_model = Some(model.key.to_string()));
            if app.ears.listening() {
                return load(ix, false).await;
            }
        }
        "talk" if ix.is_owner() && value == Some(talk::SERVER_VOICE) => {
            return turn_on_server_talk(ix).await;
        }
        "talk" if ix.is_owner() => match value.and_then(talk::voice) {
            Some(voice) => return turn_on_talk(ix, voice).await,
            None => {
                app.update_prefs(|p| p.talk = false);
                *app.ears.talker.lock() = None;
                tracing::info!("Talking back (beta) is off");
            }
        },
        "off" if ix.is_owner() => {
            app.update_prefs(|p| p.voice = false);
            *app.ears.recognizer.lock() = None;
            voice::hearing_changed(app).await;
            app.ui.mark_dirty();
            tracing::info!("Voice commands (beta) are off");
        }
        "close" => {
            ix.ack().await?;
            ix.delete_reply().await;
            return Ok(());
        }
        "names" if ix.is_owner() => {
            let current = app.prefs().voice_names.join(", ");
            return ix
                .modal(
                    "loski:m:voicenames",
                    "Names I answer to",
                    vec![cards::text_field(
                        "names",
                        "Other names",
                        Some("Besides my own name, comma-separated. \"none\" for none."),
                        "DJ, Lowski",
                        (!current.is_empty()).then_some(current),
                        120,
                    )],
                )
                .await;
        }
        "refresh" => {}
        _ => return ix.refuse().await,
    }
    ix.update_message(vec![voice_card(&app.icons, &VoiceView::read(app, ix))])
        .await
}

/// The other names form came back: up to five names, each a word or two.
pub async fn set_names(ix: &Ix, input: &str) -> Result<()> {
    let app = &ix.app;
    let mut names: Vec<String> = Vec::new();
    for name in input.split([',', ';', '\n']).map(str::trim) {
        let fits = name.chars().any(char::is_alphanumeric) && name.chars().count() <= 32;
        let new = !names.iter().any(|n| n.eq_ignore_ascii_case(name));
        if fits && new && !name.eq_ignore_ascii_case("none") && names.len() < 5 {
            names.push(name.to_string());
        }
    }
    app.update_prefs(|p| p.voice_names = names);
    ix.update_message(vec![voice_card(&app.icons, &VoiceView::read(app, ix))])
        .await
}

/// Downloads the voice if needed, starts Piper with it, and says hello in
/// the call.
async fn turn_on_talk(ix: &Ix, voice: &'static talk::Voice) -> Result<()> {
    let app = &ix.app;
    if app.ears.talk_busy.swap(true, Ordering::Relaxed) {
        return ix.ack().await;
    }
    ix.update_message(vec![voice_card(&app.icons, &VoiceView::read(app, ix))])
        .await?;
    let started = async {
        let model = talk::fetch_voice(&app.web, &app.cfg.data_dir, voice).await?;
        let talker =
            tokio::task::spawn_blocking(move || Talker::piper(&model, &scratch())).await??;
        anyhow::Ok(talker)
    }
    .await;
    app.ears.talk_busy.store(false, Ordering::Relaxed);
    let result = match started {
        Ok(talker) => {
            app.update_prefs(|p| {
                p.talk = true;
                p.talk_voice = Some(voice.key.to_string());
            });
            use_talker(app, talker);
            tracing::info!("Talking back (beta) is on, as {}", voice.label);
            if app.voice.channel().is_some() {
                speak_soon(app, personality::say_hello(&app.name()));
            }
            Ok(())
        }
        Err(e) => {
            tracing::warn!("Couldn't turn talking back on: {e:#}");
            Err(oops(format!("I couldn't turn talking back on: {e}")))
        }
    };
    let card = voice_card(&app.icons, &VoiceView::read(app, ix));
    ix.edit(vec![card]).await?;
    result
}

/// Talking back with the graphics-card voice, if it's up, and hello in the
/// call.
async fn turn_on_server_talk(ix: &Ix) -> Result<()> {
    let app = &ix.app;
    if !talk::server_ready(&app.web).await {
        return Err(oops(
            "The graphics-card voice isn't running. It needs `COMPOSE_PROFILES=gpu-voice` in .env and `loski build`, then a minute or two to load; see INSTALLATION.md.",
        ));
    }
    app.ears.server_up.store(true, Ordering::Relaxed);
    app.update_prefs(|p| {
        p.talk = true;
        p.talk_voice = Some(talk::SERVER_VOICE.to_string());
    });
    use_talker(app, Talker::server(app.web.clone(), talk::server_url()));
    tracing::info!("Talking back (beta) is on, with the graphics-card voice");
    if app.voice.channel().is_some() {
        speak_soon(app, personality::say_hello(&app.name()));
    }
    ix.update_message(vec![voice_card(&app.icons, &VoiceView::read(app, ix))])
        .await
}

/// Downloads the speech model picked if needed and loads it. Turning voice
/// commands on, you're the first person I listen to; otherwise another
/// model takes over from the one listening.
async fn load(ix: &Ix, turning_on: bool) -> Result<()> {
    let app = &ix.app;
    if app.ears.busy.swap(true, Ordering::Relaxed) {
        return ix.ack().await;
    }
    ix.update_message(vec![voice_card(&app.icons, &VoiceView::read(app, ix))])
        .await?;
    let model = speech::model(app.prefs().voice_model.as_deref());
    let loaded = async {
        let path = model.fetch(&app.web, &app.cfg.data_dir).await?;
        let started = tokio::task::spawn_blocking(move || Recognizer::start(&path)).await??;
        anyhow::Ok(started)
    }
    .await;
    app.ears.busy.store(false, Ordering::Relaxed);
    let result = match loaded {
        Ok((recognizer, tried)) => {
            if turning_on {
                let owner = app.owner_id().get();
                app.update_prefs(|p| {
                    p.voice = true;
                    if !p.voice_people.contains(&owner) {
                        p.voice_people.push(owner);
                    }
                });
                app.ears.set_people(&app.prefs().voice_people);
            }
            app.ears.use_recognizer(recognizer, tried);
            if turning_on {
                voice::hearing_changed(app).await;
            }
            app.ui.mark_dirty();
            tracing::info!(
                "Voice commands (beta) are on, with the {} speech model ({} ms to try)",
                model.label,
                tried.as_millis()
            );
            Ok(())
        }
        Err(e) => {
            tracing::warn!("Couldn't load the speech model: {e:#}");
            Err(oops(format!("I couldn't load the speech model: {e}")))
        }
    };
    let card = voice_card(&app.icons, &VoiceView::read(app, ix));
    ix.edit(vec![card]).await?;
    result
}
