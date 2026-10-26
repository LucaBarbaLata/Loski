//! Tells you when a new Loski release is out. Once a day it asks GitHub
//! for the latest release; a newer one gets a DM (once per release) and a
//! line in /status.

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use serde::Deserialize;
use twilight_model::channel::message::Component;

use super::{
    App,
    cards::{self, color},
    icons::{Icon, Icons},
};
use crate::util::truncate;

/// The version this build of Loski calls itself.
pub const RUNNING: &str = env!("CARGO_PKG_VERSION");
const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);
/// Remembers which release you were already told about.
const MARKER: &str = "update_notified";
const NOTES_MAX: usize = 1_500;

#[derive(Debug, Clone, Deserialize)]
pub struct Release {
    pub tag_name: String,
    #[serde(default)]
    pub body: Option<String>,
    pub html_url: String,
}

impl Release {
    pub fn version(&self) -> &str {
        self.tag_name.trim_start_matches('v')
    }
}

/// "2.1.0", "v2.1.0" or "2.0.0-dev" as something comparable. A pre-release
/// sorts before the release it leads up to.
fn parse(version: &str) -> Option<(u64, u64, u64, bool)> {
    let version = version.trim().trim_start_matches('v');
    let (numbers, pre_release) = match version.split_once('-') {
        Some((numbers, _)) => (numbers, true),
        None => (version, false),
    };
    let mut parts = numbers.split('.');
    let mut next = || -> Option<u64> {
        match parts.next() {
            Some(part) => part.parse().ok(),
            None => Some(0),
        }
    };
    Some((next()?, next()?, next()?, !pre_release))
}

pub fn is_newer(latest: &str, running: &str) -> bool {
    matches!((parse(latest), parse(running)), (Some(a), Some(b)) if a > b)
}

async fn latest_release(app: &App) -> Result<Option<Release>> {
    let url = format!(
        "https://api.github.com/repos/{}/releases/latest",
        app.cfg.update_repo
    );
    let response = app
        .web
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15))
        .send()
        .await?;
    // No release published yet.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(response.error_for_status()?.json().await?))
}

/// The DM for a new release: what it is, what changed, and how to get it.
pub fn update_card(icons: &Icons, release: &Release) -> Component {
    let notes = release
        .body
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| truncate(n, NOTES_MAX))
        .unwrap_or_else(|| "No notes for this release.".to_string());
    cards::card(
        Some(color::BRAND),
        vec![
            cards::text(format!(
                "{} **Loski {} is out**\n-# You're running {RUNNING}.",
                icons.md(Icon::Music),
                release.version()
            )),
            cards::divider(),
            cards::text(notes),
            cards::divider(),
            cards::text("-# Update with `.\\loski update` on the computer Loski runs on."),
            cards::row(vec![cards::link(&release.html_url, "Release notes")]),
        ],
    )
}

async fn check(app: &App) {
    let release = match latest_release(app).await {
        Ok(Some(release)) => release,
        Ok(None) => return,
        Err(e) => {
            tracing::debug!("couldn't check for a new release: {e:#}");
            return;
        }
    };
    *app.latest_release.lock() = Some(release.clone());
    if !is_newer(release.version(), RUNNING)
        || app.store.read_marker(MARKER).as_deref() == Some(release.version())
    {
        return;
    }
    tracing::info!("Loski {} is out (running {RUNNING})", release.version());
    if app.dm_owner(update_card(&app.icons, &release)).await {
        app.store.write_marker(MARKER, release.version());
    }
}

/// Checks a couple of minutes after starting, then once a day.
pub async fn check_loop(app: Arc<App>) {
    if !app.cfg.update_check {
        return;
    }
    tokio::time::sleep(Duration::from_secs(120)).await;
    loop {
        check(&app).await;
        tokio::time::sleep(CHECK_EVERY).await;
    }
}

/// The newer release, if one is known, for /status.
pub fn newer_release(app: &App) -> Option<Release> {
    app.latest_release
        .lock()
        .clone()
        .filter(|r| is_newer(r.version(), RUNNING))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_like_people_do() {
        assert!(is_newer("2.1.0", "2.0.0"));
        assert!(is_newer("v2.0.0", "2.0.0-dev"));
        assert!(is_newer("2.0.1", "2.0.0"));
        assert!(is_newer("10.0.0", "9.9.9"));
        assert!(!is_newer("2.0.0", "2.0.0"));
        assert!(!is_newer("1.9.0", "2.0.0"));
        assert!(!is_newer("2.0.0-beta", "2.0.0"));
        assert!(!is_newer("nonsense", "2.0.0"));
        assert!(is_newer("2.1", "2.0.9"));
    }
}
