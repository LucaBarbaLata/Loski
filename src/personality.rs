//! Loski's voice. Slightly smug about music, warm about everything else.

use rand::seq::IndexedRandom;

fn pick(lines: &[&'static str]) -> &'static str {
    lines.choose(&mut rand::rng()).copied().unwrap_or_default()
}

// Said out loud in the call (talking back): short, and plain words. Every
// one of these is made ahead (see `spoken_lines`), so they come at once.

const YES: [&str; 3] = ["Yes?", "I'm listening.", "Yeah?"];
const SKIPPED: [&str; 3] = ["Skipping!", "Next one.", "Skipped. Didn't like it either."];
const PAUSED: [&str; 3] = ["Paused.", "Pausing.", "Holding it right there."];
const RESUMED: [&str; 3] = ["Here we go.", "Back on.", "And we're back."];
const MISSED: [&str; 3] = [
    "Sorry, I didn't get that.",
    "Didn't catch that, sorry.",
    "Say that again?",
];
const BACK: [&str; 3] = ["Going back.", "One more time.", "Rewinding."];
const SHUFFLED: [&str; 2] = ["Shuffled.", "Shaking things up."];
const LIKED: [&str; 3] = ["Liked!", "Good taste.", "Heart added."];
const BYE: [&str; 3] = ["Bye!", "See you!", "Catch you later!"];
pub const VOTED_SKIP: &str = "The call voted. Skipping!";
pub const UNLIKED: &str = "Like taken back.";
pub const LIKED_SONGS: &str = "Playing your liked songs.";
pub const RADIO: &str = "Starting a radio from this song.";
pub const REPEAT_SONG: &str = "Repeating this song.";
pub const REPEAT_QUEUE: &str = "Repeating the whole queue.";
pub const REPEAT_OFF: &str = "Repeat's off.";
pub const AFTER_SONG: &str = "I'll stop after this song.";
pub const SLEEP_OFF: &str = "Sleep timer's off.";
const FAILED: [&str; 5] = [
    "Sorry, that one's for whoever runs me.",
    "Sorry, I couldn't find that one.",
    "Nothing's playing right now.",
    "I'm not in a call.",
    "Sorry, that didn't work.",
];

pub fn say_paused() -> &'static str {
    pick(&PAUSED)
}

pub fn say_resumed() -> &'static str {
    pick(&RESUMED)
}

pub fn say_skipped() -> &'static str {
    pick(&SKIPPED)
}

pub fn say_back() -> &'static str {
    pick(&BACK)
}

pub fn say_bye() -> &'static str {
    pick(&BYE)
}

pub fn say_shuffled() -> &'static str {
    pick(&SHUFFLED)
}

pub fn say_liked() -> &'static str {
    pick(&LIKED)
}

pub fn say_yes() -> &'static str {
    pick(&YES)
}

pub fn say_missed() -> &'static str {
    pick(&MISSED)
}

pub fn say_volume(level: u8) -> String {
    format!("Volume {level}.")
}

pub fn say_hello(name: &str) -> String {
    format!("Hi! It's {name}. I'll answer out loud from now on.")
}

/// What to say when a command didn't work, from what went wrong.
pub fn say_failed(error: &str) -> &'static str {
    let [refused, not_found, nothing, no_call, other] = FAILED;
    if error.contains("whoever runs me") {
        refused
    } else if error.contains("couldn't find") {
        not_found
    } else if error.contains("nothing's playing") {
        nothing
    } else if error.contains("not in a call") {
        no_call
    } else {
        other
    }
}

/// Every line Loski says out loud that's always the same, the most heard
/// first, to make them ahead.
pub fn spoken_lines(name: &str) -> Vec<String> {
    let mut lines: Vec<String> = [
        &YES[..],
        &SKIPPED,
        &PAUSED,
        &RESUMED,
        &MISSED,
        &BACK,
        &LIKED,
        &SHUFFLED,
        &BYE,
        &[
            VOTED_SKIP,
            UNLIKED,
            LIKED_SONGS,
            RADIO,
            REPEAT_SONG,
            REPEAT_QUEUE,
            REPEAT_OFF,
            AFTER_SONG,
            SLEEP_OFF,
        ],
        &FAILED,
    ]
    .concat()
    .into_iter()
    .map(String::from)
    .collect();
    lines.extend((0..=100).step_by(10).map(say_volume));
    lines.push(say_hello(name));
    lines
}

pub fn not_owner() -> &'static str {
    pick(&[
        "I'm a one-person band, and you're not the person. 🎧",
        "Flattered, but I only take requests from my human.",
        "Nice try. My aux cord has exactly one owner.",
        "Sorry, private DJ. Ask the person who owns me nicely.",
    ])
}

pub fn queued() -> &'static str {
    pick(&[
        "Queued. Excellent taste, as usual.",
        "In line and ready to go.",
        "Added. This one's a vibe.",
        "Noted. I'll get to it.",
        "On the list. Good pick.",
    ])
}

pub fn now_starting() -> &'static str {
    pick(&[
        "Here we go.",
        "Volume up. Let's go.",
        "Dropping the needle.",
        "Starting the show.",
    ])
}

pub fn join_voice_first() -> &'static str {
    pick(&[
        "Hop into a voice channel first and I'll follow you in.",
        "Where am I supposed to play this? Join a voice channel first.",
        "I need a voice channel to sing in. Join one, then try again.",
    ])
}

pub fn nothing_playing() -> &'static str {
    pick(&[
        "Nothing's playing. The silence is lovely though.",
        "There's nothing playing right now. Try /play.",
        "My turntable is empty. Give me something with /play.",
    ])
}

pub fn skipped() -> &'static str {
    pick(&[
        "Skipped. We don't talk about that one.",
        "Next!",
        "Moving on.",
        "Skipped. No hard feelings, song.",
    ])
}

pub fn leaving_alone() -> &'static str {
    pick(&[
        "You left, so I'm heading out too. 👋",
        "No you, no music. See you next time. 👋",
        "Packing up the decks. Catch you later. 👋",
    ])
}

pub fn leaving_idle() -> &'static str {
    pick(&[
        "It's been quiet for a while, so I'm heading out. 👋",
        "Nothing left to play. Leaving the channel. 👋",
    ])
}

pub fn goodbye() -> &'static str {
    pick(&["Stopped and left. 👋", "Mic drop. 👋", "Show's over. 👋"])
}

pub fn nothing_found() -> &'static str {
    pick(&[
        "I couldn't find anything for that. Try different words or a Spotify link.",
        "Spotify came back empty-handed. Maybe check the spelling?",
    ])
}

pub fn pick_prompt() -> &'static str {
    pick(&[
        "Which one did you mean?",
        "Pick your poison.",
        "Found a few. Which one?",
    ])
}

pub fn shuffled() -> &'static str {
    pick(&[
        "Shuffled. Chaos mode on.",
        "Shuffled. Let fate decide.",
        "Shuffled the deck.",
    ])
}

pub fn sleep_reached() -> &'static str {
    pick(&[
        "Sleep timer's up. Good night. 🌙",
        "That's the sleep timer. Sweet dreams. 🌙",
        "Fading out for the night. 🌙",
    ])
}
