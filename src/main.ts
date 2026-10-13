import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

type Mode = "focus" | "short" | "long";

interface Snapshot {
  mode: Mode;
  remaining: number;
  total: number;
  running: boolean;
  completed: number;
  cycles_per_long: number;
}

const LABELS: Record<Mode, string> = { focus: "Focus", short: "Short break", long: "Long break" };
const CIRCUMFERENCE = 2 * Math.PI * 88;

const $ = <T extends Element = HTMLElement>(id: string) => document.getElementById(id) as unknown as T;
const ring = $<SVGCircleElement>("ring");
const timeEl = $("time");
const modeEl = $("mode");
const dotsEl = $("dots");
const pinBtn = $("pin");
const appWindow = getCurrentWindow();

const pad = (n: number) => String(n).padStart(2, "0");

function render(s: Snapshot) {
  document.body.dataset.mode = s.mode;
  document.body.dataset.running = String(s.running);

  const clock = `${pad(Math.floor(s.remaining / 60))}:${pad(s.remaining % 60)}`;
  timeEl.textContent = clock;
  modeEl.textContent = LABELS[s.mode];
  document.title = `${clock} · ${LABELS[s.mode]}`;

  // The ring fills up as the phase runs.
  ring.style.strokeDashoffset = String(CIRCUMFERENCE * (s.remaining / s.total));

  // One dot per focus session in the current set; all lit during the long break.
  const lit = s.mode === "long" ? s.cycles_per_long : s.completed % s.cycles_per_long;
  dotsEl.replaceChildren(
    ...Array.from({ length: s.cycles_per_long }, (_, i) => {
      const dot = document.createElement("i");
      if (i < lit) dot.className = "on";
      return dot;
    }),
  );
}

// Temporary end-of-phase chime. It moves to the Rust audio engine in Phase 2,
// so it still plays when the webview is closed to save memory.
let audio: AudioContext | undefined;
function chime() {
  if (!audio) return;
  const now = audio.currentTime;
  [523.25, 783.99].forEach((freq, i) => {
    const osc = audio!.createOscillator();
    const gain = audio!.createGain();
    osc.type = "sine";
    osc.frequency.value = freq;
    gain.gain.setValueAtTime(0.0001, now + i * 0.18);
    gain.gain.exponentialRampToValueAtTime(0.25, now + i * 0.18 + 0.02);
    gain.gain.exponentialRampToValueAtTime(0.0001, now + i * 0.18 + 1.6);
    osc.connect(gain).connect(audio!.destination);
    osc.start(now + i * 0.18);
    osc.stop(now + i * 0.18 + 1.7);
  });
}

const command = (name: string) => invoke<Snapshot>(name).then(render);
const toggle = () => {
  audio ??= new AudioContext();
  command("timer_toggle");
};

$("toggle").addEventListener("click", toggle);
$("reset").addEventListener("click", () => command("timer_reset"));
$("skip").addEventListener("click", () => command("timer_skip"));
$("min").addEventListener("click", () => appWindow.minimize());
$("close").addEventListener("click", () => appWindow.close());
pinBtn.addEventListener("click", async () => {
  const pinned = pinBtn.getAttribute("aria-pressed") !== "true";
  await appWindow.setAlwaysOnTop(pinned);
  pinBtn.setAttribute("aria-pressed", String(pinned));
});

window.addEventListener("keydown", (e) => {
  if (e.repeat) return;
  if (e.code === "Space") toggle();
  else if (e.key === "r" || e.key === "R") command("timer_reset");
  else if (e.key === "s" || e.key === "S") command("timer_skip");
  else return;
  e.preventDefault();
});

listen<Snapshot>("tick", (e) => render(e.payload));
listen<Mode>("phase-done", chime);
command("timer_state");
