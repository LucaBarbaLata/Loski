# Changelog

What changed in each Loski release. Loski DMs you when a new one is out; `.\loski update` brings it in.

## 3.0.0 (2026-10-29)

### Added

- Reorder the queue from the queue card: pick a song, then move it up or down a step at a time (it stays picked), or straight to any position.
- Log in to Spotify from anywhere, for Loski on a server: the login DM has the two approvals as buttons, and you send Loski the address the browser ends up on. Signing back in to search works the same way.
- `/customize`: make the bot your own. Its name, picture, banner and About me from one card, with the pictures uploaded right there, or a nickname, picture, banner and bio for one server. Its messages, commands, right-click entries, Spotify Connect device and stats pictures go by its name.
- An interactive `/help`, only you see it: a page per topic with clickable commands (they fill in the chat box) and buttons that do the main things, and **Find a command** for whatever you'd like to do. Everyone sees only what they may use, and the welcome DM is the same guide.
- Party mode for song requests, in `/settings` → Friends: everyone in the call votes on the waiting requests, and the one with the most votes plays next, by itself.
- Panel styles, in `/settings`: the artwork banner as Glass (as before), Vinyl (the cover as a sleeve, with the record sliding out) or Minimal (the cover beside a flat colour from it).
- Voice commands (beta), in `/voice`: say "Loski, skip", "Loski, pause" or "Loski, play" and a song in the call. Speech is understood on the computer Loski runs on, with a 60 MB model downloaded when you turn them on, and only for people who said yes. English only for now.
- Voice commands work with music playing from your speakers or someone else talking nearby: Loski tells speech apart from what's around it now, instead of waiting for silence that never came (on the bench, with music 10 dB under the voice, 47 of 50 commands come through, up from none). With someone else talking nearby, Loski also finds its name after what they said.
- Talking back (beta): Loski answers voice commands out loud in the call. The music fades down before it talks and comes back gradually after. Pick a voice in `/voice`: Chatterbox, the most natural, on an NVIDIA graphics card (a container of its own, turned on with `COMPOSE_PROFILES=gpu-voice`), or simpler ones through Piper anywhere else. The usual replies are made ahead, so they come at once.
- `/voice` laid out anew: whether Loski is listening to you, with its button beside it; the commands grouped by what they do; and your settings (talking back, the speech model, other names) together at the bottom.
- A mic by someone's name on the panel: Loski is listening to them for voice commands.
- Voice commands on slower computers: a **Quick** speech model to pick in `/voice` (31 MB, about 2× faster than the usual one), which `/voice` suggests when it says how long a command takes to understand on yours. A song asked for no longer gets a second listen there when the first was slow.
- `/stats` and Wrapped say how long Loski played in all, apart from your listening: every minute, with nobody in the call and songs skipped early included. Counted from this version on.
- More voice commands: "what's playing?", "play … next", "add …", "play my liked songs", "play my playlist …", "more like this", "repeat this song / the queue / off", "forward 30 seconds", "back 10 seconds", "sleep in 30 minutes", "stop after this song". A listener's "play …" goes on the requests card. **Not this one?** under a song played by voice opens the search for it.
- Voice commands learn how your artists are spelled from what you play, and Loski can answer to other names, set in `/voice`. Talking about Loski at length no longer gets a "didn't get that".
- Voice commands in two goes: say just "Loski", and the command after its "yes?".
- Voice commands: **Last heard** on the `/voice` card (what was said to Loski, what it made of it, what it did and how fast), and a Voice commands page in `/help`.
- `/panel`: bring the now playing panel to the channel you're in, until Loski leaves the call.
- `/join`: bring Loski into your voice channel without playing anything yet.
- `/24-7`: switch 24/7 on or off without opening `/settings`. Turned on while you're in a call, Loski joins you and gets the music going.
- `INSTALLATION.md`: every way to run Loski, step by step. Windows, macOS and Linux, a server or VPS, a Raspberry Pi, a NAS or Portainer, plain `docker run` and building it yourself, plus updating, backups and moving.

### Changed

- The setup page's steps sit tighter, without the gap above each heading.
- On Windows, `loski start` says when Hyper-V has reserved Loski's login ports, and how to free them.
- Loski no longer renames the bot to "Loski" on every start, or puts its own picture over one you set in Discord's developer portal. A new bot gets Loski's look, and what you change stays. `BOT_NAME` still names it.
- The invite link also asks for Change Nickname, for a nickname of its own in a server.
- The panel lives in the chat of the voice channel Loski is in, and stays there wherever `/play` is used (unless you picked a panel channel in `/settings`). It follows Loski to another channel. `/nowplaying` now shows what's playing, on a card only you see, and anyone can use it.
- The banner shows the song's genre instead of "320 kbps", and explicit songs say "Explicit" instead of just "E", there and on the search card.
- Voice commands answer short commands ("Loski, stop") that used to come out as just "L", only listen when the name comes first (so "I told Loski to stop" doesn't pause the music), and can't get stuck writing the same words over and over.
- 24/7 keeps playing when nobody's in the call, instead of pausing until someone joins. Music played to an empty room doesn't count in your stats or history.

### Fixed

- Picking a song on the first page of a long queue gave an error: two of the card's buttons had the same id. Every card is now checked for that.
- After a network blip, the panel stopped updating until the next song; it's put back up within seconds now.
- "Loski, play …" plays the song right away (and so carries on after "Loski, stop"), instead of adding it to the end of a long queue. A co-DJ who may only add songs still queues it.

- Plain `docker compose up -d` on Linux: Loski couldn't write to the `data` folder Docker created, so nothing was saved. Loski now hands the folder to its own user as it starts (to you, when `loski.sh` starts it).

## 2.0.0 (2026-10-26)

### Added

- Live lyrics: the panel's Lyrics button posts a card that follows the song line by line, and `/lyrics` shows the words of any song. From Spotify, or LRCLIB when Spotify has none.
- The song in the voice channel's status. Needs the Set Voice Channel Status permission; the invite link now asks for it.
- When the Spotify app is in charge, the panel's up next is the app's own queue.
- A DM when a new Loski release is out, with what changed, and the running version in `/status`.
- `/settings`: the panel channel, a compact panel, leave timers, 24/7, crossfade, volume levelling, audio quality, timezone and song requests, changed from Discord and saved in `data/preferences.json`.
- 24/7 mode: Loski stays in voice, comes back after a restart, keeps going with a radio when the queue runs out, and pauses while nobody's listening.
- My sound: your own five-band EQ, next to the other sound filters.
- A compact panel without artwork, for busy chats and phones.
- Undo for `/clear`, `/remove`, `/dedupe` and `/shuffle`.
- Song requests can be switched off.
- Co-DJs: pick friends by name, or a DJ role, to control Loski fully or only add songs.
- Likes: everyone in the call can press ♥ on the panel; the recap shows the most liked songs, and `/liked from:friends` plays them.
- Who's listening, on the panel, and who you listened with (and how long) in the recap and the history.
- Votes to skip for everyone in the call.
- Smart shuffle: songs by the same artist don't play back to back.
- Loski Wrapped: your year in two pictures with `/wrapped`, and in your DMs on January 1st.
- Top genres on `/stats` and its picture, from MusicBrainz.
- A Together tab on `/stats`: time and top songs with each friend.
- `/history` searches by words, time, a friend in the call, or friends' requests.
- Blend: a mix of your top songs and friends' likes and requests, remade every Monday.
- Milestone DMs: hours with Loski, songs played, days in a row.
- A setup page: without keys yet, http://127.0.0.1:5588 asks for the Discord token and Spotify keys, checks them and saves them. No `.env` editing.
- Claiming: without `OWNER_ID`, whoever DMs Loski `claim` and the code from the setup page becomes the owner.
- `loski.sh`: the launcher for Linux and macOS, with the same commands as `loski.cmd`. On Linux it runs Loski as you.
- `loski backup` and `loski restore` to move your history, mixes, settings and Spotify login to another computer.
- Each version tag becomes a GitHub release, with its notes from this changelog.
- Pictures of the artwork, `/stats` and Wrapped in the README.

### Changed

- A new frosted-glass look for the artwork banner, the stats picture and the panel.
- The panel shows the next two songs with their covers, and puts Lyrics on its second row. Open in Spotify moved under the buttons, and the Sound menu joined More, so the panel has one menu instead of two.
- `/history` shows the five newest songs with their covers.
- Song requests gather on one card instead of one card each, and a co-DJ's requests go straight into the queue.
- Loski leaves a minute after the last person who runs it leaves the call, not only you.
- `.env.example` leaves `OWNER_ID` empty, for claiming; a missing `.env` is made from it.

## 1.0.0 (2026-10-26)

The first public release.

### Playback

- Real Spotify audio at 320 kbps through librespot, sent to Discord at the best bitrate the server allows.
- Gapless playback, optional crossfade that keeps albums gapless, skipped silence at the start and end of songs, and a limiter.
- Sound filters (bass, treble and vocal boost, nightcore, slowed + reverb, 8D), soft pause, smooth volume, a sleep timer and volume levelling.
- Spotify Connect: pick Loski in the Spotify app and your phone becomes the remote. Follow mode mirrors your own Spotify.

### In Discord

- A live now-playing panel with an artwork banner drawn for each song, and where the song is playing from.
- Search with tabs and covers, your Liked Songs, playlists and radio, and right-click "Play with Loski" on Spotify links.
- A queue card, history you can replay, saved mixes, and song requests from anyone in the server.
- Listening stats as a picture, a monthly recap in your DMs, and a recap of each session when Loski leaves.
- `/status`, and DMs when something breaks that Loski can't fix by itself.

### Running it

- `loski.cmd` to start, stop, update and check on Loski; daily log files.
- Prebuilt images for 64-bit Intel/AMD and ARM at `ghcr.io/lucabarbalata/loski`.
- MIT licence.
