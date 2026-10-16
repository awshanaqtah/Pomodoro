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
  goal: number;
}

interface Scene {
  name: string;
  levels: number[];
}

interface Sound {
  focus_mix: number[];
  break_mix: number[];
  muted: boolean;
  timeline: Timeline;
  music: { follow: boolean; on_breaks: boolean };
  pin: "normal" | "top" | "desktop";
  scenes: Scene[];
  fade_out: boolean;
}

// Same order as the Rust engine, whose layer names are also the file names for your recordings.
const LAYERS = [
  {
    name: "Rain",
    icon: '<path d="M7 4.5 5.5 8M13 4.5 11.5 8M19 4.5 17.5 8M10 11l-1.5 3.5M16 11l-1.5 3.5M7 17.5 5.5 21M13 17.5 11.5 21" />',
  },
  {
    name: "Fire",
    icon: '<path d="M12 21c-3.9 0-6.5-2.6-6.5-6.2 0-3.4 2.6-5.3 3.6-8.8 2.2 1.5 3 3.4 3 5.2 1-.6 1.8-1.7 2-3 2 1.6 4.4 4 4.4 6.8 0 3.5-2.6 6-6.5 6Z" />',
  },
  { name: "Thunder", icon: '<path d="M13.5 3 6 13.5h5.5L10 21l8-11h-5.5l1-7Z" />' },
  { name: "Wind", icon: '<path d="M3 8h11a3 3 0 1 0-3-3M3 12h15a3 3 0 1 1-3 3M3 16h7" />' },
  {
    name: "Stream",
    icon: '<path d="M3 8c2 0 2-1.5 4.5-1.5S10 8 12 8s2-1.5 4.5-1.5S19 8 21 8M3 13c2 0 2-1.5 4.5-1.5S10 13 12 13s2-1.5 4.5-1.5S19 13 21 13M3 18c2 0 2-1.5 4.5-1.5S10 18 12 18s2-1.5 4.5-1.5S19 18 21 18" />',
  },
  {
    name: "Ocean",
    icon: '<path d="M2 16c2.5 0 3-2 5-2s2.5 2 5 2 3-2 5-2 2.5 2 5 2M2 20c2.5 0 3-2 5-2s2.5 2 5 2 3-2 5-2 2.5 2 5 2M5 12c.5-4 3.5-7 7.5-7 2 0 3.6.8 4.5 2-2.3-.4-4.3.8-4.8 3" />',
  },
  { name: "Crickets", icon: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5Z" />' },
  { name: "Noise", title: "Brown noise: deep, even noise that covers voices and traffic", icon: '<path d="M3 12h2l2-5 3 10 3-12 3 14 2-7h3" />' },
];

const BUILT_IN_SCENES: Scene[] = [
  { name: "Rainy cabin", levels: [0.6, 0.5, 0, 0, 0, 0, 0, 0] },
  { name: "Storm night", levels: [0.75, 0, 0.55, 0.35, 0, 0, 0, 0] },
  { name: "Forest stream", levels: [0, 0, 0, 0.2, 0.6, 0, 0.15, 0] },
  { name: "Ocean", levels: [0, 0, 0, 0.25, 0, 0.7, 0, 0] },
  { name: "Summer night", levels: [0, 0.3, 0, 0.15, 0, 0, 0.5, 0] },
  { name: "Deep focus", levels: [0.2, 0, 0, 0, 0, 0, 0, 0.5] },
  { name: "Silence", levels: [0, 0, 0, 0, 0, 0, 0, 0] },
];

const silentMix = () => LAYERS.map(() => 0);
let sound: Sound = {
  focus_mix: silentMix(),
  break_mix: silentMix(),
  muted: false,
  timeline: { focus: 25, short: 5, long: 15, cycles: 4, auto_breaks: true, auto_focus: false, goal: 8 },
  music: { follow: true, on_breaks: false },
  pin: "normal",
  scenes: [],
  fade_out: true,
};
let scope: Scope = "focus"; // the mix shown on the Sounds page
let lastMode: Mode | undefined;
let custom: boolean[] = []; // layers playing your own recording
const layersEl = $("layers");
const tabs = Array.from(document.querySelectorAll<HTMLButtonElement>(".sounds .tabs button"));
const muteBtn = $("mute");

layersEl.replaceChildren(
  ...LAYERS.map((layer, i) => {
    const row = document.createElement("label");
    row.className = "layer";
    if (layer.title) row.title = layer.title;
    row.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${layer.icon}</svg><span>${layer.name}</span>
      <input type="range" min="0" max="100" data-layer="${i}" aria-label="${layer.name}" />
      <i class="mine" title="Playing your own recording"></i>`;
    return row;
  }),
);
const sliders = Array.from(layersEl.querySelectorAll<HTMLInputElement>("input"));
const rows = Array.from(layersEl.querySelectorAll<HTMLElement>(".layer"));

const paintSlider = (el: HTMLInputElement) => el.style.setProperty("--fill", `${el.value}%`);
const mixFor = (s: Scope) => (s === "focus" ? sound.focus_mix : sound.break_mix);
/** Scene buttons on the Timer page change whatever is playing now. */
const liveScope = (): Scope => (lastMode === undefined || lastMode === "focus" ? "focus" : "break");
const sameMix = (a: number[], b: number[]) => b.every((v, i) => Math.abs(v - (a[i] ?? 0)) < 0.005);

function renderMixer() {
  const mix = mixFor(scope);
  sliders.forEach((el, i) => {
    el.value = String(Math.round((mix[i] ?? 0) * 100));
    paintSlider(el);
  });
  rows.forEach((row, i) => row.classList.toggle("custom", !!custom[i]));
  tabs.forEach((t) => t.setAttribute("aria-selected", String(t.dataset.scope === scope)));
  muteBtn.setAttribute("aria-pressed", String(sound.muted));
  document.body.dataset.muted = String(sound.muted);
  renderScenes();
}

function sceneChip(scene: Scene, target: Scope, removable: boolean): HTMLButtonElement {
  const chip = document.createElement("button");
  chip.className = `scene${sameMix(mixFor(target), scene.levels) ? " on" : ""}`;
  chip.textContent = scene.name;
  chip.addEventListener("click", () => applyScene(scene, target));
  if (removable) {
    const x = document.createElement("span");
    x.className = "x";
    x.textContent = "×";
    x.title = `Delete "${scene.name}"`;
    x.addEventListener("click", (e) => {
      e.stopPropagation();
      invoke<Scene[]>("scene_delete", { name: scene.name }).then((list) => {
        sound.scenes = list;
        renderScenes();
      });
    });
    chip.append(x);
  }
  return chip;
}

function renderScenes() {
  const quick = liveScope();
  $("scenes-quick").replaceChildren(
    ...[...BUILT_IN_SCENES, ...sound.scenes].map((sc) => sceneChip(sc, quick, false)),
  );
  const save = document.createElement("button");
  save.className = "scene add-scene";
  save.textContent = "+ Save this mix";
  save.addEventListener("click", () => nameNewScene(save));
  $("scenes-full").replaceChildren(
    ...BUILT_IN_SCENES.map((sc) => sceneChip(sc, scope, false)),
    ...sound.scenes.map((sc) => sceneChip(sc, scope, true)),
    save,
  );
}

function applyScene(scene: Scene, target: Scope) {
  const levels = LAYERS.map((_, i) => scene.levels[i] ?? 0);
  if (target === "focus") sound.focus_mix = levels;
  else sound.break_mix = levels;
  renderMixer();
  invoke("sound_set_mix", { scope: target, levels });
}

function nameNewScene(button: HTMLButtonElement) {
  const input = document.createElement("input");
  input.className = "scene-name";
  input.placeholder = "Name this mix";
  input.maxLength = 24;
  button.replaceWith(input);
  input.focus();
  let finished = false;
  const finish = (save: boolean) => {
    if (finished) return;
    finished = true;
    const name = input.value.trim();
    if (!save || !name) return renderScenes();
    invoke<Scene[]>("scene_save", { name, levels: mixFor(scope) }).then((list) => {
      sound.scenes = list;
      renderScenes();
    });
  };
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") finish(true);
    else if (e.key === "Escape") finish(false);
  });
  input.addEventListener("blur", () => finish(true));
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
  mixFor(scope)[layer] = value;
  paintSlider(el);
  renderScenes();
  invoke("sound_set_level", { scope, layer, value, save });
}

function setMuted(muted: boolean) {
  sound.muted = muted;
  renderMixer();
  invoke("sound_set_muted", { muted });
}

/** Rescans the sounds folder; layers with a recording get a dot and play it. */
const refreshCustom = () =>
  invoke<boolean[]>("sounds_custom").then((flags) => {
    custom = flags;
    renderMixer();
  });

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
$("sounds-folder").addEventListener("click", () => invoke("sounds_open_folder"));
$("sounds-reload").addEventListener("click", refreshCustom);
// The quick scene row scrolls sideways with the mouse wheel.
$("scenes-quick").addEventListener(
  "wheel",
  (e) => {
    e.preventDefault();
    $("scenes-quick").scrollLeft += e.deltaY + e.deltaX;
  },
  { passive: false },
);

// --- your timeline ---

type NumberKey = "focus" | "short" | "long" | "cycles" | "goal";
type FlagKey = "auto_breaks" | "auto_focus";

const panel = $("settings");
const gearBtn = $("gear");
const numberInputs = Array.from(panel.querySelectorAll<HTMLInputElement>('input[type="number"]'));
const flagInputs = Array.from(panel.querySelectorAll<HTMLInputElement>('input[type="checkbox"][data-key]'));

function renderTimeline() {
  for (const el of numberInputs) el.value = String(sound.timeline[el.dataset.key as NumberKey]);
  for (const el of flagInputs) el.checked = sound.timeline[el.dataset.key as FlagKey];
  fadeBox.checked = sound.fade_out;
  followBox.checked = sound.music.follow;
  breaksBox.checked = sound.music.on_breaks;
  breaksBox.disabled = !sound.music.follow;
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
  loadProgress();
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

// --- pages ---

const pageTabs = Array.from(document.querySelectorAll<HTMLButtonElement>(".pages button"));
const pages = Array.from(document.querySelectorAll<HTMLElement>(".page"));

function showPage(name: string) {
  if (name === "sounds") refreshCustom();
  pageTabs.forEach((t) => t.setAttribute("aria-selected", String(t.dataset.page === name)));
  pages.forEach((p) => (p.hidden = p.dataset.page !== name));
}
pageTabs.forEach((t) => t.addEventListener("click", () => showPage(t.dataset.page!)));

// --- tasks ---

type Kind = "daily" | "once" | "calendar";

interface Task {
  id: string;
  title: string;
  kind: Kind;
  estimate: number;
  sessions: number;
  done: boolean;
  time: string | null;
  start: number | null;
}

interface Tasks {
  day: string;
  active: string | null;
  list: Task[];
}

const ICON = {
  check: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m5 12.5 4.5 4.5L19 7.5" /></svg>',
  remove: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m7 7 10 10M17 7 7 17" /></svg>',
  daily:
    '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M17 3l3 3-3 3M20 6H8a4 4 0 0 0-4 4v1M7 21l-3-3 3-3M4 18h12a4 4 0 0 0 4-4v-1" /></svg>',
  calendar:
    '<svg viewBox="0 0 24 24" aria-hidden="true"><rect x="4" y="5.5" width="16" height="14.5" rx="2" /><path d="M4 10h16M8.5 3.5v4M15.5 3.5v4" /></svg>',
};

let tasks: Tasks = { day: "", active: null, list: [] };
const listEl = $("task-list");
const emptyEl = $("task-empty");
const countEl = $("task-count");
const activeEl = $("active-task");
const kindBtn = $("task-kind");
const titleInput = $<HTMLInputElement>("task-title");

// Open tasks first; calendar events in time order; everything else as added.
const ordered = (list: Task[]) =>
  [...list].sort((a, b) => Number(a.done) - Number(b.done) || (a.start ?? 1e4) - (b.start ?? 1e4));

function renderTasks(t: Tasks) {
  tasks = t;
  const open = t.list.filter((x) => !x.done).length;
  countEl.textContent = open ? String(open) : "";
  emptyEl.hidden = t.list.length > 0;
  const active = t.list.find((x) => x.id === t.active);
  activeEl.textContent = active ? active.title : "";
  listEl.replaceChildren(...ordered(t.list).map(taskRow));
}

const taskCall = (name: string, args: Record<string, unknown>) => invoke<Tasks>(name, args).then(renderTasks);

function taskRow(task: Task): HTMLLIElement {
  const li = document.createElement("li");
  li.className = `task${task.done ? " done" : ""}${task.id === tasks.active ? " active" : ""}`;
  li.title = task.id === tasks.active ? "Working on this. Tap to stop." : "Tap to work on this";
  li.innerHTML = `<button class="check" aria-label="Mark done">${ICON.check}</button>
    <div class="body"><span class="title"></span><span class="meta"></span></div>`;
  li.querySelector(".title")!.textContent = task.title;

  const meta = li.querySelector(".meta")!;
  if (task.kind === "daily") meta.insertAdjacentHTML("beforeend", `${ICON.daily}<span>daily</span>`);
  if (task.kind === "calendar") {
    meta.insertAdjacentHTML("beforeend", ICON.calendar);
    const when = document.createElement("span");
    when.textContent = task.time ?? "all day";
    meta.append(when);
  }
  const pips = document.createElement("span");
  pips.className = "pips";
  pips.title = `${task.sessions} of ${task.estimate} focus sessions. Click to change the plan.`;
  for (let i = 0; i < Math.max(task.estimate, task.sessions); i++) {
    const pip = document.createElement("i");
    if (i < task.sessions) pip.className = "on";
    pips.append(pip);
  }
  meta.append(pips);

  li.querySelector(".check")!.addEventListener("click", (e) => {
    e.stopPropagation();
    taskCall("task_update", { id: task.id, done: !task.done });
  });
  pips.addEventListener("click", (e) => {
    e.stopPropagation();
    taskCall("task_update", { id: task.id, estimate: (task.estimate % 6) + 1 });
  });
  // Calendar events come back on the next sync, so they're ticked off rather than removed.
  if (task.kind !== "calendar") {
    const remove = document.createElement("button");
    remove.className = "remove";
    remove.setAttribute("aria-label", "Remove task");
    remove.innerHTML = ICON.remove;
    remove.addEventListener("click", (e) => {
      e.stopPropagation();
      taskCall("task_remove", { id: task.id });
    });
    li.append(remove);
  }
  li.addEventListener("click", () =>
    taskCall("task_set_active", { id: task.id === tasks.active ? null : task.id }),
  );
  return li;
}

kindBtn.addEventListener("click", () => {
  const daily = kindBtn.getAttribute("aria-pressed") !== "true";
  kindBtn.setAttribute("aria-pressed", String(daily));
  kindBtn.title = daily ? "Daily: comes back every morning" : "One-off: gone the day after you finish it";
});
$("add-task").addEventListener("submit", (e) => {
  e.preventDefault();
  const title = titleInput.value.trim();
  if (!title) return;
  const kind = kindBtn.getAttribute("aria-pressed") === "true" ? "daily" : "once";
  taskCall("task_add", { title, kind, estimate: 1 });
  titleInput.value = "";
});

// --- calendar ---

interface CalendarStatus {
  connected: boolean;
  syncing: boolean;
  last_sync: string | null;
  error: string | null;
  events: number;
}

const calEl = $("cal");
const calText = $("cal-text");
const calForm = $<HTMLFormElement>("cal-form");
const calUrl = $<HTMLInputElement>("cal-url");
const calEdit = $("cal-edit");
const calRefresh = $("cal-refresh");

function renderCalendar(c: CalendarStatus) {
  calEl.classList.toggle("syncing", c.syncing);
  calEl.classList.toggle("error", !!c.error && !c.syncing);
  calRefresh.hidden = !c.connected;
  $("cal-remove").hidden = !c.connected;
  calUrl.placeholder = c.connected ? "Paste a new link to replace the saved one" : "Paste the secret iCal address";
  if (!c.connected) calText.textContent = "Import today's events from Google Calendar";
  else if (c.syncing) calText.textContent = "Syncing your calendar…";
  else if (c.error) calText.textContent = c.error;
  else {
    const n = c.events === 1 ? "1 event" : `${c.events} events`;
    calText.textContent = `${n} today · synced ${c.last_sync ?? ""}`;
  }
}

function showCalForm(open: boolean) {
  calForm.hidden = !open;
  calEdit.setAttribute("aria-expanded", String(open));
  if (open) calUrl.focus();
}

calEl.querySelector(".cal-row")!.addEventListener("click", (e) => {
  if ((e.target as Element).closest("#cal-refresh")) return;
  showCalForm(calEdit.getAttribute("aria-expanded") !== "true");
});
calRefresh.addEventListener("click", () => invoke("calendar_refresh"));
calForm.addEventListener("submit", (e) => {
  e.preventDefault();
  const url = calUrl.value.trim();
  if (!url) return;
  invoke("calendar_set_url", { url });
  calUrl.value = "";
  showCalForm(false);
});
$("cal-remove").addEventListener("click", () => {
  invoke("calendar_set_url", { url: "" });
  showCalForm(false);
});

// --- progress and the forest ---

interface Progress {
  today_sessions: number;
  today_minutes: number;
  streak: number;
  best_streak: number;
  week: { label: string; minutes: number; today: boolean }[];
}

const GOAL_CIRCUMFERENCE = 2 * Math.PI * 52;
const FOREST_SLOTS = 24;
const forestEl = $("forest");
let plantedToday = -1; // trees already drawn, so only new ones animate in

const duration = (min: number) => (min < 60 ? `${min}m` : `${Math.floor(min / 60)}h${min % 60 ? ` ${min % 60}m` : ""}`);

function renderProgress(p: Progress) {
  const goal = sound.timeline.goal;
  $("goal-count").textContent = String(p.today_sessions);
  $("goal-of").textContent = `of ${goal} session${goal === 1 ? "" : "s"}`;
  $("goal-ring").style.strokeDashoffset = String(GOAL_CIRCUMFERENCE * (1 - Math.min(1, p.today_sessions / goal)));
  $("focused").textContent = p.today_minutes
    ? `${duration(p.today_minutes)} focused today${p.today_sessions >= goal ? " · goal reached 🌲" : ""}`
    : "Nothing yet today";
  $("streak").textContent = String(p.streak);
  $("best").textContent = String(p.best_streak);
  $("week-total").textContent = duration(p.week.reduce((sum, d) => sum + d.minutes, 0));

  const most = Math.max(60, ...p.week.map((d) => d.minutes));
  $("bars").replaceChildren(
    ...p.week.map((d) => {
      const bar = document.createElement("div");
      bar.className = `day-bar${d.minutes ? " on" : ""}${d.today ? " today" : ""}`;
      bar.title = `${d.label}: ${duration(d.minutes)}`;
      const fill = document.createElement("i");
      fill.style.height = `${(d.minutes / most) * 100}%`;
      const label = document.createElement("span");
      label.textContent = d.label.slice(0, 2);
      bar.append(fill, label);
      return bar;
    }),
  );
  $("forest-note").textContent =
    p.today_sessions >= FOREST_SLOTS
      ? "Your forest is full today. What a day."
      : "Every focus session plants a tree in your forest below.";
  plantForest(p.today_sessions);
}

// Tree layout is fixed per day (seeded by the date), so earlier trees never move.
function forestLayout(day: string) {
  let seed = [...day].reduce((h, c) => (h * 31 + c.charCodeAt(0)) >>> 0, 7);
  const rand = () => (seed = (seed * 1664525 + 1013904223) >>> 0) / 2 ** 32;
  const slots = Array.from({ length: FOREST_SLOTS }, (_, i) => i);
  for (let i = slots.length - 1; i > 0; i--) {
    const j = Math.floor(rand() * (i + 1));
    [slots[i], slots[j]] = [slots[j], slots[i]];
  }
  return slots.map((slot) => ({ x: 7 + slot * 12.1 + (rand() - 0.5) * 6, h: 20 + rand() * 22 }));
}

function pine(x: number, h: number) {
  const base = 56;
  const w = h * 0.5;
  const tier = (bottom: number, top: number, half: number) =>
    `M${(x - half).toFixed(1)} ${bottom.toFixed(1)}L${x.toFixed(1)} ${top.toFixed(1)}L${(x + half).toFixed(1)} ${bottom.toFixed(1)}Z`;
  return (
    tier(base, base - h * 0.62, w * 0.5) + tier(base - h * 0.3, base - h * 0.86, w * 0.38) + tier(base - h * 0.56, base - h, w * 0.26)
  );
}

function plantForest(sessions: number) {
  const count = Math.min(sessions, FOREST_SLOTS);
  if (count === plantedToday) return;
  const layout = forestLayout(new Date().toLocaleDateString("en-CA")); // YYYY-MM-DD, local
  const cycles = sound.timeline.cycles;
  const trees = layout.slice(0, count).map((t, i) => ({ ...t, i }));
  // Taller trees behind shorter ones.
  trees.sort((a, b) => b.h - a.h);
  forestEl.replaceChildren(
    ...trees.map(({ x, h, i }) => {
      const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
      path.setAttribute("d", pine(x, h));
      const glow = (i + 1) % cycles === 0; // every full set grows a glowing tree
      path.setAttribute("class", `tree${glow ? " glow" : ""}${plantedToday >= 0 && i >= plantedToday ? " grow" : ""}`);
      return path;
    }),
  );
  plantedToday = count;
}

const loadProgress = () => invoke<Progress>("progress_get").then(renderProgress);

// --- music (Spotify or whatever is playing, through Windows media controls) ---

interface NowPlaying {
  available: boolean;
  spotify: boolean;
  app: string;
  title: string;
  artist: string;
  playing: boolean;
  art: string | null;
}

const musicEl = $("music");
const musicArt = $<HTMLImageElement>("music-art");
const followBox = $<HTMLInputElement>("music-follow");
const fadeBox = $<HTMLInputElement>("fade-out");
fadeBox.addEventListener("change", () => {
  sound.fade_out = fadeBox.checked;
  invoke("fade_out_set", { on: fadeBox.checked });
});
const breaksBox = $<HTMLInputElement>("music-breaks");

function renderMusic(n: NowPlaying) {
  musicEl.classList.toggle("off", !n.available);
  musicEl.classList.toggle("playing", n.playing);
  musicEl.title = n.available ? "" : "Open Spotify";
  musicArt.hidden = !n.art;
  if (n.art) musicArt.src = n.art;
  $("music-title").textContent = n.available ? n.title || n.app : "Spotify";
  $("music-sub").textContent = n.available
    ? [n.artist, n.spotify ? "" : n.app].filter(Boolean).join(" · ") || n.app
    : "Open Spotify to play music here";
}

musicEl.addEventListener("click", () => {
  if (musicEl.classList.contains("off")) invoke("music_open_spotify");
});
musicEl.querySelectorAll<HTMLButtonElement>(".music-btns button").forEach((b) =>
  b.addEventListener("click", (e) => {
    e.stopPropagation();
    if (b.dataset.action === "toggle") musicEl.classList.toggle("playing"); // feels instant; Windows confirms within a second
    invoke("music_control", { action: b.dataset.action });
  }),
);

function saveMusicSettings() {
  sound.music = { follow: followBox.checked, on_breaks: breaksBox.checked };
  breaksBox.disabled = !sound.music.follow;
  invoke("music_settings_set", { follow: sound.music.follow, onBreaks: sound.music.on_breaks });
}
followBox.addEventListener("change", saveMusicSettings);
breaksBox.addEventListener("change", saveMusicSettings);

const command = (name: string) => invoke<Snapshot>(name).then(render);
const toggle = () => command("timer_toggle");

$("toggle").addEventListener("click", toggle);
$("reset").addEventListener("click", () => command("timer_reset"));
$("skip").addEventListener("click", () => command("timer_skip"));
$("min").addEventListener("click", () => appWindow.minimize());
$("close").addEventListener("click", () => appWindow.close());
// Pin cycles: normal window → always on top → pinned to the desktop (behind everything).
type Pin = "normal" | "top" | "desktop";
const PIN_NEXT: Record<Pin, Pin> = { normal: "top", top: "desktop", desktop: "normal" };
const PIN_TITLE: Record<Pin, string> = {
  normal: "Normal window. Click to keep it on top.",
  top: "Always on top. Click to pin it to the desktop.",
  desktop: "Pinned to the desktop, behind your windows. Click for a normal window.",
};
function renderPin(pin: Pin) {
  pinBtn.dataset.pin = pin;
  pinBtn.title = PIN_TITLE[pin];
}
pinBtn.addEventListener("click", () => {
  const pin = PIN_NEXT[(pinBtn.dataset.pin as Pin) ?? "normal"];
  renderPin(pin);
  invoke("window_pin", { pin });
});

window.addEventListener("keydown", (e) => {
  if (e.repeat) return;
  if (!panel.hidden) {
    if (e.key === "Escape") showSettings(false);
    return; // typing in the settings shouldn't drive the timer
  }
  if (e.target instanceof HTMLInputElement && e.target.type !== "range") return;
  if (e.code === "Space") toggle();
  else if (e.key === "r" || e.key === "R") command("timer_reset");
  else if (e.key === "s" || e.key === "S") command("timer_skip");
  else if (e.key === "m" || e.key === "M") setMuted(!sound.muted);
  else return;
  e.preventDefault();
});

listen<Snapshot>("tick", (e) => render(e.payload));
listen("tasks-changed", () => taskCall("tasks_get", {}).then(loadProgress));
listen<CalendarStatus>("calendar-status", (e) => renderCalendar(e.payload));
listen<NowPlaying>("now-playing", (e) => renderMusic(e.payload));
listen<boolean>("muted", (e) => {
  sound.muted = e.payload; // changed from the tray menu
  renderMixer();
});
invoke<NowPlaying>("music_now").then(renderMusic);
invoke<CalendarStatus>("calendar_status").then(renderCalendar);
taskCall("tasks_get", {});
command("timer_state");
invoke<Sound>("sound_state").then((s) => {
  sound = s;
  renderMixer();
  renderPin(s.pin);
  refreshCustom();
  loadProgress();
});
