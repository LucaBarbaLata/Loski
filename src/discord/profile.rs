//! The bot's profile: its name, picture, banner and "About me".
//!
//! A new bot gets Loski's own look. From then on what you choose wins,
//! whether with /customize or in Discord's developer portal: Loski only
//! keeps its own art up to date where you haven't changed it, and only
//! renames the bot to a name you picked (/customize, or BOT_NAME). A
//! server's own look (nickname, picture, banner, bio) lives in Discord, and
//! only /customize changes it.

use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use twilight_http::error::ErrorType;
use twilight_model::{
    id::{Id, marker::GuildMarker},
    user::CurrentUser,
};

use super::{App, commands, oops};
use crate::store::write_atomic;

const AVATAR: &[u8] = include_bytes!("../../assets/avatar.png");
const BANNER: &[u8] = include_bytes!("../../assets/banner.png");
pub const ABOUT: &str = "Private Spotify DJ. Streams real Spotify audio into voice and only takes requests from its owner.";
/// What the bot is called when nobody picked a name.
pub const DEFAULT_NAME: &str = "Loski";
const FILE: &str = "profile.json";

/// One profile change at a time, so `data/profile.json` stays whole.
static BUSY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// What Loski knows about the profile, kept in `data/profile.json`.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Record {
    /// Digests of Loski's own picture and banner, as last uploaded.
    avatar: String,
    banner: String,
    /// Discord's hashes for them, to notice a change made elsewhere.
    avatar_hash: Option<String>,
    banner_hash: Option<String>,
    /// The name you picked; it wins over BOT_NAME.
    name: Option<String>,
    /// A picture, banner or About me of your own, which Loski leaves alone.
    own_avatar: bool,
    own_banner: bool,
    own_about: bool,
}

impl Record {
    fn load(dir: &Path) -> Option<Self> {
        std::fs::read_to_string(dir.join(FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    fn save(&self, dir: &Path) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = write_atomic(&dir.join(FILE), json.as_bytes());
        }
    }
}

/// A picture to upload, as it came.
pub struct Image {
    pub bytes: Vec<u8>,
    /// "image/png", "image/gif"…
    pub mime: String,
}

impl Image {
    fn data_uri(&self) -> String {
        format!("data:{};base64,{}", self.mime, STANDARD.encode(&self.bytes))
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn png_uri(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", STANDARD.encode(png))
}

/// Which part of the profile goes back to Loski's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Name,
    Picture,
    Banner,
    About,
    All,
}

impl App {
    /// The bot's name in Discord, which its messages, commands and Spotify
    /// Connect go by.
    pub fn name(&self) -> String {
        self.name.lock().clone()
    }

    /// The name the bot goes back to: BOT_NAME, or Loski.
    pub fn default_name(&self) -> String {
        self.cfg
            .bot_name
            .clone()
            .unwrap_or_else(|| DEFAULT_NAME.to_string())
    }
}

pub async fn sync(app: Arc<App>) {
    if !app.cfg.sync_profile {
        return;
    }
    if let Err(e) = sync_inner(&app).await {
        tracing::warn!("Couldn't update the bot's profile: {e:#}");
    }
}

/// On startup: the name you picked, and Loski's own art and About me
/// wherever you haven't put your own.
async fn sync_inner(app: &App) -> Result<()> {
    let _busy = BUSY.lock().await;
    let dir = &app.cfg.data_dir;
    let mut record = Record::load(dir).unwrap_or_default();
    let me = app.http.current_user().await?.model().await?;

    if let Some(name) = record.name.clone().or_else(|| app.cfg.bot_name.clone())
        && me.name != name
    {
        match rename(app, &name).await {
            Ok(()) => tracing::info!("Renamed the bot from {} to {name}", me.name),
            Err(e) => tracing::warn!("Couldn't rename the bot to {name}: {e}"),
        }
    }

    let avatar_due = art_due(
        &mut record.own_avatar,
        &mut record.avatar_hash,
        &record.avatar,
        me.avatar.map(|h| h.to_string()),
        AVATAR,
    );
    let banner_due = art_due(
        &mut record.own_banner,
        &mut record.banner_hash,
        &record.banner,
        me.banner.map(|h| h.to_string()),
        BANNER,
    );
    if avatar_due || banner_due {
        let (avatar, banner) = (png_uri(AVATAR), png_uri(BANNER));
        let mut update = app.http.update_current_user();
        if avatar_due {
            update = update.avatar(Some(&avatar));
        }
        if banner_due {
            update = update.banner(Some(&banner));
        }
        let me = update.await?.model().await?;
        if avatar_due {
            record.avatar = digest(AVATAR);
            record.avatar_hash = me.avatar.map(|h| h.to_string());
        }
        if banner_due {
            record.banner = digest(BANNER);
            record.banner_hash = me.banner.map(|h| h.to_string());
        }
        tracing::info!("Gave the bot Loski's own picture and banner");
    }

    if !record.own_about {
        let application = app.http.current_user_application().await?.model().await?;
        let about = application.description.trim();
        if about.is_empty() {
            app.http
                .update_current_user_application()
                .description(ABOUT)
                .await?;
        } else if about != ABOUT {
            // Written in the developer portal: yours.
            record.own_about = true;
        }
    }
    record.save(dir);
    Ok(())
}

/// Whether Loski's own art should go up, given the hash of what Discord
/// shows now. Art someone else put there (in the developer portal, say)
/// becomes yours and stays.
fn art_due(
    own: &mut bool,
    ours: &mut Option<String>,
    uploaded: &str,
    now: Option<String>,
    art: &[u8],
) -> bool {
    if *own {
        return false;
    }
    match (now, ours.clone()) {
        (None, _) => true,
        (Some(now), Some(ours)) if now != ours => {
            *own = true;
            false
        }
        // A new bot that came with a picture of its own.
        (Some(_), None) if uploaded.is_empty() => {
            *own = true;
            false
        }
        // Set by an older Loski that didn't note the hash.
        (Some(now), None) => {
            *ours = Some(now);
            uploaded != digest(art)
        }
        (Some(_), Some(_)) => uploaded != digest(art),
    }
}

/// Discord renamed the bot, perhaps from the developer portal: that's the
/// name you picked now.
pub async fn on_user_update(app: &App, user: &CurrentUser) {
    if user.name == app.name() {
        return;
    }
    {
        let _busy = BUSY.lock().await;
        let dir = &app.cfg.data_dir;
        let mut record = Record::load(dir).unwrap_or_default();
        record.name = Some(user.name.clone());
        record.save(dir);
    }
    *app.name.lock() = user.name.clone();
    follow_name(app).await;
}

/// Renames the bot in Discord. The new name is in use straight away, so the
/// update Discord sends back isn't taken for a rename from elsewhere.
async fn rename(app: &App, name: &str) -> Result<()> {
    let old = std::mem::replace(&mut *app.name.lock(), name.to_string());
    match within(
        "name",
        app.http.update_current_user().username(name).into_future(),
    )
    .await
    {
        Ok(_) => {
            follow_name(app).await;
            Ok(())
        }
        Err(e) => {
            *app.name.lock() = old;
            Err(e)
        }
    }
}

/// Commands, right-click entries and Spotify Connect take on the bot's name.
async fn follow_name(app: &App) {
    let guilds: Vec<Id<GuildMarker>> = app.cache.iter().guilds().map(|g| *g.key()).collect();
    for guild in guilds {
        if let Err(e) = commands::register(app, guild).await {
            tracing::warn!("could not update the commands for the new name: {e:#}");
        }
    }
    if let Some(core) = app.core.get() {
        core.engine.restart_connect();
    }
}

/// A new name, picked in /customize.
pub async fn set_name(app: &App, name: &str) -> Result<()> {
    let _busy = BUSY.lock().await;
    rename(app, name).await?;
    let dir = &app.cfg.data_dir;
    let mut record = Record::load(dir).unwrap_or_default();
    record.name = Some(name.to_string());
    record.save(dir);
    Ok(())
}

/// A new About me; empty (or Loski's own text) goes back to Loski's own.
pub async fn set_about(app: &App, about: &str) -> Result<()> {
    let _busy = BUSY.lock().await;
    let mine = about.trim().is_empty() || about.trim() == ABOUT;
    let text = if mine { ABOUT } else { about.trim() };
    within(
        "About me",
        app.http
            .update_current_user_application()
            .description(text)
            .into_future(),
    )
    .await?;
    let dir = &app.cfg.data_dir;
    let mut record = Record::load(dir).unwrap_or_default();
    record.own_about = !mine;
    record.save(dir);
    Ok(())
}

/// A picture of your own.
pub async fn set_picture(app: &App, image: &Image) -> Result<()> {
    let _busy = BUSY.lock().await;
    let uri = image.data_uri();
    let me = within(
        "picture",
        app.http
            .update_current_user()
            .avatar(Some(&uri))
            .into_future(),
    )
    .await?
    .model()
    .await?;
    let dir = &app.cfg.data_dir;
    let mut record = Record::load(dir).unwrap_or_default();
    record.own_avatar = true;
    record.avatar_hash = me.avatar.map(|h| h.to_string());
    record.save(dir);
    Ok(())
}

/// A banner of your own.
pub async fn set_banner(app: &App, image: &Image) -> Result<()> {
    let _busy = BUSY.lock().await;
    let uri = image.data_uri();
    let me = within(
        "banner",
        app.http
            .update_current_user()
            .banner(Some(&uri))
            .into_future(),
    )
    .await?
    .model()
    .await?;
    let dir = &app.cfg.data_dir;
    let mut record = Record::load(dir).unwrap_or_default();
    record.own_banner = true;
    record.banner_hash = me.banner.map(|h| h.to_string());
    record.save(dir);
    Ok(())
}

/// Back to Loski's own name, picture, banner or About me.
pub async fn reset(app: &App, part: Part) -> Result<()> {
    let all = part == Part::All;
    if all || part == Part::About {
        set_about(app, "").await?;
    }
    let _busy = BUSY.lock().await;
    let dir = &app.cfg.data_dir;
    let (picture, banner) = (all || part == Part::Picture, all || part == Part::Banner);
    if picture || banner {
        let (avatar_uri, banner_uri) = (png_uri(AVATAR), png_uri(BANNER));
        let mut update = app.http.update_current_user();
        if picture {
            update = update.avatar(Some(&avatar_uri));
        }
        if banner {
            update = update.banner(Some(&banner_uri));
        }
        let me = within("picture", update.into_future())
            .await?
            .model()
            .await?;
        let mut record = Record::load(dir).unwrap_or_default();
        if picture {
            record.own_avatar = false;
            record.avatar = digest(AVATAR);
            record.avatar_hash = me.avatar.map(|h| h.to_string());
        }
        if banner {
            record.own_banner = false;
            record.banner = digest(BANNER);
            record.banner_hash = me.banner.map(|h| h.to_string());
        }
        record.save(dir);
    }
    if all || part == Part::Name {
        let name = app.default_name();
        if app.name() != name {
            rename(app, &name).await?;
        }
        let mut record = Record::load(dir).unwrap_or_default();
        record.name = None;
        record.save(dir);
    }
    Ok(())
}

/// The bot's look in one server. `None` leaves that part as it is.
#[derive(Default)]
pub struct ServerLook {
    /// An empty nickname goes back to the bot's name.
    pub nick: Option<String>,
    pub bio: Option<String>,
    pub picture: Option<Image>,
    pub banner: Option<Image>,
}

/// Changes the bot's look in one server. The nickname goes on its own, as
/// it needs a permission the rest doesn't.
pub async fn set_server(app: &App, guild: Id<GuildMarker>, look: &ServerLook) -> Result<()> {
    let picture = look.picture.as_ref().map(Image::data_uri);
    let banner = look.banner.as_ref().map(Image::data_uri);
    if picture.is_some() || banner.is_some() || look.bio.is_some() {
        let mut update = app.http.update_current_member(guild);
        if let Some(picture) = &picture {
            update = update.avatar(Some(picture));
        }
        if let Some(banner) = &banner {
            update = update.banner(Some(banner));
        }
        if let Some(bio) = &look.bio {
            update = update.bio(Some(bio));
        }
        within("look in this server", update.into_future()).await?;
    }
    if let Some(nick) = &look.nick {
        let nick = (!nick.is_empty()).then_some(nick.as_str());
        within(
            "nickname",
            app.http
                .update_current_member(guild)
                .nick(nick)
                .into_future(),
        )
        .await?;
    }
    Ok(())
}

/// The bot looks the same in this server as everywhere else again.
pub async fn reset_server(app: &App, guild: Id<GuildMarker>) -> Result<()> {
    within(
        "look in this server",
        app.http
            .update_current_member(guild)
            .avatar(None)
            .banner(None)
            .bio(None)
            .into_future(),
    )
    .await?;
    let nick = app
        .cache
        .member(guild, app.bot_id)
        .and_then(|m| m.nick().map(str::to_string));
    if nick.is_some() {
        within(
            "nickname",
            app.http
                .update_current_member(guild)
                .nick(None)
                .into_future(),
        )
        .await?;
    }
    Ok(())
}

/// A profile request, with Discord's refusals in words and a limit on how
/// long it may take.
async fn within<T>(
    what: &str,
    request: impl Future<Output = Result<T, twilight_http::Error>>,
) -> Result<T> {
    match tokio::time::timeout(Duration::from_secs(20), request).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(refused(what, &e)),
        Err(_) => Err(oops(format!(
            "Discord took too long with the {what}. Try again in a moment."
        ))),
    }
}

fn refused(what: &str, error: &twilight_http::Error) -> anyhow::Error {
    let said = discord_says(error);
    let lower = said.to_lowercase();
    if lower.contains("too fast") || lower.contains("rate limit") {
        oops(format!(
            "Discord only lets a bot change its {what} a couple of times an hour. Try again a bit later."
        ))
    } else if lower.contains("missing permissions") && what == "nickname" {
        oops(
            "I'm not allowed to change my nickname here. Give my role the **Change Nickname** permission, then try again.",
        )
    } else {
        tracing::warn!("Discord turned down the {what}: {error}");
        oops(format!("Discord didn't take the {what}: {said}"))
    }
}

/// What Discord said was wrong, in its own words where it gives some.
fn discord_says(error: &twilight_http::Error) -> String {
    fn first_detail(value: &serde_json::Value) -> Option<String> {
        let object = value.as_object()?;
        if let Some(errors) = object.get("_errors").and_then(|e| e.as_array()) {
            return errors.first()?.get("message")?.as_str().map(str::to_string);
        }
        object.values().find_map(first_detail)
    }
    if let ErrorType::Response { body, .. } = error.kind()
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(body)
    {
        if let Some(detail) = json.get("errors").and_then(first_detail) {
            return detail;
        }
        if let Some(message) = json.get("message").and_then(|m| m.as_str()) {
            return message.to_string();
        }
    }
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_art_you_put_there() {
        let ours = digest(AVATAR);
        let due = |own: bool, hash: Option<&str>, uploaded: &str, now: Option<&str>| {
            let (mut own, mut hash) = (own, hash.map(str::to_string));
            let due = art_due(
                &mut own,
                &mut hash,
                uploaded,
                now.map(str::to_string),
                AVATAR,
            );
            (due, own, hash)
        };
        // A new bot without a picture gets Loski's.
        assert_eq!(due(false, None, "", None), (true, false, None));
        // A new bot that has one keeps it.
        assert_eq!(due(false, None, "", Some("abc")), (false, true, None));
        // Changed in the developer portal since Loski set it: yours now.
        assert!(due(false, Some("abc"), &ours, Some("def")).1);
        // Still Loski's, and the art hasn't changed.
        assert!(!due(false, Some("abc"), &ours, Some("abc")).0);
        // Still Loski's, and the art has.
        assert!(due(false, Some("abc"), "old", Some("abc")).0);
        // An older Loski's record: note the hash, upload only new art.
        assert_eq!(
            due(false, None, &ours, Some("abc")),
            (false, false, Some("abc".to_string()))
        );
        // Yours: never touched.
        assert!(!due(true, Some("abc"), "old", None).0);
    }
}
