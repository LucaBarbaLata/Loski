# Loski 🎧

A private Discord music bot that streams **real Spotify audio** (your Premium account, 320 kbps) into a voice channel. You control it, along with any co-DJs you pick. Built in Rust on [librespot](https://github.com/librespot-org/librespot), [songbird](https://github.com/serenity-rs/songbird) and [twilight](https://github.com/twilight-rs/twilight), with every message drawn as a Discord card using Loski's own icon set.

> **Heads-up:** streaming Spotify through librespot isn't allowed by Spotify's terms of service. Loski is meant for private use with your own account. Spotify could, in theory, act on an account that does this.
>
> Loski is an independent project. It isn't affiliated with, endorsed by or sponsored by Spotify or Discord. Spotify and Discord are trademarks of their respective owners.

![The artwork Loski draws for each song on its now-playing panel](docs/images/banner.jpg)

<p>
  <img src="docs/images/stats.jpg" width="49%" alt="The /stats picture: listening time, a daily chart, top songs and top artists">
  <img src="docs/images/wrapped.jpg" width="49%" alt="Loski Wrapped: the year's minutes and highlights">
</p>

<sub>Drawn by Loski itself: the artwork on the now-playing panel, `/stats`, and Loski Wrapped. The songs in `/stats` and Wrapped are made up; the album cover belongs to its owners.</sub>

## Features

- Real Spotify playback at 320 kbps, sent to Discord at the best bitrate your server allows
- `/play` with live search suggestions as you type, or a pick list for free text
- Spotify links for songs, albums, playlists (including Spotify-made ones) and artists (top tracks), plus `spotify.link` short links
- Gapless playback, plus optional crossfade (`/crossfade`) that keeps albums gapless; silence at the start and end of songs is skipped, and a limiter keeps boosted sound from clipping
- Sound filters: bass, treble and vocal boost, nightcore, slowed + reverb and 8D, from `/filter` or the panel's Sound menu
- Soft pause and resume, smooth volume changes, and a sleep timer that fades out (`/sleep`)
- Spotify Connect: pick **Loski** in the Spotify app's device list and your phone becomes the remote
- Your library and stations: `/liked`, `/playlist` (your playlists suggested as you type), `/radio`
- Search with tabs for songs, albums, playlists and artists; right-click any message with a Spotify link to play it
- Follow mode: mirrors whatever you're playing on your own Spotify, in sync
- A live now-playing panel in the voice channel's chat: an artwork banner Loski draws for each song (frosted glass, a vinyl record sliding out of its sleeve, or minimal) with its genre, a colour taken from the cover, where the song is playing from, an icon progress bar, the next two songs with their covers (from your Spotify app's queue when it's in charge), and controls
- Live lyrics that follow the song line by line, from the panel's Lyrics button, and `/lyrics` for any song
- The song in the voice channel's status, so people see what's on before they join
- When Loski leaves, the panel turns into a recap of the session, with a button to keep it as a mix
- Search results as a card with covers: choose one, then Play now, Play next or Add to queue
- A queue card where you can pick any song to play now, move up or remove
- Follows you between voice channels; leaves a minute after you do, or stays around the clock with 24/7 mode, playing even to an empty room
- `/settings` in Discord for the panel channel, a compact panel, leaving, 24/7, crossfade, audio quality, timezone and song requests
- Make it your own with `/customize`: the bot's name, picture, banner and About me, everywhere or just in one server
- An interactive `/help`: a page per topic, clickable commands, buttons that do the thing, and a search for what you'd like to do
- Voice commands (beta): say "Loski, skip" or "Loski, play …" in the call, understood on your own computer and only for people who said yes
- My sound: your own five-band EQ, next to the sound filters
- Remembers your volume and crossfade settings; keeps a listening history you can replay from (`/history`)
- Listening stats as a picture: time, top songs, artists and genres, streaks and a daily chart (`/stats`), who you listen with, a recap in your DMs on the 1st of each month, and Loski Wrapped on January 1st (`/wrapped`)
- A Blend mix every Monday: your top songs woven with what friends liked or asked for
- Small DMs for milestones: 100 hours with Loski, your 1,000th song, 30 days in a row
- Saved mixes: keep a queue under a name, load, rename or delete it later (`/mix`, or "Save as mix" on the queue card)
- Song requests: anyone in the server can `/request` a song; waiting requests gather on one card, or in party mode the call votes and the top request plays next
- Friends: co-DJs by name or with a DJ role, a ♥ on the panel for everyone in the call, votes to skip, and who's listening
- Smart shuffle: songs by the same artist don't play back to back
- Queue tools: remove a run of songs, drop repeats (`/dedupe`), move a song to play next
- `/status` to check on Spotify, Discord and the audio at a glance, and a DM when a new Loski release is out
- Fixes itself where it can: rejoins voice after a dropout, reconnects to Spotify, and DMs you when something needs you
- Cleans up after itself: confirmations disappear after a few seconds, and queue changes can be undone for a moment

## Setup

You need Docker (Docker Desktop on Windows and macOS) and Spotify Premium. This is the quick way, on your own computer; a server or VPS, a Raspberry Pi, a NAS, plain Docker and building it yourself are in [INSTALLATION.md](INSTALLATION.md).

### 1. Start it

On Windows, double-click `loski.cmd`, or run it from a terminal in the repo folder:

```powershell
.\loski start
```

On Linux or macOS (more in [INSTALLATION.md](INSTALLATION.md#linux)):

```bash
./loski.sh start
```

It starts Docker if it isn't running, downloads the ready-made Loski image the first time (see [Prebuilt image](#prebuilt-image)), and waits until Loski says it's ready.

### 2. The setup page

The first time, your browser opens **http://127.0.0.1:5588**, which asks for three keys and shows where to get each one:

1. **A Discord bot.** At [discord.com/developers/applications](https://discord.com/developers/applications), click **New Application**, name it what your bot should be called (**Loski**, say; `/customize` changes it later), open **Bot**, click **Reset Token** and copy the token.
2. **A Spotify app.** At [developer.spotify.com/dashboard](https://developer.spotify.com/dashboard), click **Create app**, set the redirect URI to exactly `http://127.0.0.1:5588/callback`, tick **Web API**, and copy the **Client ID** and **Client Secret**.

The page checks them with Discord and Spotify and saves them in `data/setup.env`. (You can put them in `.env` yourself instead; `.env` always wins. See `.env.example`.)

### 3. Invite, claim and log in

Once the keys are saved, the same page shows the rest:

1. **Invite Loski** to your server with the button. It asks for View Channels, Send Messages, Embed Links, Read Message History, Connect, Speak, Set Voice Channel Status (for the song under the channel's name) and Change Nickname (for a nickname of its own, with `/customize`). No privileged intents are needed.
2. **Make it yours.** Send Loski a direct message saying `claim` and the six-digit code on the page (it's in the log too, and `loski start` prints it). Whoever sends it first becomes the owner, and the code is only on this computer, so nobody else in the server can beat you to it. Setting `OWNER_ID` in `.env` skips this.
3. **Log in to Spotify**: two quick approvals, one for streaming and one for search, from the page or the buttons in the DM Loski sends you. After that you never need to log in again. The login is stored in `./data`.

### Day to day

| Command | What it does |
| --- | --- |
| `.\loski start` | Start Docker Desktop if needed, then Loski. |
| `.\loski stop` | Stop Loski. It leaves voice and removes its panel first. |
| `.\loski restart` | Restart Loski. |
| `.\loski update` | Get the latest code and image from GitHub, then restart. A new image is ready a few minutes after a push, once the checks pass. |
| `.\loski build` | Build Loski from this folder instead of downloading it (for your own changes), then restart. |
| `.\loski logs` | Follow the live log. `.\loski logs files` opens the saved daily logs. |
| `.\loski status` | Whether Loski is running, and its last log lines. |
| `.\loski selftest` | Check the Spotify connection, search, radio and your library from inside Loski. |
| `.\loski backup` | Save your history, mixes, likes, settings, `.env` and Spotify login to `loski-backup-<date>.tar.gz`. |
| `.\loski restore <file>` | Bring a backup back, here or on another computer, and start Loski. What was there before is kept in `data.before-restore-<time>`. |

`loski.cmd` runs `scripts/loski.ps1` with permission to run just that script, so you don't have to change PowerShell's script policy. On Linux and macOS, `./loski.sh` takes the same commands. Plain `docker compose pull`, `docker compose up -d` and `docker compose down` work too.

A backup holds your Discord token and Spotify login, so keep it somewhere private; `.gitignore` already keeps it out of the repo.

### Other ways to run it

[INSTALLATION.md](INSTALLATION.md) has every way, step by step: Windows, macOS and Linux with the launchers, a server or VPS (with the Spotify login through Discord), a Raspberry Pi, a NAS or Portainer, plain `docker run`, and building it yourself. It also covers updating, backups, moving Loski to another computer, and what to do when something's off.

### Prebuilt image

Every push to `main` that passes the checks publishes an image to GitHub's container registry:

| Tag | What it is |
| --- | --- |
| `ghcr.io/lucabarbalata/loski:latest` | The newest build of `main`. This is what `docker-compose.yml` runs. |
| `ghcr.io/lucabarbalata/loski:sha-1a2b3c4` | One particular commit, for going back to a version that worked. |
| `ghcr.io/lucabarbalata/loski:1.2.0` | A release, when a `v1.2.0` tag is pushed. |

A `v1.2.0` tag also becomes a GitHub release, with the `1.2.0` part of `CHANGELOG.md` as its notes, once both images are up. That's what Loski's "a new version is out" DM reads, so every release needs its section in the changelog (the release job stops with an error otherwise). Tags with a dash, like `v1.3.0-beta.1`, become pre-releases, which nobody gets a DM about.

Each tag holds two builds, and Docker picks the right one by itself: `linux/amd64` for 64-bit Intel and AMD machines, and `linux/arm64` for 64-bit ARM, like a Raspberry Pi 4 or 5 on a 64-bit OS, an Apple Silicon Mac or an ARM server. Both are built natively on GitHub's own machines, side by side. To run Loski without the rest of the repo, `docker-compose.yml` and a filled-in `.env` are enough: `docker compose up -d` downloads the image.

## Commands

| Command | What it does |
| --- | --- |
| `/play song [next]` | Search, or paste a Spotify link. `next:True` puts it right after the current song. |
| `/liked [shuffle] [from]` | Your Liked Songs (shuffled unless you say otherwise), or `from:friends` for the songs your friends liked on the panel. |
| `/playlist name [shuffle]` | One of your playlists; start typing to see them. |
| `/radio [song]` | Spotify's radio for a song, or for the one playing. Replaces what's coming up. |
| `/pause`, `/resume` | Pause and resume. |
| `/skip [count]` | Skip one song, or several. |
| `/previous` | Restart the song, or go back if it just started. |
| `/seek position` | `1:23`, `95`, `+15` or `-10`. |
| `/nowplaying` | What's playing: the song, where it's at and what's next, on a card only you see. |
| `/panel` | Bring the panel to the channel you're in, until Loski leaves the call. It lives in the voice channel's chat otherwise. |
| `/queue [page]` | See what's coming up. |
| `/skipto position` | Jump ahead in the queue. |
| `/remove position [to]` | Remove a song from the queue, or a run of them (`/remove 3 to:7`). |
| `/dedupe` | Remove songs that are in the queue more than once. |
| `/move from [to]` | Reorder the queue. Leave out `to` to play the song next. |
| `/clear` | Empty the queue (the current song keeps playing). |
| `/shuffle` | Shuffle the queue. |
| `/repeat mode` | Off, this song, or the queue. |
| `/volume [level]` | 0–100. Remembered between sessions. |
| `/crossfade [seconds]` | 0–12 seconds. 0 means gapless. Songs from the same album always play gapless. |
| `/filter sound` | Bass boost, treble boost, vocal boost, nightcore, slowed + reverb, 8D, My sound, or off. |
| `/sleep when` | Fade out and leave in 15 minutes to 2 hours, after this song, or cancel. |
| `/normalize [on]` | Volume levelling between songs, like Spotify's setting. Remembered. |
| `/follow` | Toggle follow mode. |
| `/lyrics [song]` | Lyrics for any song, a page at a time (the one playing if you leave it out). Start typing to search. |
| `/history [search] [when] [with] [requests]` | Recently played songs, or search them: words from the title or artist, today / the last 7 or 30 days / the last year, songs with a friend in the call, or only friends' requests. Pick one to play it again. |
| `/stats [range]` | Listening time, top songs, artists and genres, streaks and when you listen. Tabs for the last 7 days, this month, all time and Together (time and songs with each friend), and a button that queues your top songs. |
| `/wrapped [year]` | Loski Wrapped: your year (so far) in two pictures. It also arrives in your DMs on January 1st. |
| `/mix save name` | Save every song in the queue (played ones too) under a name. Saving under a name you used replaces it. |
| `/mix load name [shuffle] [next]` | Add a saved mix to the queue. |
| `/mix list`, `/mix delete name` | See your mixes (and add one from the menu), or delete one. Up to 25. **Blend** is remade every Monday. |
| `/status` | Whether Spotify, Discord and the audio are healthy: login, response times, voice server, bitrate, buffer, uptime. |
| `/request song` | For everyone in the server: ask for a song. Waiting requests gather on one card where you or a co-DJ play them next, add them or decline them; in party mode, everyone in the call votes and the top request plays next. A co-DJ's own requests go straight into the queue. |
| `/settings` | Everything you can change without editing `.env`, in one card only you can see. |
| `/customize` | Make the bot your own: its name, picture, banner and About me, everywhere or in one server. |
| `/voice` | Voice commands (beta): you turn them on, and everyone says whether Loski may listen to them. |
| `/join` | Come into your voice channel without playing anything yet (or move to it). |
| `/24-7 [on]` | Switch 24/7 on or off (leave `on` out to switch it): Loski stays in the call and keeps the music going, even with nobody there. Turned on while you're in a call, it joins you. |
| `/stop` | Stop, clear the queue and leave. With 24/7 on, Loski stays away until the next `/play`. |
| `/help` | A guide only you see: a page per topic, with commands you can click to fill in and buttons that do the thing, and **Find a command** for whatever you'd like to do. It only shows what you may use. |
| Right-click → Apps → Play with Loski / Queue with Loski | Plays the Spotify link in that message, now or at the end of the queue. |

The queue card lets you pick a song to play now, play next, move up or down a step at a time, move to any position, or remove; it also saves the queue as a mix, and offers to remove repeats when there are any. The `/mix list` card does the same for mixes: add one to the queue, play it next, rename or delete it.

The panel's buttons cover previous, play/pause, skip, shuffle, repeat, volume, queue, lyrics and stop, with an Open in Spotify link under them. Its More menu jumps 15 seconds back or forward, jumps to a time you type, restarts the song, starts a radio from it, switches the sound filter, or moves the panel to the bottom of the chat. When the queue runs out, the panel offers to replay it.

`/clear`, `/remove`, `/dedupe` and `/shuffle` answer with an Undo button that puts the queue back as it was, for 15 seconds.

The Lyrics button posts a card under the panel that follows the song: the line being sung, the one before and the next few, with a button for all of them. It moves on to the next song by itself, and goes away with Close or when Loski leaves.

The artwork banner is drawn with the bundled Bricolage Grotesque and Noto Sans fonts (SIL Open Font License, see `assets/fonts`). Titles in scripts they don't cover, such as Japanese or Arabic, use a compact layout with the cover as a thumbnail instead.

## Settings

`/settings` opens a card only you can see. Every change is saved in `data/preferences.json` and used straight away, no restart:

- **Panel channel.** The panel lives in the chat of the voice channel Loski is in, wherever `/play` was used. Pick a channel and it always goes there instead; clear it to go back. `/panel` brings it to another channel until Loski leaves the call.
- **Panel look.** Full, with the artwork and up next, in one of three styles: **Glass** (the cover blurred behind a frosted panel), **Vinyl** (the cover as a sleeve with the record sliding out) or **Minimal** (the cover beside a flat colour from it). Or compact: no artwork, one row of buttons, and the rest (volume, lyrics, shuffle, repeat) in the More menu. Handy on a phone.
- **24/7** (`/24-7`, or in `/settings`). Loski stays in its voice channel instead of leaving, and comes back to it after a restart or an update. The music keeps going whether or not anyone's in the channel: when the queue runs out, a Spotify radio picks up from the last song. Time it plays to an empty room doesn't count in your stats. `/stop` (or Leave on the panel) sends it away until the next `/play`.
- **Leaving.** How long Loski waits after you leave the call, and after the music ends.
- **Sound.** Crossfade, volume levelling and the audio quality sent to Discord.
- **My sound.** Bass, low-mid, mid, high-mid and treble, each from -8 to +8 dB. Turn it on from the card, the panel's More menu or `/filter`.
- **Timezone.** When days start for `/stats` and the monthly recap.
- **Friends.** Co-DJs, song requests and votes to skip; see below.

The settings in `.env` (`LEAVE_AFTER_SECS`, `IDLE_LEAVE_SECS`, `AUDIO_BITRATE`, `TIMEZONE`, `NORMALISE`) still work as starting values; anything changed in `/settings` wins.

## Make it yours

`/customize` shows the bot's banner, picture, name and About me on a card only you can see. **Edit profile** changes any of them in one form, with the pictures uploaded right there (PNG, JPG, GIF or WebP). Discord allows two name changes an hour.

- **Its name goes everywhere:** its messages, its commands, the right-click entries ("Play with …"), the Spotify app's device list and the corner of the stats pictures.
- **In one server.** **Edit for this server** gives it a nickname, picture, banner and bio there only; **Same as everywhere** takes them off again.
- **Back to Loski's own** puts back the name, picture, banner or About me, or all four.

A new bot starts with Loski's own look. Whatever you change, here or in Discord's developer portal, stays as you set it.

## Voice commands (beta)

Talk to Loski in the call: its name first, then what you'd like.

- **Turning them on.** `/voice` → **Turn on** downloads a 60 MB speech model once (whisper.cpp's base English model, from its page on Hugging Face, checked against its fingerprint) into `data/models`, and adds you as the first person Loski listens to. Friends press **Listen to me** in `/voice`; nobody else is listened to, and a mic by someone's name on the panel shows who is.
- **What to say**, in English, the name first: "Loski, pause" (or stop), "resume" (or start), "skip", "go back", "volume 30", "louder", "quieter", "shuffle", "like this", "what's playing?", "play Numb by Linkin Park", "play Numb next", "add Numb", "play my liked songs", "play my playlist Gym", "more like this", "repeat this song", "forward 30 seconds", "back 10 seconds", "sleep in 30 minutes", "stop after this song", "leave". If a song it picked isn't the one you meant, **Not this one?** under it opens the search. Loski learns how your artists are spelled from what you play, and answers to other names you give it in `/voice`. With a name of your own from `/customize`, say that name instead.
- **Talking back.** Pick a voice under **Talking back** in `/voice` and Loski answers out loud: "Skipping!", "Playing Numb, by Linkin Park", "Yes?" when you say just its name. The music fades down before it talks and comes back gradually after. The most natural voice is [Chatterbox](https://github.com/resemble-ai/chatterbox), which runs on an NVIDIA graphics card in a container of its own (see [INSTALLATION.md](INSTALLATION.md#talking-back-with-your-graphics-card)). Without one there are simpler voices (Lessac, Ryan or Alan, about 60 MB each) through [Piper](https://github.com/rhasspy/piper), which comes with Loski's image. The usual replies are made ahead, so they come at once.
- **Or in two goes.** Say just "Loski", wait for its "yes?", and say the command within a few seconds.
- **What did it hear?** `/voice` shows the last few things said to Loski, what it made of them and what it did; your own other talk shows there too for two minutes, only to you. `/help` has a page with every command.
- **Same rules as the buttons.** Anyone can say "like this" and "what's playing", "skip" from a listener is their vote to skip, and "play …" from a listener goes on the requests card. A co-DJ who may add songs adds them at the end; playing now or next, and the rest, needs full control; your liked songs and playlists are yours. Loski answers in the chat, and when it hears its name but not a command, it says what it heard.
- **Private.** Loski stops deafening itself in calls while voice commands are on. Short bits of speech are turned into text on the computer Loski runs on and then forgotten; no audio is kept or sent anywhere. During the beta, what's said to Loski goes in its log, so the commands can be tuned; other talk is only counted.
- **What it needs.** An Intel or AMD processor with AVX2 (most from the last ten years) or a 64-bit ARM one; `/voice` says so if it can't run. It picks commands out over music from your speakers and other people talking, though headphones still help.
- **On a slower computer.** `/voice` says how long a command takes to understand on yours. A Raspberry Pi or an old laptop can pick the **Quick** speech model there (31 MB, whisper.cpp's tiny English model): about 2× faster, though a little less sure of what it hears.

## Friends

Nobody but you controls Loski until you say so, in `/settings` → Friends:

- **Co-DJs.** Pick people by name, or a role (say, **DJ**) and everyone who has it. Co-DJs either control everything (pause, skip, volume, the queue, filters, stop) or only add songs. Your Spotify library, stats, history, mixes and settings stay yours either way.
- **Likes.** Everyone in the call can press ♥ on the panel. It never changes the music; the recap shows the most liked songs, and `/liked from:friends` plays what your friends liked.
- **Who's listening.** The panel names who's in the call, and the recap says who you listened with and for how long.
- **Votes to skip.** Anyone in the call who isn't a co-DJ can press Skip to vote; once more than half the call has, the song skips. You can turn it off.
- **Requests.** `/request` adds to one card with every waiting request, where you or a co-DJ answer them one by one or all at once.
- **Party mode.** Song requests set to party mode: each request on the card gets a vote button for everyone in the call (asking counts as a vote), and the request with the most votes goes up next by itself, one at a time; on a tie, whoever asked first. You and co-DJs can still answer requests by hand.

Loski leaves a minute after the last person who runs it (you or a co-DJ) leaves the call, unless 24/7 is on.

## Good to know

- **When something breaks.** If voice drops, Loski rejoins on its own (and tells you if it can't). If the Spotify connection stays down, or Spotify signs it out of search, you get a DM with what to do, and another when it's fixed.
- **Logs.** Besides `docker logs`, Loski writes a log file per day to `data/logs` and keeps the last 14 days, so you can see what happened even after a restart.

- **Loski played.** `/stats` and Wrapped also say how long Loski played in all: every minute, with nobody in the call too, and songs skipped early. It's kept hour by hour in `data/played.json`, apart from your listening.
- **Stats and history.** A song counts once it has played for 30 seconds (half of it for very short songs), the same rule Spotify uses, and only the time it actually played is added up. Days and months start at midnight in your timezone, which is UTC until you set it in `/settings` (or `TIMEZONE=Europe/Bucharest` in `.env`). On the 1st of each month Loski DMs you a recap of the month before, and on January 1st Loski Wrapped for the year; if it was off that day, they come the next time it starts. History lives in `data/history.jsonl`, mixes in `data/mixes.json` and likes in `data/likes.jsonl`.
- **Genres** come from [MusicBrainz](https://musicbrainz.org), the open music encyclopedia, because Spotify no longer shares artist genres with apps like Loski. Loski looks up the artists you play, about one a second in the background, and keeps the answers in `data/genres.json`; artists MusicBrainz doesn't know are left out. Only artist names are sent. The banner shows the genre once the artist has been looked up, so a new artist's first song may go without.
- **Blend** is rebuilt every Monday (or the first time Loski runs after one) from your 30 most played songs of the last four weeks and up to 30 that friends liked or asked for, taking turns. It's saved as the mix **Blend**, so `/mix load name:Blend` plays it.
- **Milestones** (10, 25, 50, 100… hours; 100, 500, 1,000… songs; 7, 14, 30… days in a row) arrive as DMs. The ones you'd already passed when you updated aren't sent.

- **Lyrics** come from Spotify (the same ones its app shows, timed to the song), or from [LRCLIB](https://lrclib.net) when Spotify has none. Some songs have them only as plain text; then the card shows them a page at a time instead of following along.
- **Voice channel status.** If Loski's role doesn't have Set Voice Channel Status (invited before 2.0, say), everything else works and the log says once how to turn it on: Server Settings → Roles → Loski → Set Voice Channel Status.
- **Updates.** Once a day Loski checks GitHub for a newer release and DMs you about it once. `UPDATE_CHECK=false` turns that off.

- **Audio quality.** Loski sends Discord the highest bitrate your server's boost level allows: 96 kbps unboosted, up to 384 kbps at level 3. You can set it yourself with `AUDIO_BITRATE`.
- **Spotify Connect.** Loski shows up as a speaker in your Spotify app whenever it's running. Pick it and Loski joins the voice channel you're in and plays there; the app's play, skip, seek and volume all work, and Discord's buttons control it too. Pick another device to hand back. Running `/play` takes over again with the Discord queue.
- **Follow mode** plays the same song as your Spotify, at the same position. Your own device keeps playing too, so mute it to avoid hearing it twice. Pausing from Discord sticks until you change something in Spotify.
- **Spotify history and Wrapped.** Songs played through Loski don't count, because librespot doesn't report plays to Spotify. Loski keeps its own history instead.
- **Leaving.** Loski leaves 1 minute after you leave voice (`LEAVE_AFTER_SECS`), or 5 minutes after the queue runs out (`IDLE_LEAVE_SECS`). The queue isn't saved between sessions.
- **Other people** can listen, `/request` songs, like and vote to skip. Only the owner (`OWNER_ID`, or whoever claimed Loski) and the co-DJs they pick can use the other commands and buttons. A claimed owner is remembered in `data/owner`; delete it to claim Loski again.
- **Profile and icons.** A new bot gets Loski's own picture, banner and "About me", and Loski uploads its icons as its own emojis. What you change with `/customize` or in Discord's developer portal stays. `BOT_NAME` in `.env` names the bot at startup (a name picked in `/customize` wins), and `SYNC_PROFILE=false` leaves the profile alone.
- **Restarting.** `docker compose down` or a restart makes Loski leave voice and remove its panel first, so it never looks stuck in a channel.
- **"Ports are not available" on Windows.** Hyper-V now and then reserves a block of ports that includes Loski's login ports (5588 and 8898), and Docker can't start Loski then; `loski start` tells you when that's why. To free them, open PowerShell as administrator and run `net stop winnat; net start winnat` (or restart Windows), then start Loski again.

## Development

The project builds inside Docker, so you don't need a local Rust toolchain:

```bash
docker build -t loski-dev - <<'EOF'
FROM rust:1.99.1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends cmake pkg-config libopus-dev clang libclang-dev && rustup component add clippy rustfmt
EOF
docker run --rm -v "$PWD:/src" -v loski-cargo:/usr/local/cargo/registry -v loski-target:/target -e CARGO_TARGET_DIR=/target -w /src loski-dev cargo test
```

The tests need no Spotify or Discord connection. They cover the queue, the sound filters, stats, every card against Discord's limits, and the playback engine, which runs whole sessions (gapless, crossfade, repeat, sleep timer, unplayable songs, history) against fake players that record what they're told.

Voice commands have a bench of their own: spoken commands and everyday talk, played clean, over music and over someone else talking, through the same speech cutting and model Loski uses. It reports how many commands work, how often talk sets something off, and how long answers take. Make the clips on Windows with `scripts/voice-bench.ps1` (into `data/voice-bench`), then run it with the speech model from `data/models`:

```bash
docker run --rm -v "$PWD:/src" -v loski-cargo:/usr/local/cargo/registry -v loski-target:/target -e CARGO_TARGET_DIR=/target -e LOSKI_SPEECH_MODEL=data/models/ggml-base.en-q5_1.bin -e LOSKI_BENCH=data/voice-bench -w /src loski-dev cargo test voice_bench -- --ignored --nocapture
```

On GitHub, `.github/workflows/checks.yml` runs formatting, clippy and the tests on every push, then builds the Docker image. For `main` and version tags it also publishes the image (see [Prebuilt image](#prebuilt-image)); pull requests only build it.

How the code is laid out:

```
src/
  audio.rs          librespot → crossfading mixer → songbird, with click-free cuts
  dsp.rs            sound filters: EQ presets, nightcore, slowed + reverb, 8D
  engine/           queue, two playback decks, gapless and crossfade rules, follow mode,
                    Spotify Connect, sleep timer, listening time (tests.rs drives it all)
  spotify/          librespot session, one-time login server, Web API search, your library
  discord/          gateway, slash commands, cards, the live panel, voice join/follow/leave,
                    stats, mixes, /status, icon and profile sync
  stats.rs          listening stats and the monthly recap, in your timezone
  store.rs          settings, history and mixes in ./data
  banner.rs         the artwork banner drawn for each song
  speech.rs         voice commands: the speech model, cutting speech up, what it asks for
  genres.rs         artist genres from MusicBrainz
  color.rs          card colour from album art
  logging.rs        console and daily log files
  personality.rs    Loski's lines
assets/             icons, fonts, avatar and banner art
scripts/loski.ps1   the launcher behind loski.cmd
```

## License

MIT, see [LICENSE](LICENSE). The bundled fonts, Bricolage Grotesque and Noto Sans, are under the SIL Open Font License (see `assets/fonts`).
