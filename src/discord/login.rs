//! The Spotify login from Discord. Its DM has the approval pages as buttons,
//! for when Loski runs somewhere your browser isn't (on a server, say):
//! Spotify then sends the browser to a 127.0.0.1 page that won't load, and
//! you paste that page's address back here to finish.

use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use twilight_model::channel::{Message, message::Component};

use super::{App, cards};
use crate::spotify::auth::{Login, Pasted};

/// The first login, for streaming and search.
pub const FIRST: &str = "### Log in to Spotify\nI need you to log in to Spotify once so I can stream your music: two quick approvals, then you're done for good.";
/// Signing back in to search after Spotify signed Loski out.
pub const AGAIN: &str = "### Sign in to Spotify again\nSpotify signed me out of search and follow mode. Playback keeps working meanwhile.";

/// The login waiting to finish, if any: its approval pages, and where the
/// addresses you paste go.
#[derive(Default)]
pub struct Pending {
    links: Mutex<(Option<String>, Option<String>)>,
    paste: Mutex<Option<mpsc::UnboundedSender<Pasted>>>,
}

/// Remembers `login` while it runs, for the DM and the addresses you paste.
pub fn start(app: &App, login: &Login) -> mpsc::UnboundedReceiver<Pasted> {
    let (tx, rx) = mpsc::unbounded_channel();
    *app.login.links.lock() = login.links();
    *app.login.paste.lock() = Some(tx);
    rx
}

pub fn end(app: &App) {
    *app.login.links.lock() = (None, None);
    *app.login.paste.lock() = None;
}

/// The login DM: the page on Loski's computer, and buttons for anywhere else.
pub fn login_card(
    intro: &str,
    local: &str,
    (streaming, search): &(Option<String>, Option<String>),
) -> Component {
    let both = streaming.is_some() && search.is_some();
    let mut buttons = Vec::new();
    if let Some(url) = streaming {
        buttons.push(cards::link(
            url,
            if both { "1 · Streaming" } else { "Streaming" },
        ));
    }
    if let Some(url) = search {
        buttons.push(cards::link(
            url,
            if both {
                "2 · Search and follow"
            } else {
                "Search and follow"
            },
        ));
    }
    let mut parts = vec![cards::text(format!(
        "{intro}\n\n**On the computer I'm running on:** open {local}\n**Anywhere else** (I'm on a server, say): press {} below. Spotify then sends your browser to a page that won't load, because it points at 127.0.0.1. That's expected: copy that page's address from the address bar and send it to me here{}.",
        if both { "the buttons" } else { "the button" },
        if both { ", once per button" } else { "" }
    ))];
    if !buttons.is_empty() {
        parts.push(cards::row(buttons));
    }
    cards::card(Some(cards::color::BRAND), parts)
}

/// DMs you the login that's waiting (`intro` says why). Returns whether it arrived.
pub async fn dm(app: &App, intro: &str) -> bool {
    let links = app.login.links.lock().clone();
    app.dm_owner(login_card(intro, &app.cfg.login_url(), &links))
        .await
}

/// An address you pasted in our DM, finishing a login. Returns whether the
/// message was one (so it isn't read as anything else).
pub async fn on_paste(app: &App, message: &Message) -> bool {
    if message.guild_id.is_some() || message.author.id != app.owner_id() {
        return false;
    }
    let text = message.content.trim();
    if !(text.contains("code=") || text.contains("error=")) {
        return false;
    }
    let waiting = app.login.paste.lock().clone();
    let card = match waiting {
        None => cards::info(
            &app.icons,
            "I'm logged in to Spotify already; there's nothing to finish.",
        ),
        Some(waiting) => {
            let (reply, answer) = oneshot::channel();
            let sent = waiting.send(Pasted {
                text: text.to_string(),
                reply,
            });
            match (sent, answer.await) {
                (Ok(()), Ok(Ok(done))) => {
                    cards::toast(&app.icons, &done, None, cards::color::SUCCESS)
                }
                (Ok(()), Ok(Err(problem))) => cards::error(&app.icons, &problem),
                _ => cards::error(
                    &app.icons,
                    "That login ended before I could use this. Use the buttons in my latest message.",
                ),
            }
        }
    };
    let _ = app
        .http
        .create_message(message.channel_id)
        .flags(cards::V2)
        .components(&[card])
        .await;
    true
}
