//! The desktop-widget side: tray icon, show and hide, pin modes, remembered position and
//! start-with-Windows. Hiding destroys the webview (it's most of the memory); the timer,
//! sounds and Spotify keep running in Rust, and showing builds a fresh webview in under a second.

use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewWindow, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_notification::NotificationExt;

use crate::{App, Mode, Snapshot, Timer};

pub const MAIN: &str = "main";

#[derive(Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Pin {
    #[default]
    Normal,
    /// Above every other window.
    Top,
    /// Behind every other window, like a desktop gadget, and off the taskbar.
    Desktop,
}

pub struct Tray {
    icon: TrayIcon<Wry>,
    toggle: MenuItem<Wry>,
    skip: MenuItem<Wry>,
    mute: CheckMenuItem<Wry>,
    autostart: CheckMenuItem<Wry>,
}

// ---------------------------------------------------------------- window

pub fn apply_pin(window: &WebviewWindow, pin: Pin) {
    let _ = window.set_always_on_top(pin == Pin::Top);
    let _ = window.set_always_on_bottom(pin == Pin::Desktop);
    let _ = window.set_skip_taskbar(pin == Pin::Desktop);
}

/// Shows the widget, building it if it was closed to the tray.
pub fn show(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    if let Err(e) = create(app) {
        eprintln!("could not open the widget: {e}");
    }
}

/// Closes the widget to the tray. Its webview is destroyed to give the memory back.
pub fn hide(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(MAIN) {
        let _ = w.destroy();
    }
}

fn toggle(app: &AppHandle) {
    let visible = app
        .get_webview_window(MAIN)
        .is_some_and(|w| w.is_visible().unwrap_or(false) && !w.is_minimized().unwrap_or(false));
    if visible { hide(app) } else { show(app) }
}

fn create(app: &AppHandle) -> tauri::Result<()> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == MAIN)
        .cloned()
        .expect("main window is defined in tauri.conf.json");
    let window = WebviewWindowBuilder::from_config(app, &config)?.build()?;

    let (pos, pin) = {
        let s = app.state::<App>().settings.lock().unwrap().clone();
        (s.window_pos, s.pin)
    };
    if let Some([x, y]) = pos.filter(|p| on_screen(&window, *p)) {
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
    apply_pin(&window, pin);
    window.show()?; // created hidden, so it appears already in place
    window.set_focus()
}

/// True if a saved position is still on a connected monitor (screens get unplugged).
fn on_screen(window: &WebviewWindow, [x, y]: [i32; 2]) -> bool {
    window.available_monitors().unwrap_or_default().iter().any(|m| {
        let (p, s) = (m.position(), m.size());
        x >= p.x - 40 && y >= p.y - 10 && x < p.x + s.width as i32 - 80 && y < p.y + s.height as i32 - 80
    })
}

pub fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    if window.label() != MAIN {
        return;
    }
    let app = window.app_handle();
    let state = app.state::<App>();
    match event {
        // Minimizing reports a parking position of -32000; that's not where you put it.
        WindowEvent::Moved(p) if p.x > -30000 && p.y > -30000 => {
            state.settings.lock().unwrap().window_pos = Some([p.x, p.y]);
        }
        WindowEvent::Destroyed => {
            let first_time = {
                let mut s = state.settings.lock().unwrap();
                !std::mem::replace(&mut s.tray_hint_shown, true)
            };
            state.save_settings();
            if first_time && !state.quitting.load(Ordering::Relaxed) {
                let _ = app
                    .notification()
                    .builder()
                    .title("Pomodoro is still running")
                    .body("Your timer and sounds keep going. Click the tray icon to bring the widget back.")
                    .show();
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- tray

pub fn build_tray(app: &AppHandle) -> tauri::Result<Tray> {
    let muted = app.state::<App>().settings.lock().unwrap().muted;
    let show_item = MenuItem::with_id(app, "show", "Show widget", true, None::<&str>)?;
    let toggle_item = MenuItem::with_id(app, "toggle", "Start focus", true, None::<&str>)?;
    let skip = MenuItem::with_id(app, "skip", "Skip to break", true, None::<&str>)?;
    let mute = CheckMenuItem::with_id(app, "mute", "Mute ambience", true, muted, None::<&str>)?;
    let autostart = CheckMenuItem::with_id(app, "autostart", "Start with Windows", true, autostart_enabled(), None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Pomodoro", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &PredefinedMenuItem::separator(app)?,
            &toggle_item,
            &skip,
            &PredefinedMenuItem::separator(app)?,
            &mute,
            &autostart,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let icon = TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().expect("bundle has an icon"))
        .tooltip("Pomodoro")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                toggle(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(Tray { icon, toggle: toggle_item, skip, mute, autostart })
}

fn on_menu(app: &AppHandle, id: &str) {
    let state = app.state::<App>();
    match id {
        "show" => show(app),
        "toggle" => {
            state.with_timer(Timer::toggle);
        }
        "skip" => {
            state.with_timer(|t| t.advance(false));
        }
        "mute" => {
            let muted = state.tray.get().is_some_and(|t| t.mute.is_checked().unwrap_or(false));
            state.set_muted(muted);
            let _ = app.emit("muted", muted);
        }
        "autostart" => {
            let on = state.tray.get().is_some_and(|t| t.autostart.is_checked().unwrap_or(false));
            if let Err(e) = set_autostart(on) {
                eprintln!("could not change start with Windows: {e}");
            }
            if let Some(t) = state.tray.get() {
                let _ = t.autostart.set_checked(autostart_enabled());
            }
        }
        "quit" => {
            state.quitting.store(true, Ordering::Relaxed);
            state.save_settings();
            app.exit(0);
        }
        _ => {}
    }
}

/// Tooltip shows the time left; menu labels follow the timer. Called when the snapshot changes.
pub fn update_tray(tray: &Tray, snap: &Snapshot, last: Option<&Snapshot>) {
    let phase = match snap.mode {
        Mode::Focus => "Focus",
        Mode::Short => "Short break",
        Mode::Long => "Long break",
    };
    let clock = format!("{:02}:{:02}", snap.remaining / 60, snap.remaining % 60);
    let paused = if snap.running { "" } else { " (paused)" };
    let _ = tray.icon.set_tooltip(Some(format!("{phase} · {clock}{paused}")));

    if last.is_some_and(|l| l.running == snap.running && l.mode == snap.mode) {
        return; // labels only change with the phase or play state
    }
    let toggle = match (snap.running, snap.mode) {
        (true, _) => "Pause",
        (false, Mode::Focus) => "Start focus",
        (false, _) => "Start break",
    };
    let skip = if snap.mode == Mode::Focus { "Skip to break" } else { "Skip to focus" };
    let _ = tray.toggle.set_text(toggle);
    let _ = tray.skip.set_text(skip);
}

pub fn set_tray_muted(tray: &Tray, muted: bool) {
    let _ = tray.mute.set_checked(muted);
}

// ---------------------------------------------------------------- start with Windows

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_NAME: &str = "Pomodoro";

fn launch_command() -> Option<String> {
    std::env::current_exe().ok().map(|p| format!("\"{}\"", p.display()))
}

/// On only if the startup entry points at this copy of the app.
pub fn autostart_enabled() -> bool {
    let current = windows_registry::CURRENT_USER.open(RUN_KEY).and_then(|k| k.get_string(RUN_NAME)).ok();
    current.is_some() && current == launch_command()
}

fn set_autostart(on: bool) -> windows_registry::Result<()> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY)?;
    match launch_command() {
        Some(cmd) if on => key.set_string(RUN_NAME, cmd),
        _ => key.remove_value(RUN_NAME).or(Ok(())),
    }
}
