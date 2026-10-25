//! Keeps the bot's name, avatar, banner and "About me" in line with the
//! files in the repo. Images are only uploaded when their art changes.

use std::sync::Arc;

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::App;
use crate::store::write_atomic;

const AVATAR: &[u8] = include_bytes!("../../assets/avatar.png");
const BANNER: &[u8] = include_bytes!("../../assets/banner.png");
const ABOUT: &str = "Private Spotify DJ. Streams real Spotify audio into voice and only takes requests from its owner.";

#[derive(Default, Serialize, Deserialize)]
struct Record {
    avatar: String,
    banner: String,
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn data_uri(png: &[u8]) -> String {
    format!("data:image/png;base64,{}", STANDARD.encode(png))
}

pub async fn sync(app: Arc<App>) {
    if !app.cfg.sync_profile {
        return;
    }
    if let Err(e) = sync_inner(&app).await {
        tracing::warn!("Couldn't update the bot's profile: {e:#}");
    }
}

async fn sync_inner(app: &App) -> Result<()> {
    let record_path = app.cfg.data_dir.join("profile.json");
    let mut record: Record = std::fs::read_to_string(&record_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let me = app.http.current_user().await?.model().await?;

    // Name changes are rate limited by Discord, so they go on their own.
    if me.name != app.cfg.bot_name {
        match app
            .http
            .update_current_user()
            .username(&app.cfg.bot_name)
            .await
        {
            Ok(_) => tracing::info!("Renamed the bot from {} to {}", me.name, app.cfg.bot_name),
            Err(e) => {
                tracing::warn!("Couldn't rename the bot (Discord allows two renames per hour): {e}")
            }
        }
    }

    let (avatar_digest, banner_digest) = (digest(AVATAR), digest(BANNER));
    let avatar_stale = me.avatar.is_none() || record.avatar != avatar_digest;
    let banner_stale = me.banner.is_none() || record.banner != banner_digest;
    if avatar_stale || banner_stale {
        let (avatar, banner) = (data_uri(AVATAR), data_uri(BANNER));
        let mut update = app.http.update_current_user();
        if avatar_stale {
            update = update.avatar(Some(&avatar));
        }
        if banner_stale {
            update = update.banner(Some(&banner));
        }
        update.await?;
        record.avatar = avatar_digest;
        record.banner = banner_digest;
        if let Ok(json) = serde_json::to_string_pretty(&record) {
            let _ = write_atomic(&record_path, json.as_bytes());
        }
        tracing::info!("Updated the bot's avatar and banner");
    }

    let application = app.http.current_user_application().await?.model().await?;
    if application.description != ABOUT {
        app.http
            .update_current_user_application()
            .description(ABOUT)
            .await?;
    }
    Ok(())
}
