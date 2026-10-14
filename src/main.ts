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

interface Timeline {
  focus: number;
  short: number;
  long: number;
  cycles: number;
  auto_breaks: boolean;
  auto_focus: boolean;
}

interface Sound {
  focus_mix: number[];
  break_mix: number[];
  muted: boolean;
  timeline: Timeline;
}

let sound: Sound = {
  focus_mix: [0, 0, 0],
  break_mix: [0, 0, 0],
  muted: false,
  timeline: { focus: 25, short: 5, long: 15, cycles: 4, auto_breaks: true, auto_focus: false },
};
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

// --- your timeline ---

type NumberKey = "focus" | "short" | "long" | "cycles";
type FlagKey = "auto_breaks" | "auto_focus";

const panel = $("settings");
const gearBtn = $("gear");
const numberInputs = Array.from(panel.querySelectorAll<HTMLInputElement>('input[type="number"]'));
const flagInputs = Array.from(panel.querySelectorAll<HTMLInputElement>('input[type="checkbox"]'));

function renderTimeline() {
  for (const el of numberInputs) el.value = String(sound.timeline[el.dataset.key as NumberKey]);
  for (const el of flagInputs) el.checked = sound.timeline[el.dataset.key as FlagKey];
}

function setNumber(key: NumberKey, raw: number) {
  const el = numberInputs.find((i) => i.dataset.key === key)!;
  const value = Math.min(Number(el.max), Math.max(Number(el.min), Math.round(raw) || Number(el.min)));
  sound.timeline[key] = value;
  saveTimeline();
}

function saveTimeline() {
  renderTimeline();
  invoke<Snapshot>("timeline_set", { timeline: sound.timeline }).then(render);
}

function showSettings(open: boolean) {
  panel.hidden = !open;
  gearBtn.setAttribute("aria-expanded", String(open));
  if (open) renderTimeline();
}

panel.querySelectorAll<HTMLButtonElement>(".step button").forEach((b) =>
  b.addEventListener("click", () => {
    const key = b.dataset.key as NumberKey;
    setNumber(key, sound.timeline[key] + Number(b.dataset.delta));
  }),
);
numberInputs.forEach((el) => el.addEventListener("change", () => setNumber(el.dataset.key as NumberKey, Number(el.value))));
flagInputs.forEach((el) =>
  el.addEventListener("change", () => {
    sound.timeline[el.dataset.key as FlagKey] = el.checked;
    saveTimeline();
  }),
);
gearBtn.addEventListener("click", () => showSettings(gearBtn.getAttribute("aria-expanded") !== "true"));
$("settings-done").addEventListener("click", () => showSettings(false));

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
  if (!panel.hidden) {
    if (e.key === "Escape") showSettings(false);
    return; // typing in the settings shouldn't drive the timer
  }
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
