//! Ambience engine. Eight layers (rain, fire, thunder, wind, stream, ocean, crickets and brown
//! noise) are synthesized live, so nothing ships as audio files, and any layer can be swapped for
//! your own recording, streamed from disk. A cpal stream runs one mixer callback; when every layer
//! has faded to silence the stream is paused to cost zero CPU.

use std::f32::consts::{PI, TAU};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

pub const LAYERS: usize = 8;

/// Layer names, which are also the file names for your own recordings (`rain.mp3`, `fire.ogg`, ...).
pub const LAYER_NAMES: [&str; LAYERS] = ["rain", "fire", "thunder", "wind", "stream", "ocean", "crickets", "brown"];

/// Per-layer trim so every slider at 100% sits around -18 dBFS RMS: ambience, not foreground.
/// Measured with the `layer_levels` test.
const TRIM: [f32; LAYERS] = [0.5, 0.5, 0.75, 0.44, 0.33, 0.36, 1.3, 0.6];

/// Recordings are usually mastered loud; this brings them near the synthesized layers.
const FILE_TRIM: f32 = 0.5;

/// One ambience source producing stereo samples.
trait Layer: Send {
    fn next(&mut self) -> (f32, f32);
}

/// A replacement layer for the running mixer: (index, layer, trim).
type Swap = (usize, Box<dyn Layer>, f32);

/// State shared between the app and the audio callback.
struct Shared {
    targets: [AtomicU32; LAYERS], // f32 bits, 0.0..=1.0
    chime: AtomicBool,
    /// Set by the callback once every layer has faded out and the chime has ended.
    idle: AtomicBool,
    /// Set when Windows invalidates the stream, e.g. the default output device changed.
    broken: AtomicBool,
    /// Your recordings by layer; read whenever a stream opens.
    custom: Mutex<[Option<PathBuf>; LAYERS]>,
    /// The running mixer's sample rate and swap inbox, replaced when the stream is rebuilt.
    live: Mutex<Option<(f32, Sender<Swap>)>>,
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
    pub fn start(custom: [Option<PathBuf>; LAYERS]) -> Self {
        let shared = Arc::new(Shared {
            targets: Default::default(),
            chime: AtomicBool::new(false),
            idle: AtomicBool::new(true),
            broken: AtomicBool::new(false),
            custom: Mutex::new(custom),
            live: Mutex::new(None),
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

    /// Swaps layers between synthesized and your recordings, only where something changed.
    pub fn set_custom(&self, custom: [Option<PathBuf>; LAYERS]) {
        let changed: Vec<usize> = {
            let mut current = self.shared.custom.lock().unwrap();
            let changed = (0..LAYERS).filter(|&i| current[i] != custom[i]).collect();
            *current = custom.clone();
            changed
        };
        let live = self.shared.live.lock().unwrap().clone();
        if let Some((sr, inbox)) = live {
            let mut rng = Rng::seeded();
            for i in changed {
                let _ = inbox.send(make_layer(i, sr, &mut rng, custom[i].as_deref()));
            }
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

/// Builds layer `i`: your recording if there is a playable one, otherwise the synthesized sound.
fn make_layer(i: usize, sr: f32, rng: &mut Rng, custom: Option<&Path>) -> Swap {
    if let Some(layer) = custom.and_then(|path| FileLayer::open(path, sr)) {
        return (i, Box::new(layer), FILE_TRIM);
    }
    let layer: Box<dyn Layer> = match i {
        0 => Box::new(Rain::new(sr, rng.fork())),
        1 => Box::new(Fire::new(sr, rng.fork())),
        2 => Box::new(Thunder::new(sr, rng.fork())),
        3 => Box::new(Wind::new(sr, rng.fork())),
        4 => Box::new(Stream::new(sr, rng.fork())),
        5 => Box::new(Ocean::new(sr, rng.fork())),
        6 => Box::new(Crickets::new(sr, rng.fork())),
        _ => Box::new(BrownNoise::new(sr, rng.fork())),
    };
    (i, layer, TRIM[i])
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
    let sr = config.sample_rate as f32;

    let mut rng = Rng::seeded();
    let custom = shared.custom.lock().unwrap().clone();
    let layers = (0..LAYERS).map(|i| make_layer(i, sr, &mut rng, custom[i].as_deref())).collect();
    let (inbox, swaps) = mpsc::channel();
    *shared.live.lock().unwrap() = Some((sr, inbox));
    let mut mixer = Mixer::new(sr, shared.clone(), layers, swaps);

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
    trims: [f32; LAYERS],
    layers: Vec<Box<dyn Layer>>,
    swaps: Receiver<Swap>,
    fade: f32,
    chime: Chime,
}

impl Mixer {
    fn new(sr: f32, shared: Arc<Shared>, initial: Vec<Swap>, swaps: Receiver<Swap>) -> Self {
        let mut trims = [1.0; LAYERS];
        let mut layers = Vec::with_capacity(LAYERS);
        for (i, layer, trim) in initial {
            trims[i] = trim;
            layers.push(layer);
        }
        Self {
            shared,
            gains: [0.0; LAYERS],
            trims,
            layers,
            swaps,
            // ~0.8 s time constant: slow enough to feel like a fade, not a switch.
            fade: 1.0 - (-1.0 / (0.8 * sr)).exp(),
            chime: Chime::new(sr),
        }
    }

    fn fill<T: Copy>(&mut self, out: &mut [T], channels: usize, conv: impl Fn(f32) -> T) {
        while let Ok((i, layer, trim)) = self.swaps.try_recv() {
            self.layers[i] = layer;
            self.trims[i] = trim;
        }
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
                let (a, b) = self.layers[i].next();
                let g = *g * self.trims[i];
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

impl Layer for Rain {
    fn next(&mut self) -> (f32, f32) {
        Rain::next(self)
    }
}

impl Layer for Fire {
    fn next(&mut self) -> (f32, f32) {
        Fire::next(self)
    }
}

impl Layer for Thunder {
    fn next(&mut self) -> (f32, f32) {
        Thunder::next(self)
    }
}

/// tan(x) for the small angles of cutoffs well below Nyquist; within 0.5% up to x = 0.65.
fn tan_small(x: f32) -> f32 {
    x * (1.0 + x * x * (1.0 / 3.0 + x * x * (2.0 / 15.0)))
}

/// Zero-delay-feedback state-variable filter (Andrew Simper's form), cheap enough to sweep its
/// cutoff every sample. `run` returns (low-pass, band-pass).
#[derive(Clone, Copy, Default)]
struct Svf {
    ic1: f32,
    ic2: f32,
}

impl Svf {
    fn run(&mut self, x: f32, cutoff: f32, q: f32, sr: f32) -> (f32, f32) {
        let g = tan_small(PI * cutoff.min(sr * 0.2) / sr);
        let k = 1.0 / q;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        (v2, v1)
    }
}

// ---------------------------------------------------------------- wind

/// Gusts of filtered noise. Stronger gusts whistle higher, and each ear wanders a little apart.
struct Wind {
    sr: f32,
    rng: Rng,
    gust: Drift,
    ch: [WindChannel; 2],
}

struct WindChannel {
    pink: Pink,
    svf: Svf,
    wobble: Drift,
    rumble: OnePole,
}

impl Wind {
    fn new(sr: f32, rng: Rng) -> Self {
        let ch = || WindChannel {
            pink: Pink::default(),
            svf: Svf::default(),
            wobble: Drift::new(0.8, 1.25, 1.2, sr),
            rumble: OnePole::new(120.0, sr),
        };
        Self { sr, rng, gust: Drift::new(0.15, 1.0, 3.0, sr), ch: [ch(), ch()] }
    }
}

impl Layer for Wind {
    fn next(&mut self) -> (f32, f32) {
        let gust = self.gust.next(&mut self.rng);
        let mut out = [0.0; 2];
        for (c, o) in self.ch.iter_mut().zip(out.iter_mut()) {
            let noise = c.pink.next(self.rng.bi());
            let cutoff = (180.0 + 650.0 * gust * gust) * c.wobble.next(&mut self.rng);
            let (_, band) = c.svf.run(noise, cutoff, 1.8, self.sr);
            *o = band * (0.35 + 1.2 * gust) + c.rumble.lp(noise) * 0.4 * gust;
        }
        (out[0], out[1])
    }
}

// ---------------------------------------------------------------- stream

/// A brook: a soft bed of rushing water plus bubbling, where narrow filters hop between random
/// pitches every few milliseconds.
struct Stream {
    sr: f32,
    rng: Rng,
    bed: [(OnePole, OnePole); 2],
    bubbles: [Bubble; 4],
    glide: f32,
    swell: f32,
}

struct Bubble {
    svf: Svf,
    cutoff: f32,
    goal: f32,
    amp: f32,
    amp_goal: f32,
    hold: u32,
    pan: f32,
}

impl Stream {
    fn new(sr: f32, mut rng: Rng) -> Self {
        let bubble = |rng: &mut Rng| Bubble {
            svf: Svf::default(),
            cutoff: 800.0,
            goal: 800.0,
            amp: 0.0,
            amp_goal: 0.0,
            hold: 0,
            pan: rng.bi() * 0.7,
        };
        let bubbles = [bubble(&mut rng), bubble(&mut rng), bubble(&mut rng), bubble(&mut rng)];
        let bed = || (OnePole::new(1400.0, sr), OnePole::new(200.0, sr));
        Self {
            sr,
            rng,
            bed: [bed(), bed()],
            bubbles,
            glide: 1.0 - decay(0.003, sr),
            swell: 1.0 - decay(0.02, sr),
        }
    }
}

impl Layer for Stream {
    fn next(&mut self) -> (f32, f32) {
        let mut out = [0.0; 2];
        for ((lp, hp), o) in self.bed.iter_mut().zip(out.iter_mut()) {
            *o = hp.hp(lp.lp(self.rng.bi())) * 0.5;
        }
        for b in &mut self.bubbles {
            if b.hold == 0 {
                let r = self.rng.f();
                b.goal = 350.0 + 2300.0 * r * r;
                b.amp_goal = if self.rng.f() < 0.25 { 0.0 } else { 0.3 + 0.7 * self.rng.f() };
                b.hold = ((0.006 + 0.03 * self.rng.f()) * self.sr) as u32;
            }
            b.hold -= 1;
            b.cutoff += (b.goal - b.cutoff) * self.glide;
            b.amp += (b.amp_goal - b.amp) * self.swell;
            let (_, band) = b.svf.run(self.rng.bi(), b.cutoff, 6.0, self.sr);
            let s = band * b.amp;
            out[0] += s * (1.0 - b.pan.max(0.0));
            out[1] += s * (1.0 + b.pan.min(0.0));
        }
        (out[0], out[1])
    }
}

// ---------------------------------------------------------------- ocean

/// Waves: every 7–13 s a swell builds, breaks into bright foam, then washes back out, arriving
/// a little from the left or the right.
struct Ocean {
    sr: f32,
    rng: Rng,
    t: u32,
    len: u32,
    peak: f32,
    crest: f32,
    pan: f32,
    ch: [(Pink, Brown, Svf); 2],
}

impl Ocean {
    fn new(sr: f32, rng: Rng) -> Self {
        let mut ocean = Self { sr, rng, t: 0, len: 1, peak: 0.0, crest: 0.5, pan: 0.0, ch: Default::default() };
        ocean.next_wave();
        ocean.t = ocean.len / 3; // start mid-swell, not in silence
        ocean
    }

    fn next_wave(&mut self) {
        self.t = 0;
        self.len = ((7.0 + 6.0 * self.rng.f()) * self.sr) as u32;
        self.peak = 0.6 + 0.4 * self.rng.f();
        self.crest = 0.4 + 0.2 * self.rng.f();
        self.pan = self.rng.bi() * 0.4;
    }

    fn envelope(&self) -> f32 {
        let p = self.t as f32 / self.len as f32;
        let shape = if p < self.crest {
            let x = p / self.crest;
            x * x * (3.0 - 2.0 * x)
        } else {
            (-(p - self.crest) * 7.0).exp()
        };
        0.12 + shape * self.peak
    }
}

impl Layer for Ocean {
    fn next(&mut self) -> (f32, f32) {
        if self.t >= self.len {
            self.next_wave();
        }
        self.t += 1;
        let e = self.envelope();
        let cutoff = 220.0 + 3000.0 * e * e;
        let mut out = [0.0; 2];
        for ((pink, brown, svf), o) in self.ch.iter_mut().zip(out.iter_mut()) {
            let w = self.rng.bi();
            let noise = pink.next(w) * 0.7 + brown.next(w) * 0.5;
            let (low, _) = svf.run(noise, cutoff, 0.8, self.sr);
            *o = low * (0.25 + 1.3 * e);
        }
        (out[0] * (1.0 - self.pan.max(0.0)), out[1] * (1.0 + self.pan.min(0.0)))
    }
}

// ---------------------------------------------------------------- crickets

/// A few crickets, each with its own pitch and rhythm, over a faint night breeze.
struct Crickets {
    sr: f32,
    rng: Rng,
    bugs: [Cricket; 3],
    air: [(Pink, OnePole); 2],
}

struct Cricket {
    freq: f32,
    phase: f32,
    t: u32,
    period: u32,
    pulses: u32,
    pulse_len: u32,
    amp: f32,
    pan: f32,
    resting: u32,
}

impl Crickets {
    fn new(sr: f32, mut rng: Rng) -> Self {
        let bug = |rng: &mut Rng| Cricket {
            freq: 4200.0 + 1200.0 * rng.f(),
            phase: 0.0,
            t: (rng.f() * sr * 0.4) as u32,
            period: ((0.45 + 0.45 * rng.f()) * sr) as u32,
            pulses: 3 + (rng.f() * 3.0) as u32,
            pulse_len: (sr / (25.0 + 15.0 * rng.f())) as u32,
            amp: 0.25 + 0.15 * rng.f(),
            pan: rng.bi() * 0.7,
            resting: 0,
        };
        let bugs = [bug(&mut rng), bug(&mut rng), bug(&mut rng)];
        let air = || (Pink::default(), OnePole::new(400.0, sr));
        Self { sr, rng, bugs, air: [air(), air()] }
    }
}

impl Layer for Crickets {
    fn next(&mut self) -> (f32, f32) {
        let mut out = [0.0; 2];
        for ((pink, lp), o) in self.air.iter_mut().zip(out.iter_mut()) {
            *o = lp.lp(pink.next(self.rng.bi())) * 0.08;
        }
        for c in &mut self.bugs {
            if c.resting > 0 {
                c.resting -= 1; // crickets pause now and then
                continue;
            }
            c.t += 1;
            if c.t >= c.period {
                c.t = 0;
                if self.rng.f() < 0.08 {
                    c.resting = ((2.0 + 4.0 * self.rng.f()) * self.sr) as u32;
                }
            }
            if c.t / c.pulse_len >= c.pulses {
                continue;
            }
            let x = (c.t % c.pulse_len) as f32 / c.pulse_len as f32;
            let env = (PI * x).sin().powi(2);
            c.phase = (c.phase + c.freq / self.sr).fract();
            let s = (TAU * c.phase).sin() * env * c.amp;
            out[0] += s * (1.0 - c.pan.max(0.0));
            out[1] += s * (1.0 + c.pan.min(0.0));
        }
        (out[0], out[1])
    }
}

// ---------------------------------------------------------------- brown noise

/// Deep, even noise that masks voices and traffic. Independent per ear, so it feels wide.
struct BrownNoise {
    rng: Rng,
    ch: [Brown; 2],
}

impl BrownNoise {
    fn new(_sr: f32, rng: Rng) -> Self {
        Self { rng, ch: Default::default() }
    }
}

impl Layer for BrownNoise {
    fn next(&mut self) -> (f32, f32) {
        let l = self.ch[0].next(self.rng.bi());
        let r = self.ch[1].next(self.rng.bi());
        (l, r)
    }
}

// ---------------------------------------------------------------- your own recordings

/// Plays a recording on a loop. A decoder thread streams it through a ring buffer of about half
/// a second, so a long file costs no more memory than a short one.
struct FileLayer {
    ring: rtrb::Consumer<f32>,
    stop: Arc<AtomicBool>,
}

impl FileLayer {
    fn open(path: &Path, sr: f32) -> Option<Self> {
        if let Err(e) = open_decoder(path) {
            eprintln!("can't play {}: {e}", path.display());
            return None; // keep the synthesized layer
        }
        let (producer, ring) = rtrb::RingBuffer::new((sr as usize).max(4096));
        let stop = Arc::new(AtomicBool::new(false));
        let (path, flag) = (path.to_path_buf(), stop.clone());
        std::thread::Builder::new()
            .name("sound-file".into())
            .spawn(move || decode_loop(&path, sr, producer, &flag))
            .ok()?;
        Some(Self { ring, stop })
    }
}

impl Drop for FileLayer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Layer for FileLayer {
    fn next(&mut self) -> (f32, f32) {
        if self.ring.slots() < 2 {
            return (0.0, 0.0); // decoder hasn't caught up yet
        }
        let l = self.ring.pop().unwrap_or(0.0);
        let r = self.ring.pop().unwrap_or(l);
        (l, r)
    }
}

type Decoder = (
    Box<dyn symphonia::core::formats::FormatReader>,
    Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    u32,
);

fn open_decoder(path: &Path) -> Result<Decoder, symphonia::core::errors::Error> {
    use symphonia::core::codecs::audio::AudioDecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::probe::Hint;
    use symphonia::core::formats::{FormatOptions, TrackType};
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;

    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let format =
        symphonia::default::get_probe().probe(&hint, mss, FormatOptions::default(), MetadataOptions::default())?;
    let track = format.default_track(TrackType::Audio).ok_or(Error::Unsupported("no audio track"))?;
    let id = track.id;
    let params = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or(Error::Unsupported("not audio"))?;
    let decoder = symphonia::default::get_codecs().make_audio_decoder(params, &AudioDecoderOptions::default())?;
    Ok((format, decoder, id))
}

/// Decodes the file over and over (reopening it at the end), resampled to the device rate.
fn decode_loop(path: &Path, sr_out: f32, mut out: rtrb::Producer<f32>, stop: &AtomicBool) {
    let mut resampler = Linear::default();
    let mut samples: Vec<f32> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        let Ok((mut format, mut decoder, track)) = open_decoder(path) else { return };
        let mut decoded_any = false;
        while let Ok(Some(packet)) = format.next_packet() {
            if packet.track_id != track {
                continue;
            }
            let Ok(audio) = decoder.decode(&packet) else { continue };
            let channels = audio.spec().channels().count().max(1);
            resampler.ratio(audio.spec().rate() as f64 / sr_out as f64);
            samples.resize(audio.samples_interleaved(), 0.0);
            audio.copy_to_slice_interleaved(&mut samples);
            decoded_any = true;
            for frame in samples.chunks(channels) {
                let (l, r) = (frame[0], *frame.get(1).unwrap_or(&frame[0]));
                let more = resampler.push((l, r), |a, b| {
                    while out.slots() < 2 {
                        if stop.load(Ordering::Relaxed) {
                            return false;
                        }
                        std::thread::sleep(Duration::from_millis(10)); // buffer full: wait
                    }
                    let _ = out.push(a);
                    let _ = out.push(b);
                    true
                });
                if !more {
                    return;
                }
            }
        }
        if !decoded_any {
            return; // nothing playable; don't spin
        }
    }
}

/// Streaming linear-interpolation resampler for stereo frames.
#[derive(Default)]
struct Linear {
    step: f64,
    frac: f64,
    prev: Option<(f32, f32)>,
}

impl Linear {
    fn ratio(&mut self, source_per_output: f64) {
        self.step = if source_per_output > 0.0 { source_per_output } else { 1.0 };
    }

    /// Feeds one input frame and emits every output frame that falls before it. Stops early,
    /// returning false, if `emit` does.
    fn push(&mut self, cur: (f32, f32), mut emit: impl FnMut(f32, f32) -> bool) -> bool {
        let Some(prev) = self.prev.replace(cur) else { return true };
        while self.frac < 1.0 {
            let t = self.frac as f32;
            if !emit(prev.0 + (cur.0 - prev.0) * t, prev.1 + (cur.1 - prev.1) * t) {
                return false;
            }
            self.frac += self.step;
        }
        self.frac -= 1.0;
        true
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

    /// Renders each synthesized layer and prints its loudness, used to set TRIM.
    #[test]
    fn layer_levels() {
        let sr = 48_000.0;
        let mut rng = Rng(12345);
        let n = sr as usize * 20;
        let mut loud = Vec::new();
        for i in 0..LAYERS {
            let (_, mut layer, trim) = make_layer(i, sr, &mut rng, None);
            if i == 2 {
                // thunder waits a few seconds before its first strike; measure from a strike
                let mut t = Thunder::new(sr, Rng(777));
                t.until_next = 1;
                layer = Box::new(t);
            }
            let (mut sum, mut peak) = (0.0f64, 0.0f32);
            for _ in 0..n {
                let (l, r) = layer.next();
                let (l, r) = (l * trim, r * trim);
                sum += (l * l + r * r) as f64 * 0.5;
                peak = peak.max(l.abs()).max(r.abs());
            }
            let rms = (sum / n as f64).sqrt();
            println!("{:9} rms {rms:.3}  peak {peak:.3}", LAYER_NAMES[i]);
            if !(rms > 0.05 && rms < 0.2 && peak < 1.0) {
                loud.push(LAYER_NAMES[i]);
            }
        }
        assert!(loud.is_empty(), "levels out of range: {loud:?}");
    }

    #[test]
    fn resampler_keeps_the_rate() {
        let mut rs = Linear::default();
        rs.ratio(44_100.0 / 48_000.0);
        let mut emitted = 0;
        for i in 0..44_100 {
            rs.push((i as f32, 0.0), |_, _| {
                emitted += 1;
                true
            });
        }
        assert!((emitted as i32 - 48_000).abs() < 3, "{emitted}");
    }

    /// Writes a short WAV, plays it as a layer at a different sample rate, and expects sound.
    #[test]
    fn plays_your_own_recording() {
        let path = std::env::temp_dir().join("pomodoro-test-tone.wav");
        let rate = 44_100u32;
        let samples: Vec<i16> = (0..rate)
            .flat_map(|i| {
                let v = ((i as f32 * 440.0 * TAU / rate as f32).sin() * 12_000.0) as i16;
                [v, v]
            })
            .collect();
        let data_len = (samples.len() * 2) as u32;
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((36 + data_len).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(2u16.to_le_bytes()); // stereo
        wav.extend(rate.to_le_bytes());
        wav.extend((rate * 4).to_le_bytes());
        wav.extend(4u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(data_len.to_le_bytes());
        samples.iter().for_each(|s| wav.extend(s.to_le_bytes()));
        std::fs::write(&path, wav).unwrap();

        let mut layer = FileLayer::open(&path, 48_000.0).expect("WAV opens");
        std::thread::sleep(Duration::from_millis(300)); // let the decoder fill the buffer
        let peak = (0..4800).map(|_| layer.next().0.abs()).fold(0.0f32, f32::max);
        assert!(peak > 0.2, "peak {peak}");
        drop(layer);
        let _ = std::fs::remove_file(&path);

        assert!(FileLayer::open(Path::new("no-such-file.ogg"), 48_000.0).is_none());
    }
}
