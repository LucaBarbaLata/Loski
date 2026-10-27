mod blend;
mod cards;
mod commands;
mod components;
mod friends;
mod icons;
mod interact;
mod library;
mod login;
mod lyrics;
mod milestones;
mod mixes;
mod panel;
mod profile;
mod recap;
mod requests;
mod settings;
mod sound;
mod stats;
mod status;
mod updates;
mod voice;

#[cfg(test)]
mod card_tests;

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result};
use librespot_core::authentication::Credentials;
use parking_lot::Mutex;
use songbird::{Songbird, shards::TwilightMap};
use tokio::sync::mpsc;
use twilight_cache_inmemory::{DefaultInMemoryCache, ResourceType};
use twilight_gateway::{
    CloseFrame, Event, EventTypeFlags, Intents, MessageSender, Shard, ShardId, StreamExt as _,
};
use twilight_http::Client;
use twilight_model::{
    application::interaction::InteractionType,
    channel::{Message, message::Component},
    gateway::{
        payload::{
            incoming::GuildCreate,
            outgoing::{UpdatePresence, UpdateVoiceState},
        },
        presence::{ActivityType, MinimalActivity, Status},
    },
    id::{
        Id,
        marker::{ApplicationMarker, ChannelMarker, MessageMarker, UserMarker},
    },
};

use crate::{
    config::Config,
    engine::{Engine, FollowTarget},
    genres::{self, Genres},
    spotify::{
        self, Spotify, auth,
        web::{SearchItem, SearchKind, WebApi},
    },
    stats::HistoryFilter,
    store::{Prefs, Store},
    util::truncate,
};

/// Where the claimed owner is remembered, inside the data folder.
pub const OWNER_MARKER: &str = "owner";

/// A fresh six-digit code for claiming Loski.
pub fn claim_code() -> String {
    use rand::Rng;
    format!("{:06}", rand::rng().random_range(0..1_000_000))
}

/// How long replies stay before cleaning themselves up.
pub const SHORT_SECS: u64 = 10;
pub const ERROR_SECS: u64 = 15;
pub const LONG_SECS: u64 = 90;

/// An error meant for the person using the bot, shown as written.
#[derive(Debug)]
pub struct Oops(pub String);

impl std::fmt::Display for Oops {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Oops {}

pub fn oops(message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Oops(message.into()))
}

/// Everything that needs Spotify, available once the login is done.
pub struct Core {
    pub spotify: Arc<Spotify>,
    pub engine: Arc<Engine>,
}

/// A search card waiting for you to choose something.
pub struct Pick {
    pub query: String,
    pub kind: SearchKind,
    pub items: Vec<SearchItem>,
    /// Index into `items` the buttons act on.
    pub selected: usize,
    pub created: Instant,
}

pub struct App {
    pub cfg: Arc<Config>,
    pub http: Arc<Client>,
    pub cache: DefaultInMemoryCache,
    pub songbird: Arc<Songbird>,
    pub sender: MessageSender,
    pub app_id: Id<ApplicationMarker>,
    pub bot_id: Id<UserMarker>,
    pub web: reqwest::Client,
    pub store: Arc<Store>,
    pub core: OnceLock<Arc<Core>>,
    pub ui: panel::Ui,
    pub voice: voice::Voice,
    pub icons: Arc<icons::Icons>,
    pub picks: Mutex<HashMap<u64, Pick>>,
    pub started: Instant,
    /// The latest heartbeat round trip to Discord's gateway.
    pub ping: Mutex<Option<Duration>>,
    pub requests: requests::Requests,
    /// The Spotify login waiting to finish, for addresses pasted in DMs.
    pub login: login::Pending,
    /// Who's listening, and their likes and votes for the song playing.
    pub room: friends::Room,
    /// Problems you've been told about and not yet told are fixed.
    alerts: Mutex<HashSet<&'static str>>,
    /// The newest release GitHub told us about.
    pub latest_release: Mutex<Option<updates::Release>>,
    /// What you set in /settings (see `settings.rs` for the accessors).
    prefs: Mutex<Prefs>,
    /// Your Discord user id; 0 until someone claims Loski.
    owner: AtomicU64,
    /// The code that claims Loski, while it has no owner.
    claim: Option<String>,
    /// Your artists' genres, looked up on MusicBrainz.
    pub genres: Arc<Genres>,
    /// What the history card shows: the last /history search, and how it reads.
    pub history_view: Mutex<(HistoryFilter, String)>,
}

impl App {
    pub fn core(&self) -> Result<Arc<Core>> {
        self.core.get().cloned().ok_or_else(|| {
            oops(format!(
                "I'm not connected to Spotify yet. Open **{}** on this computer to log in. You only need to do this once.",
                self.cfg.login_url()
            ))
        })
    }

    pub fn owner_id(&self) -> Id<UserMarker> {
        // Nobody yet: an id no Discord account has, so nothing matches it.
        Id::new(self.owner.load(Ordering::Relaxed).max(1))
    }

    pub fn has_owner(&self) -> bool {
        self.owner.load(Ordering::Relaxed) != 0
    }

    /// "Listening to …" under the bot's name. With nothing playing it
    /// points at /play.
    pub fn set_listening(&self, what: Option<&str>) {
        let name = what.map_or_else(|| "/play".to_string(), |w| truncate(w, 120));
        let activity = MinimalActivity {
            kind: ActivityType::Listening,
            name,
            url: None,
        };
        if let Ok(presence) =
            UpdatePresence::new(vec![activity.into()], false, None, Status::Online)
        {
            let _ = self.sender.command(&presence);
        }
    }

    /// Posts a card outside of any command and removes it after `secs`.
    pub async fn announce(
        self: &Arc<Self>,
        channel: Id<ChannelMarker>,
        card: Component,
        secs: u64,
    ) {
        let components = [card];
        let sent = self
            .http
            .create_message(channel)
            .flags(cards::V2)
            .components(&components)
            .await;
        match sent {
            Ok(response) => {
                if let Ok(message) = response.model().await {
                    self.delete_later(channel, message.id, secs);
                }
            }
            Err(e) => tracing::warn!("could not post a message: {e}"),
        }
    }

    /// Sends you a card in your DMs. Returns whether it arrived.
    pub async fn dm_owner(&self, card: Component) -> bool {
        let Ok(dm) = self.http.create_private_channel(self.owner_id()).await else {
            return false;
        };
        let Ok(dm) = dm.model().await else {
            return false;
        };
        self.http
            .create_message(dm.id)
            .flags(cards::V2)
            .components(&[card])
            .await
            .is_ok()
    }

    /// Tells you about a problem Loski can't fix right now, once per problem.
    pub async fn alert(&self, key: &'static str, text: &str) {
        tracing::warn!("{text}");
        if self.alerts.lock().insert(key) {
            self.dm_owner(cards::error(&self.icons, text)).await;
        }
    }

    /// The problem behind `key` went away; you hear about it only if you
    /// were told about the problem.
    pub async fn all_clear(&self, key: &'static str, text: &str) {
        if self.alerts.lock().remove(key) {
            tracing::info!("{text}");
            self.dm_owner(cards::toast(&self.icons, text, None, cards::color::SUCCESS))
                .await;
        }
    }

    pub fn delete_later(
        self: &Arc<Self>,
        channel: Id<ChannelMarker>,
        message: Id<MessageMarker>,
        secs: u64,
    ) {
        let app = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(secs)).await;
            let _ = app.http.delete_message(channel, message).await;
        });
    }
}

/// Connects to Discord and runs until shut down. `claim` is the code that
/// makes someone the owner, when there isn't one yet.
pub async fn run(cfg: Arc<Config>, claim: Option<String>) -> Result<()> {
    let http = Arc::new(Client::new(cfg.discord_token.clone()));
    let bot = http
        .current_user()
        .await
        .context("Discord rejected the bot token. Check DISCORD_TOKEN in .env")?
        .model()
        .await?;
    let application = http.current_user_application().await?.model().await?;

    // Direct messages are for claiming Loski; their text comes without a
    // privileged intent.
    let intents = Intents::GUILDS | Intents::GUILD_VOICE_STATES | Intents::DIRECT_MESSAGES;
    let mut shard = Shard::new(ShardId::ONE, cfg.discord_token.clone(), intents);
    let sender = shard.sender();
    let shards = TwilightMap::new(HashMap::from([(shard.id().number(), sender.clone())]));
    let songbird = Arc::new(Songbird::twilight(Arc::new(shards), bot.id));

    let cache = DefaultInMemoryCache::builder()
        // Members and users come with voice states: names for who's listening.
        .resource_types(
            ResourceType::GUILD
                | ResourceType::CHANNEL
                | ResourceType::VOICE_STATE
                | ResourceType::MEMBER
                | ResourceType::USER,
        )
        .build();
    let web = reqwest::Client::builder()
        .user_agent(concat!("Loski/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let store =
        Arc::new(Store::new(cfg.data_dir.clone()).context("could not create the data folder")?);
    let prefs = store.load_prefs();
    let names = store.load_names();
    let genres = Genres::load(&cfg.data_dir);
    let owner = cfg.owner_id.unwrap_or_else(|| {
        store
            .read_marker(OWNER_MARKER)
            .and_then(|id| id.parse().ok())
            .unwrap_or(0)
    });

    let app = Arc::new(App {
        cfg: cfg.clone(),
        http,
        cache,
        songbird,
        sender,
        app_id: application.id,
        bot_id: bot.id,
        web,
        store,
        core: OnceLock::new(),
        ui: panel::Ui::default(),
        voice: voice::Voice::default(),
        icons: Arc::new(icons::Icons::default()),
        picks: Mutex::new(HashMap::new()),
        started: Instant::now(),
        ping: Mutex::new(None),
        requests: requests::Requests::default(),
        login: login::Pending::default(),
        room: friends::Room::new(names),
        alerts: Mutex::new(HashSet::new()),
        latest_release: Mutex::new(None),
        prefs: Mutex::new(prefs),
        owner: AtomicU64::new(owner),
        claim: claim.filter(|_| owner == 0),
        genres: genres.clone(),
        history_view: Mutex::new(Default::default()),
    });
    tokio::spawn(genres::fill_loop(genres, app.web.clone()));
    app.ui
        .set_fixed(app.prefs().panel_channel.and_then(Id::new_checked));

    {
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = icons::sync(
                app.icons.clone(),
                &app.http,
                app.app_id,
                app.cfg.data_dir.clone(),
            )
            .await
            {
                tracing::warn!("Icons unavailable, using standard emoji instead: {e:#}");
            }
        });
    }
    tokio::spawn(profile::sync(app.clone()));
    tokio::spawn(updates::check_loop(app.clone()));
    tokio::spawn(init_core(app.clone()));

    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => {
                tracing::info!("Shutting down");
                voice::leave(&app, None).await;
                shard.close(CloseFrame::NORMAL);
                break;
            }
            item = shard.next_event(EventTypeFlags::all()) => {
                let Some(item) = item else { break };
                let event = match item {
                    Ok(event) => event,
                    Err(e) => {
                        tracing::warn!("gateway error: {e}");
                        continue;
                    }
                };
                if matches!(event, Event::GatewayHeartbeatAck) {
                    *app.ping.lock() = shard.latency().recent().first().copied();
                }
                app.cache.update(&event);
                app.songbird.process(&event).await;
                tokio::spawn(handle_event(app.clone(), event));
            }
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn handle_event(app: Arc<App>, event: Event) {
    match event {
        Event::Ready(ready) => {
            tracing::info!("Logged in to Discord as {}", ready.user.name);
            if let Some(code) = app.claim.as_deref().filter(|_| !app.has_owner()) {
                tracing::warn!(
                    "No owner yet: DM {} \"claim {code}\" from your Discord account to become its owner.",
                    ready.user.name
                );
            }
            app.set_listening(None);
            if ready.guilds.is_empty() {
                tracing::warn!(
                    "Loski isn't in any server yet. Invite it with: https://discord.com/oauth2/authorize?client_id={}&scope=bot+applications.commands&permissions=281474979941376",
                    app.app_id
                );
            }
        }
        Event::GuildCreate(guild) => {
            if let Err(e) = commands::register(&app, guild.id()).await {
                tracing::warn!("could not register commands: {e:#}");
            }
            // After a crash Discord may still show us in a voice channel.
            if let GuildCreate::Available(guild) = guild.as_ref() {
                let stuck = guild
                    .voice_states
                    .iter()
                    .any(|v| v.user_id == app.bot_id && v.channel_id.is_some());
                if stuck && app.songbird.get(guild.id).is_none() {
                    let _ = app
                        .sender
                        .command(&UpdateVoiceState::new(guild.id, None, false, false));
                }
            }
        }
        Event::VoiceStateUpdate(update) => voice::on_voice_state(&app, &update.0).await,
        Event::MessageCreate(message) => {
            // A login address from you first; otherwise maybe a claim.
            if !login::on_paste(&app, &message.0).await {
                claim_by_dm(&app, &message.0).await;
            }
        }
        Event::InteractionCreate(interaction) => {
            let interaction = interaction.0;
            match interaction.kind {
                InteractionType::ApplicationCommand => commands::run(app, interaction).await,
                InteractionType::ApplicationCommandAutocomplete => {
                    commands::autocomplete(app, interaction).await;
                }
                InteractionType::MessageComponent => components::handle(app, interaction).await,
                InteractionType::ModalSubmit => components::modal(app, interaction).await,
                _ => {}
            }
        }
        _ => {}
    }
}

/// Logs in to Spotify (asking for the one-time browser login if needed) and
/// starts the playback engine.
async fn init_core(app: Arc<App>) {
    loop {
        match try_init_core(&app).await {
            Ok(core) => {
                let _ = app.core.set(core);
                tracing::info!("Loski is ready");
                tokio::spawn(voice::come_back(app.clone()));
                welcome_once(&app).await;
                tokio::spawn(stats::recap_loop(app.clone()));
                tokio::spawn(blend::blend_loop(app.clone()));
                tokio::spawn(milestones::milestone_loop(app.clone()));
                return;
            }
            Err(e) => {
                tracing::error!("Spotify setup failed: {e:#}. Retrying in 15 seconds.");
                tokio::time::sleep(Duration::from_secs(15)).await;
            }
        }
    }
}

/// "claim 123456" in a direct message makes you the owner, while Loski has
/// none. The code is on the setup page and in the log, so a stranger in your
/// server can't claim it first.
async fn claim_by_dm(app: &Arc<App>, message: &Message) {
    if message.guild_id.is_some() || message.author.bot || app.has_owner() {
        return;
    }
    let Some(code) = app.claim.as_deref() else {
        return;
    };
    let text = message.content.trim().to_lowercase();
    if !text.starts_with("claim") {
        return;
    }
    let claimed = text.split_whitespace().collect::<Vec<_>>() == ["claim", code];
    let card = if claimed {
        app.owner.store(message.author.id.get(), Ordering::Relaxed);
        app.store
            .write_marker(OWNER_MARKER, &message.author.id.to_string());
        tracing::info!("{} claimed Loski and is its owner now", message.author.name);
        cards::toast(
            &app.icons,
            "**You're my owner now.** Everything is yours to control; friends can be co-DJs from `/settings`.",
            None,
            cards::color::SUCCESS,
        )
    } else {
        cards::error(
            &app.icons,
            "That's not the code. It's on the setup page, and in my log (`loski logs`).",
        )
    };
    let _ = app
        .http
        .create_message(message.channel_id)
        .flags(cards::V2)
        .components(&[card])
        .await;
    if claimed {
        match app.core.get() {
            Some(_) => welcome_once(app).await,
            None => ask_owner_to_log_in(app).await,
        }
    }
}

/// The first time everything works, DM the owner the command overview.
async fn welcome_once(app: &App) {
    let marker = app.cfg.data_dir.join("welcomed");
    if marker.exists() {
        return;
    }
    let Ok(dm) = app.http.create_private_channel(app.owner_id()).await else {
        return;
    };
    let Ok(dm) = dm.model().await else { return };
    let card = commands::help_card(&app.icons);
    let sent = app
        .http
        .create_message(dm.id)
        .flags(cards::V2)
        .components(&[card])
        .await;
    if sent.is_ok() {
        let _ = std::fs::write(marker, b"");
    }
}

async fn try_init_core(app: &Arc<App>) -> Result<Arc<Core>> {
    let cfg = &app.cfg;
    let mut credentials = spotify::cached_credentials(cfg);
    let mut web_token = WebApi::load_token(cfg);

    if credentials.is_none() || web_token.is_none() {
        let login = auth::Login::new(cfg, credentials.is_none(), web_token.is_none())?;
        let mut pasted = login::start(app, &login);
        ask_owner_to_log_in(app).await;
        let result = login.finish(cfg, &app.web, &mut pasted).await;
        login::end(app);
        let result = result?;
        if let Some(token) = result.librespot_token {
            credentials = Some(Credentials::with_access_token(token));
        }
        if let Some(token) = result.web_token {
            WebApi::save_token(cfg, &token)?;
            web_token = Some(token);
        }
    }
    let (Some(credentials), Some(web_token)) = (credentials, web_token) else {
        anyhow::bail!("login did not finish");
    };

    let spotify = match Spotify::connect(cfg.clone(), app.web.clone(), credentials, web_token).await
    {
        Ok(spotify) => Arc::new(spotify),
        Err(e) => {
            if format!("{e:#}").contains("Login failed") {
                tracing::warn!("Stored Spotify login was rejected; you'll need to log in again");
                spotify::forget_credentials(cfg);
            }
            return Err(e);
        }
    };

    let (notices_tx, notices_rx) = mpsc::unbounded_channel();
    let engine = Engine::new(
        spotify.session(),
        cfg.normalise,
        app.store.clone(),
        notices_tx,
    );
    let core = Arc::new(Core { spotify, engine });

    tokio::spawn(panel::run(app.clone(), core.clone(), notices_rx));
    tokio::spawn(follow_loop(core.clone()));
    tokio::spawn(session_watchdog(app.clone(), core.clone()));
    tokio::spawn(web_watchdog(app.clone(), core.clone()));
    tokio::spawn(voice::heal_loop(app.clone()));
    tokio::spawn(lyrics::follow_loop(app.clone(), core.clone()));
    tokio::spawn(connect_queue_loop(core.clone()));
    tokio::spawn(spotify::connect::run(cfg.clone(), core.engine.clone()));
    Ok(core)
}

async fn ask_owner_to_log_in(app: &App) {
    let url = app.cfg.login_url();
    tracing::warn!(
        "Spotify login needed. Open {url} in your browser on this computer, or use the buttons in the DM Loski sent you (from anywhere)."
    );
    login::dm(app, login::FIRST).await;
}

/// While follow mode is on, mirrors what your Spotify is playing.
async fn follow_loop(core: Arc<Core>) {
    let mut wait = Duration::from_secs(1);
    loop {
        tokio::time::sleep(wait).await;
        if !core.engine.is_following() {
            wait = Duration::from_secs(1);
            continue;
        }
        match core.spotify.web.now_listening().await {
            Ok(now) => {
                wait = Duration::from_millis(1500);
                core.engine.follow_sync(now.map(|n| FollowTarget {
                    track: Arc::new(n.track),
                    playing: n.is_playing,
                    progress_ms: n.progress_ms,
                }));
            }
            Err(e) => {
                tracing::warn!("follow mode: {e:#}");
                wait = Duration::from_secs(10);
            }
        }
    }
}

/// librespot sessions occasionally drop; reconnect and hand the new one to
/// the players. If it stays down, you get a DM (and another when it's back).
async fn session_watchdog(app: Arc<App>, core: Arc<Core>) {
    let mut failures = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(5 * failures.clamp(1, 12))).await;
        if !core.spotify.session().is_invalid() {
            continue;
        }
        tracing::warn!("Spotify connection lost, reconnecting");
        match core.spotify.reconnect().await {
            Ok(session) => {
                core.engine.set_session(session);
                tracing::info!("Spotify reconnected");
                failures = 0;
                app.all_clear("spotify", "Spotify is back. Music can play again.")
                    .await;
            }
            Err(e) => {
                failures += 1;
                tracing::error!("Spotify reconnect failed: {e:#}");
                if failures >= 3 {
                    let text = if format!("{e:#}").contains("Login failed") {
                        "Spotify rejected my saved login. Restart me (`.\\loski restart`) and I'll walk you through logging in again.".to_string()
                    } else {
                        format!(
                            "I lost my connection to Spotify and can't get it back yet ({e}). I'll keep trying."
                        )
                    };
                    app.alert("spotify", &text).await;
                }
            }
        }
    }
}

/// While the Spotify app is in control, keeps its up next on the panel
/// (it can change in the app without the song changing).
async fn connect_queue_loop(core: Arc<Core>) {
    loop {
        tokio::time::sleep(Duration::from_secs(15)).await;
        if core.engine.is_connect() {
            refresh_connect_queue(&core).await;
        }
    }
}

pub(super) async fn refresh_connect_queue(core: &Core) {
    match core.spotify.web.queue().await {
        Ok(tracks) => core.engine.set_connect_up_next(tracks),
        Err(e) => tracing::debug!("couldn't read the Spotify app's queue: {e:#}"),
    }
}

/// If Spotify signs Loski out of search and follow mode, opens the login
/// page again and DMs you the link. Playback keeps working meanwhile.
async fn web_watchdog(app: Arc<App>, core: Arc<Core>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        if !core.spotify.web.is_signed_out() {
            continue;
        }
        let login = match auth::Login::new(&app.cfg, false, true) {
            Ok(login) => login,
            Err(e) => {
                tracing::warn!("couldn't start the Spotify sign-in: {e:#}");
                continue;
            }
        };
        let mut pasted = login::start(&app, &login);
        // Once per sign-out, like other problems; the all clear follows.
        if app.alerts.lock().insert("web-login") {
            tracing::warn!(
                "Spotify signed Loski out of search and follow mode. Open {} on this computer, or use the button in Loski's DM.",
                app.cfg.login_url()
            );
            login::dm(&app, login::AGAIN).await;
        }
        let finished = login.finish(&app.cfg, &app.web, &mut pasted).await;
        login::end(&app);
        match finished {
            Ok(result) => {
                if let Some(token) = result.web_token {
                    core.spotify.web.replace_token(token).await;
                    app.all_clear("web-login", "Search and follow mode work again.")
                        .await;
                }
            }
            Err(e) => tracing::warn!("couldn't reopen the Spotify login page: {e:#}"),
        }
    }
}
