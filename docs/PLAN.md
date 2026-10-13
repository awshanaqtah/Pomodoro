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
| Audio | **rodio** (Rust) | Mixes several looping streams with per-layer volume. Decodes OGG, MP3, FLAC and WAV, so your own songs work too. |
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
| 1 | Timer polish: Windows notification at end of phase, chime from Rust, editable durations, auto-start options | ⏳ |
| 2 | **Sound engine**: rain, fire and thunder layers, a slider for each, fade in and out, a separate mix for focus and for breaks, master mute | |
| 3 | **Your own music**: pick a folder, shuffle or loop, play it over the ambience, optionally lower the ambience while a song plays | |
| 4 | Widget polish: tray menu, pin-to-desktop or always-on-top, start with Windows, settings saved, custom icon | |
| 5 | Lightness pass: measure RAM idle and playing, destroy the webview on hide, release installer, GitHub release | |

## Sounds — sourcing

Ambience loops must be **CC0 or otherwise freely redistributable** because they ship inside the
app. Candidate sources: Freesound (filter by CC0) and Pixabay Sound Effects. Each loop needs to be
seamless (crossfaded at the loop point), encoded as OGG Vorbis at about 96–128 kbps, and normalized
to the same loudness.

Your own songs are never bundled. The app reads them from a folder you choose.
