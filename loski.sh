#!/usr/bin/env bash
# Start, stop, update and check on Loski, on Linux and macOS (loski.cmd is
# the same for Windows). Run it from anywhere:
#
#   ./loski.sh start      Start Docker if needed, then Loski
#   ./loski.sh stop       Stop Loski (it leaves voice first)
#   ./loski.sh restart    Restart Loski
#   ./loski.sh update     Get the latest code and image from GitHub, then restart
#   ./loski.sh build      Build Loski from this folder instead, then restart
#   ./loski.sh logs       Follow the live log (Ctrl+C to stop watching)
#   ./loski.sh logs files Open the folder with the saved daily logs
#   ./loski.sh status     Whether Loski is running, and its last log lines
#   ./loski.sh selftest   Check the Spotify connection from inside Loski
#   ./loski.sh backup     Save your history, mixes, settings and logins to one file
#   ./loski.sh restore F  Bring a backup back (on this or another computer)

set -u

# Everything runs from the repo folder, where docker-compose.yml is.
cd "$(dirname "$0")" || exit 1

CONTAINER=loski
SETUP_PAGE=http://127.0.0.1:5588/
# Loski runs as you inside the container, so the data folder stays yours.
LOSKI_UID="$(id -u)"
LOSKI_GID="$(id -g)"
export LOSKI_UID LOSKI_GID

if [ -t 1 ]; then
    YELLOW=$'\e[33m' GREEN=$'\e[32m' RED=$'\e[31m' PLAIN=$'\e[0m'
else
    YELLOW='' GREEN='' RED='' PLAIN=''
fi
step() { printf '%s> %s%s\n' "$YELLOW" "$1" "$PLAIN"; }
done_() { printf '%s  %s%s\n' "$GREEN" "$1" "$PLAIN"; }
problem() { printf '%s  %s%s\n' "$RED" "$1" "$PLAIN"; }

# `docker compose`, or the older `docker-compose`.
compose() {
    if docker compose version >/dev/null 2>&1; then
        docker compose "$@"
    elif command -v docker-compose >/dev/null 2>&1; then
        docker-compose "$@"
    else
        problem "Docker Compose isn't installed. See https://docs.docker.com/compose/install/"
        return 1
    fi
}

open_thing() {
    if [ "$(uname)" = Darwin ]; then
        open "$1" >/dev/null 2>&1
    elif command -v xdg-open >/dev/null 2>&1 && [ -n "${DISPLAY:-}${WAYLAND_DISPLAY:-}" ]; then
        xdg-open "$1" >/dev/null 2>&1
    else
        return 1
    fi
}

test_docker() { docker info >/dev/null 2>&1; }

start_docker() {
    test_docker && return 0
    if ! command -v docker >/dev/null 2>&1; then
        problem "Docker isn't installed. See https://docs.docker.com/engine/install/"
        return 1
    fi
    if [ "$(uname)" = Darwin ]; then
        step 'Starting Docker Desktop'
        open -a Docker || {
            problem "Couldn't open Docker Desktop. Start it yourself, then run this again."
            return 1
        }
        for _ in $(seq 1 60); do
            sleep 3
            if test_docker; then
                done_ 'Docker is running'
                return 0
            fi
        done
        problem "Docker Desktop didn't start within 3 minutes. Open it yourself, then run this again."
        return 1
    fi
    if docker info 2>&1 | grep -qi 'permission denied'; then
        problem "You can't use Docker yet. Run: sudo usermod -aG docker $USER  then log out and back in."
    else
        problem "Docker isn't running. Start it with: sudo systemctl start docker"
    fi
    return 1
}

# Docker needs a .env file. Without one, Loski asks for its keys on a setup
# page instead, so an empty copy of the example is enough.
test_env() {
    mkdir -p data
    [ -f .env ] && return 0
    if [ ! -f .env.example ]; then
        problem "There's no .env file, and no .env.example to start one from."
        return 1
    fi
    cp .env.example .env
    chmod 600 .env
    done_ 'Made a .env file. Loski asks for its Discord token and Spotify keys on a setup page.'
}

started_at() { docker inspect -f '{{.State.StartedAt}}' "$CONTAINER" 2>/dev/null; }

running() { [ "$(docker inspect -f '{{.State.Running}}' "$CONTAINER" 2>/dev/null)" = true ]; }

# The claim code in the log, while Loski has no owner yet.
show_claim() {
    local claim
    claim="$(printf '%s' "$1" | grep -o 'DM .* "claim [0-9]\{6\}"' | tail -n 1)"
    if [ -n "$claim" ]; then
        local who code
        who="$(printf '%s' "$claim" | sed 's/^DM \(.*\) "claim.*$/\1/')"
        code="$(printf '%s' "$claim" | grep -o '[0-9]\{6\}')"
        step "Make it yours: in Discord, send $who a direct message saying: claim $code"
    fi
}

# Watches the log until Loski says it's ready (or why it isn't).
wait_ready() {
    local since=$1 log
    step 'Waiting for Loski to connect'
    for _ in $(seq 1 60); do
        sleep 2
        log="$(docker logs --since "$(($(date +%s) - since + 5))s" "$CONTAINER" 2>&1)"
        if printf '%s' "$log" | grep -q 'Setup needed'; then
            step "Loski needs its Discord token and Spotify keys: open $SETUP_PAGE"
            open_thing "$SETUP_PAGE" || done_ "(On a computer without a browser, see \"Running on a server\" in the README.)"
            done_ 'Fill in the page; Loski carries on by itself once they check out.'
            return 0
        fi
        if printf '%s' "$log" | grep -q 'Loski is ready'; then
            done_ 'Loski is ready. Use /play in Discord.'
            show_claim "$log"
            return 0
        fi
        if printf '%s' "$log" | grep -q 'Spotify login needed'; then
            problem "Spotify needs you to log in once: open $SETUP_PAGE in your browser."
            show_claim "$log"
            return 0
        fi
        if printf '%s' "$log" | grep -q "Loski can't start"; then
            problem "$(printf '%s' "$log" | grep -o "Loski can't start: .*" | tail -n 1)"
            return 1
        fi
        if ! running; then
            problem 'Loski stopped while starting. The last lines of its log:'
            docker logs --tail 15 "$CONTAINER"
            return 1
        fi
    done
    problem 'Loski is taking a while. Watch what it is doing with: ./loski.sh logs'
    return 1
}

image() { compose config --images 2>/dev/null | head -n 1; }

have_image() { docker image inspect "$(image)" >/dev/null 2>&1; }

# Downloads the published image. Fails if that didn't work (no network, or
# the image isn't public yet), so the caller can build instead.
update_image() {
    step 'Downloading the latest Loski image'
    compose pull && return 0
    problem "Couldn't download it (see above), so building it from this folder instead."
    return 1
}

start_loski() {
    local build=$1 before since
    before="$(started_at)"
    since="$(date +%s)"
    if [ "$build" = yes ]; then
        step 'Building and restarting Loski (a few minutes the first time)'
        compose up -d --build || {
            problem 'Docker could not start Loski (see above).'
            return 1
        }
    else
        step 'Starting Loski'
        compose up -d || {
            problem 'Docker could not start Loski (see above).'
            return 1
        }
    fi
    if [ "$(started_at)" = "$before" ] && running; then
        done_ 'Loski was already running.'
        return 0
    fi
    wait_ready "$since"
}

# Starts from the downloaded image, building only when there isn't one.
start_fresh() {
    local build=no
    if ! have_image && ! update_image; then
        build=yes
    fi
    start_loski "$build"
}

command=${1:-start}
option=${2:-}

case "$command" in
start)
    test_env && start_docker || exit 1
    start_fresh || exit 1
    ;;

stop)
    if ! test_docker; then
        done_ "Docker isn't running, so neither is Loski."
        exit 0
    fi
    step 'Stopping Loski (it leaves voice first)'
    compose stop || exit 1
    done_ 'Stopped.'
    ;;

restart)
    test_env && start_docker || exit 1
    since="$(date +%s)"
    step 'Restarting Loski'
    compose restart || exit 1
    wait_ready "$since" || exit 1
    ;;

update)
    test_env && start_docker || exit 1
    if [ -n "$(git remote 2>/dev/null)" ]; then
        step 'Getting the latest code'
        git pull --ff-only || {
            problem 'Could not update the code (see above). Nothing was changed.'
            exit 1
        }
    fi
    build=no
    update_image || build=yes
    start_loski "$build" || exit 1
    ;;

build)
    test_env && start_docker || exit 1
    start_loski yes || exit 1
    ;;

logs)
    if [ "$option" = files ]; then
        if [ ! -d data/logs ]; then
            problem "No saved logs yet. They appear in $(pwd)/data/logs once Loski has run."
            exit 1
        fi
        open_thing data/logs || done_ "The saved logs are in $(pwd)/data/logs"
        exit 0
    fi
    if ! test_docker; then
        problem "Docker isn't running. Saved logs are in data/logs (./loski.sh logs files)."
        exit 1
    fi
    compose logs -f --tail 100
    ;;

status)
    if ! test_docker; then
        problem "Docker isn't running, so Loski is offline. Start both with: ./loski.sh start"
        exit 1
    fi
    status="$(docker ps --filter "name=^${CONTAINER}$" --format '{{.Status}}')"
    if [ -n "$status" ]; then
        done_ "Loski is running ($status)."
    else
        problem 'Loski is stopped. Start it with: ./loski.sh start'
    fi
    step 'Last log lines'
    docker logs --tail 8 "$CONTAINER" 2>/dev/null
    ;;

selftest)
    if ! running; then
        problem 'Loski needs to be running for the self-test. Start it with: ./loski.sh start'
        exit 1
    fi
    docker exec "$CONTAINER" loski selftest
    ;;

backup)
    if [ ! -d data ]; then
        problem "Nothing to back up yet: there's no data folder."
        exit 1
    fi
    file=${option:-loski-backup-$(date +%Y-%m-%d).tar.gz}
    items=(data)
    [ -f .env ] && items+=(.env)
    step "Saving your history, mixes, settings and logins to $file"
    # The logs stay behind; they're only for looking back on this computer.
    tar -czf "$file" --exclude 'data/logs' --exclude '*.tmp' "${items[@]}" || {
        problem 'The backup failed (see above).'
        exit 1
    }
    chmod 600 "$file"
    done_ "Saved $file ($(du -h "$file" | cut -f1)). It holds your tokens and Spotify login: keep it private."
    done_ "On the other computer, put it in the Loski folder and run: ./loski.sh restore $file"
    ;;

restore)
    if [ -z "$option" ] || [ ! -f "$option" ]; then
        problem 'Say which backup to bring back, like: ./loski.sh restore loski-backup-2026-10-26.tar.gz'
        exit 1
    fi
    inside="$(tar -tzf "$option" 2>/dev/null)"
    if ! printf '%s\n' "$inside" | grep -q '^data/'; then
        problem "That doesn't look like a Loski backup (there's no data folder in it)."
        exit 1
    fi
    if test_docker && running; then
        step 'Stopping Loski first'
        compose stop
    fi
    # What's here now is kept, in case the backup was the wrong one.
    stamp="$(date +%Y%m%d-%H%M%S)"
    if [ -d data ]; then
        mv data "data.before-restore-$stamp"
        done_ "Kept what was here in data.before-restore-$stamp"
    fi
    if [ -f .env ] && printf '%s\n' "$inside" | grep -qx '.env'; then
        cp .env ".env.before-restore-$stamp"
    fi
    step "Bringing back $option"
    tar -xzf "$option" || {
        problem 'The backup could not be unpacked (see above).'
        exit 1
    }
    done_ 'Restored.'
    test_env && start_docker || exit 1
    start_fresh || exit 1
    ;;

help | -h | --help)
    sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
    ;;

*)
    problem "Unknown command '$command'."
    sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'
    exit 1
    ;;
esac
