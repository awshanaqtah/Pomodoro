mod audio;
mod tasks;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use audio::{Audio, LAYERS};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};
use tasks::{History, Kind, Session, Tasks};
use tauri_plugin_notification::NotificationExt;

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Focus,
    Short,
    Long,
}

/// Your timeline: phase lengths in minutes and how phases hand over to each other.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Timeline {
    focus: u32,
    short: u32,
    long: u32,
    cycles: u32,
    auto_breaks: bool,
    auto_focus: bool,
}

impl Default for Timeline {
    fn default() -> Self {
        Self { focus: 25, short: 5, long: 15, cycles: 4, auto_breaks: true, auto_focus: false }
    }
}

impl Timeline {
    fn clamped(self) -> Self {
        Self {
            focus: self.focus.clamp(1, 180),
            short: self.short.clamp(1, 60),
            long: self.long.clamp(1, 90),
            cycles: self.cycles.clamp(1, 12),
            ..self
        }
    }
}

/// The timer lives in Rust, not the webview, so it keeps exact time even when
/// the window is hidden or the webview is torn down to save memory.
struct Timer {
    mode: Mode,
    running: bool,
    ends_at: Option<Instant>,
    paused_left: Duration,
    completed: u32,
    tl: Timeline,
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
    fn new(tl: Timeline) -> Self {
        let mut t = Self {
            mode: Mode::Focus,
            running: false,
            ends_at: None,
            paused_left: Duration::ZERO,
            completed: 0,
            tl,
        };
        t.reset();
        t
    }

    fn total(&self) -> Duration {
        let minutes = match self.mode {
            Mode::Focus => self.tl.focus,
            Mode::Short => self.tl.short,
            Mode::Long => self.tl.long,
        };
        Duration::from_secs(minutes as u64 * 60)
    }

    /// New lengths apply right away only to a phase you haven't started yet.
    fn set_timeline(&mut self, tl: Timeline) {
        let untouched = !self.running && self.paused_left == self.total();
        self.tl = tl;
        if untouched {
            self.reset();
        }
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
                if counted && self.completed % self.tl.cycles == 0 {
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
            cycles_per_long: self.tl.cycles,
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
    timeline: Timeline,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            focus_mix: [0.6, 0.45, 0.0],
            break_mix: [0.5, 0.0, 0.35],
            muted: false,
            timeline: Timeline::default(),
        }
    }
}

struct App {
    timer: Mutex<Timer>,
    settings: Mutex<Settings>,
    tasks: Mutex<Tasks>,
    history: Mutex<History>,
    /// App config dir holding settings.json, tasks.json and history.json.
    dir: PathBuf,
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
        tasks::save(&self.dir.join("settings.json"), &*self.settings.lock().unwrap());
    }

    /// Edits the task list (after a day rollover if one is due), saves it, and returns it.
    fn with_tasks(&self, f: impl FnOnce(&mut Tasks)) -> Tasks {
        let mut t = self.tasks.lock().unwrap();
        t.roll_over();
        f(&mut t);
        tasks::save(&self.dir.join("tasks.json"), &*t);
        t.clone()
    }

    /// A finished focus session goes into the history and counts toward the active task.
    fn record_focus(&self, minutes: u32) {
        let list = self.with_tasks(|t| {
            if let Some(task) = t.active_task() {
                task.sessions += 1;
            }
        });
        let task = list.active.and_then(|id| list.list.into_iter().find(|t| t.id == id)).map(|t| t.title);
        let mut h = self.history.lock().unwrap();
        h.sessions.push(Session { day: tasks::today(), minutes, task });
        tasks::save(&self.dir.join("history.json"), &*h);
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
        let mut beat: u32 = 0;
        loop {
            // About once a minute, start a new day for the task list if midnight has passed.
            beat = beat.wrapping_add(1);
            if beat % 300 == 0 && state.tasks.lock().unwrap().roll_over() {
                tasks::save(&state.dir.join("tasks.json"), &*state.tasks.lock().unwrap());
                let _ = app.emit("tasks-changed", ());
            }
            let (snap, finished) = {
                let mut t = state.timer.lock().unwrap();
                let mut finished = None;
                if t.running && t.left().is_zero() {
                    let ended = t.mode;
                    t.advance(true);
                    let auto = if t.mode == Mode::Focus { t.tl.auto_focus } else { t.tl.auto_breaks };
                    if auto {
                        t.toggle();
                    }
                    finished = Some((ended, t.mode));
                }
                (t.snapshot(), finished)
            };
            if let Some((ended, next)) = finished {
                if ended == Mode::Focus {
                    let minutes = state.timer.lock().unwrap().tl.focus;
                    state.record_focus(minutes);
                    let _ = app.emit("tasks-changed", ());
                }
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
fn timeline_set(app: State<'_, App>, timeline: Timeline) -> Snapshot {
    let tl = timeline.clamped();
    app.settings.lock().unwrap().timeline = tl;
    app.save_settings();
    app.with_timer(|t| t.set_timeline(tl))
}

#[tauri::command]
fn tasks_get(app: State<'_, App>) -> Tasks {
    app.with_tasks(|_| {})
}

#[tauri::command]
fn task_add(app: State<'_, App>, title: String, kind: Kind, estimate: u32) -> Tasks {
    app.with_tasks(|t| {
        if !title.trim().is_empty() && kind != Kind::Calendar {
            t.add(&title, kind, estimate);
        }
    })
}

#[tauri::command]
fn task_update(app: State<'_, App>, id: String, done: Option<bool>, estimate: Option<u32>) -> Tasks {
    app.with_tasks(|t| {
        if let Some(task) = t.get_mut(&id) {
            if let Some(done) = done {
                task.done = done;
            }
            if let Some(estimate) = estimate {
                task.estimate = estimate.clamp(1, 12);
            }
        }
    })
}

#[tauri::command]
fn task_remove(app: State<'_, App>, id: String) -> Tasks {
    app.with_tasks(|t| t.remove(&id))
}

#[tauri::command]
fn task_set_active(app: State<'_, App>, id: Option<String>) -> Tasks {
    app.with_tasks(|t| t.active = id.filter(|id| t.list.iter().any(|task| &task.id == id)))
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
            let dir = app.path().app_config_dir()?;
            let settings: Settings = tasks::load(&dir.join("settings.json"));
            app.manage(App {
                timer: Mutex::new(Timer::new(settings.timeline.clamped())),
                settings: Mutex::new(settings),
                tasks: Mutex::new(tasks::load(&dir.join("tasks.json"))),
                history: Mutex::new(tasks::load(&dir.join("history.json"))),
                dir,
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
            timeline_set,
            tasks_get,
            task_add,
            task_update,
            task_remove,
            task_set_active,
            sound_state,
            sound_set_level,
            sound_set_muted
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
