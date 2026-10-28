//! Voice commands (beta): "Loski, skip" in the call. With voice commands on
//! (you turn them on in /voice) I stop deafening myself in calls, but I only
//! listen to people who said yes in /voice. Short bits of what they say are
//! turned into text on this computer, acted on if they start with my name,
//! and forgotten. While this is a beta, what was said to me goes in the log,
//! so the words I listen for can be tuned; other talk is only counted.

use std::{
    collections::{HashMap, HashSet},
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
use twilight_model::{channel::message::Component, id::Id};

use super::{
    App, SHORT_SECS,
    cards::{self, Btn, color},
    commands::{self, Placement},
    friends::{self, Access},
    icons::{Icon, Icons},
    interact::Ix,
    oops, voice,
};
use crate::{
    speech::{self, MODEL_MB, RATE, Recognizer, Spoken},
    util::{escape_md, truncate},
};

#[derive(CommandModel, CreateCommand)]
#[command(
    name = "voice",
    desc = "Voice commands (beta): say \"Loski, skip\" in the call"
)]
pub struct VoiceCmd;

/// A frame (20 ms) this loud or louder is someone speaking.
const LOUD: f64 = 350.0;
/// Quiet this long (in 20 ms frames) ends a bit of speech.
const END_QUIET: u32 = 30;
/// Less speech than this (in frames) is a cough, not a command.
const MIN_VOICED: u32 = 12;
/// Longer than this is talk, not a command.
const MAX_SAMPLES: usize = 7 * RATE;

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
}

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

#[derive(Default)]
struct Stream {
    user: Option<u64>,
    audio: Vec<i16>,
    /// Frames of quiet since they last spoke.
    quiet: u32,
    /// Frames of speech in `audio`.
    voiced: u32,
    /// Talking for longer than a command: ignored until they pause.
    too_long: bool,
    /// Their voice has come through at least once (said in the log).
    came_through: bool,
}

impl Stream {
    fn hear(&mut self, pcm: &[i16]) {
        let loud = rms(pcm) >= LOUD;
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
        if loud || !self.audio.is_empty() {
            self.audio.extend_from_slice(pcm);
        }
        if self.audio.len() > MAX_SAMPLES {
            self.forget();
            self.too_long = true;
        }
    }

    fn silence(&mut self) {
        self.quiet = self.quiet.saturating_add(1);
    }

    /// The speech that just ended, if any (and long enough to be a command).
    fn finished(&mut self) -> Option<Vec<i16>> {
        if self.quiet < END_QUIET || (self.audio.is_empty() && !self.too_long) {
            return None;
        }
        let enough = self.voiced >= MIN_VOICED && !self.too_long;
        let audio = std::mem::take(&mut self.audio);
        self.forget();
        enough.then_some(audio)
    }

    fn forget(&mut self) {
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

    fn set_people(&self, people: &[u64]) {
        *self.people.lock() = people.iter().copied().collect();
        // Anything half-heard from someone who just said no goes.
        self.streams.lock().values_mut().for_each(Stream::forget);
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
                        stream.hear(pcm);
                    }
                    // Never keep anything from someone who didn't say yes.
                    _ => stream.forget(),
                }
            }
            for ssrc in &tick.silent {
                if let Some(stream) = streams.get_mut(ssrc) {
                    stream.silence();
                }
            }
            for stream in streams.values_mut() {
                if let (Some(user), Some(audio)) = (stream.user, stream.finished()) {
                    finished.push(Utterance { user, audio });
                }
            }
        }
        for utterance in finished {
            // Full means people are talking over each other: let it go.
            let _ = self.heard.try_send(utterance);
        }
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
    let path = speech::model_path(&app.cfg.data_dir);
    if !path.exists() {
        return;
    }
    match tokio::task::spawn_blocking(move || Recognizer::start(&path)).await {
        Ok(Ok(recognizer)) => {
            *app.ears.recognizer.lock() = Some(Arc::new(recognizer));
            tracing::info!("Voice commands (beta) are on");
            voice::hearing_changed(&app).await;
        }
        Ok(Err(e)) => tracing::warn!("Voice commands are off: {e:#}"),
        Err(e) => tracing::warn!("Voice commands are off: {e}"),
    }
}

/// Turns what people said into text, and acts on what's for me.
pub async fn run(app: Arc<App>, mut heard: mpsc::Receiver<Utterance>) {
    while let Some(Utterance { user, audio }) = heard.recv().await {
        let Some(recognizer) = app.ears.recognizer() else {
            continue;
        };
        let name = app.name();
        let seconds = audio.len() as f32 / RATE as f32;
        let samples = audio.iter().map(|&s| f32::from(s) / 32768.0).collect();
        let heard = match recognizer.hear(samples, speech::prompt(&name)).await {
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
        let who = friends::listeners(&app)
            .into_iter()
            .find(|l| l.id == user)
            .map(|l| l.name)
            .or_else(|| app.room.name_of(user))
            .unwrap_or_else(|| "Someone".to_string());
        let meaning = speech::understand(&heard.text, &name);
        // While this is a beta: what was said to me, how quickly I got it,
        // and what it meant. Other talk isn't written down.
        let took = heard.took.as_millis();
        match &meaning {
            None => tracing::info!("Voice (beta): {who}, {seconds:.1} s, {took} ms → not for me"),
            Some(result) => tracing::info!(
                "Voice (beta): {who}, {seconds:.1} s, {took} ms: \"{}\" → {}",
                heard.text,
                match result {
                    Ok(command) => format!("{command:?}"),
                    Err(_) => "not a command I know".to_string(),
                }
            ),
        }
        let said = match meaning {
            None => continue,
            Some(Ok(command)) => match act(&app, user, command).await {
                Ok(text) => format!(
                    "{} **{}**: {text}",
                    app.icons.md(Icon::Music),
                    escape_md(&who)
                ),
                Err(e) => format!(
                    "{} **{}**: {e}",
                    app.icons.md(Icon::Problem),
                    escape_md(&who)
                ),
            },
            // Said to me, but not something I know: say what I heard, so
            // it's clear what to say instead.
            Some(Err(rest)) => {
                let heard = if rest.is_empty() {
                    "my name".to_string()
                } else {
                    format!("\"{}\"", escape_md(&truncate(&rest, 80)))
                };
                format!(
                    "{} **{}**: I heard {heard}, but that's not a command I know. Try \"{name}, pause\", \"{name}, skip\" or \"{name}, play\" and a song.",
                    app.icons.md(Icon::Info),
                    escape_md(&who),
                    name = escape_md(&name),
                )
            }
        };
        if let Some(home) = app.ui.home() {
            app.announce(home, cards::card(None, vec![cards::text(said)]), SHORT_SECS)
                .await;
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

/// Does what was asked, and says what happened.
async fn act(app: &Arc<App>, user: u64, command: Spoken) -> Result<String> {
    let core = app.core()?;
    let engine = &core.engine;
    let access = access_of(app, user);
    let needs = match command {
        // Anyone in the call likes songs, and votes to skip.
        Spoken::Like | Spoken::Skip => Access::Listener,
        Spoken::Play(_) => Access::Queue,
        _ => Access::Full,
    };
    if access < needs {
        return Err(oops("that one's for whoever runs me."));
    }
    Ok(match command {
        Spoken::Pause => {
            engine.pause();
            "paused.".into()
        }
        Spoken::Resume => {
            engine.resume();
            "carrying on.".into()
        }
        Spoken::Skip if access >= Access::Full => {
            engine.skip(1);
            "skipped.".into()
        }
        Spoken::Skip => {
            let track = engine
                .current_track()
                .ok_or_else(|| oops("nothing's playing."))?;
            let (votes, needed) = friends::vote_skip(app, &track, user)?;
            if votes >= needed {
                engine.skip(1);
                format!("the call voted to skip ({votes} of {needed}).")
            } else {
                format!("a vote to skip ({votes} of {needed}).")
            }
        }
        Spoken::Previous => {
            engine.previous();
            "back one.".into()
        }
        Spoken::Leave => {
            voice::send_away(app, None).await;
            "bye!".into()
        }
        Spoken::Louder => {
            engine.set_volume((engine.volume() + 10).min(100));
            format!("volume {}.", engine.volume())
        }
        Spoken::Quieter => {
            engine.set_volume(engine.volume().saturating_sub(10));
            format!("volume {}.", engine.volume())
        }
        Spoken::Volume(level) => {
            engine.set_volume(level);
            format!("volume {level}.")
        }
        Spoken::Shuffle => {
            engine.shuffle();
            "shuffled.".into()
        }
        Spoken::Like => {
            let track = engine
                .current_track()
                .ok_or_else(|| oops("nothing's playing."))?;
            let name = app.room.name_of(user).unwrap_or_default();
            if friends::toggle_like(app, &track, user, &name)? {
                format!("♥ {}.", escape_md(&track.title))
            } else {
                "like taken back.".into()
            }
        }
        Spoken::Play(query) => {
            let (Some(guild), Some(channel)) = (app.voice.guild(), app.voice.channel()) else {
                return Err(oops("I'm not in a call."));
            };
            let track = commands::search(&core, &query)
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
            let card = commands::enqueue(
                app,
                &core,
                (guild, channel),
                vec![track],
                Placement::End,
                None,
                None,
            )
            .await;
            card?;
            format!("{title}, coming up.")
        }
    })
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
}

impl VoiceView {
    fn read(app: &App, ix: &Ix) -> Self {
        let prefs = app.prefs();
        let me = ix.user_id.map_or(0, |u| u.get());
        Self {
            bot: app.name(),
            owner: ix.is_owner(),
            on: app.ears.listening(),
            busy: app.ears.busy.load(Ordering::Relaxed),
            cpu_ok: speech::cpu_supported(),
            me: prefs.voice_people.contains(&me),
            heard_here: friends::listeners(app)
                .into_iter()
                .filter(|l| prefs.voice_people.contains(&l.id))
                .map(|l| l.name)
                .collect(),
        }
    }
}

pub fn voice_card(icons: &Icons, view: &VoiceView) -> Component {
    let bot = escape_md(&view.bot);
    let mut parts = vec![cards::text(format!(
        "{} **Voice commands** · beta\nSay **{bot}, skip** in the call, or pause, play *a song*, volume up, like this…",
        icons.md(Icon::Music)
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
            "Getting the speech model ready ({MODEL_MB} MB, once)…"
        )));
        return cards::card(Some(color::BRAND), parts);
    }
    if !view.on {
        if view.owner {
            parts.push(cards::text(format!(
                "They're off. Turning them on downloads a {MODEL_MB} MB speech model to this computer, once, and I stop deafening myself in calls so I can hear.\n\nI only listen to people who say yes here. What they say is turned into text on this computer and never saved or sent anywhere, though while this is a beta, what's said to me goes in my log so the commands can be tuned. Commands are in English: \"Loski, pause\", \"Loski, skip\", \"Loski, play\" and a song."
            )));
            parts.push(cards::row(vec![
                Btn::label("loski:voice:on", "Turn on")
                    .style(twilight_model::channel::message::component::ButtonStyle::Success)
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
    parts.push(cards::text(if view.me {
        "I'm listening to you. Only short bits of speech, turned into text on this computer and forgotten (while this is a beta, what's said to me is logged)."
    } else {
        "I'm not listening to you. Say yes, and I'll hear commands from you in the call."
    }));
    parts.push(cards::text(match view.heard_here.as_slice() {
        [] => "-# Nobody in my call said yes yet.".to_string(),
        names => format!(
            "-# In my call I listen to {}.",
            friends::names_list(&names.iter().map(|n| escape_md(n)).collect::<Vec<_>>(), 6)
        ),
    }));
    parts.push(cards::text(format!(
        "-# **{bot}**, then: pause · resume · skip · previous · volume up · volume down · volume 50 · shuffle · like this · play *a song* · leave"
    )));
    let mut buttons = vec![if view.me {
        Btn::label("loski:voice:notme", "Stop listening to me").build(icons)
    } else {
        Btn::icon("loski:voice:me", Icon::Done)
            .with_label("Listen to me")
            .build(icons)
    }];
    if view.owner {
        buttons.push(Btn::label("loski:voice:off", "Turn off for everyone").build(icons));
    }
    buttons.push(close);
    parts.push(cards::row(buttons));
    cards::card(Some(color::BRAND), parts)
}

pub async fn voice_cmd(ix: &Ix, _: VoiceCmd) -> Result<()> {
    let view = VoiceView::read(&ix.app, ix);
    ix.reply_private(vec![voice_card(&ix.app.icons, &view)])
        .await
}

/// A button on the /voice card; `key` is what follows `loski:voice:`.
pub async fn change(ix: &Ix, key: &str) -> Result<()> {
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
        }
        "on" if ix.is_owner() => return turn_on(ix).await,
        "off" if ix.is_owner() => {
            app.update_prefs(|p| p.voice = false);
            *app.ears.recognizer.lock() = None;
            voice::hearing_changed(app).await;
            tracing::info!("Voice commands (beta) are off");
        }
        "close" => {
            ix.ack().await?;
            ix.delete_reply().await;
            return Ok(());
        }
        _ => return ix.refuse().await,
    }
    ix.update_message(vec![voice_card(&app.icons, &VoiceView::read(app, ix))])
        .await
}

/// Downloads the model if needed, loads it and starts listening. You're the
/// first person it listens to.
async fn turn_on(ix: &Ix) -> Result<()> {
    let app = &ix.app;
    if app.ears.busy.swap(true, Ordering::Relaxed) {
        return ix.ack().await;
    }
    ix.update_message(vec![voice_card(&app.icons, &VoiceView::read(app, ix))])
        .await?;
    let loaded = async {
        let path = speech::fetch_model(&app.web, &app.cfg.data_dir).await?;
        let recognizer = tokio::task::spawn_blocking(move || Recognizer::start(&path)).await??;
        anyhow::Ok(recognizer)
    }
    .await;
    app.ears.busy.store(false, Ordering::Relaxed);
    let result = match loaded {
        Ok(recognizer) => {
            let owner = app.owner_id().get();
            app.update_prefs(|p| {
                p.voice = true;
                if !p.voice_people.contains(&owner) {
                    p.voice_people.push(owner);
                }
            });
            app.ears.set_people(&app.prefs().voice_people);
            *app.ears.recognizer.lock() = Some(Arc::new(recognizer));
            voice::hearing_changed(app).await;
            tracing::info!("Voice commands (beta) are on");
            Ok(())
        }
        Err(e) => {
            tracing::warn!("Couldn't turn voice commands on: {e:#}");
            Err(oops(format!("I couldn't turn voice commands on: {e}")))
        }
    };
    let card = voice_card(&app.icons, &VoiceView::read(app, ix));
    ix.edit(vec![card]).await?;
    result
}
