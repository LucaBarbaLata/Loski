# Changelog

What changed in each Loski release. Loski DMs you when a new one is out; `.\loski update` brings it in.

## Unreleased

### Added

- Reorder the queue from the queue card: pick a song, then move it up or down a step at a time (it stays picked), or straight to any position.
- Log in to Spotify from anywhere, for Loski on a server: the login DM has the two approvals as buttons, and you send Loski the address the browser ends up on. Signing back in to search works the same way.

### Changed

- The setup page's steps sit tighter, without the gap above each heading.
- On Windows, `loski start` says when Hyper-V has reserved Loski's login ports, and how to free them.

### Fixed

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
