mod audio;
mod calendar;
mod music;
mod tasks;
mod widget;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use audio::{Audio, LAYERS, LAYER_NAMES};
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
    /// Focus sessions per day you're aiming for.
    goal: u32,
}

impl Default for Timeline {
    fn default() -> Self {
        Self { focus: 25, short: 5, long: 15, cycles: 4, auto_breaks: true, auto_focus: false, goal: 8 }
    }
}

impl Timeline {
    fn clamped(self) -> Self {
        Self {
            focus: self.focus.clamp(1, 180),
            short: self.short.clamp(1, 60),
            long: self.long.clamp(1, 90),
            cycles: self.cycles.clamp(1, 12),
            goal: self.goal.clamp(1, 24),
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
    #[serde(deserialize_with = "mix_from_any")]
    focus_mix: [f32; LAYERS],
    #[serde(deserialize_with = "mix_from_any")]
    break_mix: [f32; LAYERS],
    muted: bool,
    /// Your saved sound scenes (the built-in ones live in the UI).
    scenes: Vec<Scene>,
    /// Ambience fades down over the last minute of focus, so the chime doesn't jolt you.
    fade_out: bool,
    timeline: Timeline,
    /// Private iCal link. Stays on this PC.
    calendar_url: Option<String>,
    music: MusicSettings,
    pin: widget::Pin,
    /// Where you last left the widget, in physical pixels.
    window_pos: Option<[i32; 2]>,
    /// The "still running in the tray" notice is shown once.
    tray_hint_shown: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct Scene {
    name: String,
    #[serde(deserialize_with = "mix_from_any")]
    levels: [f32; LAYERS],
}

/// Mixes saved by older versions have fewer layers; the missing ones start silent.
fn mix_from_any<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[f32; LAYERS], D::Error> {
    let saved = Vec::<f32>::deserialize(d)?;
    let mut mix = [0.0; LAYERS];
    for (slot, v) in mix.iter_mut().zip(saved) {
        *slot = v.clamp(0.0, 1.0);
    }
    Ok(mix)
}

fn mix_from_slice(levels: &[f32]) -> [f32; LAYERS] {
    std::array::from_fn(|i| levels.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0))
}

/// Your recordings live in `<config>/sounds/` and are named after their layer: `rain.mp3`, `fire.ogg`...
fn custom_sounds(dir: &Path) -> [Option<PathBuf>; LAYERS] {
    let files: Vec<PathBuf> = std::fs::read_dir(dir.join("sounds"))
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    let playable = |p: &PathBuf| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| ["mp3", "ogg", "wav", "flac"].contains(&e.to_ascii_lowercase().as_str()))
    };
    std::array::from_fn(|i| {
        files
            .iter()
            .find(|p| {
                playable(p) && p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case(LAYER_NAMES[i]))
            })
            .cloned()
    })
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
struct MusicSettings {
    /// Spotify plays while you focus and pauses when the timer stops.
    follow: bool,
    /// With `follow`, keep Spotify playing through breaks too.
    on_breaks: bool,
}

impl Default for MusicSettings {
    fn default() -> Self {
        Self { follow: true, on_breaks: false }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            focus_mix: [0.6, 0.45, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            break_mix: [0.5, 0.0, 0.35, 0.0, 0.0, 0.0, 0.0, 0.0],
            muted: false,
            scenes: Vec::new(),
            fade_out: true,
            timeline: Timeline::default(),
            calendar_url: None,
            music: MusicSettings::default(),
            pin: widget::Pin::default(),
            window_pos: None,
            tray_hint_shown: false,
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
    calendar: Mutex<CalendarStatus>,
    /// Wakes the calendar thread for an immediate sync.
    calendar_sync: Sender<()>,
    music: Sender<music::Cmd>,
    now_playing: Arc<Mutex<music::NowPlaying>>,
    tray: OnceLock<widget::Tray>,
    /// Set by Quit, so closing windows on the way out isn't mistaken for "hide to tray".
    quitting: AtomicBool,
}

#[derive(Clone, Default, Serialize)]
struct CalendarStatus {
    connected: bool,
    syncing: bool,
    last_sync: Option<String>,
    error: Option<String>,
    events: usize,
}

impl App {
    /// Ambience plays only while the timer runs, using the mix for the current phase.
    fn sync_audio(&self) {
        let (mode, running, left) = {
            let t = self.timer.lock().unwrap();
            (t.mode, t.running, t.left())
        };
        let s = self.settings.lock().unwrap();
        let mut levels = match (running && !s.muted, mode) {
            (false, _) => [0.0; LAYERS],
            (true, Mode::Focus) => s.focus_mix,
            (true, _) => s.break_mix,
        };
        // Over the last minute of focus the ambience settles down to a fifth, then the chime.
        if s.fade_out && running && mode == Mode::Focus && left < Duration::from_secs(60) {
            let k = 0.2 + 0.8 * left.as_secs_f32() / 60.0;
            levels.iter_mut().for_each(|v| *v *= k);
        }
        self.audio.set_targets(levels);
    }

    fn set_muted(&self, muted: bool) {
        self.settings.lock().unwrap().muted = muted;
        self.sync_audio();
        self.save_settings();
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

    /// Runs a timer action, then brings the ambience and music in line with the new state.
    fn with_timer(&self, f: impl FnOnce(&mut Timer)) -> Snapshot {
        let snap = {
            let mut t = self.timer.lock().unwrap();
            f(&mut t);
            t.snapshot()
        };
        self.sync_audio();
        self.sync_music();
        snap
    }

    /// Lets Spotify follow the timer, if that's switched on. Called only when the timer
    /// changes state, so you can still override the music by hand mid-session.
    fn sync_music(&self) {
        let m = self.settings.lock().unwrap().music;
        if !m.follow {
            return;
        }
        let (mode, running) = {
            let t = self.timer.lock().unwrap();
            (t.mode, t.running)
        };
        let play = running && (mode == Mode::Focus || m.on_breaks);
        self.music_cmd(music::Cmd::Follow { play });
    }

    fn music_cmd(&self, cmd: music::Cmd) {
        let _ = self.music.send(cmd);
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

/// Keeps today's calendar events in the task list: on start, every 15 minutes, and on request.
fn spawn_calendar(app: AppHandle, wake: mpsc::Receiver<()>) {
    std::thread::spawn(move || {
        let state = app.state::<App>();
        let publish = |f: &dyn Fn(&mut CalendarStatus)| {
            let status = {
                let mut s = state.calendar.lock().unwrap();
                f(&mut s);
                s.clone()
            };
            let _ = app.emit("calendar-status", status);
        };
        loop {
            if wake.recv_timeout(Duration::from_secs(15 * 60)) == Err(mpsc::RecvTimeoutError::Disconnected) {
                return;
            }
            let url = state.settings.lock().unwrap().calendar_url.clone();
            let Some(url) = url else {
                state.with_tasks(|t| t.replace_calendar(Vec::new()));
                publish(&|s| *s = CalendarStatus::default());
                let _ = app.emit("tasks-changed", ());
                continue;
            };
            publish(&|s| {
                s.connected = true;
                s.syncing = true;
            });
            match calendar::fetch(&url) {
                Ok(ics) => {
                    let minutes = state.timer.lock().unwrap().tl.focus;
                    let events = calendar::today_tasks(&ics, chrono::Local::now().date_naive(), minutes);
                    let count = events.len();
                    state.with_tasks(|t| t.replace_calendar(events));
                    let now = chrono::Local::now().format("%H:%M").to_string();
                    publish(&|s| {
                        s.syncing = false;
                        s.last_sync = Some(now.clone());
                        s.error = None;
                        s.events = count;
                    });
                    let _ = app.emit("tasks-changed", ());
                }
                Err(e) => publish(&|s| {
                    s.syncing = false;
                    s.error = Some(e.clone());
                }),
            }
        }
    });
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
                let _ = state.calendar_sync.send(()); // fetch the new day's events
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
                state.sync_music();
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
                if snap.running && snap.mode == Mode::Focus && snap.remaining <= 61 {
                    state.sync_audio(); // the last-minute fade
                }
                let _ = app.emit("tick", snap.clone());
                if let Some(tray) = state.tray.get() {
                    widget::update_tray(tray, &snap, last.as_ref());
                }
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
fn progress_get(app: State<'_, App>) -> tasks::Progress {
    app.history.lock().unwrap().progress()
}

#[tauri::command]
fn calendar_status(app: State<'_, App>) -> CalendarStatus {
    app.calendar.lock().unwrap().clone()
}

/// Saves (or, with an empty string, removes) the iCal link and syncs right away.
#[tauri::command]
fn calendar_set_url(app: State<'_, App>, url: String) {
    let url = url.trim();
    app.settings.lock().unwrap().calendar_url = (!url.is_empty()).then(|| url.to_string());
    app.save_settings();
    let _ = app.calendar_sync.send(());
}

#[tauri::command]
fn calendar_refresh(app: State<'_, App>) {
    let _ = app.calendar_sync.send(());
}

#[tauri::command]
fn music_now(app: State<'_, App>) -> music::NowPlaying {
    app.now_playing.lock().unwrap().clone()
}

#[tauri::command]
fn music_control(app: State<'_, App>, action: String) {
    let cmd = match action.as_str() {
        "next" => music::Cmd::Next,
        "previous" => music::Cmd::Previous,
        _ => music::Cmd::Toggle,
    };
    app.music_cmd(cmd);
}

#[tauri::command]
fn music_settings_set(app: State<'_, App>, follow: bool, on_breaks: bool) {
    app.settings.lock().unwrap().music = MusicSettings { follow, on_breaks };
    app.save_settings();
    app.sync_music();
}

/// Opens (or focuses) the Spotify app through its URL scheme.
#[tauri::command]
fn music_open_spotify() {
    let _ = std::process::Command::new("explorer.exe").arg("spotify:").spawn();
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

/// Sets a whole mix at once, e.g. from a scene.
#[tauri::command]
fn sound_set_mix(app: State<'_, App>, scope: String, levels: Vec<f32>) {
    {
        let mut s = app.settings.lock().unwrap();
        let mix = mix_from_slice(&levels);
        if scope == "focus" { s.focus_mix = mix } else { s.break_mix = mix }
    }
    app.sync_audio();
    app.save_settings();
}

#[tauri::command]
fn scene_save(app: State<'_, App>, name: String, levels: Vec<f32>) -> Vec<Scene> {
    let name: String = name.trim().chars().take(24).collect();
    {
        let mut s = app.settings.lock().unwrap();
        if !name.is_empty() {
            s.scenes.retain(|sc| sc.name != name);
            s.scenes.push(Scene { name, levels: mix_from_slice(&levels) });
        }
    }
    app.save_settings();
    app.settings.lock().unwrap().scenes.clone()
}

#[tauri::command]
fn scene_delete(app: State<'_, App>, name: String) -> Vec<Scene> {
    app.settings.lock().unwrap().scenes.retain(|sc| sc.name != name);
    app.save_settings();
    app.settings.lock().unwrap().scenes.clone()
}

#[tauri::command]
fn fade_out_set(app: State<'_, App>, on: bool) {
    app.settings.lock().unwrap().fade_out = on;
    app.save_settings();
    app.sync_audio();
}

/// Rescans the sounds folder, swaps layers to match, and says which layers use your recordings.
#[tauri::command]
fn sounds_custom(app: State<'_, App>) -> Vec<bool> {
    let custom = custom_sounds(&app.dir);
    let flags = custom.iter().map(Option::is_some).collect();
    app.audio.set_custom(custom);
    flags
}

#[tauri::command]
fn sounds_open_folder(app: State<'_, App>) {
    let folder = app.dir.join("sounds");
    let _ = std::fs::create_dir_all(&folder);
    let _ = std::process::Command::new("explorer.exe").arg(&folder).spawn();
}

#[tauri::command]
fn sound_set_muted(app: State<'_, App>, muted: bool) {
    app.set_muted(muted);
    if let Some(tray) = app.tray.get() {
        widget::set_tray_muted(tray, muted);
    }
}

#[tauri::command]
fn window_pin(window: tauri::WebviewWindow, app: State<'_, App>, pin: widget::Pin) {
    app.settings.lock().unwrap().pin = pin;
    app.save_settings();
    widget::apply_pin(&window, pin);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Launching Pomodoro again just brings the running widget back.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| widget::show(app)))
        .plugin(tauri_plugin_notification::init())
        .on_window_event(widget::on_window_event)
        .setup(|app| {
            let dir = app.path().app_config_dir()?;
            let settings: Settings = tasks::load(&dir.join("settings.json"));
            let custom = custom_sounds(&dir);
            let (calendar_sync, calendar_wake) = mpsc::channel();
            let (music_tx, music_rx) = mpsc::channel();
            let now_playing = Arc::new(Mutex::new(music::NowPlaying::default()));
            let _ = calendar_sync.send(()); // first sync as soon as the thread starts
            app.manage(App {
                timer: Mutex::new(Timer::new(settings.timeline.clamped())),
                settings: Mutex::new(settings),
                tasks: Mutex::new(tasks::load(&dir.join("tasks.json"))),
                history: Mutex::new(tasks::load(&dir.join("history.json"))),
                dir,
                audio: Audio::start(custom),
                calendar: Mutex::new(CalendarStatus::default()),
                calendar_sync,
                music: music_tx,
                now_playing: now_playing.clone(),
                tray: OnceLock::new(),
                quitting: AtomicBool::new(false),
            });
            let tray = widget::build_tray(app.handle())?;
            let _ = app.state::<App>().tray.set(tray);
            widget::show(app.handle());
            music::spawn(app.handle().clone(), music_rx, now_playing);
            spawn_ticker(app.handle().clone());
            spawn_calendar(app.handle().clone(), calendar_wake);
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
            progress_get,
            music_now,
            music_control,
            music_settings_set,
            music_open_spotify,
            calendar_status,
            calendar_set_url,
            calendar_refresh,
            sound_state,
            sound_set_level,
            sound_set_muted,
            sound_set_mix,
            scene_save,
            scene_delete,
            fade_out_set,
            sounds_custom,
            sounds_open_folder,
            window_pin
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app, event| {
            // Closing the widget leaves Pomodoro running in the tray; only Quit exits.
            if let tauri::RunEvent::ExitRequested { code: None, api, .. } = event {
                api.prevent_exit();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings saved by 0.2 (three sound layers) must load whole: nothing resets on upgrade.
    #[test]
    fn old_settings_upgrade_without_losing_anything() {
        let saved = r#"{"focus_mix":[0.6,0.45,0.0],"break_mix":[0.5,0.0,0.35],"muted":true,
            "calendar_url":"https://calendar.example/basic.ics","timeline":{"focus":50,"goal":6},
            "pin":"desktop","window_pos":[100,200],"tray_hint_shown":true}"#;
        let s: Settings = serde_json::from_str(saved).unwrap();
        assert_eq!(s.focus_mix, [0.6, 0.45, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(s.break_mix[2], 0.35);
        assert!(s.muted);
        assert_eq!(s.calendar_url.as_deref(), Some("https://calendar.example/basic.ics"));
        assert_eq!((s.timeline.focus, s.timeline.goal, s.timeline.short), (50, 6, 5));
        assert!(s.pin == widget::Pin::Desktop && s.window_pos == Some([100, 200]));
        assert!(s.fade_out && s.scenes.is_empty(), "new settings get their defaults");
    }
}

#[cfg(test)]
mod live {
    /// Run by hand: checks the real settings file loads in this version (prints no values).
    #[test]
    #[ignore]
    fn real_settings_load() {
        let path = std::path::PathBuf::from(std::env::var("APPDATA").unwrap())
            .join("com.awshanaqtah.pomodoro")
            .join("settings.json");
        let text = std::fs::read_to_string(path).unwrap();
        let s: super::Settings = serde_json::from_str(&text).expect("settings load");
        println!(
            "loaded: calendar linked: {}, focus {} min, layers in focus mix: {}",
            s.calendar_url.is_some(),
            s.timeline.focus,
            s.focus_mix.len()
        );
    }
}
