//! Ambience engine: rain, fire and thunder are synthesized live, so there are no
//! audio files to ship or decode. A cpal stream runs one mixer callback; when
//! every layer has faded to silence the stream is paused to cost zero CPU.

use std::f32::consts::TAU;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const LAYERS: usize = 3; // rain, fire, thunder

/// Per-layer trim so every slider at 100% sits around -18 dBFS RMS: ambience, not foreground.
/// Measured with the `layer_levels` test.
const TRIM: [f32; LAYERS] = [0.5, 0.5, 0.75];

/// Lock-free state shared between the app and the audio callback.
struct Shared {
    targets: [AtomicU32; LAYERS], // f32 bits, 0.0..=1.0
    chime: AtomicBool,
    /// Set by the callback once every layer has faded out and the chime has ended.
    idle: AtomicBool,
    /// Set when Windows invalidates the stream, e.g. the default output device changed.
    broken: AtomicBool,
}

impl Shared {
    fn wants_sound(&self) -> bool {
        self.chime.load(Ordering::Relaxed)
            || self.targets.iter().any(|t| f32::from_bits(t.load(Ordering::Relaxed)) > 0.0)
    }
}

pub struct Audio {
    shared: Arc<Shared>,
    wake: Sender<()>,
}

impl Audio {
    /// Starts the audio thread. With no output device the app keeps working silently
    /// and retries every few seconds, so plugging in headphones later still works.
    pub fn start() -> Self {
        let shared = Arc::new(Shared {
            targets: Default::default(),
            chime: AtomicBool::new(false),
            idle: AtomicBool::new(true),
            broken: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel::<()>();
        let (thread_shared, thread_tx) = (shared.clone(), tx.clone());
        std::thread::spawn(move || audio_thread(thread_shared, thread_tx, rx));
        Self { shared, wake: tx }
    }

    pub fn set_targets(&self, levels: [f32; LAYERS]) {
        for (slot, v) in self.shared.targets.iter().zip(levels) {
            slot.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
        if levels.iter().any(|v| *v > 0.0) {
            self.wake();
        }
    }

    pub fn chime(&self) {
        self.shared.chime.store(true, Ordering::Relaxed);
        self.wake();
    }

    fn wake(&self) {
        self.shared.idle.store(false, Ordering::Relaxed);
        let _ = self.wake.send(());
    }
}

/// Owns the cpal stream: plays it on demand, pauses it when silent, rebuilds it when it breaks.
fn audio_thread(shared: Arc<Shared>, tx: Sender<()>, rx: Receiver<()>) {
    loop {
        let Some(stream) = open_stream(shared.clone(), tx.clone()) else {
            std::thread::sleep(Duration::from_secs(3));
            continue;
        };
        let mut playing = false;
        loop {
            if shared.wants_sound() && !playing {
                playing = stream.play().is_ok();
            } else if playing && shared.idle.load(Ordering::Relaxed) && !shared.wants_sound() {
                playing = stream.pause().is_err();
            }
            match rx.recv_timeout(Duration::from_secs(1)) {
                Err(RecvTimeoutError::Disconnected) => return,
                _ if shared.broken.swap(false, Ordering::Relaxed) => break, // rebuild
                _ => {}
            }
        }
    }
}

fn open_stream(shared: Arc<Shared>, tx: Sender<()>) -> Option<cpal::Stream> {
    let device = cpal::default_host().default_output_device()?;
    let supported = device.default_output_config().ok()?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels as usize;
    let mut mixer = Mixer::new(config.sample_rate as f32, shared.clone());
    let err = move |e: cpal::Error| {
        eprintln!("audio stream error: {e}");
        if matches!(e.kind(), cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::DeviceNotAvailable) {
            shared.broken.store(true, Ordering::Relaxed);
            let _ = tx.send(());
        }
    };

    let stream = match format {
        cpal::SampleFormat::F32 => device.build_output_stream(
            config,
            move |out: &mut [f32], _: &cpal::OutputCallbackInfo| mixer.fill(out, channels, |v| v),
            err,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_output_stream(
            config,
            move |out: &mut [i16], _: &cpal::OutputCallbackInfo| {
                mixer.fill(out, channels, |v| (v * i16::MAX as f32) as i16)
            },
            err,
            None,
        ),
        other => {
            eprintln!("unsupported sample format {other}");
            return None;
        }
    }
    .ok()?;
    // Start paused; the audio thread plays it when there is something to hear.
    let _ = stream.pause();
    Some(stream)
}

// ---------------------------------------------------------------- mixer

struct Mixer {
    shared: Arc<Shared>,
    gains: [f32; LAYERS],
    fade: f32,
    rain: Rain,
    fire: Fire,
    thunder: Thunder,
    chime: Chime,
}

impl Mixer {
    fn new(sr: f32, shared: Arc<Shared>) -> Self {
        let mut rng = Rng::seeded();
        Self {
            shared,
            gains: [0.0; LAYERS],
            // ~0.8 s time constant: slow enough to feel like a fade, not a switch.
            fade: 1.0 - (-1.0 / (0.8 * sr)).exp(),
            rain: Rain::new(sr, rng.fork()),
            fire: Fire::new(sr, rng.fork()),
            thunder: Thunder::new(sr, rng.fork()),
            chime: Chime::new(sr),
        }
    }

    fn fill<T: Copy>(&mut self, out: &mut [T], channels: usize, conv: impl Fn(f32) -> T) {
        let targets = self.shared.targets.each_ref().map(|t| f32::from_bits(t.load(Ordering::Relaxed)));
        if self.shared.chime.swap(false, Ordering::Relaxed) {
            self.chime.trigger();
        }

        for frame in out.chunks_mut(channels) {
            let (mut l, mut r) = (0.0, 0.0);
            for i in 0..LAYERS {
                let g = &mut self.gains[i];
                *g += (targets[i] - *g) * self.fade;
                if *g < 1e-4 && targets[i] == 0.0 {
                    *g = 0.0;
                    continue; // silent layers cost nothing
                }
                let (a, b) = match i {
                    0 => self.rain.next(),
                    1 => self.fire.next(),
                    _ => self.thunder.next(),
                };
                let g = *g * TRIM[i];
                l += a * g;
                r += b * g;
            }
            let c = self.chime.next();
            let (l, r) = (soft_clip(l + c), soft_clip(r + c));
            match frame {
                [] => {}
                [m] => *m = conv((l + r) * 0.5),
                [fl, fr, rest @ ..] => {
                    *fl = conv(l);
                    *fr = conv(r);
                    rest.iter_mut().for_each(|s| *s = conv(0.0));
                }
            }
        }

        let silent = self.gains.iter().all(|g| *g == 0.0) && !self.chime.active();
        self.shared.idle.store(silent, Ordering::Relaxed);
    }
}

fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

// ---------------------------------------------------------------- DSP helpers

struct Rng(u32);

impl Rng {
    fn seeded() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(1);
        Self(nanos | 1)
    }
    fn fork(&mut self) -> Self {
        Self(self.u32() | 1)
    }
    fn u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        x
    }
    /// Uniform in [0, 1).
    fn f(&mut self) -> f32 {
        (self.u32() >> 8) as f32 / (1u32 << 24) as f32
    }
    /// Uniform in [-1, 1).
    fn bi(&mut self) -> f32 {
        self.f() * 2.0 - 1.0
    }
    /// True on average `per_sec` times per second.
    fn chance(&mut self, per_sec: f32, sr: f32) -> bool {
        self.f() < per_sec / sr
    }
}

/// One-pole low-pass. `hp()` gives the matching high-pass.
#[derive(Clone, Copy)]
struct OnePole {
    y: f32,
    a: f32,
}

impl OnePole {
    fn new(cutoff: f32, sr: f32) -> Self {
        Self { y: 0.0, a: 1.0 - (-TAU * cutoff / sr).exp() }
    }
    fn lp(&mut self, x: f32) -> f32 {
        self.y += self.a * (x - self.y);
        self.y
    }
    fn hp(&mut self, x: f32) -> f32 {
        x - self.lp(x)
    }
}

/// Paul Kellet's economy pink noise filter.
#[derive(Default)]
struct Pink([f32; 3]);

impl Pink {
    fn next(&mut self, white: f32) -> f32 {
        let b = &mut self.0;
        b[0] = 0.99765 * b[0] + white * 0.099_046;
        b[1] = 0.963 * b[1] + white * 0.296_516_4;
        b[2] = 0.57 * b[2] + white * 1.052_691_3;
        (b[0] + b[1] + b[2] + white * 0.1848) * 0.25
    }
}

#[derive(Default)]
struct Brown(f32);

impl Brown {
    fn next(&mut self, white: f32) -> f32 {
        self.0 = (self.0 + 0.02 * white) / 1.02;
        self.0 * 3.5
    }
}

/// A slowly wandering value in [lo, hi]: picks a new goal every `period` seconds and glides to it.
struct Drift {
    v: f32,
    goal: f32,
    lo: f32,
    hi: f32,
    left: u32,
    period: u32,
    glide: f32,
}

impl Drift {
    fn new(lo: f32, hi: f32, period_s: f32, sr: f32) -> Self {
        let mid = (lo + hi) * 0.5;
        Self {
            v: mid,
            goal: mid,
            lo,
            hi,
            left: 0,
            period: (period_s * sr) as u32,
            glide: 1.0 - (-1.0 / (period_s * 0.5 * sr)).exp(),
        }
    }
    fn next(&mut self, rng: &mut Rng) -> f32 {
        if self.left == 0 {
            self.goal = self.lo + (self.hi - self.lo) * rng.f();
            self.left = self.period / 2 + (rng.f() * self.period as f32) as u32;
        }
        self.left -= 1;
        self.v += (self.goal - self.v) * self.glide;
        self.v
    }
}

/// Per-sample decay multiplier for a time constant in seconds.
fn decay(tau_s: f32, sr: f32) -> f32 {
    (-1.0 / (tau_s * sr)).exp()
}

// ---------------------------------------------------------------- rain

/// Two independent channels of filtered pink noise for width, plus droplet ticks and slow gusts.
struct Rain {
    sr: f32,
    rng: Rng,
    ch: [RainChannel; 2],
    gust: Drift,
}

struct RainChannel {
    pink: Pink,
    lp: OnePole,
    hp: OnePole,
    drop_hp: OnePole,
    drop_env: f32,
    drop_decay: f32,
}

impl Rain {
    fn new(sr: f32, rng: Rng) -> Self {
        let ch = || RainChannel {
            pink: Pink::default(),
            lp: OnePole::new(6500.0, sr),
            hp: OnePole::new(350.0, sr),
            drop_hp: OnePole::new(2500.0, sr),
            drop_env: 0.0,
            drop_decay: decay(0.004, sr),
        };
        Self { sr, rng, ch: [ch(), ch()], gust: Drift::new(0.75, 1.15, 4.0, sr) }
    }

    fn next(&mut self) -> (f32, f32) {
        let gust = self.gust.next(&mut self.rng);
        let mut out = [0.0; 2];
        for (c, o) in self.ch.iter_mut().zip(out.iter_mut()) {
            let w = self.rng.bi();
            let body = c.hp.hp(c.lp.lp(c.pink.next(w)));
            if self.rng.chance(35.0, self.sr) {
                let a = self.rng.f();
                c.drop_env = c.drop_env.max(0.1 + 0.5 * a * a * a);
                c.drop_decay = decay(0.002 + 0.006 * self.rng.f(), self.sr);
            }
            c.drop_env *= c.drop_decay;
            let tick = c.drop_hp.hp(self.rng.bi()) * c.drop_env;
            *o = (body * 1.1 + tick * 0.5) * gust;
        }
        (out[0], out[1])
    }
}

// ---------------------------------------------------------------- fire

/// A low brown-noise roar that flickers, a faint hiss, and clustered crackles and pops.
struct Fire {
    sr: f32,
    rng: Rng,
    brown: Brown,
    roar_lp: OnePole,
    hiss_hp: OnePole,
    flicker: Drift,
    crack_hp: OnePole,
    crack_env: f32,
    crack_decay: f32,
    crack_pan: f32,
    cluster: f32,
    pop_lp: OnePole,
    pop_env: f32,
    pop_decay: f32,
    pop_pan: f32,
}

impl Fire {
    fn new(sr: f32, rng: Rng) -> Self {
        Self {
            sr,
            rng,
            brown: Brown::default(),
            roar_lp: OnePole::new(500.0, sr),
            hiss_hp: OnePole::new(3000.0, sr),
            flicker: Drift::new(0.6, 1.2, 0.35, sr),
            crack_hp: OnePole::new(1800.0, sr),
            crack_env: 0.0,
            crack_decay: decay(0.002, sr),
            crack_pan: 0.0,
            cluster: 0.0,
            pop_lp: OnePole::new(900.0, sr),
            pop_env: 0.0,
            pop_decay: decay(0.015, sr),
            pop_pan: 0.0,
        }
    }

    fn next(&mut self) -> (f32, f32) {
        let sr = self.sr;
        let flicker = self.flicker.next(&mut self.rng);
        let roar = self.roar_lp.lp(self.brown.next(self.rng.bi())) * 1.6 * flicker;
        let hiss = self.hiss_hp.hp(self.rng.bi()) * 0.015 * flicker;

        // Crackles come in bursts: each one briefly raises the odds of another.
        if self.rng.chance(5.0 + self.cluster, sr) {
            let a = self.rng.f();
            self.crack_env = 0.15 + 0.85 * a * a * a * a;
            self.crack_decay = decay(0.0008 + 0.003 * self.rng.f(), sr);
            self.crack_pan = self.rng.bi() * 0.6;
            self.cluster = 60.0 * self.rng.f();
        }
        self.cluster *= 0.9995;
        self.crack_env *= self.crack_decay;
        let crack = self.crack_hp.hp(self.rng.bi()) * self.crack_env * 0.7;

        if self.rng.chance(0.6, sr) {
            self.pop_env = 0.3 + 0.4 * self.rng.f();
            self.pop_pan = self.rng.bi() * 0.5;
        }
        self.pop_env *= self.pop_decay;
        let pop = self.pop_lp.lp(self.rng.bi()) * self.pop_env * 2.5;

        let base = roar + hiss;
        let l = base + crack * (1.0 - self.crack_pan.max(0.0)) + pop * (1.0 - self.pop_pan.max(0.0));
        let r = base + crack * (1.0 + self.crack_pan.min(0.0)) + pop * (1.0 + self.pop_pan.min(0.0));
        (l, r)
    }
}

// ---------------------------------------------------------------- thunder

/// Distant storm: every 15–45 s a strike rolls in (sometimes with a near crack), then rumbles out.
struct Thunder {
    sr: f32,
    rng: Rng,
    until_next: u32,
    brown: [Brown; 2],
    rumble_lp: [OnePole; 2],
    crack_lp: OnePole,
    env: f32,
    peak: f32,
    attacking: bool,
    attack: f32,
    release: f32,
    crack_env: f32,
    crack_decay: f32,
    roll: Drift,
}

impl Thunder {
    fn new(sr: f32, mut rng: Rng) -> Self {
        // The first strike comes early so you can hear what the layer sounds like.
        let until_next = ((4.0 + 6.0 * rng.f()) * sr) as u32;
        Self {
            sr,
            rng,
            until_next,
            brown: Default::default(),
            rumble_lp: [OnePole::new(140.0, sr); 2],
            crack_lp: OnePole::new(2500.0, sr),
            env: 0.0,
            peak: 0.0,
            attacking: false,
            attack: 0.0,
            release: 0.0,
            crack_env: 0.0,
            crack_decay: decay(0.12, sr),
            roll: Drift::new(0.35, 1.0, 0.12, sr),
        }
    }

    fn strike(&mut self) {
        let sr = self.sr;
        self.peak = 0.5 + 0.5 * self.rng.f();
        self.attacking = true;
        self.attack = 1.0 - decay(0.15 + 0.5 * self.rng.f(), sr);
        self.release = decay(1.8 + 2.5 * self.rng.f(), sr);
        if self.rng.f() < 0.35 {
            self.crack_env = 0.35 * self.peak;
        }
        self.until_next = ((15.0 + 30.0 * self.rng.f()) * sr) as u32;
    }

    fn next(&mut self) -> (f32, f32) {
        if self.until_next == 0 {
            self.strike();
        }
        self.until_next -= 1;

        if self.attacking {
            self.env += (self.peak - self.env) * self.attack;
            if self.env > self.peak * 0.95 {
                self.attacking = false;
            }
        } else {
            self.env *= self.release;
        }
        if self.env < 1e-4 && self.crack_env < 1e-4 {
            return (0.0, 0.0);
        }

        let roll = self.roll.next(&mut self.rng);
        let crack = self.crack_lp.lp(self.rng.bi()) * self.crack_env;
        self.crack_env *= self.crack_decay;
        let mut out = [0.0; 2];
        for i in 0..2 {
            let rumble = self.rumble_lp[i].lp(self.brown[i].next(self.rng.bi()));
            out[i] = rumble * 5.0 * self.env * roll + crack;
        }
        (out[0], out[1])
    }
}

// ---------------------------------------------------------------- chime

/// Two soft bell notes (C5 then G5) marking the end of a phase.
struct Chime {
    sr: f32,
    t: u32,
    len: u32,
}

impl Chime {
    const NOTES: [(f32, f32); 2] = [(523.25, 0.0), (783.99, 0.18)];

    fn new(sr: f32) -> Self {
        let len = (2.6 * sr) as u32;
        Self { sr, t: len, len }
    }
    fn trigger(&mut self) {
        self.t = 0;
    }
    fn active(&self) -> bool {
        self.t < self.len
    }
    fn next(&mut self) -> f32 {
        if !self.active() {
            return 0.0;
        }
        let now = self.t as f32 / self.sr;
        self.t += 1;
        Self::NOTES
            .iter()
            .filter(|(_, start)| now >= *start)
            .map(|(f, start)| {
                let t = now - start;
                let attack = (t / 0.005).min(1.0);
                let body = (TAU * f * t).sin() * (-t * 2.2).exp();
                let shimmer = (TAU * f * 2.76 * t).sin() * (-t * 5.0).exp() * 0.3;
                (body + shimmer) * attack * 0.2
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Renders each layer and prints its loudness, used to balance the layers against each other.
    #[test]
    fn layer_levels() {
        let sr = 48_000.0;
        let mut rng = Rng(12345);
        let mut rain = Rain::new(sr, rng.fork());
        let mut fire = Fire::new(sr, rng.fork());
        let mut thunder = Thunder::new(sr, rng.fork());
        thunder.until_next = 1;
        let seconds = 10;
        let n = sr as usize * seconds;
        let stats = |name: &str, trim: f32, f: &mut dyn FnMut() -> (f32, f32)| {
            let (mut sum, mut peak) = (0.0f64, 0.0f32);
            for _ in 0..n {
                let (l, r) = f();
                let (l, r) = (l * trim, r * trim);
                sum += (l * l + r * r) as f64 * 0.5;
                peak = peak.max(l.abs()).max(r.abs());
            }
            let rms = (sum / n as f64).sqrt();
            println!("{name:8} rms {rms:.3}  peak {peak:.3}");
            assert!(rms > 0.01 && peak < 3.0, "{name} level out of range");
        };
        stats("rain", TRIM[0], &mut || rain.next());
        stats("fire", TRIM[1], &mut || fire.next());
        stats("thunder", TRIM[2], &mut || thunder.next());
    }
}
