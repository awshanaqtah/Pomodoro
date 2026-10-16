# Pomodoro — plan

## Background

A Pomodoro timer that lives on the desktop as a small widget. Three things matter, in order:

1. **Sound.** Layered ambience: rain and fire playing together, thunder on top, and your own music
   alongside it. Each layer has its own volume. This is the main feature, not an extra.
2. **Lightness.** It runs all day, so it has to stay close to invisible in Task Manager.
3. **Feel.** A calm nature theme ("forest at dusk"): an ember accent while focusing, rain-blue on
   short breaks, moss-green on long breaks.

## Stack

| Layer | Choice | Why |
|---|---|---|
| Shell | **Tauri v2** | Uses Windows' built-in WebView2 instead of bundling Chromium. Installer is about 5 MB; Electron would be 80+ MB and use 150–300 MB of RAM. |
| Core | **Rust** | The timer and audio live here, so they keep running when the window is hidden. |
| Audio | **cpal** (Rust) | One mixer callback; rain, fire and thunder are synthesized live, so there are no audio files to ship. |
| Music | **Windows media controls** (`windows` crate) | Shows and controls Spotify (or any player) with no login and no API keys. |
| Calendar | **ureq** + **rrule** | Reads a private iCal link and expands repeating events. |
| UI | **Vanilla TypeScript + CSS**, built with Vite | No framework runtime. The UI only draws state that Rust sends it. |
| Settings | JSON file in `%APPDATA%` | No accounts, no cloud. |

Options we passed on: Electron (too heavy); native Rust UI such as egui or iced (lighter still, but
a polished nature theme takes much more work); WPF or WinUI (needs the .NET runtime).

### How it stays light

- The timer is one Rust thread that wakes every 200 ms. It sends an event to the UI only when the
  displayed second changes.
- When the widget goes to the tray, the webview is **destroyed**, not just hidden. Only the Rust
  process stays, playing audio and keeping time (target ~10–15 MB). Reopening the widget recreates
  the webview.
- Audio loops are OGG files of about 1–3 MB, streamed from disk rather than held in memory.
- Release builds use LTO, strip symbols and `panic = "abort"` (already set in `Cargo.toml`).

## Architecture

```
┌──────────── Rust (always alive) ────────────┐        ┌──── Webview (optional) ────┐
│ Timer thread ── tick / phase-done events ───┼──────▶ │ render(snapshot)           │
│ Audio engine (rodio): rain │ fire │ thunder │        │ ring, time, controls,      │
│                │ music playlist             │ ◀──────┼ invoke(timer_*, audio_*)   │
│ Tray icon, settings.json                    │        └────────────────────────────┘
└─────────────────────────────────────────────┘
```

## Process

| Phase | What ships | Status |
|---|---|---|
| 0 | Repo, Tauri scaffold, frameless widget window, Rust timer core | ✅ done |
| 2 | **Sound engine**: synthesized rain, fire and thunder, a slider for each, fades, separate focus and break mixes, mute, chime, Windows notification, auto-rebuild on device change | ✅ done |
| 1 | **Your timeline**: you set the focus, short break and long break lengths, sessions per set and a daily goal; auto-start options | ✅ done |
| 3 | **Tasks, progress, rewards** (see below): tasks, Google Calendar import, Progress page and growing forest are done; streak unlocks are still to do | ✅ mostly |
| 4 | **Spotify**: now-playing card with album art and ⏮ ⏯ ⏭ through Windows media controls (no login, works with Spotify Free); Spotify plays while you focus and pauses on breaks or pause, optionally keeps playing on breaks | ✅ done |
| 5 | **Widget polish**: tray icon (time left on hover, Start/Pause, Skip, Mute, Start with Windows, Quit), ✕ hides to the tray, pin modes (normal, always on top, pinned to the desktop), remembered position, single instance, app icon | ✅ done |
| 6 | **Lightness**: the webview is destroyed while hidden, so the tray app idles at about 8 MB (145 MB with the widget open); installer is 2.5 MB. Still to do: a GitHub release per version | ✅ mostly |

### Feature rounds (after 0.2)

| Round | Version | Features | Status |
|---|---|---|---|
| Sound | 0.3.0 | Wind, stream, ocean, crickets and brown noise layers; scenes (built-in and your own); your own recordings per layer from a folder; fade-out over the last minute of focus; Sounds tab | ✅ done |
| Focus | 0.4.0 | Mini mode (232×64 pill), global shortcuts (Ctrl+Alt+P / M / W), away pause that gives back the lost minutes, break ideas | ✅ done |
| Progress | 0.5.0 | Streak unlocks, forest calendar, time per task, session notes | ⏳ next |
| Look | 0.6.0 | Time-of-day theme, seasons, living background, size options | |
| Bigger | 0.7.0 | Auto-update through a public releases repo; focus together with Stella through a small Cloudflare Worker relay | |

### Phase 3: tasks, progress, rewards

- **Tasks.** A small checklist in the widget. Tasks are either *daily* (they reset each morning, for
  things done every day) or *one-off*. You pick the active task, and each finished focus session
  counts toward it (for example 2 of 3 sessions).
- **Progress.** Today's focus minutes against a daily goal (a ring), your streak in days, and a
  7-day bar chart.
- **Rewards: your forest grows.** The treeline at the bottom is your day. Each finished focus session
  plants a tree, and a full set grows a special glowing tree. Streaks unlock extras such as a night
  theme and new ambience layers (crickets, wind, ocean). Rewards are only ever gained; resetting a
  session takes nothing away.
- **Calendar import.** Paste your calendar's private iCal link once; today's events become
  tasks and resync every 15 minutes.
- **Storage.** `tasks.json` and `history.json` (one row per finished session: date, minutes, task)
  next to `settings.json`.
- **UI.** The widget gets three small pages, Timer, Tasks and Progress, so it stays compact.

## Sounds — sourcing

Ambience loops must be **CC0 or otherwise freely redistributable** because they ship inside the
app. Candidate sources: Freesound (filter by CC0) and Pixabay Sound Effects. Each loop needs to be
seamless (crossfaded at the loop point), encoded as OGG Vorbis at about 96–128 kbps, and normalized
to the same loudness.

Your own recordings are never bundled: put `rain.mp3`, `fire.ogg` and so on in the Custom sounds
folder (Sounds tab → Open folder) and that layer plays them, streamed from disk.
