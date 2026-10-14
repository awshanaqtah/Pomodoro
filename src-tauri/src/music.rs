//! Now playing and playback control for Spotify (or whatever is playing) through the Windows
//! media session API, the same one behind the media keys and the volume flyout. No login,
//! works with Spotify Free, and costs one cheap query per second.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde::Serialize;
use tauri::{AppHandle, Emitter};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionMediaProperties as Properties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};

#[derive(Clone, Copy)]
pub enum Cmd {
    Toggle,
    Next,
    Previous,
    /// From the timer: make Spotify play or pause. Never touches other apps.
    Follow { play: bool },
}

#[derive(Clone, Default, PartialEq, Serialize)]
pub struct NowPlaying {
    pub available: bool,
    pub spotify: bool,
    pub app: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    /// Album art as a data: URI.
    pub art: Option<String>,
}

pub fn spawn(app: AppHandle, rx: Receiver<Cmd>, shared: Arc<Mutex<NowPlaying>>) {
    std::thread::spawn(move || {
        // SAFETY: called once at the start of this thread, before any other WinRT call on it.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let manager = match Manager::RequestAsync().and_then(|op| op.join()) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("media controls unavailable: {e}");
                return;
            }
        };
        let mut art = Art::default();
        let mut last = NowPlaying::default();
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Err(RecvTimeoutError::Disconnected) => return,
                Ok(cmd) => {
                    if let Err(e) = run(&manager, cmd) {
                        eprintln!("media control failed: {e}");
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = read(&manager, &mut art).unwrap_or_default();
            if now != last {
                *shared.lock().unwrap() = now.clone();
                let _ = app.emit("now-playing", now.clone());
                last = now;
            }
        }
    });
}

fn sessions(manager: &Manager) -> Vec<Session> {
    let Ok(list) = manager.GetSessions() else { return Vec::new() };
    let count = list.Size().unwrap_or(0);
    (0..count).filter_map(|i| list.GetAt(i).ok()).collect()
}

fn app_id(s: &Session) -> String {
    s.SourceAppUserModelId().map(|id| id.to_string()).unwrap_or_default()
}

fn spotify(manager: &Manager) -> Option<Session> {
    sessions(manager).into_iter().find(|s| app_id(s).to_lowercase().contains("spotify"))
}

/// What the card shows and the buttons control: Spotify if it's open, otherwise whatever
/// Windows considers the current media session.
fn current(manager: &Manager) -> Option<Session> {
    spotify(manager).or_else(|| manager.GetCurrentSession().ok())
}

fn is_playing(s: &Session) -> bool {
    s.GetPlaybackInfo().and_then(|i| i.PlaybackStatus()).is_ok_and(|st| st == Status::Playing)
}

fn run(manager: &Manager, cmd: Cmd) -> windows::core::Result<()> {
    let session = match cmd {
        Cmd::Follow { .. } => spotify(manager),
        _ => current(manager),
    };
    let Some(s) = session else { return Ok(()) };
    let done = match cmd {
        Cmd::Toggle => s.TryTogglePlayPauseAsync()?.join()?,
        Cmd::Next => s.TrySkipNextAsync()?.join()?,
        Cmd::Previous => s.TrySkipPreviousAsync()?.join()?,
        Cmd::Follow { play } if play != is_playing(&s) => {
            if play { s.TryPlayAsync()?.join()? } else { s.TryPauseAsync()?.join()? }
        }
        Cmd::Follow { .. } => true,
    };
    if !done {
        eprintln!("the media app ignored the request");
    }
    Ok(())
}

/// Turns "Spotify.exe", "Chrome", a full exe path or a Store id like
/// "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic" into a short name.
fn friendly(id: &str) -> String {
    // Store apps are "<package family>!<entry point>", and the entry point is often just "App".
    let (package, entry) = id.split_once('!').unwrap_or(("", id));
    let tail = if entry.eq_ignore_ascii_case("app") { package } else { entry };
    let tail = tail.rsplit('\\').next().unwrap_or(tail);
    let tail = tail.split('_').next().unwrap_or(tail); // drop the "_8wekyb3d8bbwe" publisher hash
    let name = tail.trim_end_matches(".exe").trim_end_matches(".EXE");
    let name = name.rsplit('.').next().unwrap_or(name);
    if name.to_lowercase().contains("spotify") { "Spotify".into() } else { name.to_string() }
}

/// Album art cache: re-read only when the track changes, retrying a few times because apps
/// often publish the artwork a moment after the title.
#[derive(Default)]
struct Art {
    track: String,
    uri: Option<String>,
    tries: u8,
}

fn read(manager: &Manager, art: &mut Art) -> Option<NowPlaying> {
    let s = current(manager)?;
    let props = s.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let title = props.Title().ok()?.to_string();
    let artist = props.Artist().map(|a| a.to_string()).unwrap_or_default();
    let id = app_id(&s);

    let track = format!("{id}\u{1}{title}\u{1}{artist}");
    if art.track != track {
        *art = Art { track, uri: None, tries: 0 };
    }
    if art.uri.is_none() && art.tries < 5 {
        art.tries += 1;
        art.uri = thumbnail(&props);
    }

    Some(NowPlaying {
        available: true,
        spotify: id.to_lowercase().contains("spotify"),
        app: friendly(&id),
        title,
        artist,
        playing: is_playing(&s),
        art: art.uri.clone(),
    })
}

fn thumbnail(props: &Properties) -> Option<String> {
    let stream = props.Thumbnail().ok()?.OpenReadAsync().ok()?.join().ok()?;
    let size = u32::try_from(stream.Size().ok()?).ok().filter(|n| *n > 0 && *n < 2_000_000)?;
    let mime = stream
        .ContentType()
        .map(|c| c.to_string())
        .ok()
        .filter(|c| c.starts_with("image/"))
        .unwrap_or_else(|| "image/png".into());
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).ok()?).ok()?;
    reader.LoadAsync(size).ok()?.join().ok()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes).ok()?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Some(format!("data:{mime};base64,{b64}"))
}

#[cfg(test)]
mod tests {
    use super::friendly;

    #[test]
    fn friendly_app_names() {
        assert_eq!(friendly("Spotify.exe"), "Spotify");
        assert_eq!(friendly("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(friendly("Chrome"), "Chrome");
        assert_eq!(friendly("C:\\Program Files\\VideoLAN\\VLC\\vlc.exe"), "vlc");
        assert_eq!(friendly("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"), "ZuneMusic");
        assert_eq!(friendly("5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App"), "WhatsAppDesktop");
    }
}

#[cfg(test)]
mod live {
    /// Run by hand to see what Windows reports: `cargo test --lib live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn list_media_sessions() {
        let _ = unsafe { super::RoInitialize(super::RO_INIT_MULTITHREADED) };
        let manager = super::Manager::RequestAsync().unwrap().join().unwrap();
        let mut art = super::Art::default();
        for s in super::sessions(&manager) {
            println!("session: {} (playing: {})", super::friendly(&super::app_id(&s)), super::is_playing(&s));
        }
        let now = super::read(&manager, &mut art);
        println!(
            "card would show: {:?}",
            now.map(|n| (n.app, n.playing, n.art.map(|a| a.len())))
        );
    }
}
