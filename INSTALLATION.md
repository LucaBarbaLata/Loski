# Installing Loski

Loski runs anywhere Docker does: your own computer, a server, a Raspberry Pi or a NAS. Pick the way that fits, then do the [first start](#first-start), which is the same everywhere.

| Where | How | |
| --- | --- | --- |
| A Windows PC | The `loski` launcher | [Windows](#windows) |
| A Mac | The `loski.sh` launcher | [macOS](#macos) |
| A Linux desktop | The `loski.sh` launcher | [Linux](#linux) |
| A server or VPS | Docker Compose, keys in `.env` | [A server or VPS](#a-server-or-vps) |
| A Raspberry Pi | Either of the two above | [Raspberry Pi](#raspberry-pi) |
| A NAS or a Docker app | A compose stack | [A NAS, Portainer and friends](#a-nas-portainer-and-friends) |
| Anywhere with Docker | One `docker run` | [Just Docker](#just-docker) |
| Your own changes | Build it from the code | [Building it yourself](#building-it-yourself) |

## What you need

- **Spotify Premium.** Spotify only streams to Premium accounts.
- **Docker.** Docker Desktop on Windows and macOS, Docker Engine with the Compose plugin on Linux ([docs.docker.com/engine/install](https://docs.docker.com/engine/install/)).
- **A 64-bit computer.** Loski's image comes for `linux/amd64` (Intel and AMD) and `linux/arm64` (a Raspberry Pi 4 or 5, Apple Silicon, ARM servers). Docker picks the right one.
- **For voice commands (optional):** an Intel or AMD processor with AVX2 (most from the last ten years) or a 64-bit ARM one, and 60 MB for the speech model, downloaded when you turn them on in `/voice`.
- **Two sets of keys:** a Discord bot and a Spotify app. The setup page walks you through them, and so does [Your keys](#your-keys).

## Windows

1. Install [Docker Desktop](https://www.docker.com/products/docker-desktop/).
2. Get Loski: clone the repo, or on GitHub click **Code → Download ZIP** and unzip it.
3. In that folder, in PowerShell:

   ```powershell
   .\loski start
   ```

   It starts Docker Desktop if needed, downloads Loski the first time, and opens the setup page at http://127.0.0.1:5588.

Then carry on with the [first start](#first-start). `.\loski` also does `stop`, `restart`, `update`, `logs`, `status`, `backup` and `restore` (see the README).

## macOS

1. Install [Docker Desktop](https://www.docker.com/products/docker-desktop/).
2. Clone the repo (or download the ZIP), and make the launcher runnable once:

   ```bash
   chmod +x loski.sh
   ```

3. Start it:

   ```bash
   ./loski.sh start
   ```

   It opens Docker Desktop if it isn't running, then the setup page.

## Linux

1. Install Docker Engine with the Compose plugin, and add yourself to the `docker` group (`sudo usermod -aG docker $USER`, then log out and in). `loski.sh` tells you if either is missing.
2. Clone the repo, `chmod +x loski.sh`, and:

   ```bash
   ./loski.sh start
   ```

`loski.sh` has Loski run as you, so `data/` belongs to you and backups need no `sudo`. On a computer without a screen it prints the setup page's address and the claim code instead of opening a browser.

## A server or VPS

Nothing needs a browser on the server: the keys go in `.env`, and the Spotify login works from your own browser through Discord.

1. Install Docker Engine with the Compose plugin.
2. Make a folder with Loski's two files:

   ```bash
   mkdir loski && cd loski
   curl -fsSLO https://raw.githubusercontent.com/LucaBarbaLata/Loski/main/docker-compose.yml
   curl -fsSL https://raw.githubusercontent.com/LucaBarbaLata/Loski/main/.env.example -o .env
   ```

   (Or clone the repo, which also gets you `loski.sh`.)
3. Put your [keys](#your-keys) in `.env`: `DISCORD_TOKEN`, `SPOTIFY_CLIENT_ID` and `SPOTIFY_CLIENT_SECRET`. Add `OWNER_ID` (your Discord user ID) to skip claiming.
4. Start it:

   ```bash
   docker compose up -d
   ```

5. Read the log for what's next: the invite link while Loski isn't in a server yet, and the claim code while it has no owner.

   ```bash
   docker compose logs loski
   ```

6. Log in to Spotify from any browser: Loski DMs you two buttons. Each approval ends on a `127.0.0.1` page that doesn't load, which is expected. Copy that page's address and send it to Loski in the same DM.

Good to know on a server:

- **Coming back after a reboot.** `restart: on-failure` in `docker-compose.yml` restarts Loski if it crashes, but not always after the server restarts. Change it to `restart: unless-stopped` and it always comes back (until you stop it yourself).
- **Nothing is open to the internet.** Ports 5588 and 8898 only listen on the server itself (`127.0.0.1`). To use the setup page from your laptop anyway, tunnel them: `ssh -L 5588:127.0.0.1:5588 -L 8898:127.0.0.1:8898 you@server`, then open http://127.0.0.1:5588.
- **One copy per bot.** Stop Loski on your own computer before starting it on the server (or the other way round). Two copies with the same Discord token both answer every button, and Discord shows errors like "Unknown interaction".

## Raspberry Pi

A Pi 4 or 5 with a 64-bit OS (Raspberry Pi OS 64-bit, say) runs the ARM image. With a screen, follow [Linux](#linux). Without one, follow [A server or VPS](#a-server-or-vps): keys in `.env`, and the Spotify login through Discord. Voice commands work on a Pi too, but expect them to answer more slowly than on a PC.

## A NAS, Portainer and friends

Anything that runs compose stacks works: Portainer, Synology's Container Manager, Unraid, CasaOS and the like. These usually want everything in one file, so put the keys in the stack itself:

```yaml
services:
  loski:
    image: ghcr.io/lucabarbalata/loski:latest
    container_name: loski
    environment:
      DISCORD_TOKEN: "your bot token"
      SPOTIFY_CLIENT_ID: "your client id"
      SPOTIFY_CLIENT_SECRET: "your client secret"
      # OWNER_ID: "your Discord user ID"
    volumes:
      # Where Loski keeps everything; any folder on the NAS.
      - /path/on/your/nas/loski:/data
    ports:
      - "127.0.0.1:5588:5588"
      - "127.0.0.1:8898:8898"
    init: true
    restart: unless-stopped
```

Then follow steps 5 and 6 of [A server or VPS](#a-server-or-vps): the log has the invite link and claim code, and the Spotify login goes through Discord. Any other setting from `.env.example` can go under `environment` too.

## Just Docker

No compose, no repo: one command, with your keys in a `.env` file next to it.

```bash
docker run -d --name loski --init --restart unless-stopped \
  --env-file .env \
  -v "$PWD/data:/data" \
  -p 127.0.0.1:5588:5588 -p 127.0.0.1:8898:8898 \
  ghcr.io/lucabarbalata/loski:latest
```

Without a `.env`, leave out `--env-file` and open http://127.0.0.1:5588 for the setup page. `docker logs loski` shows the invite link and claim code.

## Building it yourself

For your own changes:

- **With the launchers:** `.\loski build` (or `./loski.sh build`) builds the image from the folder and restarts Loski with it.
- **With Compose:** `docker compose up -d --build`.
- **Without Docker,** on Linux: Loski is one Rust program. Install Rust (the version in the `Dockerfile` is the one Loski is tested with) and `cmake`, `pkg-config`, `libopus-dev`, `clang` and `libclang-dev`, then:

  ```bash
  cargo build --release
  ./target/release/loski
  ```

  It reads `.env` from the folder you run it in and keeps its data in `./data`.

## First start

The same wherever Loski runs.

1. **Keys.** Either the setup page (http://127.0.0.1:5588 on the computer Loski runs on) or `.env`. The page checks them with Discord and Spotify and saves them in `data/setup.env`; `.env` always wins.
2. **Invite the bot** to your server, with the setup page's button or the link in the log.
3. **Make it yours.** DM the bot `claim` and the six-digit code from the setup page or the log. Whoever sends it first becomes the owner. Setting `OWNER_ID` skips this.
4. **Log in to Spotify:** two approvals, one for streaming and one for search. On the computer Loski runs on, the setup page's button does it. From anywhere else, use the buttons in the DM Loski sends you, and send it back the address each one ends on.
5. **Make it look the way you want** with `/customize`: name, picture, banner and About me, everywhere or in one server.

## Your keys

**A Discord bot.** At [discord.com/developers/applications](https://discord.com/developers/applications), click **New Application** and name it what your bot should be called (Loski, say). Open **Bot**, click **Reset Token** and copy the token. No privileged intents are needed.

**A Spotify app.** At [developer.spotify.com/dashboard](https://developer.spotify.com/dashboard), click **Create app**, set the redirect URI to exactly `http://127.0.0.1:5588/callback`, tick **Web API**, and copy the **Client ID** and **Client Secret**. The redirect URI stays `127.0.0.1` even when Loski runs on a server.

## Updating

| How you run Loski | Update with |
| --- | --- |
| The launchers | `.\loski update` or `./loski.sh update` |
| Docker Compose | `docker compose pull`, then `docker compose up -d` |
| Just Docker | `docker pull ghcr.io/lucabarbalata/loski:latest`, then remove the container and run it again |
| A NAS or Portainer | Pull the image again and redeploy the stack |

Loski DMs you when a new release is out, with what changed.

## Backups and moving

Everything Loski keeps is in its data folder: the Spotify login, history, likes, mixes, settings and logs. `loski backup` (or `./loski.sh backup`) saves it all, with `.env`, to `loski-backup-<date>.tar.gz`. It holds your Discord token and Spotify login, so keep it somewhere private. Without the launchers, stop Loski and copy the data folder and `.env`.

To move Loski to another computer:

1. On the old one: `loski backup`, then stop Loski there, so the same bot isn't running twice.
2. Copy `loski-backup-<date>.tar.gz` into the Loski folder on the new one (the repo, or just `docker-compose.yml` and a launcher).
3. On the new one: `loski restore loski-backup-<date>.tar.gz`. It unpacks everything, keeps whatever was there before in `data.before-restore-<time>`, and starts Loski: same history, mixes, settings and Spotify login, no setup page.

## Files and ports

| What | Why |
| --- | --- |
| `data/` (`/data` in the container) | Everything Loski keeps. Back it up; keep it private, as it holds your Spotify login. |
| `.env` | Your keys and settings (see `.env.example`). Keep it private. |
| Port 5588 | The setup page, and the redirect for Spotify's search login. |
| Port 8898 | The redirect for Spotify's streaming login. |

Both ports only listen on `127.0.0.1`, and they're only used while setting up and logging in.

## When something's off

- **"Ports are not available" on Windows.** Hyper-V has reserved Loski's ports. Run `net stop winnat; net start winnat` in PowerShell as administrator (or restart Windows), then start Loski again. `loski start` says when this is why.
- **"Unknown interaction", or buttons answered twice.** Loski is running twice with the same bot token, on two computers say. Stop one.
- **"Logs won't be saved… Permission denied" on Linux.** An older Loski couldn't use a `data` folder Docker created. Update, or give the folder to Loski's user once: `sudo chown -R 10001:10001 data`.
- **The Spotify login page doesn't load.** That's expected when Loski runs on another computer: copy the page's address and send it to Loski in its DM.
- **Anything else.** `/status` shows what's healthy, `loski selftest` (or `docker exec loski loski selftest`) checks Spotify from inside Loski, and the logs are in `data/logs`.

## Uninstalling

Stop and remove Loski with `docker compose down` (or `docker rm -f loski`), remove the image with `docker image rm ghcr.io/lucabarbalata/loski:latest`, and delete the folder. Delete the bot in Discord's developer portal if you won't need it again.
