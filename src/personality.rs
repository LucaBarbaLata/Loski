//! Loski's voice. Slightly smug about music, warm about everything else.

use rand::seq::IndexedRandom;

fn pick(lines: &[&'static str]) -> &'static str {
    lines.choose(&mut rand::rng()).copied().unwrap_or_default()
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
