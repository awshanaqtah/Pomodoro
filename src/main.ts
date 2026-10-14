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
  timeline: { focus: 25, short: 5, long: 15, cycles: 4, auto_breaks: true, auto_focus: false, goal: 8 },
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

type NumberKey = "focus" | "short" | "long" | "cycles" | "goal";
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
      bar.className = `bar${d.minutes ? " on" : ""}${d.today ? " today" : ""}`;
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
invoke<CalendarStatus>("calendar_status").then(renderCalendar);
taskCall("tasks_get", {});
command("timer_state");
invoke<Sound>("sound_state").then((s) => {
  sound = s;
  renderMixer();
  loadProgress();
});
