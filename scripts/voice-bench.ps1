# Makes the clips for the voice commands bench (see `voice_bench` in
# src/speech.rs) with the voices Windows can speak in: commands, each with
# what it should do, and everyday talk, where nothing should happen.
#
#   powershell -File scripts\voice-bench.ps1 [-Out data\voice-bench]

param([string]$Out = (Join-Path $PSScriptRoot "..\data\voice-bench"))

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Speech

# What's said, a tab, and what it should do.
$commands = @(
    "Loski, stop`tPause",
    "Loski start`tResume",
    "Loski, pause the music`tPause",
    "Loski, resume`tResume",
    "Okay Loski, pause`tPause",
    "Loski, skip`tSkip",
    "Loski, skip this song`tSkip",
    "Loski, next song please`tSkip",
    "Loski, can you skip this one`tSkip",
    "Loski, go back`tPrevious",
    "Loski, previous song`tPrevious",
    "Loski, volume thirty`tVolume(30)",
    "Loski, set the volume to fifty`tVolume(50)",
    "Loski, turn it up`tLouder",
    "Loski, quieter`tQuieter",
    "Loski, turn it down a bit`tQuieter",
    "Hey Loski, shuffle`tShuffle",
    "Loski, shuffle the queue`tShuffle",
    "Loski, play Numb by Linkin Park`tPlay:numb by linkin park",
    "Loski, play Bohemian Rhapsody`tPlay:bohemian rhapsody",
    "Loski, play Blinding Lights by The Weeknd`tPlay:blinding lights by the weeknd",
    "Loski, play The Diary of Jane`tPlay:the diary of jane",
    "Loski, I like this song`tLike",
    "Loski, like this`tLike",
    "Loski, leave`tLeave",
    "Loski, what's playing?`tWhatsPlaying",
    "Loski, what song is this?`tWhatsPlaying",
    "Loski, play Numb next`tPlayNext:numb",
    "Loski, add Bohemian Rhapsody`tAdd:bohemian rhapsody",
    "Loski, play my liked songs`tLiked",
    "Loski, play my playlist Chill Vibes`tPlaylist:chill vibes",
    "Loski, more like this`tRadio",
    "Loski, repeat this song`tRepeat(Song)",
    "Loski, repeat the queue`tRepeat(Queue)",
    "Loski, stop repeating`tRepeat(Off)",
    "Loski, forward thirty seconds`tSeek(30)",
    "Loski, go back ten seconds`tSeek(-10)",
    "Loski, sleep in thirty minutes`tSleep(Some(30))",
    "Loski, stop after this song`tSleep(None)",
    "Loski, cancel the sleep timer`tSleepOff"
)

# Said in a call, not to Loski: nothing should happen.
$talk = @(
    "I want to skip this level, it's way too hard.",
    "Stop it, that's so funny.",
    "Can you play the game with us tonight?",
    "Next time we should go to the beach.",
    "Turn it up, I can't hear you at all.",
    "Loski is honestly the best bot we have.",
    "I told Loski to stop yesterday and it did.",
    "Hey, what do you want to play next?",
    "Let's go back to the lobby and wait.",
    "Pause the video for a second.",
    "Did you like this song or not?",
    "Lower the volume on your mic, please.",
    "We could shuffle the teams this round.",
    "I think Loski played this one before.",
    "Wait, wait, wait, someone is at the door.",
    "Leave the party and join mine.",
    "That was the best match all week.",
    "My brother keeps stealing my headphones.",
    "Do you remember the name of that song?",
    "Play it again, that was hilarious.",
    "We need one more player for the ranked game.",
    "Who wants pizza after this?",
    "Honestly the new update ruined everything.",
    "Loski, you are the best, you know that?",
    "Go back to the start, you missed the key.",
    "Let's start the next round now.",
    "Can you send me the link to that playlist?",
    "I love this song so much.",
    "Skip the cutscene, it takes forever.",
    "The previous season was way better than this one.",
    "What's playing on the radio this weekend?",
    "Add me on Discord after the game.",
    "I could listen to this on repeat all day.",
    "Fast forward to the good part of the movie.",
    "I'm going to sleep in thirty minutes, I swear."
)

$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$voices = $synth.GetInstalledVoices() |
    Where-Object { $_.Enabled -and $_.VoiceInfo.Culture.Name -like "en-*" } |
    ForEach-Object { $_.VoiceInfo.Name }
if (-not $voices) { throw "No English voices installed (Settings > Time & language > Speech)." }
$format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000,
    [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
    [System.Speech.AudioFormat.AudioChannel]::Mono)
$utf8 = New-Object System.Text.UTF8Encoding($false)

function Say($dir, $lines) {
    New-Item -ItemType Directory -Force $dir | Out-Null
    $i = 0
    foreach ($voice in $voices) {
        $synth.SelectVoice($voice)
        $who = $voice.Split(" ")[1]
        foreach ($line in $lines) {
            $i++
            $name = Join-Path $dir ("{0:D3}-{1}" -f $i, $who)
            $synth.SetOutputToWaveFile("$name.wav", $format)
            $synth.Speak($line.Split("`t")[0])
            $synth.SetOutputToNull()
            [IO.File]::WriteAllText("$name.txt", $line, $utf8)
        }
    }
    $i
}

$made = (Say (Join-Path $Out "commands") $commands) + (Say (Join-Path $Out "talk") $talk)
"$made clips in $Out, spoken by $($voices -join ', ')"
