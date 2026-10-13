use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

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

type SharedTimer = Arc<Mutex<Timer>>;

/// Background ticker: finishes phases and pushes a snapshot to the UI only when something changed.
fn spawn_ticker(app: AppHandle, timer: SharedTimer) {
    std::thread::spawn(move || {
        let mut last: Option<Snapshot> = None;
        loop {
            let (snap, finished) = {
                let mut t = timer.lock().unwrap();
                let mut finished = None;
                if t.running && t.left().is_zero() {
                    finished = Some(t.mode);
                    t.advance(true);
                    // Breaks start on their own; the next focus session waits for you.
                    if t.mode != Mode::Focus {
                        t.toggle();
                    }
                }
                (t.snapshot(), finished)
            };
            if let Some(ended) = finished {
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
fn timer_state(timer: State<'_, SharedTimer>) -> Snapshot {
    timer.lock().unwrap().snapshot()
}

#[tauri::command]
fn timer_toggle(timer: State<'_, SharedTimer>) -> Snapshot {
    let mut t = timer.lock().unwrap();
    t.toggle();
    t.snapshot()
}

#[tauri::command]
fn timer_reset(timer: State<'_, SharedTimer>) -> Snapshot {
    let mut t = timer.lock().unwrap();
    t.reset();
    t.snapshot()
}

#[tauri::command]
fn timer_skip(timer: State<'_, SharedTimer>) -> Snapshot {
    let mut t = timer.lock().unwrap();
    t.advance(false);
    t.snapshot()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let timer: SharedTimer = Arc::new(Mutex::new(Timer::new()));
    tauri::Builder::default()
        .manage(timer.clone())
        .setup(move |app| {
            spawn_ticker(app.handle().clone(), timer);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![timer_state, timer_toggle, timer_reset, timer_skip])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
