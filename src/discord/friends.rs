//! Friends: co-DJs (by name, or everyone with a DJ role), who's listening,
//! likes and votes to skip. Nobody but you controls Loski until you say so.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use anyhow::Result;
use parking_lot::Mutex;
use twilight_model::{
    channel::message::{
        Component,
        component::{SelectDefaultValue, SelectMenu, SelectMenuOption, SelectMenuType},
    },
    id::{
        Id,
        marker::{RoleMarker, UserMarker},
    },
};

use super::{
    App,
    cards::{self, Btn, color},
    icons::{Icon, Icons},
    interact::Ix,
    library, oops, settings,
};
use crate::{
    spotify::auth::now_unix,
    store::{Like, Prefs},
    track::TrackInfo,
    util::{escape_md, truncate},
};

/// What someone may do with Loski, least to most.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Access {
    /// Anyone: listen, like, vote to skip, `/request`, `/lyrics`, `/help`.
    Listener,
    /// A co-DJ who can add songs and look at the queue.
    Queue,
    /// A co-DJ with full control of playback.
    Full,
    /// You: everything, including your library, stats and settings.
    Owner,
}

/// Who may use a slash command (or a right-click command, by its name).
pub fn command_needs(name: &str) -> Access {
    match name {
        "request" | "lyrics" | "help" => Access::Listener,
        "play" | "queue" | "nowplaying" | library::QUEUE_WITH => Access::Queue,
        "settings" | "stats" | "wrapped" | "history" | "mix" | "follow" | "liked" | "playlist"
        | "status" | "normalize" => Access::Owner,
        _ => Access::Full,
    }
}

/// Who may press a button or use a menu, by its custom id.
pub fn component_needs(id: &str) -> Access {
    const OWNER: [&str; 10] = [
        "loski:set:",
        "loski:eq:",
        "loski:friends:",
        "loski:mix",
        "loski:stats:",
        "loski:recap:",
        "loski:hsel",
        "loski:hback",
        "loski:hgo:",
        "loski:qsavemix",
    ];
    // Skip is a vote for anyone who can't skip by themselves.
    const LISTENER: [&str; 3] = ["loski:like", "loski:skip", "loski:lyrics"];
    const QUEUE: [&str; 9] = [
        "loski:queue",
        "loski:qpage:",
        "loski:qsel:",
        "loski:qclose",
        "pick:",
        "picktab:",
        "pickcancel:",
        "pickgo:",
        "req:",
    ];
    if OWNER.iter().any(|p| id.starts_with(p)) {
        Access::Owner
    } else if LISTENER.iter().any(|p| id.starts_with(p)) {
        Access::Listener
    } else if id.starts_with("pickgo:") && id.ends_with(":now") {
        // "Play now" on a search card skips what's playing.
        Access::Full
    } else if QUEUE.iter().any(|p| id.starts_with(p)) {
        Access::Queue
    } else {
        Access::Full
    }
}

/// Who may send a pop-up form back.
pub fn modal_needs(id: &str) -> Access {
    match id {
        "loski:m:seek" => Access::Full,
        _ => Access::Owner,
    }
}

/// A co-DJ: picked by name, or holding the DJ role.
pub fn is_co_dj(prefs: &Prefs, user: u64, roles: &[Id<RoleMarker>]) -> bool {
    prefs.co_djs.contains(&user)
        || prefs
            .dj_role
            .is_some_and(|role| roles.iter().any(|r| r.get() == role))
}

/// What a co-DJ may do.
pub fn co_dj_access(prefs: &Prefs) -> Access {
    if prefs.djs_queue_only {
        Access::Queue
    } else {
        Access::Full
    }
}

/// Someone in Loski's voice channel.
#[derive(Debug, Clone)]
pub struct Listener {
    pub id: u64,
    pub name: String,
}

/// Who's in Loski's voice channel: people only (no bots), you first.
pub fn listeners(app: &App) -> Vec<Listener> {
    let (Some(guild), Some(channel)) = (app.voice.guild(), app.voice.channel()) else {
        return Vec::new();
    };
    let Some(states) = app.cache.voice_channel_states(channel) else {
        return Vec::new();
    };
    let ids: Vec<Id<UserMarker>> = states
        .map(|state| state.user_id())
        .filter(|user| *user != app.bot_id)
        .collect();
    let mut people: Vec<Listener> = ids
        .into_iter()
        .filter_map(|user| {
            let cached = app
                .cache
                .user(user)
                .map(|u| (u.bot, u.global_name.clone(), u.name.clone()));
            if cached.as_ref().is_some_and(|(bot, _, _)| *bot) {
                return None;
            }
            let nick = app
                .cache
                .member(guild, user)
                .and_then(|m| m.nick().map(str::to_string));
            let name = nick
                .or_else(|| cached.as_ref().and_then(|(_, global, _)| global.clone()))
                .or_else(|| cached.map(|(_, _, name)| name))
                .unwrap_or_else(|| "Someone".to_string());
            Some(Listener {
                id: user.get(),
                name,
            })
        })
        .collect();
    let owner = app.owner_id().get();
    people.sort_by_key(|l| (l.id != owner, l.name.to_lowercase()));
    people
}

/// You or a co-DJ is in the call, so someone's there to run things.
pub fn someone_in_charge(app: &App) -> bool {
    let Some(guild) = app.voice.guild() else {
        return false;
    };
    let prefs = app.prefs();
    let owner = app.owner_id().get();
    listeners(app).iter().any(|l| {
        let roles = app
            .cache
            .member(guild, Id::new(l.id))
            .map(|m| m.roles().to_vec())
            .unwrap_or_default();
        l.id == owner || is_co_dj(&prefs, l.id, &roles)
    })
}

/// Likes and votes for the song playing, and who's who.
pub struct Room {
    state: Mutex<RoomState>,
}

struct RoomState {
    /// The song the likes and votes below are for.
    track: Option<Arc<TrackInfo>>,
    /// Who liked it: user id and name.
    likes: BTreeMap<u64, String>,
    votes: HashSet<u64>,
    /// Songs liked this session and by whom, for the recap.
    session: Vec<(Arc<TrackInfo>, Vec<String>)>,
    /// Friends' names by user id, also kept in `data/friends.json`.
    names: HashMap<u64, String>,
    /// Who was in the call at the last look, to notice when that changes.
    present: Vec<(u64, String)>,
}

impl Room {
    pub fn new(names: HashMap<u64, String>) -> Self {
        Self {
            state: Mutex::new(RoomState {
                track: None,
                likes: BTreeMap::new(),
                votes: HashSet::new(),
                session: Vec::new(),
                names,
                present: Vec::new(),
            }),
        }
    }

    /// The name a friend had when Loski last saw them.
    pub fn name_of(&self, id: u64) -> Option<String> {
        self.state.lock().names.get(&id).cloned()
    }
}

impl RoomState {
    /// Moves on to `track`, returning the likes of the song before it.
    fn move_to(&mut self, track: Option<&Arc<TrackInfo>>) -> Vec<Like> {
        if track.is_some() && self.track.as_ref().map(|t| t.id) == track.map(|t| t.id) {
            return Vec::new();
        }
        self.votes.clear();
        let likes = std::mem::take(&mut self.likes);
        let previous = std::mem::replace(&mut self.track, track.cloned());
        let Some(previous) = previous.filter(|_| !likes.is_empty()) else {
            return Vec::new();
        };
        self.session
            .push((previous.clone(), likes.values().cloned().collect()));
        let at = now_unix();
        likes
            .keys()
            .map(|&by| Like {
                at,
                uri: previous.spotify_uri(),
                title: previous.title.clone(),
                artists: previous.artists_joined(),
                cover: previous.cover_thumb_url.clone(),
                by,
            })
            .collect()
    }

    /// Votes from people still in the call.
    fn votes_from(&self, people: &[Listener]) -> usize {
        self.votes
            .iter()
            .filter(|v| people.iter().any(|l| l.id == **v))
            .count()
    }
}

fn save_likes(app: &App, likes: Vec<Like>) {
    if likes.is_empty() {
        return;
    }
    let store = app.store.clone();
    tokio::task::spawn_blocking(move || store.append_likes(&likes));
}

/// A new song started: the last one's likes are kept, and votes start over.
pub fn song_changed(app: &App, track: &Arc<TrackInfo>) {
    let likes = app.room.state.lock().move_to(Some(track));
    save_likes(app, likes);
}

/// Loski left: the last song's likes are kept, and the session's liked
/// songs (with who liked them) go to the recap.
pub fn session_over(app: &App) -> Vec<(Arc<TrackInfo>, Vec<String>)> {
    let (likes, session) = {
        let mut st = app.room.state.lock();
        let likes = st.move_to(None);
        (likes, std::mem::take(&mut st.session))
    };
    save_likes(app, likes);
    session
}

/// The heart on the panel: anyone in the call can like the song, and
/// pressing it again takes the like back. Returns whether it's liked now.
pub fn toggle_like(app: &App, track: &Arc<TrackInfo>, user: u64, name: &str) -> Result<bool> {
    if !listeners(app).iter().any(|l| l.id == user) {
        return Err(oops("Join the voice channel to like songs."));
    }
    let (earlier, liked) = {
        let mut st = app.room.state.lock();
        // The song may have changed before its notice arrived here.
        let earlier = st.move_to(Some(track));
        let liked = if st.likes.remove(&user).is_some() {
            false
        } else {
            st.likes.insert(user, name.to_string());
            true
        };
        (earlier, liked)
    };
    save_likes(app, earlier);
    Ok(liked)
}

/// A vote to skip this song from someone who can't skip by themselves.
/// Returns the votes so far and how many it takes (more than half the call).
pub fn vote_skip(app: &App, track: &Arc<TrackInfo>, user: u64) -> Result<(usize, usize)> {
    if app.prefs().vote_skip_off {
        return Err(oops("Voting to skip is off."));
    }
    let people = listeners(app);
    if !people.iter().any(|l| l.id == user) {
        return Err(oops("Join the voice channel to vote."));
    }
    let (earlier, votes) = {
        let mut st = app.room.state.lock();
        let earlier = st.move_to(Some(track));
        st.votes.insert(user);
        (earlier, st.votes_from(&people))
    };
    save_likes(app, earlier);
    Ok((votes, people.len() / 2 + 1))
}

/// Who's in the call and what they make of this song, for the panel.
#[derive(Debug, Clone, Default)]
pub struct RoomView {
    pub listeners: Vec<String>,
    pub liked_by: Vec<String>,
    /// Votes to skip this song so far, and how many it takes.
    pub votes: Option<(usize, usize)>,
}

pub fn room_view(app: &App, track: Option<&TrackInfo>) -> RoomView {
    let people = listeners(app);
    let st = app.room.state.lock();
    let same = track.is_some() && st.track.as_ref().map(|t| t.id) == track.map(|t| t.id);
    if !same {
        return RoomView {
            listeners: people.into_iter().map(|l| l.name).collect(),
            ..RoomView::default()
        };
    }
    let votes = st.votes_from(&people);
    RoomView {
        liked_by: st.likes.values().cloned().collect(),
        votes: (votes > 0).then_some((votes, people.len() / 2 + 1)),
        listeners: people.into_iter().map(|l| l.name).collect(),
    }
}

/// Someone came or went: the history learns who you're listening with,
/// the panel shows it, and new friends' names are remembered.
pub fn people_changed(app: &App) {
    let people = listeners(app);
    let present: Vec<(u64, String)> = people.iter().map(|l| (l.id, l.name.clone())).collect();
    {
        // Voice updates come for the whole server (mutes, other channels).
        let mut st = app.room.state.lock();
        if st.present == present {
            return;
        }
        st.present = present;
    }
    let owner = app.owner_id().get();
    let friends: Vec<&Listener> = people.iter().filter(|l| l.id != owner).collect();
    if let Some(core) = app.core.get() {
        core.engine
            .set_audience(friends.iter().map(|l| l.id).collect());
    }
    let names = {
        let mut st = app.room.state.lock();
        let mut changed = false;
        for friend in &friends {
            if st.names.get(&friend.id) != Some(&friend.name) {
                st.names.insert(friend.id, friend.name.clone());
                changed = true;
            }
        }
        changed.then(|| st.names.clone())
    };
    if let Some(names) = names {
        let store = app.store.clone();
        tokio::task::spawn_blocking(move || store.save_names(&names));
    }
    app.ui.mark_dirty();
}

/// Songs your friends liked (not you), most liked first, then most recent.
pub fn friends_liked(app: &App) -> Vec<String> {
    let owner = app.owner_id().get();
    let mut songs: HashMap<String, (usize, i64)> = HashMap::new();
    for like in app.store.all_likes() {
        if like.by == owner {
            continue;
        }
        let song = songs.entry(like.uri).or_default();
        song.0 += 1;
        song.1 = song.1.max(like.at);
    }
    let mut songs: Vec<_> = songs.into_iter().collect();
    songs.sort_by(|a, b| (b.1.0, b.1.1, &a.0).cmp(&(a.1.0, a.1.1, &b.0)));
    songs.into_iter().map(|(uri, _)| uri).collect()
}

/// "**Luca**, **Ana** and 3 more".
pub fn names_list(names: &[String], shown: usize) -> String {
    let mut list: Vec<String> = names
        .iter()
        .take(shown)
        .map(|n| format!("**{}**", escape_md(&truncate(n, 32))))
        .collect();
    let more = names.len().saturating_sub(shown);
    match (list.len(), more) {
        (0, _) => String::new(),
        (_, 0) if list.len() > 1 => {
            let last = list.pop().unwrap_or_default();
            format!("{} and {last}", list.join(", "))
        }
        (_, 0) => list.join(""),
        (_, more) => format!("{} and {more} more", list.join(", ")),
    }
}

fn option(label: &str, value: &str, description: &str, current: bool) -> SelectMenuOption {
    let mut option = cards::option(label, value, Some(description.to_string()), None);
    option.default = current;
    option
}

fn picker(
    id: &str,
    kind: SelectMenuType,
    max: u8,
    placeholder: &str,
    chosen: Vec<SelectDefaultValue>,
) -> Component {
    cards::row(vec![Component::SelectMenu(SelectMenu {
        id: None,
        channel_types: None,
        custom_id: id.to_string(),
        default_values: (!chosen.is_empty()).then_some(chosen),
        disabled: false,
        kind,
        max_values: Some(max),
        // Clearing it takes everyone off.
        min_values: Some(0),
        options: None,
        placeholder: Some(placeholder.to_string()),
        required: None,
    })])
}

/// The Friends part of /settings: co-DJs, the DJ role, what they may do,
/// requests and votes to skip.
pub fn friends_card(icons: &Icons, prefs: &Prefs) -> Component {
    let djs = prefs
        .co_djs
        .iter()
        .filter_map(|id| Id::new_checked(*id))
        .map(SelectDefaultValue::User)
        .collect();
    let role = prefs
        .dj_role
        .and_then(Id::new_checked)
        .map(SelectDefaultValue::Role)
        .into_iter()
        .collect();
    let who = match (prefs.co_djs.len(), prefs.dj_role) {
        (0, None) => "Nobody but you controls me right now.".to_string(),
        (0, Some(role)) => format!("Everyone with <@&{role}> is a co-DJ."),
        (n, None) => format!("{n} co-DJ{} by name.", if n == 1 { "" } else { "s" }),
        (n, Some(role)) => format!(
            "{n} co-DJ{} by name, and everyone with <@&{role}>.",
            if n == 1 { "" } else { "s" }
        ),
    };
    cards::card(
        Some(color::BRAND),
        vec![
            cards::text(format!(
                "{} **Friends**\nCo-DJs can run me too, as much as you let them. You can take it back anytime.",
                icons.md(Icon::Friends)
            )),
            cards::divider(),
            cards::text(format!("-# CO-DJS\n{who}")),
            picker(
                "loski:friends:djs",
                SelectMenuType::User,
                10,
                "Co-DJs by name",
                djs,
            ),
            picker(
                "loski:friends:role",
                SelectMenuType::Role,
                1,
                "DJ role: everyone with it is a co-DJ",
                role,
            ),
            cards::row(vec![cards::select(
                "loski:friends:level",
                "What co-DJs can do",
                false,
                vec![
                    option(
                        "Co-DJs control everything",
                        "full",
                        "Pause, skip, volume, the queue, filters, stop",
                        !prefs.djs_queue_only,
                    ),
                    option(
                        "Co-DJs only add songs",
                        "queue",
                        "/play and the queue card, nothing that changes what's on",
                        prefs.djs_queue_only,
                    ),
                ],
            )]),
            cards::text(
                "-# EVERYONE IN THE SERVER\nYour library, stats, history, mixes and settings stay yours either way.",
            ),
            cards::row(vec![cards::select(
                "loski:friends:requests",
                "Song requests",
                false,
                vec![
                    option(
                        "Song requests: anyone can ask",
                        "on",
                        "With /request; they wait on one card for you or a co-DJ",
                        !prefs.requests_off,
                    ),
                    option(
                        "Song requests: off",
                        "off",
                        "/request tells people requests are off",
                        prefs.requests_off,
                    ),
                ],
            )]),
            cards::row(vec![cards::select(
                "loski:friends:votes",
                "Votes to skip",
                false,
                vec![
                    option(
                        "Vote to skip: on",
                        "on",
                        "Anyone in the call presses Skip; more than half skips",
                        !prefs.vote_skip_off,
                    ),
                    option(
                        "Vote to skip: off",
                        "off",
                        "Only you and co-DJs can skip",
                        prefs.vote_skip_off,
                    ),
                ],
            )]),
            cards::row(vec![Btn::label("loski:friends:back", "Back").build(icons)]),
        ],
    )
}

/// A menu or button on the Friends card; `key` is what follows `loski:friends:`.
pub async fn change(ix: &Ix, key: &str, values: &[String]) -> Result<()> {
    let app = &ix.app;
    let value = values.first().map(String::as_str);
    let owner = app.owner_id().get();
    match key {
        "djs" => {
            let mut djs: Vec<u64> = values
                .iter()
                .filter_map(|v| v.parse().ok())
                .filter(|id| *id != owner)
                .collect();
            djs.dedup();
            app.update_prefs(|p| p.co_djs = djs);
        }
        "role" => {
            let role = value.and_then(|v| v.parse().ok());
            app.update_prefs(|p| p.dj_role = role);
        }
        "level" => app.update_prefs(|p| p.djs_queue_only = value == Some("queue")),
        "requests" => app.update_prefs(|p| p.requests_off = value == Some("off")),
        "votes" => {
            app.update_prefs(|p| p.vote_skip_off = value == Some("off"));
            app.ui.mark_dirty();
        }
        "back" => return settings::show(ix).await,
        _ => {}
    }
    show(ix).await
}

pub async fn show(ix: &Ix) -> Result<()> {
    let card = friends_card(&ix.app.icons, &ix.app.prefs());
    ix.update_message(vec![card]).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_by_command_and_button() {
        assert_eq!(command_needs("request"), Access::Listener);
        assert_eq!(command_needs("play"), Access::Queue);
        assert_eq!(command_needs("skip"), Access::Full);
        assert_eq!(command_needs("settings"), Access::Owner);
        assert_eq!(command_needs(library::PLAY_WITH), Access::Full);
        assert_eq!(command_needs(library::QUEUE_WITH), Access::Queue);
        assert_eq!(component_needs("loski:like"), Access::Listener);
        assert_eq!(component_needs("loski:skip"), Access::Listener);
        assert_eq!(component_needs("loski:toggle"), Access::Full);
        assert_eq!(component_needs("pickgo:12:next"), Access::Queue);
        assert_eq!(component_needs("pickgo:12:now"), Access::Full);
        assert_eq!(component_needs("loski:qsavemix"), Access::Owner);
        assert_eq!(component_needs("loski:qsel:0"), Access::Queue);
        assert_eq!(component_needs("loski:mixsel"), Access::Owner);
        assert_eq!(component_needs("req:all:next"), Access::Queue);
        assert!(Access::Owner > Access::Full && Access::Full > Access::Queue);
    }

    #[test]
    fn co_djs_by_name_or_role() {
        let prefs = Prefs {
            co_djs: vec![5],
            dj_role: Some(9),
            ..Prefs::default()
        };
        assert!(is_co_dj(&prefs, 5, &[]));
        assert!(is_co_dj(&prefs, 6, &[Id::new(9)]));
        assert!(!is_co_dj(&prefs, 6, &[Id::new(8)]));
        assert!(!is_co_dj(&Prefs::default(), 5, &[]));
        assert_eq!(co_dj_access(&prefs), Access::Full);
    }

    #[test]
    fn lists_names_nicely() {
        let names: Vec<String> = ["Ana", "Vlad", "Ioana", "Luca", "Maria"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(names_list(&names[..1], 3), "**Ana**");
        assert_eq!(names_list(&names[..2], 3), "**Ana** and **Vlad**");
        assert_eq!(
            names_list(&names, 3),
            "**Ana**, **Vlad**, **Ioana** and 2 more"
        );
        assert_eq!(names_list(&[], 3), "");
    }
}
