# Loski 🎧

A private Discord music bot that streams **real Spotify audio** (your Premium account, 320 kbps) into a voice channel. Only you can control it. Built in Rust on [librespot](https://github.com/librespot-org/librespot), [songbird](https://github.com/serenity-rs/songbird) and [twilight](https://github.com/twilight-rs/twilight), with every message drawn as a Discord card using Loski's own icon set.

> **Heads-up:** streaming Spotify through librespot isn't allowed by Spotify's terms of service. Loski is meant for private use with your own account. Spotify could, in theory, act on an account that does this.
>
> Loski is an independent project. It isn't affiliated with, endorsed by or sponsored by Spotify or Discord. Spotify and Discord are trademarks of their respective owners.

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
- A live now-playing panel: an artwork banner Loski draws for each song, a colour taken from the cover, where the song is playing from, an icon progress bar, the next three songs, and controls
- When Loski leaves, the panel turns into a recap of the session, with a button to keep it as a mix
- Search results as a card with covers: choose one, then Play now, Play next or Add to queue
- A queue card where you can pick any song to play now, move up or remove
- Follows you between voice channels; leaves a minute after you do
- Remembers your volume and crossfade settings; keeps a listening history you can replay from (`/history`)
- Listening stats as a picture: time, top songs and artists, streaks and a daily chart (`/stats`), plus a recap in your DMs on the 1st of each month
- Saved mixes: keep a queue under a name, load, rename or delete it later (`/mix`, or "Save as mix" on the queue card)
- Song requests: anyone in the server can `/request` a song, and you decide with one click
- Queue tools: remove a run of songs, drop repeats (`/dedupe`), move a song to play next
- `/status` to check on Spotify, Discord and the audio at a glance
- Fixes itself where it can: rejoins voice after a dropout, reconnects to Spotify, and DMs you when something needs you
- Cleans up after itself: confirmations disappear after a few seconds

## Setup

You need Docker Desktop and Spotify Premium.

### 1. Create the Discord bot

1. Go to [discord.com/developers/applications](https://discord.com/developers/applications) and click **New Application**. Name it **Loski**.
2. Open **Bot**, click **Reset Token** and copy the token.
3. Invite it to your server (replace `APP_ID` with the Application ID from **General Information**):

   ```
   https://discord.com/oauth2/authorize?client_id=APP_ID&scope=bot+applications.commands&permissions=3230720
   ```

   That grants View Channels, Send Messages, Embed Links, Read Message History, Connect and Speak. No privileged intents are needed.

### 2. Create the Spotify app

1. Go to [developer.spotify.com/dashboard](https://developer.spotify.com/dashboard) and click **Create app**.
2. Set the redirect URI to exactly `http://127.0.0.1:5588/callback` and tick **Web API**.
3. Copy the **Client ID** and **Client Secret**.

### 3. Configure

Copy `.env.example` to `.env` and fill in `DISCORD_TOKEN`, `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET`. `OWNER_ID` is already set to your Discord account.

### 4. Start it

Double-click `loski.cmd`, or run it from a terminal in the repo folder:

```powershell
.\loski start
```

It starts Docker Desktop if it isn't running, downloads the ready-made Loski image the first time (see [Prebuilt image](#prebuilt-image)), and waits until Loski says it's ready.

The first time, Loski sends you a DM asking you to open **http://127.0.0.1:5588** on this computer. That page walks you through two Spotify approvals: one for streaming, one for search. After that you never need to log in again. The login is stored in `./data`.

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

`loski.cmd` runs `scripts/loski.ps1` with permission to run just that script, so you don't have to change PowerShell's script policy. Plain `docker compose pull`, `docker compose up -d` and `docker compose down` work too.

### Prebuilt image

Every push to `main` that passes the checks publishes an image to GitHub's container registry:

| Tag | What it is |
| --- | --- |
| `ghcr.io/lucabarbalata/loski:latest` | The newest build of `main`. This is what `docker-compose.yml` runs. |
| `ghcr.io/lucabarbalata/loski:sha-1a2b3c4` | One particular commit, for going back to a version that worked. |
| `ghcr.io/lucabarbalata/loski:1.2.0` | A release, when a `v1.2.0` tag is pushed. |

Each tag holds two builds, and Docker picks the right one by itself: `linux/amd64` for 64-bit Intel and AMD machines, and `linux/arm64` for 64-bit ARM, like a Raspberry Pi 4 or 5 on a 64-bit OS, an Apple Silicon Mac or an ARM server. Both are built natively on GitHub's own machines, side by side. To run Loski without the rest of the repo, `docker-compose.yml` and a filled-in `.env` are enough: `docker compose up -d` downloads the image.

## Commands

| Command | What it does |
| --- | --- |
| `/play song [next]` | Search, or paste a Spotify link. `next:True` puts it right after the current song. |
| `/liked [shuffle]` | Your Liked Songs (shuffled unless you say otherwise). |
| `/playlist name [shuffle]` | One of your playlists; start typing to see them. |
| `/radio [song]` | Spotify's radio for a song, or for the one playing. Replaces what's coming up. |
| `/pause`, `/resume` | Pause and resume. |
| `/skip [count]` | Skip one song, or several. |
| `/previous` | Restart the song, or go back if it just started. |
| `/seek position` | `1:23`, `95`, `+15` or `-10`. |
| `/nowplaying` | Bring the panel back to the bottom of the chat. |
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
| `/filter sound` | Bass boost, treble boost, vocal boost, nightcore, slowed + reverb, 8D, or off. |
| `/sleep when` | Fade out and leave in 15 minutes to 2 hours, after this song, or cancel. |
| `/normalize [on]` | Volume levelling between songs, like Spotify's setting. Remembered. |
| `/follow` | Toggle follow mode. |
| `/history` | Recently played songs; pick one to play it again. |
| `/stats [range]` | Listening time, top songs and artists, streaks and when you listen. Tabs for the last 7 days, this month and all time, and a button that queues your top songs. |
| `/mix save name` | Save every song in the queue (played ones too) under a name. Saving under a name you used replaces it. |
| `/mix load name [shuffle] [next]` | Add a saved mix to the queue. |
| `/mix list`, `/mix delete name` | See your mixes (and add one from the menu), or delete one. Up to 25. |
| `/status` | Whether Spotify, Discord and the audio are healthy: login, response times, voice server, bitrate, buffer, uptime. |
| `/request song` | For everyone in the server: ask for a song. You get a card with Play next, Add to queue and Decline. |
| `/stop` | Stop, clear the queue and leave. |
| `/help` | Everything Loski can do. |
| Right-click → Apps → Play with Loski / Queue with Loski | Plays the Spotify link in that message, now or at the end of the queue. |

The queue card lets you pick a song to play now, play next or remove, saves the queue as a mix, and offers to remove repeats when there are any. The `/mix list` card does the same for mixes: add one to the queue, play it next, rename or delete it.

The panel's buttons cover previous, play/pause, skip, shuffle, repeat, volume, queue, open in Spotify and stop. Its More menu jumps 15 seconds back or forward, jumps to a time you type, restarts the song, or moves the panel to the bottom of the chat. When the queue runs out, the panel offers to replay it.

The artwork banner is drawn with the bundled Bricolage Grotesque and Noto Sans fonts (SIL Open Font License, see `assets/fonts`). Titles in scripts they don't cover, such as Japanese or Arabic, use a compact layout with the cover as a thumbnail instead.

## Good to know

- **When something breaks.** If voice drops, Loski rejoins on its own (and tells you if it can't). If the Spotify connection stays down, or Spotify signs it out of search, you get a DM with what to do, and another when it's fixed.
- **Logs.** Besides `docker logs`, Loski writes a log file per day to `data/logs` and keeps the last 14 days, so you can see what happened even after a restart.

- **Stats and history.** A song counts once it has played for 30 seconds (half of it for very short songs), the same rule Spotify uses, and only the time it actually played is added up. Days and months start at midnight in `TIMEZONE`, which is UTC unless you set it in `.env` (for example `TIMEZONE=Europe/Bucharest`). On the 1st of each month Loski DMs you a recap of the month before; if it was off that day, the recap comes the next time it starts. History lives in `data/history.jsonl` and mixes in `data/mixes.json`.

- **Audio quality.** Loski sends Discord the highest bitrate your server's boost level allows: 96 kbps unboosted, up to 384 kbps at level 3. You can set it yourself with `AUDIO_BITRATE`.
- **Spotify Connect.** Loski shows up as a speaker in your Spotify app whenever it's running. Pick it and Loski joins the voice channel you're in and plays there; the app's play, skip, seek and volume all work, and Discord's buttons control it too. Pick another device to hand back. Running `/play` takes over again with the Discord queue.
- **Follow mode** plays the same song as your Spotify, at the same position. Your own device keeps playing too, so mute it to avoid hearing it twice. Pausing from Discord sticks until you change something in Spotify.
- **Spotify history and Wrapped.** Songs played through Loski don't count, because librespot doesn't report plays to Spotify. Loski keeps its own history instead.
- **Leaving.** Loski leaves 1 minute after you leave voice (`LEAVE_AFTER_SECS`), or 5 minutes after the queue runs out (`IDLE_LEAVE_SECS`). The queue isn't saved between sessions.
- **Other people** can listen and `/request` songs, but only `OWNER_ID` can use the other commands and the buttons.
- **Profile and icons.** On startup Loski sets its own name, avatar, banner and "About me" from `assets/`, and uploads its icons as its own emojis. Art is only re-uploaded when it changes. Set `SYNC_PROFILE=false` to leave the profile alone, or `BOT_NAME` to call it something else.
- **Restarting.** `docker compose down` or a restart makes Loski leave voice and remove its panel first, so it never looks stuck in a channel.

## Development

The project builds inside Docker, so you don't need a local Rust toolchain:

```bash
docker build -t loski-dev - <<'EOF'
FROM rust:1.99.1-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends cmake pkg-config libopus-dev && rustup component add clippy rustfmt
EOF
docker run --rm -v "$PWD:/src" -v loski-cargo:/usr/local/cargo/registry -v loski-target:/target -e CARGO_TARGET_DIR=/target -w /src loski-dev cargo test
```

The tests need no Spotify or Discord connection. They cover the queue, the sound filters, stats, every card against Discord's limits, and the playback engine, which runs whole sessions (gapless, crossfade, repeat, sleep timer, unplayable songs, history) against fake players that record what they're told.

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
  color.rs          card colour from album art
  logging.rs        console and daily log files
  personality.rs    Loski's lines
assets/             icons, fonts, avatar and banner art
scripts/loski.ps1   the launcher behind loski.cmd
```

## License

MIT, see [LICENSE](LICENSE). The bundled fonts, Bricolage Grotesque and Noto Sans, are under the SIL Open Font License (see `assets/fonts`).
