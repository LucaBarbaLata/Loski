<#
.SYNOPSIS
    Start, stop, update and check on Loski.

.DESCRIPTION
    Run it through loski.cmd in the repo folder:

        .\loski start      Start Docker Desktop if needed, then Loski
        .\loski stop       Stop Loski (it leaves voice first)
        .\loski restart    Restart Loski
        .\loski update     Get the latest code and image from GitHub, then restart
        .\loski build      Build Loski from this folder instead, then restart
        .\loski logs       Follow the live log (Ctrl+C to stop watching)
        .\loski logs files Open the folder with the saved daily logs
        .\loski status     Whether Loski is running, and its last log lines
        .\loski selftest   Check the Spotify connection from inside Loski
        .\loski backup     Save your history, mixes, settings and logins to one file
        .\loski restore F  Bring a backup back (on this or another computer)
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
$SetupPage = 'http://127.0.0.1:5588/'

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

# Docker needs a .env file. Without one, Loski asks for its keys on a setup
# page instead, so an empty copy of the example is enough.
function Test-Env {
    if (Test-Path -LiteralPath '.env') { return $true }
    if (-not (Test-Path -LiteralPath '.env.example')) {
        Write-Problem "There's no .env file, and no .env.example to start one from."
        return $false
    }
    Copy-Item -LiteralPath '.env.example' -Destination '.env'
    Write-Done 'Made a .env file. Loski asks for its Discord token and Spotify keys on a setup page.'
    return $true
}

# The claim code in the log, while Loski has no owner yet.
function Show-Claim([string]$Log) {
    if ($Log -match 'DM (.+) "claim (\d{6})"') {
        Write-Step "Make it yours: in Discord, send $($Matches[1]) a direct message saying: claim $($Matches[2])"
    }
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
        $log = docker logs $Container --since $sinceArg 2>&1 | Out-String
        if ($log -match 'Setup needed') {
            Write-Step "Loski needs its Discord token and Spotify keys: opening $SetupPage"
            Start-Process $SetupPage
            Write-Done 'Fill in the page; Loski carries on by itself once they check out.'
            return $true
        }
        if ($log -match 'Loski is ready') {
            Write-Done 'Loski is ready. Use /play in Discord.'
            Show-Claim $log
            return $true
        }
        if ($log -match 'Spotify login needed') {
            Write-Problem "Spotify needs you to log in once: open $SetupPage in your browser."
            Show-Claim $log
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

# The image docker-compose.yml runs (the one GitHub Actions publishes).
function Get-Image {
    $images = docker compose config --images 2>$null
    return "$($images | Select-Object -First 1)".Trim()
}

function Test-Image {
    docker image inspect (Get-Image) *> $null
    return $LASTEXITCODE -eq 0
}

# Downloads the published image. Returns false if that didn't work (no
# network, or the image isn't public yet), so the caller can build instead.
function Update-Image {
    Write-Step 'Downloading the latest Loski image'
    docker compose pull
    if ($LASTEXITCODE -eq 0) { return $true }
    Write-Problem "Couldn't download it (see above), so building it from this folder instead."
    return $false
}

# Hyper-V now and then sets aside a block of ports that includes Loski's
# login ports (5588 and 8898), and Docker can't publish them then. Says so,
# with the fix, when that's why Loski didn't start.
function Show-ReservedPorts {
    $ranges = netsh interface ipv4 show excludedportrange protocol=tcp | ForEach-Object {
        if ($_ -match '^\s*(\d+)\s+(\d+)') { , @([int]$Matches[1], [int]$Matches[2]) }
    }
    $blocked = @(5588, 8898) | Where-Object {
        $port = $_
        $ranges | Where-Object { $port -ge $_[0] -and $port -le $_[1] }
    }
    if ($blocked) {
        Write-Problem "Windows has set aside port $($blocked -join ' and ') for itself (Hyper-V does this now and then), so Docker can't use it."
        Write-Problem 'To free it, open PowerShell as administrator and run:  net stop winnat; net start winnat'
        Write-Problem 'Then start Loski again. Restarting Windows works too.'
    }
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
        Show-ReservedPorts
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
        # The first time, fetch the ready-made image rather than compiling.
        $build = -not (Test-Image) -and -not (Update-Image)
        if (-not (Start-Loski -Build:$build)) { exit 1 }
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
        $build = -not (Update-Image)
        if (-not (Start-Loski -Build:$build)) { exit 1 }
    }

    'build' {
        if (-not (Test-Env) -or -not (Start-Docker)) { exit 1 }
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

    'backup' {
        if (-not (Test-Path -LiteralPath 'data')) {
            Write-Problem "Nothing to back up yet: there's no data folder."
            exit 1
        }
        $file = if ($Option) { $Option } else { "loski-backup-$(Get-Date -Format 'yyyy-MM-dd').tar.gz" }
        $items = @('data')
        if (Test-Path -LiteralPath '.env') { $items += '.env' }
        Write-Step "Saving your history, mixes, settings and logins to $file"
        # The logs stay behind; they're only for looking back on this computer.
        tar -czf $file --exclude 'data/logs' --exclude '*.tmp' @items
        if ($LASTEXITCODE -ne 0) {
            Write-Problem 'The backup failed (see above).'
            exit 1
        }
        $size = '{0:N1} MB' -f ((Get-Item -LiteralPath $file).Length / 1MB)
        Write-Done "Saved $file ($size). It holds your tokens and Spotify login: keep it private."
        Write-Done "On the other computer, put it in the Loski folder and run: loski restore $file"
    }

    'restore' {
        if (-not $Option -or -not (Test-Path -LiteralPath $Option)) {
            Write-Problem 'Say which backup to bring back, like: .\loski restore loski-backup-2026-10-26.tar.gz'
            exit 1
        }
        $inside = tar -tzf $Option 2>$null
        if ($LASTEXITCODE -ne 0 -or -not ($inside | Where-Object { $_ -like 'data/*' })) {
            Write-Problem "That doesn't look like a Loski backup (there's no data folder in it)."
            exit 1
        }
        if ((Test-Docker) -and (Test-Running)) {
            Write-Step 'Stopping Loski first'
            docker compose stop
        }
        # What's here now is kept, in case the backup was the wrong one.
        $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
        if (Test-Path -LiteralPath 'data') {
            Rename-Item -LiteralPath 'data' -NewName "data.before-restore-$stamp"
            Write-Done "Kept what was here in data.before-restore-$stamp"
        }
        if ((Test-Path -LiteralPath '.env') -and ($inside -contains '.env')) {
            Copy-Item -LiteralPath '.env' -Destination ".env.before-restore-$stamp"
        }
        Write-Step "Bringing back $Option"
        tar -xzf $Option
        if ($LASTEXITCODE -ne 0) {
            Write-Problem 'The backup could not be unpacked (see above).'
            exit 1
        }
        Write-Done 'Restored.'
        if (-not (Test-Env) -or -not (Start-Docker)) { exit 1 }
        $build = -not (Test-Image) -and -not (Update-Image)
        if (-not (Start-Loski -Build:$build)) { exit 1 }
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
