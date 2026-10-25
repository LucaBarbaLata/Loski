<#
.SYNOPSIS
    Start, stop, update and check on Loski.

.DESCRIPTION
    Run it through loski.cmd in the repo folder:

        .\loski start      Start Docker Desktop if needed, then Loski
        .\loski stop       Stop Loski (it leaves voice first)
        .\loski restart    Restart Loski
        .\loski update     Get the latest code, rebuild and restart
        .\loski logs       Follow the live log (Ctrl+C to stop watching)
        .\loski logs files Open the folder with the saved daily logs
        .\loski status     Whether Loski is running, and its last log lines
        .\loski selftest   Check the Spotify connection from inside Loski
#>
param(
    [Parameter(Position = 0)]
    [string]$Command = 'start',

    [Parameter(Position = 1)]
    [string]$Option = ''
)

# Everything runs from the repo folder, where docker-compose.yml is.
Set-Location -LiteralPath (Split-Path -Parent $PSScriptRoot)

$Container = 'loski'
$DockerDesktop = Join-Path $env:ProgramFiles 'Docker\Docker\Docker Desktop.exe'

function Write-Step([string]$Text) { Write-Host "> $Text" -ForegroundColor Yellow }
function Write-Done([string]$Text) { Write-Host "  $Text" -ForegroundColor Green }
function Write-Problem([string]$Text) { Write-Host "  $Text" -ForegroundColor Red }

function Test-Docker {
    docker info --format '{{.ServerVersion}}' *> $null
    return $LASTEXITCODE -eq 0
}

# Docker Desktop sometimes exits right after launching when it was fully
# stopped, so it gets a second launch if it isn't up after 30 seconds.
function Start-Docker {
    if (Test-Docker) { return $true }
    if (-not (Test-Path -LiteralPath $DockerDesktop)) {
        Write-Problem "Docker Desktop isn't installed at $DockerDesktop."
        return $false
    }
    Write-Step 'Starting Docker Desktop'
    Start-Process -FilePath $DockerDesktop
    $started = Get-Date
    $relaunched = $false
    while (((Get-Date) - $started).TotalMinutes -lt 3) {
        Start-Sleep -Seconds 3
        if (Test-Docker) {
            Write-Done 'Docker is running'
            return $true
        }
        $running = Get-Process -Name 'Docker Desktop' -ErrorAction SilentlyContinue
        if (-not $running -and -not $relaunched -and ((Get-Date) - $started).TotalSeconds -gt 30) {
            Start-Process -FilePath $DockerDesktop
            $relaunched = $true
        }
    }
    Write-Problem "Docker Desktop didn't start within 3 minutes. Open it yourself, then run this again."
    return $false
}

function Test-Env {
    if (Test-Path -LiteralPath '.env') { return $true }
    Write-Problem "There's no .env file yet. Copy .env.example to .env and fill it in."
    return $false
}

# When the container last started; changes whenever it's (re)created.
function Get-StartedAt {
    $at = docker inspect -f '{{.State.StartedAt}}' $Container 2>$null
    if ($LASTEXITCODE -ne 0) { return '' }
    return "$at".Trim()
}

function Test-Running {
    $state = docker inspect -f '{{.State.Running}}' $Container 2>$null
    return $LASTEXITCODE -eq 0 -and "$state".Trim() -eq 'true'
}

# Watches the log until Loski says it's ready (or why it isn't).
function Wait-Ready([datetime]$Since) {
    Write-Step 'Waiting for Loski to connect'
    # A few seconds early, in case Docker's clock is a little behind.
    $sinceArg = $Since.AddSeconds(-5).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
    $deadline = (Get-Date).AddSeconds(120)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds 2
        $log = docker logs $Container --since $sinceArg 2>$null | Out-String
        if ($log -match 'Loski is ready') {
            Write-Done 'Loski is ready. Use /play in Discord.'
            return $true
        }
        if ($log -match 'Spotify login needed') {
            Write-Problem 'Spotify needs you to log in once: open http://127.0.0.1:5588/ in your browser.'
            return $true
        }
        if ($log -match "Loski can't start: ([^\r\n]+)") {
            Write-Problem $Matches[1]
            return $false
        }
        if (-not (Test-Running)) {
            Write-Problem 'Loski stopped while starting. The last lines of its log:'
            docker logs $Container --tail 15
            return $false
        }
    }
    Write-Problem 'Loski is taking a while. Watch what it is doing with: .\loski logs'
    return $false
}

function Start-Loski([switch]$Build) {
    $before = Get-StartedAt
    $since = Get-Date
    if ($Build) {
        Write-Step 'Building and restarting Loski (a few minutes the first time)'
        docker compose up -d --build
    }
    else {
        Write-Step 'Starting Loski'
        docker compose up -d
    }
    if ($LASTEXITCODE -ne 0) {
        Write-Problem 'Docker could not start Loski (see above).'
        return $false
    }
    if ((Get-StartedAt) -eq $before -and (Test-Running)) {
        Write-Done 'Loski was already running.'
        return $true
    }
    return Wait-Ready $since
}

switch ($Command) {
    'start' {
        if (-not (Test-Env) -or -not (Start-Docker)) { exit 1 }
        if (-not (Start-Loski)) { exit 1 }
    }

    'stop' {
        if (-not (Test-Docker)) {
            Write-Done "Docker isn't running, so neither is Loski."
            exit 0
        }
        Write-Step 'Stopping Loski (it leaves voice first)'
        docker compose stop
        if ($LASTEXITCODE -ne 0) { exit 1 }
        Write-Done 'Stopped.'
    }

    'restart' {
        if (-not (Test-Env) -or -not (Start-Docker)) { exit 1 }
        $since = Get-Date
        Write-Step 'Restarting Loski'
        docker compose restart
        if ($LASTEXITCODE -ne 0) { exit 1 }
        if (-not (Wait-Ready $since)) { exit 1 }
    }

    'update' {
        if (-not (Test-Env) -or -not (Start-Docker)) { exit 1 }
        $remotes = git remote 2>$null
        if ($remotes) {
            Write-Step 'Getting the latest code'
            git pull --ff-only
            if ($LASTEXITCODE -ne 0) {
                Write-Problem 'Could not update the code (see above). Nothing was changed.'
                exit 1
            }
        }
        else {
            Write-Done 'No GitHub remote yet, so building the code in this folder.'
        }
        if (-not (Start-Loski -Build)) { exit 1 }
    }

    'logs' {
        if ($Option -eq 'files') {
            $folder = Join-Path (Get-Location) 'data\logs'
            if (-not (Test-Path -LiteralPath $folder)) {
                Write-Problem "No saved logs yet. They appear in $folder once Loski has run."
                exit 1
            }
            Start-Process explorer.exe $folder
            exit 0
        }
        if (-not (Test-Docker)) {
            Write-Problem "Docker isn't running. Saved logs are in data\logs (.\loski logs files)."
            exit 1
        }
        docker compose logs -f --tail 100
    }

    'status' {
        if (-not (Test-Docker)) {
            Write-Problem "Docker Desktop isn't running, so Loski is offline. Start both with: .\loski start"
            exit 1
        }
        $status = docker ps --filter "name=^$Container$" --format '{{.Status}}'
        if ($status) {
            Write-Done "Loski is running ($status)."
        }
        else {
            Write-Problem 'Loski is stopped. Start it with: .\loski start'
        }
        Write-Step 'Last log lines'
        docker logs $Container --tail 8 2>$null
    }

    'selftest' {
        if (-not (Test-Running)) {
            Write-Problem 'Loski needs to be running for the self-test. Start it with: .\loski start'
            exit 1
        }
        docker exec $Container loski selftest
        exit $LASTEXITCODE
    }

    'help' {
        Get-Help $PSCommandPath -Detailed | Select-Object -ExpandProperty description | Out-String | Write-Host
    }

    default {
        Write-Problem "Unknown command '$Command'."
        Get-Help $PSCommandPath -Detailed | Select-Object -ExpandProperty description | Out-String | Write-Host
        exit 1
    }
}
