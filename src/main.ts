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
  followMode(s.mode);

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

// --- ambience mixer ---

type Scope = "focus" | "break";

interface Sound {
  focus_mix: number[];
  break_mix: number[];
  muted: boolean;
}

let sound: Sound = { focus_mix: [0, 0, 0], break_mix: [0, 0, 0], muted: false };
let scope: Scope = "focus";
let lastMode: Mode | undefined;
const sliders = Array.from(document.querySelectorAll<HTMLInputElement>(".layer input"));
const tabs = Array.from(document.querySelectorAll<HTMLButtonElement>(".tabs button"));
const muteBtn = $("mute");

const paintSlider = (el: HTMLInputElement) => el.style.setProperty("--fill", `${el.value}%`);

function renderMixer() {
  const mix = scope === "focus" ? sound.focus_mix : sound.break_mix;
  sliders.forEach((el, i) => {
    el.value = String(Math.round(mix[i] * 100));
    paintSlider(el);
  });
  tabs.forEach((t) => t.setAttribute("aria-selected", String(t.dataset.scope === scope)));
  muteBtn.setAttribute("aria-pressed", String(sound.muted));
  document.body.dataset.muted = String(sound.muted);
}

// Follow the timer: show the mix that is about to play whenever the phase changes.
function followMode(mode: Mode) {
  if (mode === lastMode) return;
  lastMode = mode;
  scope = mode === "focus" ? "focus" : "break";
  renderMixer();
}

function setLevel(el: HTMLInputElement, save: boolean) {
  const layer = Number(el.dataset.layer);
  const value = Number(el.value) / 100;
  (scope === "focus" ? sound.focus_mix : sound.break_mix)[layer] = value;
  paintSlider(el);
  invoke("sound_set_level", { scope, layer, value, save });
}

function setMuted(muted: boolean) {
  sound.muted = muted;
  renderMixer();
  invoke("sound_set_muted", { muted });
}

sliders.forEach((el) => {
  el.addEventListener("input", () => setLevel(el, false));
  el.addEventListener("change", () => setLevel(el, true));
});
tabs.forEach((t) =>
  t.addEventListener("click", () => {
    scope = t.dataset.scope as Scope;
    renderMixer();
  }),
);
muteBtn.addEventListener("click", () => setMuted(!sound.muted));

const command = (name: string) => invoke<Snapshot>(name).then(render);
const toggle = () => command("timer_toggle");

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
  else if (e.key === "m" || e.key === "M") setMuted(!sound.muted);
  else return;
  e.preventDefault();
});

listen<Snapshot>("tick", (e) => render(e.payload));
command("timer_state");
invoke<Sound>("sound_state").then((s) => {
  sound = s;
  renderMixer();
});
