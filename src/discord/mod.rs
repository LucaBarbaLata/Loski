mod cards;
mod commands;
mod components;
mod icons;
mod interact;
mod library;
mod mixes;
mod panel;
mod profile;
mod recap;
mod requests;
mod sound;
mod stats;
mod status;
mod voice;

#[cfg(test)]
mod card_tests;

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, OnceLock},
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
    channel::message::Component,
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
    spotify::{
        self, Spotify, auth,
        web::{SearchItem, SearchKind, WebApi},
    },
    store::Store,
    util::truncate,
};

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
    /// Problems you've been told about and not yet told are fixed.
    alerts: Mutex<HashSet<&'static str>>,
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
        Id::new(self.cfg.owner_id)
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

pub async fn run(cfg: Arc<Config>) -> Result<()> {
    let http = Arc::new(Client::new(cfg.discord_token.clone()));
    let bot = http
        .current_user()
        .await
        .context("Discord rejected the bot token. Check DISCORD_TOKEN in .env")?
        .model()
        .await?;
    let application = http.current_user_application().await?.model().await?;

    let intents = Intents::GUILDS | Intents::GUILD_VOICE_STATES;
    let mut shard = Shard::new(ShardId::ONE, cfg.discord_token.clone(), intents);
    let sender = shard.sender();
    let shards = TwilightMap::new(HashMap::from([(shard.id().number(), sender.clone())]));
    let songbird = Arc::new(Songbird::twilight(Arc::new(shards), bot.id));

    let cache = DefaultInMemoryCache::builder()
        .resource_types(ResourceType::GUILD | ResourceType::CHANNEL | ResourceType::VOICE_STATE)
        .build();
    let web = reqwest::Client::builder()
        .user_agent(concat!("Loski/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let store =
        Arc::new(Store::new(cfg.data_dir.clone()).context("could not create the data folder")?);

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
        alerts: Mutex::new(HashSet::new()),
    });

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
            app.set_listening(None);
            if ready.guilds.is_empty() {
                tracing::warn!(
                    "Loski isn't in any server yet. Invite it with: https://discord.com/oauth2/authorize?client_id={}&scope=bot+applications.commands&permissions=3230720",
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
                welcome_once(&app).await;
                tokio::spawn(stats::recap_loop(app.clone()));
                return;
            }
            Err(e) => {
                tracing::error!("Spotify setup failed: {e:#}. Retrying in 15 seconds.");
                tokio::time::sleep(Duration::from_secs(15)).await;
            }
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
        ask_owner_to_log_in(app).await;
        let result = auth::login(cfg, &app.web, credentials.is_none(), web_token.is_none()).await?;
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
    tokio::spawn(spotify::connect::run(cfg.clone(), core.engine.clone()));
    Ok(core)
}

async fn ask_owner_to_log_in(app: &App) {
    let url = app.cfg.login_url();
    tracing::warn!("Spotify login needed. Open {url} in your browser on this computer.");
    let Ok(dm) = app.http.create_private_channel(app.owner_id()).await else {
        return;
    };
    let Ok(dm) = dm.model().await else { return };
    let card = cards::card(
        Some(cards::color::BRAND),
        vec![cards::text(format!(
            "### One quick thing before we start\n\
             I need you to log in to Spotify once so I can stream your music.\n\n\
             Open **{url}** in your browser on the computer I'm running on. \
             It's two quick approvals, then you're done for good."
        ))],
    );
    let _ = app
        .http
        .create_message(dm.id)
        .flags(cards::V2)
        .components(&[card])
        .await;
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

/// If Spotify signs Loski out of search and follow mode, opens the login
/// page again and DMs you the link. Playback keeps working meanwhile.
async fn web_watchdog(app: Arc<App>, core: Arc<Core>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        if !core.spotify.web.is_signed_out() {
            continue;
        }
        let url = app.cfg.login_url();
        app.alert(
            "web-login",
            &format!(
                "Spotify signed me out of search and follow mode. Open **{url}** on this computer to sign back in. Playback keeps working meanwhile."
            ),
        )
        .await;
        match auth::login(&app.cfg, &app.web, false, true).await {
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
