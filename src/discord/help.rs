//! /help: a card only you see, with a page per topic. Commands on it are
//! Discord's clickable ones (they fill in the chat box), each page has
//! buttons that do the thing, and "Find a command" searches by what you
//! want to do. Everyone sees only what they may use.

use std::collections::HashMap;

use anyhow::Result;
use twilight_model::{
    channel::message::Component,
    id::{
        Id,
        marker::{CommandMarker, GuildMarker},
    },
};

use super::{
    App,
    cards::{self, Btn, color},
    commands::{self, HelpCmd},
    friends::{self, Access},
    icons::{Icon, Icons},
    interact::Ix,
    library,
};
use crate::util::{escape_md, truncate};

/// What /help is about, one page each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    Playing,
    Queue,
    Sound,
    Phone,
    Music,
    Stats,
    Friends,
    Settings,
}

impl Topic {
    pub const ALL: [Topic; 8] = [
        Topic::Playing,
        Topic::Queue,
        Topic::Sound,
        Topic::Phone,
        Topic::Music,
        Topic::Stats,
        Topic::Friends,
        Topic::Settings,
    ];

    fn key(self) -> &'static str {
        match self {
            Topic::Playing => "playing",
            Topic::Queue => "queue",
            Topic::Sound => "sound",
            Topic::Phone => "phone",
            Topic::Music => "music",
            Topic::Stats => "stats",
            Topic::Friends => "friends",
            Topic::Settings => "settings",
        }
    }

    fn from_key(key: &str) -> Option<Topic> {
        Topic::ALL.into_iter().find(|t| t.key() == key)
    }

    fn title(self) -> &'static str {
        match self {
            Topic::Playing => "Playing",
            Topic::Queue => "The queue",
            Topic::Sound => "Sound",
            Topic::Phone => "From your phone",
            Topic::Music => "Your music",
            Topic::Stats => "Stats and recaps",
            Topic::Friends => "Friends",
            Topic::Settings => "Settings and look",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Topic::Playing => Icon::Play,
            Topic::Queue => Icon::Queue,
            Topic::Sound => Icon::Filters,
            Topic::Phone => Icon::Connect,
            Topic::Music => Icon::Liked,
            Topic::Stats => Icon::Stats,
            Topic::Friends => Icon::Friends,
            Topic::Settings => Icon::Settings,
        }
    }

    fn intro(self) -> &'static str {
        match self {
            Topic::Playing => "Join a voice channel first; I come to you.",
            Topic::Queue => "Songs line up behind the one playing.",
            Topic::Sound => "Changes are heard straight away, by everyone in the call.",
            Topic::Phone => "Control me from the Spotify app.",
            Topic::Music => "Your Spotify library, and what you've played with me.",
            Topic::Stats => "Counted from what you play with me.",
            Topic::Friends => "Everyone in the call can join in.",
            Topic::Settings => "Cards only you can see.",
        }
    }

    /// One line for the topic menu.
    fn summary(self) -> &'static str {
        match self {
            Topic::Playing => "Play, pause, skip, volume, lyrics",
            Topic::Queue => "What's next, and changing it",
            Topic::Sound => "Filters, crossfade, sleep timer",
            Topic::Phone => "Spotify Connect and follow mode",
            Topic::Music => "Liked Songs, playlists, history, mixes",
            Topic::Stats => "Stats, Wrapped, monthly recaps",
            Topic::Friends => "Requests, likes, votes, co-DJs",
            Topic::Settings => "Settings, customize, status",
        }
    }

    /// The page's buttons: what each runs (see `commands::shortcut`), its
    /// label and icon.
    fn actions(self) -> &'static [(&'static str, &'static str, Icon)] {
        match self {
            Topic::Playing => &[
                ("nowplaying", "Bring the panel down", Icon::Music),
                ("lyrics", "Lyrics", Icon::Lyrics),
            ],
            Topic::Queue => &[("queue", "Show the queue", Icon::Queue)],
            Topic::Sound => &[("mysound", "My sound", Icon::Filters)],
            Topic::Phone => &[],
            Topic::Music => &[
                ("history", "Recent plays", Icon::History),
                ("mix", "My mixes", Icon::Add),
            ],
            Topic::Stats => &[("stats", "My stats", Icon::Stats)],
            Topic::Friends => &[("friends", "Co-DJs", Icon::Friends)],
            Topic::Settings => &[
                ("settings", "Settings", Icon::Settings),
                ("customize", "Customize", Icon::Settings),
                ("status", "Status", Icon::Info),
            ],
        }
    }
}

/// One line of help.
pub struct Entry {
    pub topic: Topic,
    /// The commands it's about, shown as clickable commands in front.
    pub commands: &'static [&'static str],
    /// `{bot}` is the bot's name, `{play_with}` its right-click entry.
    pub text: &'static str,
    /// More words "Find a command" goes by.
    pub keywords: &'static str,
    pub needs: Access,
}

const fn entry(
    topic: Topic,
    commands: &'static [&'static str],
    text: &'static str,
    keywords: &'static str,
    needs: Access,
) -> Entry {
    Entry {
        topic,
        commands,
        text,
        keywords,
        needs,
    }
}

use Access::{Full, Listener, Owner, Queue};

pub const ENTRIES: &[Entry] = &[
    entry(
        Topic::Playing,
        &["play"],
        "a song, album, playlist or artist. Start typing for suggestions, or paste a Spotify link.",
        "song music start listen search link add album playlist artist",
        Queue,
    ),
    entry(
        Topic::Playing,
        &["pause", "resume"],
        "stop for a moment, and carry on.",
        "hold continue unpause",
        Full,
    ),
    entry(
        Topic::Playing,
        &["skip", "previous"],
        "the next song (or several), or back to the last one.",
        "next back forward restart",
        Full,
    ),
    entry(
        Topic::Playing,
        &["seek"],
        "jump to a time: `1:23`, `+15` or `-10`.",
        "time position forward rewind jump",
        Full,
    ),
    entry(
        Topic::Playing,
        &["volume"],
        "0 to 100, remembered for next time.",
        "loud quiet louder quieter level",
        Full,
    ),
    entry(
        Topic::Playing,
        &["nowplaying"],
        "bring the panel, with all the buttons, back to the bottom of the chat.",
        "panel controls player buttons",
        Queue,
    ),
    entry(
        Topic::Playing,
        &["lyrics"],
        "the words, line by line as the song plays, or for any song you name.",
        "words sing karaoke text",
        Listener,
    ),
    entry(
        Topic::Playing,
        &["voice"],
        "voice commands (beta): say \"{bot}, skip\" or \"{bot}, play …\" in the call.",
        "voice speak say talk microphone mic hands",
        Listener,
    ),
    entry(
        Topic::Playing,
        &[],
        "Right-click a message with a Spotify link → **Apps** → **{play_with}**.",
        "right click message link apps menu",
        Full,
    ),
    entry(
        Topic::Playing,
        &["join"],
        "bring me into your voice channel without playing anything yet.",
        "join come enter call channel summon connect",
        Queue,
    ),
    entry(
        Topic::Playing,
        &["stop"],
        "stop, clear the queue and leave the call. I also leave a minute after you do, unless 24/7 is on.",
        "leave quit disconnect end",
        Full,
    ),
    entry(
        Topic::Playing,
        &["24-7"],
        "I stay in the call and keep the music going, even with nobody there. A radio picks up when the queue runs out.",
        "24/7 247 always stay forever nonstop alone radio keep",
        Owner,
    ),
    entry(
        Topic::Queue,
        &["queue"],
        "what's coming up. Pick a song to play it now or next, move it, or remove it.",
        "list next upcoming order",
        Queue,
    ),
    entry(
        Topic::Queue,
        &["skipto"],
        "jump ahead to a song in the queue.",
        "jump go position",
        Full,
    ),
    entry(
        Topic::Queue,
        &["move"],
        "move a song somewhere else in the queue, or to play next.",
        "reorder order position up down",
        Full,
    ),
    entry(
        Topic::Queue,
        &["remove", "dedupe"],
        "take out a song or a run of them, or the repeats.",
        "delete drop duplicates repeats",
        Full,
    ),
    entry(
        Topic::Queue,
        &["shuffle", "repeat"],
        "mix up what's coming, or play a song or the queue again.",
        "random loop again",
        Full,
    ),
    entry(
        Topic::Queue,
        &["clear"],
        "empty the queue; the song playing carries on. Undo is there for a moment.",
        "empty wipe undo",
        Full,
    ),
    entry(
        Topic::Sound,
        &["filter"],
        "bass, treble or vocal boost, nightcore, slowed + reverb, 8D or My sound. Also in the panel's Sound menu.",
        "bass nightcore slowed reverb 8d effect vocal treble",
        Full,
    ),
    entry(
        Topic::Sound,
        &["crossfade"],
        "blend each song into the next; albums stay gapless.",
        "fade blend gap gapless transition",
        Full,
    ),
    entry(
        Topic::Sound,
        &["normalize"],
        "even out loud and quiet songs.",
        "level loudness normalise",
        Owner,
    ),
    entry(
        Topic::Sound,
        &["sleep"],
        "fade out and leave later, or after this song.",
        "timer bedtime night fade",
        Full,
    ),
    entry(
        Topic::Sound,
        &[],
        "**My sound** is your own five-band EQ, in Settings or the panel's More menu.",
        "eq equalizer equaliser bass treble mid",
        Owner,
    ),
    entry(
        Topic::Phone,
        &[],
        "Pick **{bot}** in the Spotify app's device list, and your phone becomes the remote.",
        "spotify connect device phone app remote",
        Owner,
    ),
    entry(
        Topic::Phone,
        &["follow"],
        "play along with whatever you're playing on Spotify.",
        "mirror sync spotify phone",
        Owner,
    ),
    entry(
        Topic::Music,
        &["liked"],
        "your Liked Songs, or `from:friends` for the ones your friends liked.",
        "likes favourites favorites hearts library",
        Owner,
    ),
    entry(
        Topic::Music,
        &["playlist"],
        "one of your playlists; start typing to see them.",
        "playlists library",
        Owner,
    ),
    entry(
        Topic::Music,
        &["radio"],
        "songs like the one playing, or like one you name.",
        "similar station recommendations more",
        Full,
    ),
    entry(
        Topic::Music,
        &["history"],
        "what you played, searched by words, time or friend. Play any of it again.",
        "recent played past search",
        Owner,
    ),
    entry(
        Topic::Music,
        &["mix save", "mix load"],
        "keep the queue under a name, and bring it back later.",
        "save load keep",
        Owner,
    ),
    entry(
        Topic::Music,
        &["mix list"],
        "your saved mixes, with **Blend**: your top songs and your friends' likes, new every Monday.",
        "mixes blend saved",
        Owner,
    ),
    entry(
        Topic::Stats,
        &["stats"],
        "listening time, top songs, artists and genres, streaks, and who you listen with.",
        "statistics top time numbers chart genres",
        Owner,
    ),
    entry(
        Topic::Stats,
        &["wrapped"],
        "your year in two pictures. It also arrives in your DMs on January 1st.",
        "year review annual",
        Owner,
    ),
    entry(
        Topic::Stats,
        &[],
        "A recap of the month arrives in your DMs on the 1st, and a note for milestones like your 1,000th song.",
        "monthly recap milestone dm",
        Owner,
    ),
    entry(
        Topic::Friends,
        &["request"],
        "ask for a song. Requests wait on one card for whoever runs me, or in party mode the call votes and the top one plays next.",
        "ask suggest want song party vote",
        Listener,
    ),
    entry(
        Topic::Friends,
        &[],
        "Press ♥ on the panel for songs you like, and Skip to vote to skip.",
        "like heart vote skip",
        Listener,
    ),
    entry(
        Topic::Friends,
        &[],
        "Co-DJs, picked by name or with a DJ role, get the controls you choose: Settings → Friends.",
        "co-dj dj role friends permissions control",
        Owner,
    ),
    entry(
        Topic::Settings,
        &["settings"],
        "the panel and its look (glass, vinyl, minimal or compact), leaving, 24/7, sound, requests and friends, in one card.",
        "options preferences leave panel channel compact timezone style vinyl minimal glass look",
        Owner,
    ),
    entry(
        Topic::Settings,
        &["customize"],
        "my name, picture, banner and About me, everywhere or in one server.",
        "name avatar picture banner profile bio nickname rename look",
        Owner,
    ),
    entry(
        Topic::Settings,
        &["status"],
        "whether Spotify, Discord and the audio are healthy.",
        "health ping problems working broken",
        Owner,
    ),
];

/// A server's commands by name, as Discord registered them.
pub type CommandIds = HashMap<String, Id<CommandMarker>>;

/// Commands as Discord's clickable ones, where it gave us their ids.
#[derive(Default)]
pub struct Mentions(CommandIds);

impl Mentions {
    pub fn new(ids: CommandIds) -> Self {
        Self(ids)
    }

    /// `/mix save` as `</mix save:123>`, or as code without an id.
    fn of(&self, command: &str) -> String {
        let base = command.split(' ').next().unwrap_or(command);
        match self.0.get(base) {
            Some(id) => format!("</{command}:{id}>"),
            None => format!("`/{command}`"),
        }
    }
}

/// Whose help it is.
pub struct HelpView {
    pub bot: String,
    pub access: Access,
    /// In a server (so the buttons that do things work), not a DM.
    pub in_server: bool,
    pub mentions: Mentions,
}

impl HelpView {
    pub fn read(app: &App, access: Access, guild: Option<Id<GuildMarker>>) -> Self {
        let ids = guild
            .and_then(|g| app.command_ids.lock().get(&g).cloned())
            .unwrap_or_default();
        Self {
            bot: app.name(),
            access,
            in_server: guild.is_some(),
            mentions: Mentions::new(ids),
        }
    }

    fn line(&self, entry: &Entry) -> String {
        let (play_with, _) = library::menu_names(&self.bot);
        let text = entry
            .text
            .replace("{bot}", &escape_md(&self.bot))
            .replace("{play_with}", &escape_md(&play_with));
        if entry.commands.is_empty() {
            return format!("- {text}");
        }
        let commands: Vec<String> = entry.commands.iter().map(|c| self.mentions.of(c)).collect();
        format!("- {} {text}", commands.join(" "))
    }

    fn visible(&self, topic: Topic) -> impl Iterator<Item = &'static Entry> + '_ {
        ENTRIES
            .iter()
            .filter(move |e| e.topic == topic && e.needs <= self.access)
    }

    fn topics(&self) -> Vec<Topic> {
        Topic::ALL
            .into_iter()
            .filter(|&t| self.visible(t).next().is_some())
            .collect()
    }
}

fn topic_menu(icons: &Icons, view: &HelpView, current: Option<Topic>) -> Component {
    let options = view
        .topics()
        .into_iter()
        .map(|topic| {
            let mut option = cards::option(
                topic.title(),
                topic.key(),
                Some(topic.summary().to_string()),
                Some(icons.emoji(topic.icon())),
            );
            option.default = Some(topic) == current;
            option
        })
        .collect();
    cards::row(vec![cards::select(
        "loski:help:topic",
        "Pick a topic…",
        false,
        options,
    )])
}

/// The first page: who you are to the bot, how to start, and the topics.
pub fn home_card(icons: &Icons, view: &HelpView) -> Component {
    let bot = escape_md(&view.bot);
    let intro = match view.access {
        Access::Owner => {
            "Your private Spotify DJ. You run me; co-DJs you pick get some of the controls, and everyone in the call can listen, like songs and vote to skip."
        }
        Access::Full => {
            "A private Spotify DJ, and you're one of its co-DJs: you can play, skip and change the queue."
        }
        Access::Queue => {
            "A private Spotify DJ, and you're one of its co-DJs: you can add songs and look at the queue."
        }
        Access::Listener => {
            "A private Spotify DJ. Everyone in the call can ask for songs, like them and vote to skip."
        }
    };
    let start = if view.access >= Access::Queue {
        format!(
            "**Get started:** join a voice channel and {} a song. The panel that shows up has the controls.",
            view.mentions.of("play")
        )
    } else {
        format!(
            "**Join in:** {} a song, press ♥ on the panel for songs you like, and Skip to vote.",
            view.mentions.of("request")
        )
    };
    let mut buttons = vec![
        Btn::icon("loski:help:find", Icon::Search)
            .with_label("Find a command")
            .build(icons),
    ];
    if view.in_server {
        buttons.push(Btn::label("loski:help:close", "Close").build(icons));
    }
    cards::card(
        Some(color::BRAND),
        vec![
            cards::text(format!("{} **{bot}**\n{intro}", icons.md(Icon::Music))),
            cards::text(start),
            cards::divider(),
            cards::text("-# Pick a topic, or find a command by what you'd like to do."),
            topic_menu(icons, view, None),
            cards::row(buttons),
        ],
    )
}

/// One topic's page: its commands, and buttons that do the main things.
pub fn topic_card(icons: &Icons, view: &HelpView, topic: Topic) -> Component {
    let lines: Vec<String> = view.visible(topic).map(|e| view.line(e)).collect();
    let mut parts = vec![
        cards::text(format!(
            "{} **{}**\n-# {}",
            icons.md(topic.icon()),
            topic.title(),
            topic.intro()
        )),
        cards::text(lines.join("\n")),
        cards::divider(),
        topic_menu(icons, view, Some(topic)),
    ];
    let mut buttons: Vec<Component> = if view.in_server {
        topic
            .actions()
            .iter()
            .filter(|(command, ..)| action_needs(command) <= view.access)
            .map(|(command, label, icon)| {
                Btn::icon(&format!("loski:help:do:{command}"), *icon)
                    .with_label(label)
                    .build(icons)
            })
            .collect()
    } else {
        Vec::new()
    };
    buttons.push(Btn::label("loski:help:home", "Home").build(icons));
    if view.in_server {
        buttons.push(Btn::label("loski:help:close", "Close").build(icons));
    }
    parts.push(cards::row(buttons));
    cards::card(Some(color::BRAND), parts)
}

/// "Find a command": the best matches for what you typed.
pub fn found_card(icons: &Icons, view: &HelpView, query: &str) -> Component {
    let found = search(view, query);
    let mut parts = vec![cards::text(format!(
        "{} **{}**",
        icons.md(Icon::Search),
        escape_md(&truncate(query, 80))
    ))];
    if found.is_empty() {
        parts.push(cards::text(
            "Nothing matches that. Try other words, or pick a topic.",
        ));
    } else {
        let lines: Vec<String> = found
            .iter()
            .map(|e| format!("{}\n-# in {}", view.line(e), e.topic.title()))
            .collect();
        parts.push(cards::text(lines.join("\n")));
    }
    parts.push(cards::divider());
    parts.push(topic_menu(icons, view, None));
    let mut buttons = vec![
        Btn::icon("loski:help:find", Icon::Search)
            .with_label("Find another")
            .build(icons),
        Btn::label("loski:help:home", "Home").build(icons),
    ];
    if view.in_server {
        buttons.push(Btn::label("loski:help:close", "Close").build(icons));
    }
    parts.push(cards::row(buttons));
    cards::card(Some(color::BRAND), parts)
}

/// Words that say nothing about which command you mean.
const FILLER: [&str; 24] = [
    "a", "an", "the", "to", "my", "me", "i", "it", "is", "do", "how", "can", "want", "wanna", "of",
    "on", "in", "for", "and", "this", "that", "some", "with", "you",
];

/// The entries that match `query` best, most first. A word counts most when
/// it names a command, then when it's one of the entry's keywords, then
/// anywhere in its text; "skipping" finds "skip".
pub fn search(view: &HelpView, query: &str) -> Vec<&'static Entry> {
    let words: Vec<String> = query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '/')
        .map(|w| w.trim_start_matches('/').to_string())
        .filter(|w| w.len() >= 2 && !FILLER.contains(&w.as_str()))
        .collect();
    let close = |word: &str, other: &str| {
        word == other
            || (word.len() >= 3
                && other.len() >= 3
                && (other.starts_with(word) || word.starts_with(other)))
    };
    let mut scored: Vec<(u32, usize, &'static Entry)> = ENTRIES
        .iter()
        .enumerate()
        .filter(|(_, e)| e.needs <= view.access)
        .map(|(i, e)| {
            let names: Vec<&str> = e.commands.iter().flat_map(|c| c.split(' ')).collect();
            let keywords: Vec<&str> = e.keywords.split(' ').collect();
            let text = e.text.to_lowercase();
            let text: Vec<&str> = text
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect();
            let score = words
                .iter()
                .map(|w| {
                    if names.iter().any(|n| close(w, n)) {
                        6
                    } else if keywords.iter().any(|k| close(w, k)) {
                        3
                    } else if text.iter().any(|t| close(w, t)) {
                        1
                    } else {
                        0
                    }
                })
                .sum();
            (score, i, e)
        })
        .filter(|(score, ..)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().take(5).map(|(.., e)| e).collect()
}

/// Who may press a page's button.
fn action_needs(command: &str) -> Access {
    match command {
        "mysound" | "friends" => Access::Owner,
        command => friends::command_needs(command),
    }
}

pub async fn help(ix: &Ix, _: HelpCmd) -> Result<()> {
    let view = HelpView::read(&ix.app, ix.access(), ix.guild_id);
    ix.reply_private(vec![home_card(&ix.app.icons, &view)])
        .await
}

/// A button or menu on the help card; `key` is what follows `loski:help:`.
pub async fn change(ix: &Ix, key: &str, value: Option<&str>) -> Result<()> {
    let app = &ix.app;
    let view = || HelpView::read(app, ix.access(), ix.guild_id);
    match key {
        "topic" => match value.and_then(Topic::from_key) {
            Some(topic) => {
                ix.update_message(vec![topic_card(&app.icons, &view(), topic)])
                    .await
            }
            None => ix.ack().await,
        },
        "home" => {
            ix.update_message(vec![home_card(&app.icons, &view())])
                .await
        }
        "find" => {
            ix.modal(
                "loski:m:helpfind",
                "Find a command",
                vec![cards::text_field(
                    "what",
                    "What would you like to do?",
                    None,
                    "skip a few songs",
                    None,
                    100,
                )],
            )
            .await
        }
        "close" => {
            ix.ack().await?;
            ix.delete_reply().await;
            Ok(())
        }
        _ => match key.strip_prefix("do:") {
            Some(command) if ix.access() < action_needs(command) => ix.refuse().await,
            Some(command) => commands::shortcut(ix, command).await,
            None => ix.ack().await,
        },
    }
}

/// "Find a command" came back.
pub async fn found(ix: &Ix, query: &str) -> Result<()> {
    let view = HelpView::read(&ix.app, ix.access(), ix.guild_id);
    ix.update_message(vec![found_card(&ix.app.icons, &view, query)])
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(access: Access) -> HelpView {
        HelpView {
            bot: "Loski".to_string(),
            access,
            in_server: true,
            mentions: Mentions::default(),
        }
    }

    #[test]
    fn entries_ask_what_their_commands_need() {
        for entry in ENTRIES {
            if let Some(needs) = entry
                .commands
                .iter()
                .map(|c| friends::command_needs(c.split(' ').next().unwrap()))
                .max()
            {
                assert_eq!(entry.needs, needs, "{:?}", entry.commands);
            }
        }
    }

    #[test]
    fn everyone_sees_only_what_they_may_use() {
        let listener = view(Access::Listener);
        assert_eq!(listener.topics(), [Topic::Playing, Topic::Friends]);
        assert!(
            listener
                .visible(Topic::Playing)
                .all(|e| e.needs == Access::Listener)
        );
        assert_eq!(view(Access::Owner).topics(), Topic::ALL);
        // Nothing a co-DJ can't use, on any page.
        let dj = view(Access::Queue);
        for topic in dj.topics() {
            assert!(dj.visible(topic).all(|e| e.needs <= Access::Queue));
        }
    }

    #[test]
    fn finds_commands_by_what_you_want() {
        let owner = view(Access::Owner);
        let first = |query: &str| search(&owner, query).first().map(|e| e.commands);
        assert_eq!(first("skip a few songs"), Some(&["skip", "previous"][..]));
        assert_eq!(first("make it louder"), Some(&["volume"][..]));
        assert_eq!(first("change the bot's picture"), Some(&["customize"][..]));
        assert_eq!(first("bass"), Some(&["filter"][..]));
        assert!(
            search(&owner, "reorder the queue")
                .iter()
                .any(|e| e.commands == ["move"])
        );
        assert_eq!(first("/lyrics"), Some(&["lyrics"][..]));
        assert!(search(&owner, "the a to").is_empty());
        // A listener can't find what they may not use.
        let listener = view(Access::Listener);
        assert!(search(&listener, "volume").is_empty());
    }

    #[test]
    fn mentions_are_clickable_when_the_id_is_known() {
        let mentions = Mentions::new(HashMap::from([("mix".to_string(), Id::new(42))]));
        assert_eq!(mentions.of("mix save"), "</mix save:42>");
        assert_eq!(mentions.of("play"), "`/play`");
    }
}
