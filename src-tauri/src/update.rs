//! Updates come from a signed feed in the public Pomodoro-releases repo. The app checks a minute
//! after starting and then every six hours; installing is one click (tray or settings) and the
//! app reopens on the new version. The updater refuses anything not signed with our key.

use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::{widget, App};

#[derive(Clone, Serialize)]
pub struct UpdateState {
    pub current: String,
    pub available: Option<String>,
    /// idle, checking, available, installing or error.
    pub status: &'static str,
    pub error: Option<String>,
}

pub struct Updates {
    state: Mutex<UpdateState>,
    ready: Mutex<Option<Update>>,
}

impl Updates {
    pub fn new(current: String) -> Self {
        let state = UpdateState { current, available: None, status: "idle", error: None };
        Self { state: Mutex::new(state), ready: Mutex::new(None) }
    }

    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap().clone()
    }

    pub fn available(&self) -> bool {
        self.state.lock().unwrap().available.is_some()
    }
}

fn publish(app: &AppHandle, change: impl FnOnce(&mut UpdateState)) {
    let state = {
        let updates = app.state::<Updates>();
        let mut s = updates.state.lock().unwrap();
        change(&mut s);
        s.clone()
    };
    widget::set_tray_update(app, state.available.as_deref());
    let _ = app.emit("update", state);
}

/// Background checks stay quiet when offline; a check you asked for shows the error.
pub async fn check(app: AppHandle, quiet: bool) {
    publish(&app, |s| {
        s.status = "checking";
        s.error = None;
    });
    let result = match app.updater() {
        Ok(updater) => updater.check().await,
        Err(e) => Err(e),
    };
    match result {
        Ok(Some(update)) => {
            let version = update.version.clone();
            let first_time = app.state::<Updates>().state.lock().unwrap().available.as_deref() != Some(version.as_str());
            *app.state::<Updates>().ready.lock().unwrap() = Some(update);
            publish(&app, |s| {
                s.status = "available";
                s.available = Some(version.clone());
            });
            if first_time {
                let _ = app
                    .notification()
                    .builder()
                    .title(format!("Pomodoro {version} is ready"))
                    .body("Install it from the tray menu or ⚙ settings. It takes a few seconds.")
                    .show();
            }
        }
        Ok(None) => publish(&app, |s| {
            s.status = "idle";
            s.available = None;
        }),
        Err(e) => publish(&app, |s| {
            s.status = if quiet { "idle" } else { "error" };
            s.error = Some(e.to_string());
        }),
    }
}

/// Downloads, verifies and runs the new installer, which closes and reopens Pomodoro.
pub async fn install(app: AppHandle) {
    let ready = app.state::<Updates>().ready.lock().unwrap().take();
    let Some(update) = ready else {
        return check(app, false).await;
    };
    publish(&app, |s| s.status = "installing");
    app.state::<App>().save_settings();
    if let Err(e) = update.download_and_install(|_, _| {}, || {}).await {
        publish(&app, |s| {
            s.status = "error";
            s.error = Some(e.to_string());
        });
    }
}

pub fn spawn_checks(app: AppHandle) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(60));
        loop {
            tauri::async_runtime::block_on(check(app.clone(), true));
            std::thread::sleep(Duration::from_secs(6 * 60 * 60));
        }
    });
}
