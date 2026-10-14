mod audio;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use audio::{Audio, LAYERS};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_notification::NotificationExt;

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Focus,
    Short,
    Long,
}

/// Phase lengths in seconds. Becomes user-editable once settings land (Phase 4).
struct Durations {
    focus: u64,
    short: u64,
    long: u64,
    cycles_per_long: u32,
}

/// The timer lives in Rust, not the webview, so it keeps exact time even when
/// the window is hidden or the webview is torn down to save memory.
struct Timer {
    mode: Mode,
    running: bool,
    ends_at: Option<Instant>,
    paused_left: Duration,
    completed: u32,
    d: Durations,
}

#[derive(Clone, PartialEq, Serialize)]
struct Snapshot {
    mode: Mode,
    remaining: u64,
    total: u64,
    running: bool,
    completed: u32,
    cycles_per_long: u32,
}

impl Timer {
    fn new() -> Self {
        let d = Durations { focus: 25 * 60, short: 5 * 60, long: 15 * 60, cycles_per_long: 4 };
        Self {
            mode: Mode::Focus,
            running: false,
            ends_at: None,
            paused_left: Duration::from_secs(d.focus),
            completed: 0,
            d,
        }
    }

    fn total(&self) -> Duration {
        Duration::from_secs(match self.mode {
            Mode::Focus => self.d.focus,
            Mode::Short => self.d.short,
            Mode::Long => self.d.long,
        })
    }

    fn left(&self) -> Duration {
        match self.ends_at {
            Some(end) => end.saturating_duration_since(Instant::now()),
            None => self.paused_left,
        }
    }

    fn toggle(&mut self) {
        if self.running {
            self.paused_left = self.left();
            self.ends_at = None;
        } else {
            self.ends_at = Some(Instant::now() + self.paused_left);
        }
        self.running = !self.running;
    }

    fn reset(&mut self) {
        self.running = false;
        self.ends_at = None;
        self.paused_left = self.total();
    }

    /// Move to the next phase. Only a finished focus session counts toward the long break.
    fn advance(&mut self, counted: bool) {
        self.mode = match self.mode {
            Mode::Focus => {
                if counted {
                    self.completed += 1;
                }
                if counted && self.completed % self.d.cycles_per_long == 0 {
                    Mode::Long
                } else {
                    Mode::Short
                }
            }
            Mode::Short | Mode::Long => Mode::Focus,
        };
        self.reset();
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            mode: self.mode,
            remaining: self.left().as_millis().div_ceil(1000) as u64,
            total: self.total().as_secs(),
            running: self.running,
            completed: self.completed,
            cycles_per_long: self.d.cycles_per_long,
        }
    }
}

/// Saved to `settings.json` in the app config dir. Layer order: rain, fire, thunder.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    focus_mix: [f32; LAYERS],
    break_mix: [f32; LAYERS],
    muted: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { focus_mix: [0.6, 0.45, 0.0], break_mix: [0.5, 0.0, 0.35], muted: false }
    }
}

struct App {
    timer: Mutex<Timer>,
    settings: Mutex<Settings>,
    settings_path: PathBuf,
    audio: Audio,
}

impl App {
    /// Ambience plays only while the timer runs, using the mix for the current phase.
    fn sync_audio(&self) {
        let (mode, running) = {
            let t = self.timer.lock().unwrap();
            (t.mode, t.running)
        };
        let s = self.settings.lock().unwrap();
        let levels = match (running && !s.muted, mode) {
            (false, _) => [0.0; LAYERS],
            (true, Mode::Focus) => s.focus_mix,
            (true, _) => s.break_mix,
        };
        self.audio.set_targets(levels);
    }

    fn save_settings(&self) {
        let json = serde_json::to_string_pretty(&*self.settings.lock().unwrap()).unwrap();
        if let Some(dir) = self.settings_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&self.settings_path, json) {
            eprintln!("could not save settings: {e}");
        }
    }

    /// Runs a timer action, then brings the ambience in line with the new state.
    fn with_timer(&self, f: impl FnOnce(&mut Timer)) -> Snapshot {
        let snap = {
            let mut t = self.timer.lock().unwrap();
            f(&mut t);
            t.snapshot()
        };
        self.sync_audio();
        snap
    }
}

fn notify(app: &AppHandle, ended: Mode, next: Mode) {
    let (title, body) = match (ended, next) {
        (Mode::Focus, Mode::Long) => ("Focus done, long break", "You finished a full set. Take 15 minutes."),
        (Mode::Focus, _) => ("Focus done, take a break", "Stretch, drink some water. 5 minutes."),
        _ => ("Break's over", "Ready for the next focus session?"),
    };
    let _ = app.notification().builder().title(title).body(body).show();
}

/// Background ticker: finishes phases and pushes a snapshot to the UI only when something changed.
fn spawn_ticker(app: AppHandle) {
    std::thread::spawn(move || {
        let state = app.state::<App>();
        let mut last: Option<Snapshot> = None;
        loop {
            let (snap, finished) = {
                let mut t = state.timer.lock().unwrap();
                let mut finished = None;
                if t.running && t.left().is_zero() {
                    let ended = t.mode;
                    t.advance(true);
                    // Breaks start on their own; the next focus session waits for you.
                    if t.mode != Mode::Focus {
                        t.toggle();
                    }
                    finished = Some((ended, t.mode));
                }
                (t.snapshot(), finished)
            };
            if let Some((ended, next)) = finished {
                state.sync_audio();
                state.audio.chime();
                notify(&app, ended, next);
                let _ = app.emit("phase-done", ended);
            }
            if last.as_ref() != Some(&snap) {
                let _ = app.emit("tick", snap.clone());
                last = Some(snap);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    });
}

#[tauri::command]
fn timer_state(app: State<'_, App>) -> Snapshot {
    app.timer.lock().unwrap().snapshot()
}

#[tauri::command]
fn timer_toggle(app: State<'_, App>) -> Snapshot {
    app.with_timer(Timer::toggle)
}

#[tauri::command]
fn timer_reset(app: State<'_, App>) -> Snapshot {
    app.with_timer(Timer::reset)
}

#[tauri::command]
fn timer_skip(app: State<'_, App>) -> Snapshot {
    app.with_timer(|t| t.advance(false))
}

#[tauri::command]
fn sound_state(app: State<'_, App>) -> Settings {
    app.settings.lock().unwrap().clone()
}

/// Live slider changes. `save` is sent once when the slider is released, not on every step.
#[tauri::command]
fn sound_set_level(app: State<'_, App>, scope: String, layer: usize, value: f32, save: bool) {
    {
        let mut s = app.settings.lock().unwrap();
        let mix = if scope == "focus" { &mut s.focus_mix } else { &mut s.break_mix };
        if let Some(slot) = mix.get_mut(layer) {
            *slot = value.clamp(0.0, 1.0);
        }
    }
    app.sync_audio();
    if save {
        app.save_settings();
    }
}

#[tauri::command]
fn sound_set_muted(app: State<'_, App>, muted: bool) {
    app.settings.lock().unwrap().muted = muted;
    app.sync_audio();
    app.save_settings();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let settings = std::fs::read_to_string(&settings_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            app.manage(App {
                timer: Mutex::new(Timer::new()),
                settings: Mutex::new(settings),
                settings_path,
                audio: Audio::start(),
            });
            spawn_ticker(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            timer_state,
            timer_toggle,
            timer_reset,
            timer_skip,
            sound_state,
            sound_set_level,
            sound_set_muted
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
