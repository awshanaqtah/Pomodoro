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
  /** Minutes you were away when the timer paused itself; 0 when it didn't. */
  away: number;
  /** Something to do with the current break. */
  idea: { text: string; kind: string } | null;
}

const LABELS: Record<Mode, string> = { focus: "Focus", short: "Short break", long: "Long break" };
const CIRCUMFERENCE = 2 * Math.PI * 88;
const PILL_CIRCUMFERENCE = 2 * Math.PI * 18;

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

  // Mini mode shows the same timer in a pill.
  $("pill-time").textContent = clock;
  $("pill-ring").style.strokeDashoffset = String(PILL_CIRCUMFERENCE * (s.remaining / s.total));
  pillMode = LABELS[s.mode];
  renderPillSub();

  renderAway(s);
  renderIdea(s);

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
  /** Minutes without keyboard or mouse before focus pauses itself; 0 is off. */
  away: number;
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
  mini: boolean;
  shortcuts: boolean;
  look: Look;
}

type Size = "small" | "medium" | "large";

interface Look {
  time_theme: boolean;
  seasons: boolean;
  living: boolean;
  size: Size;
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
  timeline: { focus: 25, short: 5, long: 15, cycles: 4, auto_breaks: true, auto_focus: false, goal: 8, away: 5 },
  music: { follow: true, on_breaks: false },
  pin: "normal",
  scenes: [],
  fade_out: true,
  mini: false,
  shortcuts: true,
  look: { time_theme: true, seasons: true, living: true, size: "medium" },
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

type NumberKey = "focus" | "short" | "long" | "cycles" | "goal" | "away";
type FlagKey = "auto_breaks" | "auto_focus";

const panel = $("settings");
const gearBtn = $("gear");
const numberInputs = Array.from(panel.querySelectorAll<HTMLInputElement>('input[type="number"]'));
const flagInputs = Array.from(panel.querySelectorAll<HTMLInputElement>('input[type="checkbox"][data-key]'));

function renderTimeline() {
  for (const el of numberInputs) el.value = String(sound.timeline[el.dataset.key as NumberKey]);
  for (const el of flagInputs) el.checked = sound.timeline[el.dataset.key as FlagKey];
  fadeBox.checked = sound.fade_out;
  shortcutsBox.checked = sound.shortcuts;
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
  renderPillSub();
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

interface Reward {
  id: string;
  name: string;
  hint: string;
  unlocked: boolean;
  progress: number;
}

interface Progress {
  today_sessions: number;
  today_minutes: number;
  streak: number;
  best_streak: number;
  week: { label: string; minutes: number; today: boolean }[];
  total_sessions: number;
  rewards: Reward[];
  by_task: { title: string; minutes: number }[];
}

interface DayCell {
  date: string;
  minutes: number;
  sessions: number;
}

interface SessionView {
  index: number;
  at: string | null;
  minutes: number;
  task: string | null;
  note: string | null;
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
  renderByTask(p.by_task);
  renderRewards(p.rewards);
  plantForest(p.today_sessions);
  loadMonth();
}

const REWARD_ICONS: Record<string, string> = {
  pines: "🌲",
  maples: "🍁",
  blossoms: "🌸",
  fireflies: "✨",
  moon: "🌕",
  owl: "🦉",
  campfire: "🏕️",
  aurora: "🌌",
};
let unlocked = new Set<string>(["pines"]);

function renderRewards(list: Reward[]) {
  const next = new Set(list.filter((r) => r.unlocked).map((r) => r.id));
  if ([...next].join() !== [...unlocked].join()) plantedToday = -1; // redraw the forest with them
  unlocked = next;
  document.body.dataset.aurora = String(unlocked.has("aurora"));
  $("rewards").replaceChildren(
    ...list.map((r) => {
      const tile = document.createElement("div");
      tile.className = `reward${r.unlocked ? " on" : ""}`;
      tile.title = r.unlocked ? `${r.name}: unlocked` : `${r.name}: unlocks at ${r.hint}`;
      const icon = document.createElement("b");
      icon.textContent = REWARD_ICONS[r.id] ?? "🌱";
      const name = document.createElement("span");
      name.textContent = r.name;
      tile.append(icon, name);
      if (!r.unlocked) {
        const meter = document.createElement("span");
        meter.className = "meter";
        meter.innerHTML = `<i style="width:${Math.round(r.progress * 100)}%"></i>`;
        tile.append(meter);
      }
      return tile;
    }),
  );
}

function renderByTask(list: { title: string; minutes: number }[]) {
  const el = $("by-task");
  if (!list.length) {
    el.innerHTML = "<p>Pick a task before you focus and its time shows up here.</p>";
    return;
  }
  const most = list[0].minutes;
  el.replaceChildren(
    ...list.map((t) => {
      const row = document.createElement("div");
      row.className = "row";
      const title = document.createElement("span");
      title.textContent = t.title;
      const time = document.createElement("span");
      time.textContent = duration(t.minutes);
      const bar = document.createElement("i");
      bar.style.width = `${Math.max(4, (t.minutes / most) * 100)}%`;
      row.append(title, time, bar);
      return row;
    }),
  );
}

// --- forest calendar: one tree per day, sized by how long you focused ---

let monthShown = new Date();
monthShown.setDate(1);
let selectedDay: string | null = null;
const todayIso = () => new Date().toLocaleDateString("en-CA");

function dayTree(minutes: number) {
  if (!minutes) return '<svg viewBox="0 0 24 24"><rect x="9" y="21" width="6" height="1.5" rx=".75" opacity=".5" /></svg>';
  const h = minutes < 30 ? 7 : minutes < 90 ? 12 : minutes < 180 ? 17 : 21;
  return `<svg viewBox="0 0 24 24"><path d="${pine(12, h, 22.5)}" /></svg>`;
}

function loadMonth() {
  const year = monthShown.getFullYear();
  const month = monthShown.getMonth() + 1;
  $("month-name").textContent = monthShown.toLocaleDateString("en-GB", { month: "long", year: "numeric" });
  invoke<DayCell[]>("month_get", { year, month }).then((days) => {
    const today = todayIso();
    const lead = (new Date(year, month - 1, 1).getDay() + 6) % 7; // Monday first
    const heads = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].map((d) => {
      const el = document.createElement("span");
      el.className = "dow";
      el.textContent = d;
      return el;
    });
    const blanks = Array.from({ length: lead }, () => document.createElement("span"));
    const cells = days.map((d) => {
      const cell = document.createElement("button");
      cell.className = `day${d.date === today ? " today" : ""}${d.date > today ? " future" : ""}${d.date === selectedDay ? " selected" : ""}`;
      cell.title = d.minutes ? `${duration(d.minutes)} · ${d.sessions} session${d.sessions === 1 ? "" : "s"}` : "No focus";
      cell.innerHTML = `<span>${Number(d.date.slice(8))}</span>${dayTree(d.minutes)}`;
      cell.addEventListener("click", () => showDay(d.date));
      return cell;
    });
    $("month-grid").replaceChildren(...heads, ...blanks, ...cells);
  });
}

function showDay(date: string) {
  const detail = $("day-detail");
  if (selectedDay === date) {
    selectedDay = null;
    detail.hidden = true;
    loadMonth();
    return;
  }
  selectedDay = date;
  loadMonth();
  invoke<SessionView[]>("day_get", { date }).then((sessions) => {
    detail.hidden = false;
    if (!sessions.length) {
      detail.innerHTML = "<p>No focus that day.</p>";
      return;
    }
    detail.replaceChildren(
      ...sessions.map((s) => {
        const line = document.createElement("div");
        const when = document.createElement("span");
        when.className = "when";
        when.textContent = `${s.at ?? "--:--"} · ${s.minutes} min${s.task ? " · " : ""}`;
        line.append(when, s.task ?? "");
        if (s.note) {
          const note = document.createElement("span");
          note.className = "note-text";
          note.textContent = `“${s.note}”`;
          line.append(note);
        }
        return line;
      }),
    );
  });
}

$("month-prev").addEventListener("click", () => {
  monthShown = new Date(monthShown.getFullYear(), monthShown.getMonth() - 1, 1);
  loadMonth();
});
$("month-next").addEventListener("click", () => {
  monthShown = new Date(monthShown.getFullYear(), monthShown.getMonth() + 1, 1);
  loadMonth();
});

// --- session notes: a line about what that focus session got done ---

let noteFor: SessionView | null = null;

function askForNote(s: SessionView | null) {
  noteFor = s;
  const form = $<HTMLFormElement>("note");
  form.hidden = !s;
  if (!s) return;
  $("note-label").textContent = `What did you get done?${s.task ? ` (${s.task})` : ""}`;
  $<HTMLInputElement>("note-text").value = "";
}

function finishNote(text: string) {
  if (!noteFor) return;
  invoke("note_save", { index: noteFor.index, text }).then(loadProgress);
  askForNote(null);
}

$("note").addEventListener("submit", (e) => {
  e.preventDefault();
  finishNote($<HTMLInputElement>("note-text").value.trim());
});
$("note-skip").addEventListener("click", () => finishNote(""));
const loadNote = () => invoke<SessionView | null>("note_pending").then(askForNote);

// --- a moment of celebration when the forest gains something ---

let toastTimer: number | undefined;
function toast(text: string) {
  const el = $("toast");
  el.textContent = text;
  el.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => (el.hidden = true), 5000);
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

function pine(x: number, h: number, base = 56) {
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
  const species = ["pine", ...(unlocked.has("maples") ? ["maple"] : []), ...(unlocked.has("blossoms") ? ["blossom"] : [])];
  const kindOf = ({ x, i }: { x: number; i: number }) =>
    (i + 1) % cycles === 0 ? "pine" : species[(i * 7 + Math.round(x)) % species.length];
  forestEl.replaceChildren(
    ...trees.map((t) => {
      const { x, h, i } = t;
      const glow = (i + 1) % cycles === 0; // every full set grows a glowing tree
      const kind = kindOf(t);
      const path = svgEl("path");
      path.setAttribute("d", kind === "pine" ? pine(x, h) : roundTree(x, h, kind === "blossom"));
      path.setAttribute("class", `tree ${kind}${glow ? " glow" : ""}${plantedToday >= 0 && i >= plantedToday ? " grow" : ""}`);
      return path;
    }),
    ...snowCaps(trees.filter((t) => kindOf(t) === "pine")),
    ...forestExtras(),
  );
  plantedToday = count;
}

const svgEl = (tag: string) => document.createElementNS("http://www.w3.org/2000/svg", tag);

/** In winter, the top of every tree carries snow. */
function snowCaps(trees: { x: number; h: number }[]): SVGElement[] {
  if (document.body.dataset.season !== "winter") return [];
  return trees.map(({ x, h }) => {
    const cap = svgEl("path");
    const top = 56 - h;
    const w = h * 0.5 * 0.18;
    cap.setAttribute("d", `M${(x - w).toFixed(1)} ${(top + h * 0.14).toFixed(1)}L${x.toFixed(1)} ${top.toFixed(1)}L${(x + w).toFixed(1)} ${(top + h * 0.14).toFixed(1)}Z`);
    cap.setAttribute("class", "snow");
    return cap;
  });
}

/** A broadleaf tree: trunk plus a round crown (three puffs for blossom). */
function roundTree(x: number, h: number, puffy: boolean) {
  const base = 56;
  const r = h * 0.3;
  const cy = base - h + r;
  const circle = (cx: number, cy: number, r: number) =>
    `M${(cx - r).toFixed(1)} ${cy.toFixed(1)}a${r.toFixed(1)} ${r.toFixed(1)} 0 1 0 ${(2 * r).toFixed(1)} 0a${r.toFixed(1)} ${r.toFixed(1)} 0 1 0 ${(-2 * r).toFixed(1)} 0Z`;
  const trunk = `M${(x - 0.8).toFixed(1)} ${base}V${(cy + r * 0.6).toFixed(1)}h1.6V${base}Z`;
  const crown = puffy
    ? circle(x - r * 0.45, cy + r * 0.25, r * 0.7) + circle(x + r * 0.45, cy + r * 0.25, r * 0.7) + circle(x, cy - r * 0.2, r * 0.75)
    : circle(x, cy, r);
  return trunk + crown;
}

/** The moon, an owl, fireflies and a campfire, as you earn them. */
function forestExtras(): SVGElement[] {
  const out: SVGElement[] = [];
  if (unlocked.has("moon")) {
    const moon = svgEl("circle");
    moon.setAttribute("cx", "272");
    moon.setAttribute("cy", "14");
    moon.setAttribute("r", "5");
    moon.setAttribute("class", "moon");
    out.push(moon);
  }
  if (unlocked.has("owl")) {
    // perched on the peak of the far ridge
    const owl = svgEl("g");
    owl.innerHTML = `<path class="owl" d="M53 21c0-2.5 1.3-4 3-4s3 1.5 3 4v3h-6Z M53.3 17.6l.6-1.6 1 1.2Z M58.7 17.6l-.6-1.6-1 1.2Z" />
      <circle class="owl-eye" cx="55.1" cy="19" r=".55" /><circle class="owl-eye" cx="56.9" cy="19" r=".55" />`;
    out.push(owl);
  }
  if (unlocked.has("campfire")) {
    const fire = svgEl("g");
    fire.innerHTML = `<path class="trunk" d="M144 56.5l9-2 .5 1.2-9 2ZM145 54.5l9 2-.5 1.2-9-2Z" />
      <path class="flame" d="M149 55c-2.2 0-3.2-1.6-2.6-3.4.4-1.2 1.4-1.8 1.6-3.6 1.4 1 2 2.2 1.9 3.3.6-.4.9-1 1-1.7 1 .9 1.7 2.1 1.4 3.4-.3 1.2-1.4 2-3.3 2Z" />`;
    out.push(fire);
  }
  if (unlocked.has("fireflies")) {
    for (const [x, y] of [[40, 34], [96, 28], [131, 40], [188, 31], [233, 38], [262, 30]]) {
      const fly = svgEl("circle");
      fly.setAttribute("cx", String(x));
      fly.setAttribute("cy", String(y));
      fly.setAttribute("r", ".9");
      fly.setAttribute("class", "firefly");
      out.push(fly);
    }
  }
  return out;
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

// --- mini mode, away pause, break ideas, shortcuts ---

const pill = $("pill");
let pillMode = "Focus";

function renderPillSub() {
  const active = tasks.list.find((t) => t.id === tasks.active);
  $("pill-sub").textContent = active ? `${pillMode} · ${active.title}` : pillMode;
}

function renderMini(on: boolean) {
  sound.mini = on;
  document.body.dataset.mini = String(on);
  pill.hidden = !on;
}

function setMini(on: boolean) {
  renderMini(on);
  invoke("window_mini", { on });
}

$("mini").addEventListener("click", () => setMini(true));
$("pill-expand").addEventListener("click", () => setMini(false));
pill.addEventListener("dblclick", (e) => {
  if (!(e.target as Element).closest("button")) setMini(false);
});
$("pill-toggle").addEventListener("click", () => command("timer_toggle"));

function renderAway(s: Snapshot) {
  const away = $("away");
  away.hidden = !(s.away > 0 && !s.running);
  $("away-text").textContent = `You were away ${s.away} min, so focus paused. Those minutes didn't count.`;
}
$("away-resume").addEventListener("click", () => command("timer_toggle"));

const IDEA_ICONS: Record<string, string> = {
  eyes: '<path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6S2 12 2 12Z" /><circle cx="12" cy="12" r="2.5" />',
  stretch: '<circle cx="12" cy="4.5" r="1.8" /><path d="M5 6l7 4 7-4M12 10v5l-3 6M12 15l3 6" />',
  water: '<path d="M6 4h12l-1.5 16h-9L6 4ZM6.5 9h11" />',
  breathe: '<circle cx="12" cy="12" r="3" /><circle cx="12" cy="12" r="6.5" opacity=".55" /><circle cx="12" cy="12" r="10" opacity=".25" />',
  walk: '<circle cx="13" cy="4.5" r="1.8" /><path d="M10 21l2-6 3 3v3M12 15l1-6-4 2-1 3M13 9l3 3h3" />',
  rest: '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5Z" />',
};

function renderIdea(s: Snapshot) {
  const card = $("idea");
  card.hidden = !s.idea || s.mode === "focus";
  if (!s.idea) return;
  card.querySelector("svg")!.innerHTML = IDEA_ICONS[s.idea.kind] ?? IDEA_ICONS.rest;
  $("idea-text").textContent = s.idea.text;
}

const shortcutsBox = $<HTMLInputElement>("shortcuts");
const showShortcutError = (error: string | null) => ($("shortcut-error").textContent = error ?? "");
shortcutsBox.addEventListener("change", () => {
  sound.shortcuts = shortcutsBox.checked;
  invoke<string | null>("shortcuts_set", { on: shortcutsBox.checked }).then(showShortcutError);
});

// --- look: time of day, seasons, living background, size ---

const SCALE: Record<Size, number> = { small: 0.85, medium: 1, large: 1.15 };

function timeOfDay() {
  const h = new Date().getHours();
  return h >= 5 && h < 8 ? "dawn" : h >= 8 && h < 17 ? "day" : h >= 17 && h < 20 ? "dusk" : "night";
}

function season() {
  const m = new Date().getMonth(); // 0 = January
  return m >= 2 && m <= 4 ? "spring" : m >= 5 && m <= 7 ? "summer" : m >= 8 && m <= 10 ? "autumn" : "winter";
}

function applyLook() {
  const look = sound.look;
  const body = document.body.dataset;
  body.time = look.time_theme ? timeOfDay() : "day";
  const nextSeason = look.seasons ? season() : "summer";
  if (body.season !== nextSeason) {
    body.season = nextSeason;
    plantedToday = -1; // snow caps come and go with the season
    loadProgress();
  }
  body.living = String(look.living);
  document.documentElement.style.zoom = String(SCALE[look.size]);
  lookTime.checked = look.time_theme;
  lookSeasons.checked = look.seasons;
  lookLiving.checked = look.living;
  sizeButtons.forEach((b) => b.setAttribute("aria-checked", String(b.dataset.size === look.size)));
}

function saveLook() {
  applyLook();
  invoke("look_set", { look: sound.look });
}

// A dozen embers or raindrops, each with its own place and pace. CSS animates them.
$("weather").replaceChildren(
  ...Array.from({ length: 14 }, () => {
    const p = document.createElement("i");
    p.style.left = `${Math.random() * 100}%`;
    p.style.setProperty("--d", `${7 + Math.random() * 7}s`);
    p.style.setProperty("--delay", `${-Math.random() * 12}s`);
    p.style.setProperty("--drift", `${(Math.random() - 0.5) * 60}px`);
    return p;
  }),
);

const lookTime = $<HTMLInputElement>("look-time");
const lookSeasons = $<HTMLInputElement>("look-seasons");
const lookLiving = $<HTMLInputElement>("look-living");
const sizeButtons = Array.from(document.querySelectorAll<HTMLButtonElement>("#look-size button"));
lookTime.addEventListener("change", () => {
  sound.look.time_theme = lookTime.checked;
  saveLook();
});
lookSeasons.addEventListener("change", () => {
  sound.look.seasons = lookSeasons.checked;
  saveLook();
});
lookLiving.addEventListener("change", () => {
  sound.look.living = lookLiving.checked;
  saveLook();
});
sizeButtons.forEach((b) =>
  b.addEventListener("click", () => {
    sound.look.size = b.dataset.size as Size;
    saveLook();
  }),
);
setInterval(applyLook, 60_000); // dawn turns to day while the widget is open

// --- updates: signed, from the public releases repo ---

interface UpdateState {
  current: string;
  available: string | null;
  status: "idle" | "checking" | "available" | "installing" | "error";
  error: string | null;
}

function renderUpdate(u: UpdateState) {
  const row = document.querySelector<HTMLElement>(".update-row")!;
  const button = $("update-btn");
  row.classList.toggle("ready", u.status === "available");
  button.hidden = u.status === "checking" || u.status === "installing";
  const text: Record<UpdateState["status"], string> = {
    idle: `Version ${u.current} · up to date`,
    checking: "Checking for updates…",
    available: `Version ${u.available} is ready`,
    installing: `Installing ${u.available}… Pomodoro will reopen`,
    error: `Couldn't check for updates (${u.error ?? "offline?"})`,
  };
  $("update-text").textContent = text[u.status];
  button.textContent = u.status === "available" ? "Install" : "Check now";
  button.onclick = () => invoke(u.status === "available" ? "update_install" : "update_check");
}

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
listen<boolean>("mini", (e) => renderMini(e.payload)); // from the shortcut or tray
listen<UpdateState>("update", (e) => renderUpdate(e.payload));
invoke<UpdateState>("update_state").then(renderUpdate);
listen<Mode>("phase-done", (e) => {
  if (e.payload === "focus") loadNote();
});
listen<string[]>("reward", (e) => toast(`New in your forest: ${e.payload.join(", ")} ✨`));
loadNote();
listen<boolean>("muted", (e) => {
  sound.muted = e.payload; // changed from the tray menu
  renderMixer();
});
invoke<NowPlaying>("music_now").then(renderMusic);
invoke<CalendarStatus>("calendar_status").then(renderCalendar);
taskCall("tasks_get", {});
command("timer_state");
invoke<Sound>("sound_state").then((s) => {
  sound = { ...sound, ...s, look: { ...sound.look, ...s.look } }; // anything missing keeps its default
  renderMixer();
  renderPin(s.pin);
  renderMini(s.mini);
  applyLook();
  refreshCustom();
  invoke<string | null>("shortcuts_error").then(showShortcutError);
  loadProgress();
});
