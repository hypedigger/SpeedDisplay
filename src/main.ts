// Lumen frontend.
// The grid is fully virtualised: only visible cells exist in the DOM, so a
// folder with 50 000 files scrolls as smoothly as one with 50.

import { invoke, convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { setLang, t } from "./i18n";
import "./styles.css";

type Kind = "dir" | "image" | "raw" | "video" | "other";

interface Entry {
  name: string;
  path: string;
  kind: Kind;
  size: number;
  mtime: number;
  ext: string;
  hidden?: boolean;
}

interface Meta {
  rating: number;
  color: string;
  flag: string;
  tags: string;
  /** Non-destructive display rotation: 0, 90, 180 or 270 degrees. */
  rot: number;
  /** Where playback of this video stopped, in seconds. 0 = from the start. */
  pos: number;
}

interface Settings {
  lang: string;
  theme: string;
  previewMode: "hover" | "pregen" | "off";
  thumbSize: number;
  view: "grid" | "list";
  lastPath?: string;
  /** "last" resumes the previous session's folder, "fixed" opens startPath. */
  startMode: "last" | "fixed";
  startPath: string;
  /** Pinned folders shown at the top of the sidebar. */
  favorites: { name: string; path: string }[];
  /** Viewer fit mode: fit both / fit width / fit height / actual size. */
  fitMode: "fit" | "fitW" | "fitH" | "actual";
  /** Shrink images larger than the screen (ACDSee "reduce"). */
  fitReduce: boolean;
  /** Grow images smaller than the screen (ACDSee "enlarge"). */
  fitEnlarge: boolean;
  /** Action id -> list of key combos. */
  shortcuts: Record<string, string[]>;
  /** Default combos already merged into `shortcuts`; never re-added if removed. */
  addedCombos: string[];
  /** UI font. */
  fontSize: number;
  fontFamily: string;
  fontWeight: "400" | "600" | "700";
  /** Cache size cap in GiB; 0 = unlimited. Least recently used go first. */
  cacheLimitGb: number;
  /** Wipe the converted videos and full previews when the app closes. */
  clearPlayOnExit: boolean;
  /** Scrollbar width, percent of the 10px base (100-500). */
  scrollbarPct: number;
  /** Keep zoom & fit when moving between images. */
  keepZoom: boolean;
  /** Wrap around after the last image / before the first. */
  loopNav: boolean;
  showHidden: boolean;
  /** Extra extensions treated as images / videos (comma-separated). */
  customImgExts: string;
  customVidExts: string;
  /** External editors for "Open with". */
  editors: { name: string; path: string }[];
  savedSearches: { name: string; state: FilterState }[];
  autoRefresh: boolean;
  /** Loop video playback by default. */
  videoLoop: boolean;
  /** Seconds jumped per wheel notch over the fullscreen seek bar. */
  wheelSeekSec: number;
  /** Prev/next overlay buttons: visibility and size (percent of 46px). */
  navBtnShow: boolean;
  navBtnPct: number;
  /** Delete button in the viewer/player. */
  delBtnShow: boolean;
  /** Persisted sort order of the file grid ("name-asc", "date-desc"…). */
  sortOrder: string;
  /** Double-clicking a thumbnail opens straight into true fullscreen. */
  openFullscreen: boolean;
  /** Display options of the TRUE FULLSCREEN mode (independent from windowed). */
  fsFitMode: "fit" | "fitW" | "fitH" | "actual";
  fsFitReduce: boolean;
  fsFitEnlarge: boolean;
  fsNavBtnShow: boolean;
  fsDelBtnShow: boolean;
  /** Which buttons appear in the windowed viewer's toolbar. */
  vtShow: Record<string, boolean>;
  /** Sidebar (folder tree) width in pixels, drag-resizable. */
  sidebarWidth: number;
  /** Columns shown in the details view, by id. */
  columns: Record<string, boolean>;
  /** Media type filter: "all", "image" or "video" (persisted). */
  filterKind: "all" | "image" | "video";
  /** Sidebar shortcuts the user removed (paths, hidden from the list). */
  hiddenShortcuts: string[];
  /** True once the legacy "Raccourcis" list was merged into Favourites. */
  favImported: boolean;
  /** Also list non-media files, with their Windows shell icons. */
  showAllFiles: boolean;
  /** Full-resolution images kept decoded in RAM around the current one. */
  preloadCount: number;
  /** Write rotations into the file's EXIF Orientation tag (lossless,
   *  2 bytes) so other programs see them too. */
  writeExifRotation: boolean;
  /** Mirror ratings, colour labels and keywords into .xmp sidecars. */
  writeXmp: boolean;
}

interface FilterState {
  minSizeMb: number;
  maxSizeMb: number;
  dateFrom: string;
  dateTo: string;
  minRating: number;
  color: string;
  flag: string;
  tag: string;
  recursive: boolean;
}
const EMPTY_FILTERS: FilterState = {
  minSizeMb: 0, maxSizeMb: 0, dateFrom: "", dateTo: "",
  minRating: 0, color: "", flag: "", tag: "", recursive: false,
};

const DEFAULTS: Settings = {
  lang: "fr",
  theme: "dark",
  previewMode: "hover",
  thumbSize: 220,
  view: "grid",
  startMode: "last",
  startPath: "",
  favorites: [],
  fitMode: "fit",
  fitReduce: true,
  fitEnlarge: false,
  addedCombos: [],
  shortcuts: {
    up: ["Backspace"],
    open: ["Enter"],
    gridPrev: ["ArrowLeft"],
    gridNext: ["ArrowRight"],
    toggleView: ["F3"],
    favorite: ["Ctrl+D"],
    settings: ["F9"],
    search: ["Ctrl+F"],
    rename: ["F2"],
    delete: ["Delete"],
    copy: ["Ctrl+C"],
    cut: ["Ctrl+X"],
    paste: ["Ctrl+V", "Shift+Insert"],
    prev: ["ArrowLeft", "PageUp"],
    next: ["ArrowRight", "PageDown", "Space"],
    close: ["Escape"],
    closeStep: ["Enter"],
    zoomIn: ["+", "="],
    zoomOut: ["-"],
    fit: ["0"],
    fitW: ["2"],
    fitH: ["3"],
    actual: ["1"],
    toggleEnlarge: ["4"],
    actualSize: ["*"],
    fitScreen: ["/"],
    fullscreen: ["F11"],
    info: ["I"],
    strip: ["T"],
    pick: ["P"],
    reject: ["X"],
    rate0: ["Ctrl+0"],
    rate1: ["Ctrl+1"],
    rate2: ["Ctrl+2"],
    rate3: ["Ctrl+3"],
    rate4: ["Ctrl+4"],
    rate5: ["Ctrl+5"],
    navBack: ["Alt+ArrowLeft"],
    navForward: ["Alt+ArrowRight"],
    selectAll: ["Ctrl+A"],
    newFolder: ["Ctrl+N"],
    gridFirst: ["Home"],
    gridLast: ["End"],
    selToStart: ["Shift+Home"],
    selToEnd: ["Shift+End"],
    gridRowUp: ["ArrowUp"],
    gridRowDown: ["ArrowDown"],
    selLeft: ["Shift+ArrowLeft"],
    selRight: ["Shift+ArrowRight"],
    selUp: ["Shift+ArrowUp"],
    selDown: ["Shift+ArrowDown"],
    undo: ["Ctrl+Z"],
    properties: ["Alt+Enter"],
    copyPath: ["Ctrl+Shift+C"],
    addressBar: ["Ctrl+L"],
    viewList: ["G"],
    viewGrid: ["H"],
    rotLeft: ["L"],
    rotRight: ["R"],
    rotReset: ["Ctrl+R"],
  },
  fontSize: 13,
  fontFamily: "Segoe UI",
  fontWeight: "400",
  cacheLimitGb: 20,
  clearPlayOnExit: false,
  scrollbarPct: 200,
  keepZoom: false,
  loopNav: true,
  showHidden: false,
  customImgExts: "",
  customVidExts: "",
  editors: [],
  savedSearches: [],
  autoRefresh: true,
  videoLoop: true,
  wheelSeekSec: 25,
  navBtnShow: true,
  navBtnPct: 200,
  delBtnShow: true,
  sortOrder: "name-asc",
  openFullscreen: true,
  fsFitMode: "fit",
  fsFitReduce: true,
  fsFitEnlarge: true,
  fsNavBtnShow: true,
  fsDelBtnShow: true,
  vtShow: {
    fs: true, fit: true, fitW: true, fitH: true, actual: true,
    enlarge: true, rotl: true, rotr: true, strip: true, info: true, print: true,
    pspeed: true, pmarkA: true, pmarkB: true, pabloop: true, pcut: true,
  },
  sidebarWidth: 200,
  columns: {
    size: true, modified: true, taken: true, type: true,
    rating: false, color: false, tags: false,
    res: false, mpx: false, ratio: false,
  },
  filterKind: "all",
  hiddenShortcuts: [],
  favImported: false,
  showAllFiles: false,
  preloadCount: 5,
  writeExifRotation: true,
  writeXmp: true,
};

// Themes: the two originals plus palettes borrowed from macOS, popular
// code editors and photo managers. Colours live in styles.css.
const THEMES: { id: string; label: string }[] = [
  { id: "dark", label: "Sombre" },
  { id: "light", label: "Clair" },
  { id: "macos-dark", label: "macOS Sombre" },
  { id: "macos-light", label: "macOS Clair" },
  { id: "graphite", label: "Graphite" },
  { id: "dracula", label: "Dracula" },
  { id: "one-dark", label: "One Dark" },
  { id: "monokai", label: "Monokai" },
  { id: "solarized-dark", label: "Solarized Dark" },
  { id: "solarized-light", label: "Solarized Light" },
  { id: "nord", label: "Nord" },
  { id: "gruvbox", label: "Gruvbox" },
  { id: "tokyo-night", label: "Tokyo Night" },
  { id: "github-dark", label: "GitHub Dark" },
  { id: "github-light", label: "GitHub Light" },
  { id: "night-owl", label: "Night Owl" },
  { id: "ayu-dark", label: "Ayu Dark" },
  { id: "ayu-light", label: "Ayu Light" },
  { id: "palenight", label: "Palenight" },
  { id: "cobalt2", label: "Cobalt2" },
  { id: "lightroom", label: "Lightroom" },
  { id: "capture-one", label: "Capture One" },
  { id: "darktable", label: "Darktable" },
  { id: "charcoal", label: "Charcoal" },
];

// ---------- State ----------
let settings: Settings = { ...DEFAULTS };
let currentPath = "";
let entries: Entry[] = [];       // raw listing of the current folder
let visible: Entry[] = [];       // after filter + sort (media + folders)
let thumbs = new Map<string, string>();   // src path -> thumb file path
let previews = new Map<string, string>(); // src path -> hover clip path
let metaMap = new Map<string, Meta>();    // src path -> rating/color/flag/tags
let takenMap = new Map<string, number>(); // src path -> EXIF capture time (ms)
let dimsMap = new Map<string, [number, number]>(); // src path -> pixel size
let currentGen = 0;
let selectedIndex = -1;                   // focused item (keyboard anchor)
let selected = new Set<number>();         // multi-selection (indexes in visible)
let selAnchor = -1;                       // shift-click range anchor
let viewerIndex = -1;                     // index within mediaList()
let pregenQueue: string[] = [];
let pregenRunning = 0;
let filters: FilterState = { ...EMPTY_FILTERS };
let searchGen = 0;                        // recursive search generation

function setFocus(i: number, opts?: { ctrl?: boolean; shift?: boolean }): void {
  if (opts?.shift && selAnchor >= 0) {
    selected.clear();
    const [a, b] = [Math.min(selAnchor, i), Math.max(selAnchor, i)];
    for (let k = a; k <= b; k++) selected.add(k);
  } else if (opts?.ctrl) {
    if (selected.has(i)) selected.delete(i);
    else selected.add(i);
    selAnchor = i;
  } else {
    selected.clear();
    selected.add(i);
    selAnchor = i;
  }
  selectedIndex = i;
  updateSelStatus();
}
/** Scroll the grid (or list) so that item `i` is visible. */
function scrollToIndex(i: number): void {
  if (i < 0 || i >= visible.length) return;
  if (settings.view === "grid") {
    const row = Math.floor(i / gridCols);
    const top = GAP + row * gridRowH;
    const bottom = top + gridRowH;
    if (top < gridWrap.scrollTop) {
      gridWrap.scrollTop = Math.max(0, top - GAP);
    } else if (bottom > gridWrap.scrollTop + gridWrap.clientHeight) {
      gridWrap.scrollTop = bottom - gridWrap.clientHeight + GAP;
    }
    layoutGrid();
  } else {
    listBody.children[i]?.scrollIntoView({ block: "nearest" });
  }
}

/** How many items one row holds: a full grid row, or a single list line. */
function rowStep(): number {
  return settings.view === "grid" ? Math.max(1, gridCols) : 1;
}

/** Select from the anchor up to `to`. The anchor is the item that was
 *  focused when the shift-selection started, so repeated Shift+Arrow
 *  presses grow and shrink the same range, as in Explorer. */
function extendSelection(to: number): void {
  if (!visible.length) return;
  if (selAnchor < 0) selAnchor = selectedIndex >= 0 ? selectedIndex : 0;
  const target = Math.min(Math.max(to, 0), visible.length - 1);
  selected.clear();
  const [a, b] = selAnchor <= target ? [selAnchor, target] : [target, selAnchor];
  for (let i = a; i <= b; i++) selected.add(i);
  selectedIndex = target;
  updateSelStatus();
  repaint();
  scrollToIndex(target);
}

/** Move the shift-selection edge by `delta` items. */
function extendSelectionBy(delta: number): void {
  const from = selectedIndex >= 0 ? selectedIndex : selAnchor >= 0 ? selAnchor : 0;
  extendSelection(from + delta);
}

/** Move the focus (and single selection) by `delta` items. */
function moveFocusBy(delta: number): void {
  if (!visible.length) return;
  const from = selectedIndex >= 0 ? selectedIndex : 0;
  setFocus(Math.min(Math.max(from + delta, 0), visible.length - 1));
  repaint();
  scrollToIndex(selectedIndex);
}

function selEntries(): Entry[] {
  const list = [...selected].sort((a, b) => a - b).map((i) => visible[i]).filter(Boolean);
  if (!list.length && visible[selectedIndex]) return [visible[selectedIndex]];
  return list;
}
function updateSelStatus(): void {
  if (selected.size > 1) {
    const bytes = selEntries().reduce((s, e) => s + e.size, 0);
    statusbar.textContent = `${selected.size} ${t("selectedSuffix")} — ${fmtSize(bytes)}`;
  } else {
    statusbar.textContent = `${visible.length} ${t("items")}`;
  }
}

// ---------- DOM ----------
const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const gridWrap = $<HTMLDivElement>("grid-wrap");
const gridSpacer = $<HTMLDivElement>("grid-spacer");
const grid = $<HTMLDivElement>("grid");
const listTable = $<HTMLTableElement>("list");
const listBody = listTable.querySelector("tbody")!;
const statusbar = $<HTMLDivElement>("status-main");
const statusDisk = $<HTMLDivElement>("status-disk");
const viewer = $<HTMLDivElement>("viewer");
const viewerScroll = $<HTMLDivElement>("viewer-scroll");
const viewerImg = $<HTMLImageElement>("viewer-img");
const viewerInfo = $<HTMLDivElement>("viewer-info");
const player = $<HTMLDivElement>("player");
const playerVideo = $<HTMLVideoElement>("player-video");
const playerInfo = $<HTMLDivElement>("player-info");

// ---------- Utils ----------
function fmtSize(n: number): string {
  if (n < 1024) return `${n} o`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(0)} Ko`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} Mo`;
  return `${(n / 1024 ** 3).toFixed(2)} Go`;
}
function fmtDate(ms: number): string {
  return new Date(ms).toLocaleString();
}
function mediaList(): Entry[] {
  // Only what the viewer can actually show: a folder full of .txt or .xmp
  // files used to count as "media" and kept the viewer open on nothing.
  return visible.filter((e) => e.kind !== "dir" && e.kind !== "other");
}

// ---------- Settings ----------
async function loadSettings(): Promise<void> {
  const saved = (await invoke("get_settings")) as Partial<Settings>;
  settings = { ...DEFAULTS, ...saved };
  // Merge per-action so new actions keep their default keys.
  settings.shortcuts = { ...DEFAULTS.shortcuts, ...(saved.shortcuts ?? {}) };
  // Combos added after the user's settings file was written.
  for (const [id, combo] of [["paste", "Shift+Insert"]] as const) {
    const list = settings.shortcuts[id] ?? [];
    if (!list.includes(combo) && !settings.addedCombos?.includes(combo)) {
      settings.shortcuts[id] = [...list, combo];
    }
  }
  settings.addedCombos = [...new Set([...(saved.addedCombos ?? []), "Shift+Insert"])];
  settings.vtShow = { ...DEFAULTS.vtShow, ...(saved.vtShow ?? {}) };
  settings.columns = { ...DEFAULTS.columns, ...(saved.columns ?? {}) };
  applySettings();
}
function saveSettings(): void {
  void invoke("save_settings", { value: settings });
}
function applySettings(): void {
  document.documentElement.dataset.theme = settings.theme;
  setLang(settings.lang);
  $<HTMLSelectElement>("set-lang").value = settings.lang;
  buildThemeOptions();
  $<HTMLSelectElement>("set-theme").value = settings.theme;
  $<HTMLSelectElement>("set-preview").value = settings.previewMode;
  $<HTMLInputElement>("thumb-size").value = String(settings.thumbSize);
  $<HTMLSelectElement>("set-start").value = settings.startMode;
  $<HTMLInputElement>("set-start-path").value = settings.startPath;
  $("start-path-row").classList.toggle("hidden", settings.startMode !== "fixed");
  $<HTMLInputElement>("set-reduce").checked = settings.fitReduce;
  $<HTMLInputElement>("set-enlarge").checked = settings.fitEnlarge;
  $<HTMLInputElement>("set-loop").checked = settings.loopNav;
  $<HTMLInputElement>("set-keepzoom").checked = settings.keepZoom;
  $<HTMLInputElement>("set-hidden").checked = settings.showHidden;
  $<HTMLInputElement>("set-autorefresh").checked = settings.autoRefresh;
  $<HTMLInputElement>("set-videoloop").checked = settings.videoLoop;
  $<HTMLInputElement>("set-custom-img").value = settings.customImgExts;
  $<HTMLInputElement>("set-custom-vid").value = settings.customVidExts;
  $<HTMLSelectElement>("set-cache-limit").value = String(settings.cacheLimitGb);
  $<HTMLInputElement>("set-clear-play-exit").checked = settings.clearPlayOnExit;
  // The backend does the wiping, on the window-destroyed event.
  void invoke("set_clear_play_on_exit", { on: settings.clearPlayOnExit }).catch(() => {});
  document.documentElement.style.setProperty(
    "--scrollbar-w",
    `${Math.round((10 * settings.scrollbarPct) / 100)}px`,
  );
  $<HTMLInputElement>("set-scrollbar").value = String(settings.scrollbarPct);
  $("scrollbar-val").textContent = `${settings.scrollbarPct} %`;
  document.documentElement.style.setProperty(
    "--nav-btn",
    `${Math.round((46 * settings.navBtnPct) / 100)}px`,
  );
  applyViewButtons(); // per-mode, both overlays
  $<HTMLInputElement>("set-navbtn-size").value = String(settings.navBtnPct);
  $("navbtn-val").textContent = `${settings.navBtnPct} %`;
  $<HTMLInputElement>("set-navbtn-show").checked = settings.navBtnShow;
  $<HTMLInputElement>("set-delbtn-show").checked = settings.delBtnShow;
  syncSortUI();
  $<HTMLInputElement>("set-open-fs").checked = settings.openFullscreen;
  $<HTMLInputElement>("set-fs-reduce").checked = settings.fsFitReduce;
  $<HTMLInputElement>("set-fs-enlarge").checked = settings.fsFitEnlarge;
  $<HTMLInputElement>("set-fs-navbtn").checked = settings.fsNavBtnShow;
  $<HTMLInputElement>("set-fs-delbtn").checked = settings.fsDelBtnShow;
  $<HTMLInputElement>("set-exifrot").checked = settings.writeExifRotation;
  $<HTMLInputElement>("set-xmp").checked = settings.writeXmp;
  $<HTMLInputElement>("set-preload").value = String(settings.preloadCount);
  $("preload-val").textContent = String(settings.preloadCount);
  $<HTMLInputElement>("set-wheelseek").value = String(settings.wheelSeekSec);
  $("wheelseek-val").textContent = `${settings.wheelSeekSec} s`;
  document.documentElement.style.setProperty("--sidebar-w", `${settings.sidebarWidth}px`);
  updateViewButton();
  syncKindSeg();
  $("btn-allfiles").classList.toggle("fav-on", settings.showAllFiles);
  applyFont();
  applyVtButtons();
  buildVtOptions();
  buildShortcutEditor();
}

/** The grid/list toggle shows the icon of the view it will SWITCH TO. */
function updateViewButton(): void {
  const b = $<HTMLButtonElement>("btn-view");
  b.textContent = settings.view === "grid" ? "☰" : "▦";
  b.title = settings.view === "grid" ? t("viewToList") : t("viewToGrid");
}

// ---------- Cache tab ----------
interface CacheStats {
  thumbs_bytes: number;
  thumbs_count: number;
  previews_bytes: number;
  previews_count: number;
  play_bytes: number;
  play_count: number;
}
async function refreshCacheStats(): Promise<void> {
  const s = (await invoke("cache_stats")) as CacheStats;
  const fmt = (b: number, n: number) => `${fmtSize(b)}  (${n} ${t("filesSuffix")})`;
  $("cs-thumbs").textContent = fmt(s.thumbs_bytes, s.thumbs_count);
  $("cs-previews").textContent = fmt(s.previews_bytes, s.previews_count);
  $("cs-play").textContent = fmt(s.play_bytes, s.play_count);
  $("cs-total").textContent = fmtSize(s.thumbs_bytes + s.previews_bytes + s.play_bytes);
}
function enforceCacheLimit(): void {
  if (settings.cacheLimitGb > 0) {
    void invoke("enforce_cache_limit", {
      maxBytes: settings.cacheLimitGb * 1024 ** 3,
    }).catch(() => {});
  }
}

function applyFont(): void {
  const root = document.documentElement.style;
  root.setProperty("--ui-font-size", `${settings.fontSize}px`);
  root.setProperty("--ui-font-family", `"${settings.fontFamily}"`);
  root.setProperty("--ui-font-weight", settings.fontWeight);
  $<HTMLInputElement>("set-font-size").value = String(settings.fontSize);
  $("font-size-val").textContent = `${settings.fontSize} px`;
  $<HTMLSelectElement>("set-font-family").value = settings.fontFamily;
  $<HTMLSelectElement>("set-font-weight").value = settings.fontWeight;
}

// ---------- Sidebar: favourites + folder tree ----------
async function buildSidebar(): Promise<void> {
  await importDefaultFavorites();
  buildFavorites();
  await buildTree();
}

/** One-time migration: the old "Raccourcis" list (Pictures, Videos...) is
 *  now seeded into Favourites, where it can be reordered and removed. */
async function importDefaultFavorites(): Promise<void> {
  if (settings.favImported) return;
  settings.favImported = true;
  const homes = (await invoke("home_dirs").catch(() => [])) as [string, string][];
  const hidden = new Set(settings.hiddenShortcuts.map(normPath));
  const seeded = homes
    .filter(([, path]) => !hidden.has(normPath(path)) && !isFavorite(path))
    .map(([name, path]) => ({ name, path }));
  settings.favorites = [...seeded, ...settings.favorites];
  saveSettings();
}

// ----- Favorites -----
function isFavorite(path: string): boolean {
  return settings.favorites.some((f) => normPath(f.path) === normPath(path));
}
function toggleFavorite(path: string): void {
  if (isFavorite(path)) {
    settings.favorites = settings.favorites.filter((f) => normPath(f.path) !== normPath(path));
  } else {
    const sep = path.includes("\\") ? "\\" : "/";
    const name = path.split(sep).filter(Boolean).pop() ?? path;
    settings.favorites.push({ name, path });
  }
  saveSettings();
  buildFavorites();
  updateFavButton();
}
function buildFavorites(): void {
  const favs = $<HTMLDivElement>("favorites");
  const sep = $<HTMLDivElement>("fav-sep");
  favs.innerHTML = "";
  sep.classList.toggle("hidden", !settings.favorites.length);
  for (const f of settings.favorites) {
    const el = document.createElement("div");
    el.className = "side-item fav-item";
    el.title = f.path;
    const star = document.createElement("span");
    star.className = "fav-star";
    star.textContent = "★";
    const name = document.createElement("span");
    name.className = "fav-name";
    name.textContent = f.name;
    const rm = document.createElement("span");
    rm.className = "fav-remove";
    rm.textContent = "✕";
    rm.title = t("favRemove");
    rm.onclick = (ev) => {
      ev.stopPropagation();
      toggleFavorite(f.path);
    };
    el.append(star, name, rm);
    el.onclick = () => {
      if (justDropped) return;
      void openDir(f.path);
    };
    el.oncontextmenu = (ev) => {
      ev.stopPropagation();
      ev.preventDefault();
      ctxMenu.innerHTML = "";
      const mk = (label: string, fn: () => void) => {
        const d = document.createElement("div");
        d.className = "ctx-item";
        d.textContent = label;
        d.onclick = () => {
          hideCtx();
          fn();
        };
        ctxMenu.appendChild(d);
      };
      mk(t("ctxOpen"), () => void openDir(f.path));
      mk(t("favRemove"), () => toggleFavorite(f.path));
      mk(t("ctxReveal"), () => void invoke("reveal", { path: f.path }));
      positionCtx(ev);
    };
    favs.appendChild(el);
  }
}
function updateFavButton(): void {
  const btn = $<HTMLButtonElement>("btn-fav");
  const fav = currentPath !== "" && isFavorite(currentPath);
  btn.textContent = fav ? "★" : "☆";
  btn.classList.toggle("fav-on", fav);
  btn.title = fav ? t("favRemove") : t("favAdd");
}

// ----- Explorer-like folder tree (lazy, expands as you navigate) -----
interface TreeNode {
  path: string;
  row: HTMLDivElement;
  childrenEl: HTMLDivElement;
  twist: HTMLSpanElement;
  expanded: boolean;
  loaded: boolean;
}
const treeNodes = new Map<string, TreeNode>();
let activeTreePath = "";

function normPath(p: string): string {
  return p.replace(/[\\/]+$/, "").toLowerCase();
}

async function buildTree(): Promise<void> {
  const tree = $<HTMLDivElement>("tree");
  tree.innerHTML = "";
  treeNodes.clear();
  activeTreePath = "";
  const roots = (await invoke("list_roots")) as string[];
  for (const root of roots) {
    tree.appendChild(makeTreeNode(root, root));
  }
}

// Which pane owns the arrow keys. Clicking a folder in the tree gives them to
// the tree (up/down walk the folders); clicking the file pane gives them back.
let treeFocus = false;

/** Folder rows currently on screen, in display order (collapsed ones drop out
 *  because their hidden parent gives them no offsetParent). */
function visibleTreeRows(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>("#tree .tree-row")].filter(
    (r) => r.offsetParent !== null,
  );
}

/** Move the tree selection by `delta` rows and open that folder. */
function treeMove(delta: number): void {
  const rows = visibleTreeRows();
  if (!rows.length) return;
  const cur = rows.findIndex((r) => normPath(r.title) === normPath(activeTreePath));
  const idx = cur < 0 ? 0 : Math.min(rows.length - 1, Math.max(0, cur + delta));
  const next = rows[idx];
  if (!next || (cur >= 0 && idx === cur)) return;
  next.scrollIntoView({ block: "nearest" });
  void openDir(next.title);
}

function makeTreeNode(path: string, name: string): HTMLDivElement {
  const wrap = document.createElement("div");
  wrap.className = "tree-node";

  const row = document.createElement("div");
  row.className = "tree-row";
  row.title = path;
  const twist = document.createElement("span");
  twist.className = "twist";
  twist.textContent = "▸";
  const icon = document.createElement("span");
  icon.className = "tree-icon";
  icon.textContent = "📁";
  const label = document.createElement("span");
  label.className = "tree-label";
  label.textContent = name;
  row.append(twist, icon, label);

  const childrenEl = document.createElement("div");
  childrenEl.className = "tree-children hidden";
  wrap.append(row, childrenEl);

  const node: TreeNode = { path, row, childrenEl, twist, expanded: false, loaded: false };
  treeNodes.set(normPath(path), node);

  twist.onclick = (ev) => {
    ev.stopPropagation();
    void toggleNode(node);
  };
  // Drag a folder out of the tree to move it somewhere else. The twist
  // is excluded so expanding a node never starts a drag.
  row.onmousedown = (ev) => {
    if ((ev.target as HTMLElement).classList.contains("twist")) return;
    beginDragPaths(ev, [path]);
  };
  row.onclick = () => {
    if (justDropped) return; // a drop just landed here, don't navigate
    treeFocus = true; // the arrows now walk the folders
    void openDir(path);
  };
  row.oncontextmenu = (ev) => {
    ev.stopPropagation();
    showTreeCtxMenu(ev, path);
  };
  return wrap;
}

async function loadChildren(node: TreeNode): Promise<void> {
  if (node.loaded) return;
  node.loaded = true;
  try {
    const list = (await invoke("list_dir", { path: node.path })) as Entry[];
    const dirs = list.filter((e) => e.kind === "dir" && (settings.showHidden || !e.hidden));
    if (!dirs.length) node.twist.classList.add("leaf");
    for (const d of dirs) node.childrenEl.appendChild(makeTreeNode(d.path, d.name));
  } catch {
    node.twist.classList.add("leaf");
  }
}

async function toggleNode(node: TreeNode, expand?: boolean): Promise<void> {
  const target = expand ?? !node.expanded;
  if (target && !node.loaded) await loadChildren(node);
  node.expanded = target;
  node.childrenEl.classList.toggle("hidden", !target);
  node.twist.textContent = target ? "▾" : "▸";
}

/** Re-read one folder's children in the tree, keeping everything else
 *  expanded — used after a folder is created, moved, renamed or deleted. */
async function refreshTreeNode(path: string): Promise<void> {
  const key = normPath(path);
  const node = treeNodes.get(key);
  if (!node) return;
  // Forget the cached descendants, but keep this node's own state.
  const sep = path.includes("\\") ? "\\" : "/";
  for (const k of [...treeNodes.keys()]) {
    if (k !== key && k.startsWith(key + sep)) treeNodes.delete(k);
  }
  node.childrenEl.innerHTML = "";
  node.loaded = false;
  node.twist.classList.remove("leaf");
  if (node.expanded) {
    await loadChildren(node);
  }
}

/** Unfold the tree along `path` (like Explorer) and highlight the folder. */
async function revealInTree(path: string): Promise<void> {
  const sep = path.includes("\\") ? "\\" : "/";
  const parts = path.split(sep).filter(Boolean);
  const chain: string[] = [];
  if (sep === "/") {
    chain.push("/");
    let acc = "";
    for (const p of parts) {
      acc += "/" + p;
      chain.push(acc);
    }
  } else {
    let acc = parts[0] + "\\";
    chain.push(acc);
    for (const p of parts.slice(1)) {
      acc = acc.replace(/\\$/, "") + "\\" + p;
      chain.push(acc);
    }
  }
  for (const p of chain) {
    const node = treeNodes.get(normPath(p));
    if (!node) break;
    await toggleNode(node, true);
  }
  treeNodes.get(normPath(activeTreePath))?.row.classList.remove("active");
  activeTreePath = path;
  const act = treeNodes.get(normPath(path));
  if (act) {
    act.row.classList.add("active");
    act.row.scrollIntoView({ block: "nearest" });
  }
}

// ---------- Address bar ----------
function openAddressBar(): void {
  const input = $<HTMLInputElement>("path-input");
  $("breadcrumb").classList.add("hidden");
  input.classList.remove("hidden");
  input.value = currentPath;
  input.focus();
  input.select();
}
function closeAddressBar(): void {
  $("path-input").classList.add("hidden");
  $("breadcrumb").classList.remove("hidden");
}
$("breadcrumb").addEventListener("click", (ev) => {
  // Clicking a crumb navigates; clicking the empty part edits the path.
  if (ev.target === $("breadcrumb")) openAddressBar();
});
$<HTMLInputElement>("path-input").onkeydown = (ev) => {
  ev.stopPropagation();
  if (ev.key === "Enter") {
    const value = $<HTMLInputElement>("path-input").value.trim();
    closeAddressBar();
    if (value) {
      void openDir(value).then((ok) => {
        if (!ok) toast(t("pathInvalid"));
      });
    }
  } else if (ev.key === "Escape") {
    closeAddressBar();
  }
};
$<HTMLInputElement>("path-input").onblur = closeAddressBar;

// ---------- Breadcrumb ----------
function buildBreadcrumb(): void {
  const bc = $<HTMLDivElement>("breadcrumb");
  bc.innerHTML = "";
  const sep = currentPath.includes("\\") ? "\\" : "/";
  const parts = currentPath.split(sep).filter(Boolean);
  let acc = "";
  parts.forEach((part, i) => {
    acc += part + sep;
    const target = acc;
    const el = document.createElement("span");
    el.className = "crumb";
    el.textContent = i === 0 ? part + sep : part;
    el.onclick = () => openDir(target);
    bc.appendChild(el);
  });
}

// ---------- Folder navigation ----------
let navSeq = 0;
// Browser-like history of visited folders.
let navHistory: string[] = [];
let navPos = -1;
let navSuppress = false; // true while navigating via back/forward

function updateNavButtons(): void {
  $<HTMLButtonElement>("btn-back").disabled = navPos <= 0;
  $<HTMLButtonElement>("btn-fwd").disabled = navPos >= navHistory.length - 1;
}
function navGo(delta: number): void {
  const target = navHistory[navPos + delta];
  if (!target) return;
  navPos += delta;
  navSuppress = true;
  void openDir(target).finally(() => {
    navSuppress = false;
    updateNavButtons();
  });
}

async function openDir(path: string): Promise<boolean> {
  const seq = ++navSeq;
  try {
    const list = (await invoke("list_dir", { path })) as Entry[];
    if (seq !== navSeq) return false; // a newer navigation superseded this one
    if (!navSuppress && normPath(navHistory[navPos] ?? "") !== normPath(path)) {
      navHistory = navHistory.slice(0, navPos + 1);
      navHistory.push(path);
      navPos = navHistory.length - 1;
    }
    updateNavButtons();
    currentPath = path;
    entries = remapCustomExts(list);
    settings.lastPath = path;
    saveSettings();
    // NOTE: thumbs/previews maps are kept across navigation — revisiting a
    // folder repaints instantly from memory; backend events refresh stale ones.
    selectedIndex = -1;
    selected.clear();
    selAnchor = -1;
    pregenQueue = [];
    filters.recursive = false;
    void loadMeta(path);
    buildBreadcrumb();
    updateFavButton();
    refresh();
    void revealInTree(path);
    if (settings.autoRefresh) void invoke("watch_dir", { path }).catch(() => {});
    refreshDiskSpace();
    return true;
  } catch (e) {
    statusbar.textContent = String(e);
    return false;
  }
}
/// Custom extensions: reclassify "other" files the user wants as media.
function remapCustomExts(list: Entry[]): Entry[] {
  const imgs = settings.customImgExts.toLowerCase().split(",").map((s) => s.trim()).filter(Boolean);
  const vids = settings.customVidExts.toLowerCase().split(",").map((s) => s.trim()).filter(Boolean);
  if (!imgs.length && !vids.length) return list;
  for (const e of list) {
    if (e.kind === "other" && imgs.includes(e.ext)) e.kind = "image";
    else if (e.kind === "other" && vids.includes(e.ext)) e.kind = "video";
  }
  return list;
}

interface MetaRowDto {
  path: string; rating: number; color: string; flag: string; tags: string; rot: number;
  pos: number;
}
function mergeMetaRows(rows: MetaRowDto[]): void {
  for (const r of rows) {
    metaMap.set(r.path, {
      rating: r.rating, color: r.color, flag: r.flag, tags: r.tags, rot: r.rot ?? 0,
      pos: r.pos ?? 0,
    });
  }
}

async function loadMeta(path: string): Promise<void> {
  metaMap.clear();
  mergeMetaRows((await invoke("dir_meta", { parent: path }).catch(() => [])) as MetaRowDto[]);
  repaint();
}

/** In a recursive listing the files come from many folders: pull in the
 *  metadata of each one so ratings, labels and keywords still show. */
async function loadMetaForParents(list: Entry[]): Promise<void> {
  const parents = new Set(list.map((e) => parentOf(e.path)).filter(Boolean));
  for (const parent of parents) {
    mergeMetaRows((await invoke("dir_meta", { parent }).catch(() => [])) as MetaRowDto[]);
  }
  repaint();
}

function metaOf(path: string): Meta {
  return metaMap.get(path) ?? { rating: 0, color: "", flag: "", tags: "", rot: 0, pos: 0 };
}
// Adobe's colour label vocabulary, so Lightroom & Bridge understand ours.
const LABEL_NAMES: Record<string, string> = {
  "#e05555": "Red",
  "#e8a33d": "Orange",
  "#e5d947": "Yellow",
  "#58c26a": "Green",
  "#4f9cf0": "Blue",
  "#b06fd8": "Purple",
};
const LABEL_HEX: Record<string, string> = Object.fromEntries(
  Object.entries(LABEL_NAMES).map(([hex, name]) => [name.toLowerCase(), hex]),
);

/** Mirror a file's rating/label/keywords into its .xmp sidecar. The media
 *  file is never opened, so its bytes and dates stay exactly as they were. */
function syncXmp(path: string): void {
  if (!settings.writeXmp) return;
  const m = metaOf(path);
  void invoke("write_xmp", {
    path,
    rating: m.flag === "reject" ? -1 : m.rating,
    label: LABEL_NAMES[m.color] ?? "",
    tags: m.tags,
  }).catch(() => {});
}

/** Pull ratings/labels/keywords written by other software back in. */
async function importXmp(): Promise<void> {
  const paths = visible.filter((e) => e.kind !== "dir").map((e) => e.path);
  if (!paths.length) return;
  const rows = (await invoke("read_xmp", { paths }).catch(() => [])) as [
    string,
    { rating: number; label: string; tags: string },
  ][];
  for (const [path, x] of rows) {
    const cur = metaOf(path);
    const next: Meta = {
      ...cur,
      rating: x.rating > 0 ? x.rating : 0,
      flag: x.rating < 0 ? "reject" : cur.flag,
      color: LABEL_HEX[x.label.toLowerCase()] ?? cur.color,
      tags: x.tags || cur.tags,
    };
    metaMap.set(path, next);
    void invoke("set_meta", {
      path,
      parent: currentPath,
      rating: next.rating,
      color: next.color,
      flag: next.flag,
      tags: next.tags,
      rot: null,
    }).catch(() => {});
  }
  toast(`${t("xmpImported")} ${rows.length}`);
  repaint();
}

/** CSS rotation of an entry (display only, the file is never touched). */
function rotOf(path: string): number {
  return ((metaOf(path).rot % 360) + 360) % 360;
}

/// Apply a metadata change to every selected entry.
function setMetaSel(patch: Partial<Meta>): void {
  for (const e of selEntries()) {
    if (e.kind === "dir") continue;
    const cur = { ...metaOf(e.path), ...patch };
    metaMap.set(e.path, cur);
    void invoke("set_meta", {
      path: e.path,
      parent: currentPath,
      rating: patch.rating ?? null,
      color: patch.color ?? null,
      flag: patch.flag ?? null,
      tags: patch.tags ?? null,
      rot: patch.rot ?? null,
    }).catch(() => {});
    syncXmp(e.path);
  }
  repaint();
}

/// Silent re-list (auto-refresh) keeping selection and scroll.
async function reloadDir(): Promise<void> {
  if (!currentPath || filters.recursive) return;
  try {
    const list = (await invoke("list_dir", { path: currentPath })) as Entry[];
    const focusPath = visible[selectedIndex]?.path;
    const selectedPaths = new Set(selEntries().map((e) => e.path));
    entries = remapCustomExts(list);
    refresh();
    selected.clear();
    visible.forEach((e, i) => {
      if (selectedPaths.has(e.path)) selected.add(i);
      if (e.path === focusPath) selectedIndex = i;
    });
    repaint();
  } catch {
    /* folder gone: ignore */
  }
}

function goUp(): void {
  const sep = currentPath.includes("\\") ? "\\" : "/";
  const parts = currentPath.split(sep).filter(Boolean);
  if (parts.length <= 1) return;
  parts.pop();
  let up = parts.join(sep);
  if (sep === "\\" && !up.includes("\\")) up += "\\"; // drive root "C:\"
  if (sep === "/") up = "/" + up;
  void openDir(up);
}

// ---------- Filter + sort + thumbnail batch ----------
/** Re-read the current listing while keeping the recursive ("show everything")
 *  view alive: `openDir` always falls back to the plain single-folder listing. */
async function refreshListing(): Promise<void> {
  if (filters.recursive) {
    await startRecursiveSearch();
    return;
  }
  await openDir(currentPath);
}

/** Drop paths from the listing in place. No folder re-read, so the recursive
 *  view, the scroll position and the keyboard focus all survive a delete. */
function dropFromListing(paths: string[]): void {
  const gone = new Set(paths.map(normPath));
  const firstIdx = visible.findIndex((e) => gone.has(normPath(e.path)));
  entries = entries.filter((e) => !gone.has(normPath(e.path)));
  selected.clear();
  selAnchor = -1;
  refresh();
  if (firstIdx >= 0 && visible.length) setFocus(Math.min(firstIdx, visible.length - 1));
  else selectedIndex = -1;
  refreshDiskSpace();
}

function refresh(): void {
  const text = $<HTMLInputElement>("filter-text").value.toLowerCase();
  const kindF = settings.filterKind;
  const [sortKey, sortDir] = $<HTMLSelectElement>("sort").value.split("-");

  const f = filters;
  visible = entries.filter((e) => {
    if (e.hidden && !settings.showHidden) return false;
    if (text && !e.name.toLowerCase().includes(text)) return false;
    if (kindF === "image" && e.kind !== "image" && e.kind !== "raw" && e.kind !== "dir") return false;
    if (kindF === "video" && e.kind !== "video" && e.kind !== "dir") return false;
    if (e.kind !== "dir") {
      if (f.minSizeMb > 0 && e.size < f.minSizeMb * 1024 ** 2) return false;
      if (f.maxSizeMb > 0 && e.size > f.maxSizeMb * 1024 ** 2) return false;
      if (f.dateFrom && e.mtime < Date.parse(f.dateFrom)) return false;
      if (f.dateTo && e.mtime > Date.parse(f.dateTo) + 86_400_000) return false;
      const m = metaOf(e.path);
      if (f.minRating > 0 && m.rating < f.minRating) return false;
      if (f.color && m.color !== f.color) return false;
      if (f.flag === "pick" && m.flag !== "pick") return false;
      if (f.flag === "reject" && m.flag !== "reject") return false;
      if (f.flag === "none" && m.flag !== "") return false;
      if (f.tag && !m.tags.toLowerCase().split(",").map((s) => s.trim()).includes(f.tag.toLowerCase()))
        return false;
    }
    return e.kind !== "other" || (settings.showAllFiles && kindF === "all");
  });

  const dirMul = sortDir === "asc" ? 1 : -1;
  visible.sort((a, b) => {
    // Folders always float to the top.
    if ((a.kind === "dir") !== (b.kind === "dir")) return a.kind === "dir" ? -1 : 1;
    if (sortKey === "taken") {
      // Missing EXIF date falls back to the file date, so nothing collapses.
      const ta = takenMap.get(a.path) ?? a.mtime;
      const tb = takenMap.get(b.path) ?? b.mtime;
      return (ta - tb) * dirMul;
    }
    if (sortKey === "date") return (a.mtime - b.mtime) * dirMul;
    if (sortKey === "size") return (a.size - b.size) * dirMul;
    if (sortKey === "rating") {
      const ma = metaOf(a.path);
      const mb = metaOf(b.path);
      const score = (m: Meta) => (m.flag === "reject" ? -1 : m.rating);
      return (score(ma) - score(mb)) * dirMul;
    }
    if (sortKey === "color" || sortKey === "tags") {
      const va = sortKey === "color" ? metaOf(a.path).color : metaOf(a.path).tags;
      const vb = sortKey === "color" ? metaOf(b.path).color : metaOf(b.path).tags;
      // Untagged items sink to the bottom whichever way we sort.
      if (!va !== !vb) return va ? -1 : 1;
      return va.localeCompare(vb) * dirMul;
    }
    if (sortKey === "res" || sortKey === "ratio") {
      const da = dimsMap.get(a.path) ?? [0, 0];
      const db = dimsMap.get(b.path) ?? [0, 0];
      const val = (d: [number, number]) =>
        sortKey === "res" ? d[0] * d[1] : d[1] ? d[0] / d[1] : 0;
      return (val(da) - val(db)) * dirMul;
    }
    if (sortKey === "type") {
      return (a.ext.localeCompare(b.ext) ||
        a.name.localeCompare(b.name, undefined, { numeric: true })) * dirMul;
    }
    return a.name.localeCompare(b.name, undefined, { numeric: true }) * dirMul;
  });

  // The DOM cell pool is only valid for a given `visible` array: drop it so
  // no cell keeps the label/handlers of an entry from the previous listing.
  for (const [, el] of cellPool) el.remove();
  cellPool.clear();

  statusbar.textContent = `${visible.length} ${t("items")}`;
  if (settings.view === "grid") {
    listTable.classList.add("hidden");
    gridSpacer.classList.remove("hidden");
    layoutGrid();
  } else {
    gridSpacer.classList.add("hidden");
    listTable.classList.remove("hidden");
    layoutList();
  }
  requestThumbs();
  updateJumpButtons();
  if (settings.sortOrder.startsWith("taken")) void ensureCaptureDates();
  if (needsDims()) void ensureDims();
}

/** Read the EXIF capture dates of the current folder (once per file, the
 *  backend memoises them) and re-sort when they arrive. */
let takenPending = false;
async function ensureCaptureDates(): Promise<void> {
  if (takenPending) return;
  const missing = visible
    .filter((e) => e.kind !== "dir" && !takenMap.has(e.path))
    .map((e) => e.path);
  if (!missing.length) return;
  takenPending = true;
  try {
    const rows = (await invoke("capture_dates", {
      paths: missing,
      parent: currentPath,
    })) as [string, number][];
    for (const [path, ms] of rows) takenMap.set(path, ms);
    takenPending = false;
    refresh();
  } catch {
    takenPending = false;
  }
}

/** Pixel sizes for the resolution / megapixel / ratio columns. Only fetched
 *  when one of them is on screen: reading a header costs an open() per file. */
let dimsPending = false;
async function ensureDims(): Promise<void> {
  if (dimsPending) return;
  const missing = visible
    .filter((e) => (e.kind === "image" || e.kind === "raw") && !dimsMap.has(e.path))
    .map((e) => e.path);
  if (!missing.length) return;
  dimsPending = true;
  try {
    const rows = (await invoke("image_sizes", { paths: missing })) as [string, number, number][];
    for (const [path, w, h] of rows) dimsMap.set(path, [w, h]);
    // Files that gave nothing are remembered as unknown, so we stop asking.
    for (const p of missing) if (!dimsMap.has(p)) dimsMap.set(p, [0, 0]);
  } finally {
    dimsPending = false;
  }
  repaint();
}

/** "16:9" when it lands on a familiar ratio, "1,85" otherwise. */
const COMMON_RATIOS: [number, number][] = [
  [1, 1], [5, 4], [4, 3], [3, 2], [16, 10], [16, 9], [1.85, 1], [2.39, 1], [2, 1], [21, 9],
];
function fmtRatio(w: number, h: number): string {
  if (!w || !h) return "";
  const portrait = h > w;
  const r = portrait ? h / w : w / h;
  for (const [a, b] of COMMON_RATIOS) {
    if (Math.abs(r - a / b) / (a / b) < 0.012) {
      const lbl = Number.isInteger(a) ? `${a}:${b}` : `${a}:${b}`;
      return portrait ? lbl.split(":").reverse().join(":") : lbl;
    }
  }
  return r.toFixed(2).replace(".", ",") + (portrait ? " ↕" : " ↔");
}

function requestThumbs(): void {
  const items = visible
    .filter((e) => e.kind !== "dir" && e.kind !== "other")
    .map((e) => ({ path: e.path, kind: e.kind, ext: e.ext }));
  if (!items.length) return;
  void invoke("request_thumbs", { items, tsize: thumbTargetPx() }).then((g) => {
    currentGen = g as number;
  });
  if (settings.previewMode === "pregen") {
    pregenQueue = visible.filter((e) => e.kind === "video").map((e) => e.path);
    pumpPregen();
  }
}
function thumbTargetPx(): number {
  // Account for HiDPI so thumbs stay sharp.
  return Math.min(512, Math.round(settings.thumbSize * (window.devicePixelRatio || 1)));
}

// Background generation of hover clips, 2 at a time.
function pumpPregen(): void {
  while (pregenRunning < 2 && pregenQueue.length) {
    const path = pregenQueue.shift()!;
    if (previews.has(path)) continue;
    pregenRunning++;
    invoke("video_preview", { path })
      .then((p) => previews.set(path, p as string))
      .catch(() => {})
      .finally(() => {
        pregenRunning--;
        pumpPregen();
      });
  }
}

// ---------- Virtualised grid ----------
const GAP = 10;
let cellPool = new Map<number, HTMLDivElement>(); // visible index -> cell

// Current grid geometry, kept for rubber-band hit-testing.
let gridCols = 1;
let gridCellW = 220;
let gridRowH = 246;

function layoutGrid(): void {
  const cw = gridWrap.clientWidth - GAP;
  const base = settings.thumbSize;
  const cols = Math.max(1, Math.floor(cw / (base + GAP)));
  // Justify: stretch cells so the row fills the full width instead of
  // leaving a dead column of empty space on the right.
  const cell = Math.floor((gridWrap.clientWidth - GAP * (cols + 1)) / cols);
  gridCols = cols;
  gridCellW = cell;
  gridRowH = cell + 26 + GAP;
  const rows = Math.ceil(visible.length / cols);
  const rowH = cell + 26 + GAP; // +26 for the label strip
  gridSpacer.style.height = `${rows * rowH + GAP}px`;

  const scrollTop = gridWrap.scrollTop;
  const viewH = gridWrap.clientHeight;
  const firstRow = Math.max(0, Math.floor(scrollTop / rowH) - 2);
  const lastRow = Math.min(rows, Math.ceil((scrollTop + viewH) / rowH) + 2);
  const first = firstRow * cols;
  const last = Math.min(visible.length, lastRow * cols);

  // Drop cells that left the window.
  for (const [idx, el] of cellPool) {
    if (idx < first || idx >= last) {
      el.remove();
      cellPool.delete(idx);
    }
  }
  // Create/refresh visible cells.
  for (let i = first; i < last; i++) {
    let el = cellPool.get(i);
    if (!el) {
      el = makeCell(visible[i], i);
      cellPool.set(i, el);
      grid.appendChild(el);
    }
    const r = Math.floor(i / cols);
    const c = i % cols;
    el.style.left = `${GAP + c * (cell + GAP)}px`;
    el.style.top = `${GAP + r * rowH}px`;
    el.style.width = `${cell}px`;
    el.style.height = `${cell + 26}px`;
    el.classList.toggle("selected", selected.has(i) || i === selectedIndex);
    el.classList.toggle("cut", isCut(visible[i].path));
    hydrateCell(el, visible[i]);
  }
}

function makeCell(e: Entry, idx: number): HTMLDivElement {
  const el = document.createElement("div");
  el.className = "cell" + (e.kind === "dir" ? " folder" : "");
  el.dataset.path = e.path;
  el.title = e.path; // the flat recursive view mixes folders

  const box = document.createElement("div");
  box.className = "box";
  const lbl = document.createElement("div");
  lbl.className = "lbl";
  lbl.textContent = e.name;
  el.append(box, lbl);

  el.onmousedown = (ev) => {
    treeFocus = false;
    beginDragCandidate(ev, e, idx);
  };
  // Belt and braces: kill the web view's own image drag on the whole cell.
  el.ondragstart = (ev) => ev.preventDefault();
  el.onclick = (ev) => {
    if (dragging) return; // the mouse-up ended a drag, not a selection
    setFocus(idx, { ctrl: ev.ctrlKey, shift: ev.shiftKey });
    layoutGrid();
  };
  el.ondblclick = () => {
    if (justDropped) return;
    activate(e);
  };
  el.oncontextmenu = (ev) => {
    ev.stopPropagation();
    if (!selected.has(idx)) setFocus(idx);
    layoutGrid();
    showCtxMenu(ev, e);
  };
  if (e.kind === "video" && settings.previewMode !== "off") {
    el.onmouseenter = () => hoverStart(el, e);
    el.onmouseleave = () => hoverStop(el, e);
  }
  return el;
}

// Windows shell icons for non-media files, cached per extension.
const sysIcons = new Map<string, string | null>();
function sysIconKey(e: Entry): string {
  return ["exe", "lnk", "ico"].includes(e.ext) ? e.path : e.ext || "_none";
}

/** Fill a cell with its thumbnail (or placeholder / folder glyph / badge). */
function hydrateCell(el: HTMLDivElement, e: Entry): void {
  const box = el.querySelector<HTMLDivElement>(".box")!;
  if (e.kind === "dir") {
    if (!box.textContent) box.textContent = "📁";
    return;
  }
  if (e.kind === "other") {
    const key = sysIconKey(e);
    const icon = sysIcons.get(key);
    if (icon) {
      const img = box.querySelector<HTMLImageElement>("img.sys-icon");
      if (img?.dataset.src !== icon) {
        box.innerHTML = "";
        const im = document.createElement("img");
        im.src = convertFileSrc(icon);
        im.dataset.src = icon;
        im.className = "sys-icon";
        box.appendChild(im);
      }
    } else {
      if (!box.textContent) box.textContent = "📄";
      if (!sysIcons.has(key)) {
        sysIcons.set(key, null); // in flight
        void invoke("file_icon", { path: e.path })
          .then((p) => {
            if (p) {
              sysIcons.set(key, p as string);
              scheduleGridPaint();
            }
          })
          .catch(() => {});
      }
    }
    return;
  }
  if (box.querySelector("video")) return; // hover preview playing, leave it
  const thumb = thumbs.get(e.path);
  const img = box.querySelector("img");
  if (thumb) {
    const url = convertFileSrc(thumb);
    if (img?.dataset.src === thumb) return;
    box.innerHTML = "";
    const im = document.createElement("img");
    im.src = url;
    im.dataset.src = thumb;
    im.loading = "lazy";
    im.decoding = "async";
    box.appendChild(im);
    // The .box is square, so a plain CSS rotation stays inside it.
    im.style.transform = `rotate(${rotOf(e.path)}deg)`;
    if (e.kind === "video" || e.kind === "raw") {
      const b = document.createElement("div");
      b.className = "badge";
      b.textContent = e.kind === "video" ? "▶ " + e.ext.toUpperCase() : e.ext.toUpperCase();
      box.appendChild(b);
    }
  } else if (!box.querySelector(".ph")) {
    box.innerHTML = '<div class="ph"></div>';
  }
  const mounted = box.querySelector<HTMLImageElement>("img");
  if (mounted) mounted.style.transform = `rotate(${rotOf(e.path)}deg)`;
  hydrateMeta(el, e);
}

/// Rating stars / colour dot / pick-reject badge in the cell corner.
function hydrateMeta(el: HTMLDivElement, e: Entry): void {
  const box = el.querySelector<HTMLDivElement>(".box")!;
  let mb = box.querySelector<HTMLDivElement>(".meta-badge");
  const m = metaOf(e.path);
  const txt =
    (m.flag === "pick" ? "✓ " : m.flag === "reject" ? "✗ " : "") +
    (m.rating > 0 ? "★".repeat(m.rating) : "");
  if (!txt && !m.color) {
    mb?.remove();
    return;
  }
  if (!mb) {
    mb = document.createElement("div");
    mb.className = "meta-badge";
    box.appendChild(mb);
  }
  mb.textContent = txt;
  mb.classList.toggle("flag-pick", m.flag === "pick");
  mb.classList.toggle("flag-reject", m.flag === "reject");
  mb.style.borderLeft = m.color ? `4px solid ${m.color}` : "none";
}

// Hover previews: swap the still for the looping low-res clip.
function hoverStart(el: HTMLDivElement, e: Entry): void {
  const play = (src: string) => {
    if (!el.matches(":hover")) return; // pointer already left
    const box = el.querySelector<HTMLDivElement>(".box")!;
    const v = document.createElement("video");
    v.src = convertFileSrc(src);
    v.muted = true;
    v.loop = true;
    v.autoplay = true;
    v.playsInline = true;
    box.innerHTML = "";
    box.appendChild(v);
  };
  const cached = previews.get(e.path);
  if (cached) {
    play(cached);
  } else if (settings.previewMode === "hover") {
    invoke("video_preview", { path: e.path })
      .then((p) => {
        previews.set(e.path, p as string);
        play(p as string);
      })
      .catch(() => {});
  }
}
function hoverStop(el: HTMLDivElement, e: Entry): void {
  const box = el.querySelector<HTMLDivElement>(".box")!;
  if (box.querySelector("video")) {
    box.innerHTML = "";
    hydrateCell(el, e);
  }
}

// ---------- List view ----------
function layoutList(): void {
  listBody.innerHTML = "";
  visible.forEach((e, i) => {
    const tr = document.createElement("tr");
    tr.title = e.path;
    // A folder row is a drop target, exactly like a folder cell in the grid.
    tr.dataset.path = e.path;
    if (e.kind === "dir") tr.dataset.dir = "1";
    tr.classList.toggle("selected", selected.has(i) || i === selectedIndex);
    tr.classList.toggle("cut", isCut(e.path));
    const icon = e.kind === "dir" ? "📁 " : e.kind === "video" ? "🎬 " : e.kind === "other" ? "📄 " : "🖼 ";
    for (const c of shownColumns()) {
      const td = document.createElement("td");
      if (c.id === "name") {
        td.textContent = icon + e.name;
      } else if (c.id === "color") {
        td.textContent = columnValue(c, e);
        td.style.color = metaOf(e.path).color || "inherit";
        td.style.textAlign = "center";
      } else {
        td.textContent = columnValue(c, e);
      }
      tr.appendChild(td);
    }
    tr.onmousedown = (ev) => {
      treeFocus = false;
      beginDragCandidate(ev, e, i);
    };
    tr.ondragstart = (ev) => ev.preventDefault();
    tr.onclick = (ev) => {
      if (dragging) return;
      setFocus(i, { ctrl: ev.ctrlKey, shift: ev.shiftKey });
      layoutList();
    };
    tr.ondblclick = () => activate(e);
    tr.oncontextmenu = (ev) => {
      ev.stopPropagation();
      if (!selected.has(i)) setFocus(i);
      layoutList();
      showCtxMenu(ev, e);
    };
    listBody.appendChild(tr);
  });
}

// ---------- Activation (double-click / Enter) ----------
function activate(e: Entry): void {
  if (e.kind === "dir") {
    void openDir(e.path);
  } else if (e.kind === "other") {
    // Non-media file: open with its default Windows application.
    void invoke("open_with", { path: e.path, exe: "explorer" }).catch(() => {});
  } else if (e.kind === "video") {
    openPlayer(mediaList().findIndex((m) => m.path === e.path));
    if (settings.openFullscreen) void setViewerFullscreen(true);
  } else {
    openViewer(mediaList().findIndex((m) => m.path === e.path));
    if (settings.openFullscreen) void setViewerFullscreen(true);
  }
}

// ---------- Clipboard (shared with Explorer and the desktop) ----------
// Cut/copy go through the real Windows clipboard (CF_HDROP + Preferred
// DropEffect), so Ctrl+C here pastes in Explorer and the other way round.
// `clipboard` is only a mirror, used to enable/disable the Paste entries.
let clipboard: { paths: string[]; cut: boolean } | null = null;
/** Paths pending a move: shown greyed out, the way Explorer does. */
let cutPaths = new Set<string>();

function isCut(path: string): boolean {
  return cutPaths.size > 0 && cutPaths.has(normPath(path));
}
/** Remember (or forget) which files are marked for a move, then repaint. */
function markCut(paths: string[]): void {
  const next = new Set(paths.map(normPath));
  const changed =
    next.size !== cutPaths.size || [...next].some((p) => !cutPaths.has(p));
  cutPaths = next;
  if (changed) repaint();
}

/** Cut/copy by path, for callers with no Entry at hand (the folder tree). */
function clipSetPaths(paths: string[], cut: boolean): void {
  if (!paths.length) return;
  clipboard = { paths, cut };
  markCut(cut ? paths : []);
  void invoke("clip_set_files", { paths, cut }).catch((err) => toast(String(err)));
  statusbar.textContent = `${cut ? "\u2702" : "\u29c9"} ${
    paths.length === 1 ? baseName(paths[0]) : paths.length + " " + t("filesSuffix")
  }`;
}

function clipSet(list: Entry[], cut: boolean): void {
  if (!list.length) return;
  clipboard = { paths: list.map((e) => e.path), cut };
  markCut(cut ? clipboard.paths : []);
  void invoke("clip_set_files", { paths: clipboard.paths, cut }).catch((err) =>
    toast(String(err)),
  );
  statusbar.textContent = `${cut ? "✂" : "⧉"} ${list.length === 1 ? list[0].name : list.length + " " + t("filesSuffix")}`;
}

/** Pull the current Windows clipboard into the mirror. */
async function readClipboard(): Promise<{ paths: string[]; cut: boolean } | null> {
  const r = (await invoke("clip_get_files").catch(() => null)) as
    | { paths: string[]; cut: boolean }
    | null;
  clipboard = r && r.paths.length ? r : null;
  markCut(clipboard?.cut ? clipboard.paths : []);
  return clipboard;
}
// Explorer may have copied something while we were in the background.
window.addEventListener("focus", () => void readClipboard());

async function clipPaste(): Promise<void> {
  if (!currentPath) return;
  const clip = (await readClipboard()) ?? clipboard;
  if (!clip) {
    toast(t("clipEmpty"));
    return;
  }
  const { paths, cut } = clip;
  if (cut) clipboard = null;
  markCut([]);
  await pasteInto(paths, currentPath, cut);
}

// ---------- Context menu (Windows 11 style) ----------
const ctxMenu = $<HTMLDivElement>("ctx-menu");
let ctxShownAt = 0;

function hideCtx(): void {
  ctxMenu.classList.add("hidden");
}

function positionCtx(ev: MouseEvent): void {
  ctxMenu.classList.remove("hidden");
  ctxShownAt = Date.now();
  const x = Math.min(ev.clientX, window.innerWidth - ctxMenu.offsetWidth - 8);
  const y = Math.min(ev.clientY, window.innerHeight - ctxMenu.offsetHeight - 8);
  ctxMenu.style.left = `${Math.max(0, x)}px`;
  ctxMenu.style.top = `${Math.max(0, y)}px`;
}

function showCtxMenu(ev: MouseEvent, e: Entry | null): void {
  ev.preventDefault();
  buildCtx(e);
  positionCtx(ev);
}

// ---------- Properties ----------
let propsPath = "";
async function showProps(path: string): Promise<void> {
  propsPath = path;
  const p = (await invoke("file_props", { path }).catch((e) => {
    alert(String(e));
    return null;
  })) as {
    path: string; size: number; is_dir: boolean; files: number; folders: number;
    created: number; modified: number; readonly: boolean; hidden: boolean;
  } | null;
  if (!p) return;
  const rows: [string, string][] = [
    [t("name"), baseName(p.path)],
    [t("propsLocation"), parentOf(p.path)],
    [t("type"), p.is_dir ? t("propsFolder") : (p.path.split(".").pop() ?? "").toUpperCase()],
    [t("size"), `${fmtSize(p.size)} (${p.size.toLocaleString()} o)`],
  ];
  if (p.is_dir) rows.push([t("propsContains"), `${p.files} ${t("filesSuffix")}, ${p.folders} ${t("folders")}`]);
  rows.push([t("propsCreated"), p.created ? fmtDate(p.created) : "—"]);
  rows.push([t("modified"), p.modified ? fmtDate(p.modified) : "—"]);
  if (!p.is_dir) {
    // Straight from the file: the grid's memoised dates may not cover it.
    const shot = (await invoke("photo_info", { path: p.path }).catch(() => null)) as
      | { taken: number; camera: string; lens: string }
      | null;
    const taken = shot?.taken || takenMap.get(p.path) || 0;
    if (taken) rows.push([t("takenCol"), fmtDate(taken)]);
    if (shot?.camera) rows.push([t("camera"), shot.camera]);
    if (shot?.lens) rows.push([t("lens"), shot.lens]);
  }

  const box = $<HTMLDivElement>("props-body");
  box.innerHTML = "";
  for (const [k, v] of rows) {
    const d = document.createElement("div");
    d.className = "prop-row";
    d.innerHTML = "<span></span><span></span>";
    (d.firstElementChild as HTMLElement).textContent = k;
    (d.lastElementChild as HTMLElement).textContent = v;
    box.appendChild(d);
  }
  $<HTMLInputElement>("props-readonly").checked = p.readonly;
  $<HTMLInputElement>("props-hidden").checked = p.hidden;
  $("props-modal").classList.remove("hidden");
}
$<HTMLButtonElement>("props-close").onclick = () => $("props-modal").classList.add("hidden");
$<HTMLButtonElement>("props-apply").onclick = () => {
  void invoke("set_attributes", {
    path: propsPath,
    readonly: $<HTMLInputElement>("props-readonly").checked,
    hidden: $<HTMLInputElement>("props-hidden").checked,
  })
    .then(() => {
      $("props-modal").classList.add("hidden");
      void reloadDir();
    })
    .catch((err) => alert(String(err)));
};
$<HTMLButtonElement>("props-copy-path").onclick = () => copyToClipboard(propsPath);

/** Put text on the Windows clipboard (works from the web view). */
function copyToClipboard(text: string): void {
  navigator.clipboard
    ?.writeText(text)
    .then(() => toast(t("pathCopied")))
    .catch(() => {
      // Fallback for contexts where the async API is blocked.
      const ta = document.createElement("textarea");
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
      toast(t("pathCopied"));
    });
}

/** Free space of the volume holding the current folder. */
function refreshDiskSpace(): void {
  if (!currentPath) return;
  void invoke("disk_space", { path: currentPath })
    .then((r) => {
      const v = r as [number, number] | null;
      statusDisk.textContent = v ? `${fmtSize(v[0])} ${t("diskFree")} / ${fmtSize(v[1])}` : "";
    })
    .catch(() => (statusDisk.textContent = ""));
}

// ---------- Undoable file operations ----------
type UndoOp =
  | { kind: "move"; pairs: [string, string][] }
  | { kind: "copy"; created: string[] }
  | { kind: "rename"; from: string; to: string }
  | { kind: "delete"; paths: string[] }
  | { kind: "newFolder"; path: string };

const undoStack: UndoOp[] = [];
function pushUndo(op: UndoOp): void {
  undoStack.push(op);
  if (undoStack.length > 50) undoStack.shift();
}

async function undoLast(): Promise<void> {
  const op = undoStack.pop();
  if (!op) {
    toast(t("undoEmpty"));
    return;
  }
  try {
    switch (op.kind) {
      case "move":
        // Send each item back where it came from, restoring its name.
        for (const [from, to] of op.pairs) {
          const res = (await invoke("paste_files", {
            paths: [to],
            dest: parentOf(from),
            cut: true,
            policy: "rename",
          })) as { pairs: [string, string][] };
          const landed = res.pairs[0]?.[1];
          if (landed && landed !== from) {
            await invoke("rename_file", { path: landed, newName: baseName(from) }).catch(() => {});
          }
        }
        break;
      case "copy":
        for (const p of op.created) await invoke("delete_file", { path: p }).catch(() => {});
        break;
      case "rename":
        await invoke("rename_file", { path: op.to, newName: baseName(op.from) });
        break;
      case "delete":
        await invoke("restore_trashed", { paths: op.paths });
        break;
      case "newFolder":
        await invoke("delete_file", { path: op.path });
        break;
    }
    toast(t("undoDone"));
    await refreshListing();
  } catch (err) {
    alert(String(err));
  }
}

// ---------- Copy / move with progress, cancel and clash handling ----------
let pasteGen = 0;

interface ConflictSide {
  path: string;
  size: number;
  mtime: number;
  w: number;
  h: number;
}
interface ConflictItem {
  name: string;
  dir: boolean;
  src: ConflictSide;
  dst: ConflictSide;
}

/** How alike two items are: pixel count (40 %), aspect ratio (30 %) and file
 *  size (30 %). Whatever is unknown is simply left out of the average. */
function similarity(a: ConflictSide, b: ConflictSide): number | null {
  const parts: [number, number][] = [];
  const pxa = a.w * a.h;
  const pxb = b.w * b.h;
  const near = (x: number, y: number) => Math.min(x, y) / Math.max(x, y);
  if (pxa > 0 && pxb > 0) {
    parts.push([near(pxa, pxb), 0.4]);
    parts.push([near(a.w / a.h, b.w / b.h), 0.3]);
  }
  if (a.size > 0 && b.size > 0) parts.push([near(a.size, b.size), 0.3]);
  if (!parts.length) return null;
  const wsum = parts.reduce((s, [, w]) => s + w, 0);
  return Math.round((parts.reduce((s, [v, w]) => s + v * w, 0) / wsum) * 100);
}

/** One side of the comparison: preview plus its numbers. */
function conflictSideBox(side: ConflictSide, label: string, isDir: boolean): HTMLElement {
  const wrap = document.createElement("div");
  wrap.className = "cf-side";

  const cap = document.createElement("div");
  cap.className = "cf-cap";
  cap.textContent = label;

  const thumb = document.createElement("div");
  thumb.className = "cf-thumb";
  if (isDir) {
    thumb.textContent = "📁";
  } else {
    const img = document.createElement("img");
    img.loading = "lazy";
    img.src = convertFileSrc(side.path);
    // Videos and RAW files have nothing the web view can decode.
    img.onerror = () => {
      thumb.textContent = "🎬";
    };
    thumb.appendChild(img);
  }

  const facts = document.createElement("div");
  facts.className = "cf-facts";
  const lines = [
    side.w ? `${side.w} × ${side.h}` : "",
    side.w ? fmtRatio(side.w, side.h) : "",
    fmtSize(side.size),
    side.mtime ? fmtDate(side.mtime) : "",
  ].filter(Boolean);
  for (const l of lines) {
    const d = document.createElement("div");
    d.textContent = l;
    facts.appendChild(d);
  }
  wrap.append(cap, thumb, facts);
  return wrap;
}

/** At most this many clashes get the full side-by-side treatment. */
const CONFLICT_PREVIEWS = 12;

/** In-app replacement for prompt(): centred, with a dimmed backdrop.
 *  `selectStem` pre-selects the name without its extension, as Explorer does
 *  when renaming a file. Returns null when cancelled. */
function askText(
  title: string,
  value = "",
  opts: { selectStem?: boolean } = {},
): Promise<string | null> {
  return new Promise((resolve) => {
    const modal = $("input-modal");
    const input = $<HTMLInputElement>("input-value");
    $("input-title").textContent = title;
    input.value = value;
    modal.classList.remove("hidden");
    input.focus();
    const dot = value.lastIndexOf(".");
    if (opts.selectStem && dot > 0) input.setSelectionRange(0, dot);
    else input.select();

    const finish = (v: string | null) => {
      modal.classList.add("hidden");
      input.onkeydown = null;
      resolve(v);
    };
    input.onkeydown = (ev) => {
      // The dialog owns these keys: the grid must not see them.
      ev.stopPropagation();
      if (ev.key === "Enter") finish(input.value);
      else if (ev.key === "Escape") finish(null);
    };
    $<HTMLButtonElement>("input-ok").onclick = () => finish(input.value);
    $<HTMLButtonElement>("input-cancel").onclick = () => finish(null);
    modal.onmousedown = (ev) => {
      if (ev.target === modal) finish(null); // click outside the panel
    };
  });
}

function askConflict(items: ConflictItem[]): Promise<string | null> {
  return new Promise((resolve) => {
    const box = $<HTMLDivElement>("conflict-list");
    box.innerHTML = "";
    for (const it of items.slice(0, CONFLICT_PREVIEWS)) {
      const card = document.createElement("div");
      card.className = "cf-card";

      const head = document.createElement("div");
      head.className = "cf-name";
      head.textContent = (it.dir ? "📁 " : "") + it.name;

      const score = similarity(it.src, it.dst);
      if (score !== null) {
        const badge = document.createElement("span");
        badge.className =
          "cf-score " + (score >= 95 ? "high" : score >= 70 ? "mid" : "low");
        badge.textContent = `${score} %`;
        badge.title = t("conflictScoreTip");
        head.appendChild(badge);
      }

      const cmp = document.createElement("div");
      cmp.className = "cf-cmp";
      const arrow = document.createElement("div");
      arrow.className = "cf-arrow";
      arrow.textContent = "→";
      cmp.append(
        conflictSideBox(it.src, t("conflictIncoming"), it.dir),
        arrow,
        conflictSideBox(it.dst, t("conflictExisting"), it.dir),
      );
      card.append(head, cmp);
      box.appendChild(card);
    }
    // Beyond the preview budget, just name them.
    for (const it of items.slice(CONFLICT_PREVIEWS, 40)) {
      const d = document.createElement("div");
      d.className = "prop-row";
      d.innerHTML = "<span></span>";
      (d.firstElementChild as HTMLElement).textContent = (it.dir ? "📁 " : "") + it.name;
      box.appendChild(d);
    }
    if (items.length > 40) {
      const d = document.createElement("div");
      d.className = "prop-row";
      d.textContent = `… +${items.length - 40}`;
      box.appendChild(d);
    }
    // A folder clash is a merge, not a swap: say so, instead of letting
    // "Replace" suggest the existing folder is about to vanish.
    const anyDir = items.some((i) => i.dir);
    const hint = $("conflict-hint");
    hint.textContent = anyDir ? t("conflictMergeHint") : "";
    hint.classList.toggle("hidden", !anyDir);
    $<HTMLButtonElement>("conflict-replace").textContent = anyDir
      ? t("conflictMerge")
      : t("conflictReplace");
    $("conflict-modal").classList.remove("hidden");
    const finish = (v: string | null) => {
      $("conflict-modal").classList.add("hidden");
      resolve(v);
    };
    $<HTMLButtonElement>("conflict-rename").onclick = () => finish("rename");
    $<HTMLButtonElement>("conflict-replace").onclick = () => finish("replace");
    $<HTMLButtonElement>("conflict-skip").onclick = () => finish("skip");
    $<HTMLButtonElement>("conflict-cancel").onclick = () => finish(null);
  });
}

/** Copy or move `paths` into `dest`, asking about clashes and showing a
 *  cancellable progress panel. Records the operation for undo. */
async function pasteInto(paths: string[], dest: string, cut: boolean): Promise<void> {
  if (!paths.length || !dest) return;
  // Never drop a folder inside itself.
  const bad = paths.some((p) => isDescendantOf(dest, p));
  if (bad) {
    toast(t("dropInvalid"));
    return;
  }
  // Explorer keeps the browsing going after a move: remember the slot the
  // items occupied so the selection can land on whatever takes their place.
  const movedSet = new Set(paths.map(normPath));
  const vacatedIdx = cut ? visible.findIndex((v) => movedSet.has(normPath(v.path))) : -1;
  if (cut) {
    // Our own thumbnail workers hold the files open, and Windows will not
    // rename a folder whose files are in use: let them go first.
    await invoke("cancel_thumb_work").catch(() => {});
    pregenBusy = false;
    $("pregen-panel").classList.add("hidden");
  }
  const clashes = (await invoke("check_conflicts", { paths, dest }).catch(() => [])) as ConflictItem[];
  let policy = "rename";
  if (clashes.length) {
    const choice = await askConflict(clashes);
    if (!choice) return;
    policy = choice;
  }
  $("paste-panel").classList.remove("hidden");
  $("paste-title").textContent = cut ? t("moving") : t("copying");
  $("paste-fill").style.width = "0%";
  $("paste-detail").textContent = "";
  try {
    const out = (await invoke("paste_files", { paths, dest, cut, policy })) as {
      pairs: [string, string][];
      replaced: number;
      merged: number;
      kept: number;
      notes: string[];
      skipped: number;
      cancelled: boolean;
    };
    if (out.pairs.length) {
      // Replaced items went to the recycle bin and merged folders now hold
      // the destination's own files: neither can be undone from here.
      if (out.replaced === 0 && out.merged === 0 && out.kept === 0) {
        pushUndo(
          cut
            ? { kind: "move", pairs: out.pairs }
            : { kind: "copy", created: out.pairs.map((x) => x[1]) },
        );
      }
      const parts = [`${out.pairs.length} ${t("filesSuffix")}`];
      if (out.replaced) parts.push(`${out.replaced} ${t("conflictReplaced")}`);
      if (out.skipped) parts.push(`${out.skipped} ${t("conflictSkipped")}`);
      if (out.kept) {
        // Both copies exist on purpose: say it loudly, with the reason.
        parts.push(`${out.kept} ${t("sourceKept")}`);
        const why = out.notes.slice(0, 10).join("\n");
        alert(t("sourceKeptExplain") + "\n\n" + why);
      }
      toast((out.cancelled ? t("pasteCancelled") + " — " : "") + parts.join(", "));
    } else if (out.cancelled) {
      toast(t("pasteCancelled"));
    }
    // Folders may have appeared in (or left) the destination.
    await refreshTreeNode(dest);
    if (cut) {
      const sources = new Set(paths.map((x) => parentOf(x)));
      for (const src of sources) await refreshTreeNode(src);
    }
    // Were we standing inside something that just moved? Follow it, the way
    // Explorer keeps the window on the folder it has just relocated.
    const followed = cut
      ? out.pairs.find(([from]) => isDescendantOf(currentPath, from))
      : undefined;
    if (followed) {
      const [from, to] = followed;
      const tail = currentPath.slice(from.length);
      await openDir(to + tail);
    } else {
      await refreshListing();
      if (vacatedIdx >= 0 && visible.length) {
        // The next sibling slid into the slot; if it was the last one, take
        // the new last. Same rule as Explorer.
        setFocus(Math.min(vacatedIdx, visible.length - 1));
        repaint();
        scrollToIndex(selectedIndex);
      }
    }
  } catch (err) {
    alert(String(err));
  } finally {
    $("paste-panel").classList.add("hidden");
  }
}

void listen<{ generation: number; done: number; total: number; current: string }>(
  "paste-progress",
  (ev) => {
    pasteGen = Math.max(pasteGen, ev.payload.generation);
    if (ev.payload.generation < pasteGen) return;
    const { done, total, current } = ev.payload;
    const pct = total ? Math.min(100, Math.round((done / total) * 100)) : 0;
    $("paste-fill").style.width = `${pct}%`;
    $("paste-detail").textContent = `${fmtSize(done)} / ${fmtSize(total)} — ${current ?? ""}`;
  },
);
$<HTMLButtonElement>("paste-cancel").onclick = () => {
  void invoke("cancel_paste").catch(() => {});
};

// ---------- Recursive thumbnail pre-generation ----------
let pregenGen = 0;
let pregenBusy = false;

function startPregen(root: string): void {
  void invoke("pregen_thumbs", { root, tsize: thumbTargetPx() })
    .then((g) => {
      pregenGen = g as number;
      pregenBusy = true;
    })
    .catch((err) => alert(String(err)));
}
function cancelPregen(): void {
  void invoke("cancel_pregen").catch(() => {});
  pregenBusy = false;
  $("pregen-panel").classList.add("hidden");
}
void listen<{ generation: number; done: number; total: number }>("pregen-progress", (ev) => {
  if (ev.payload.generation < pregenGen) return;
  pregenBusy = true;
  const { done, total } = ev.payload;
  const pct = total ? Math.round((done / total) * 100) : 0;
  // Its own floating panel: browsing stays completely usable meanwhile.
  $("pregen-panel").classList.remove("hidden");
  $("pregen-title").textContent = t("pregenRunning");
  $("pregen-fill").style.width = `${pct}%`;
  $("pregen-detail").textContent = `${done} / ${total} (${pct} %)`;
});
void listen<{ generation: number; total: number; cancelled: boolean }>("pregen-done", (ev) => {
  if (ev.payload.generation < pregenGen) return;
  pregenBusy = false;
  $("pregen-panel").classList.add("hidden");
  toast(
    ev.payload.cancelled
      ? t("pregenCancelled")
      : `${t("pregenDone")} ${ev.payload.total}`,
  );
  repaint();
});
$<HTMLButtonElement>("pregen-cancel").onclick = () => cancelPregen();

// ---------- Folder operations (tree panel) ----------
function parentOf(path: string): string {
  const sep = path.includes("\\") ? "\\" : "/";
  const parts = path.split(sep).filter(Boolean);
  parts.pop();
  let up = parts.join(sep);
  if (sep === "\\" && !up.includes("\\")) up += "\\";
  if (sep === "/") up = "/" + up;
  return up;
}
function baseName(path: string): string {
  const sep = path.includes("\\") ? "\\" : "/";
  return path.split(sep).filter(Boolean).pop() ?? path;
}

async function renameFolder(path: string): Promise<void> {
  const name = baseName(path);
  const newName = await askText(t("renamePrompt"), name);
  if (!newName || newName === name) return;
  try {
    const newPath = (await invoke("rename_file", { path, newName })) as string;
    pushUndo({ kind: "rename", from: path, to: newPath });
    settings.favorites = settings.favorites.map((f) =>
      normPath(f.path) === normPath(path) ? { name: newName, path: newPath } : f,
    );
    saveSettings();
    buildFavorites();
    await refreshTreeNode(parentOf(path));
    if (normPath(currentPath) === normPath(path)) await openDir(newPath);
    else await refreshListing();
  } catch (err) {
    alert(String(err));
  }
}

async function deleteFolder(path: string): Promise<void> {
  try {
    await invoke("delete_file", { path }); // recycle bin, no confirmation
    pushUndo({ kind: "delete", paths: [path] });
    statusbar.textContent = `🗑 ${baseName(path)}`;
    settings.favorites = settings.favorites.filter(
      (f) => !normPath(f.path).startsWith(normPath(path)),
    );
    saveSettings();
    buildFavorites();
    const dest = normPath(currentPath).startsWith(normPath(path))
      ? parentOf(path)
      : currentPath;
    await refreshTreeNode(parentOf(path));
    if (normPath(dest) === normPath(currentPath)) await refreshListing();
    else await openDir(dest);
  } catch (err) {
    alert(String(err));
  }
}

/** Right-click menu for a folder row in the tree panel. */
/** The Win11-style row of icon commands at the top of a context menu.
 *  Shared by the file grid and the folder tree so both look and behave alike.
 *  Glyphs come from Segoe Fluent Icons; `plain` keeps a normal text font. */
function makeIconRow(): {
  row: HTMLDivElement;
  add: (glyph: string, title: string, fn: (() => void) | null, plain?: boolean) => void;
} {
  const row = document.createElement("div");
  row.className = "ctx-icons";
  const add = (glyph: string, title: string, fn: (() => void) | null, plain = false) => {
    const b = document.createElement("button");
    b.className = plain ? "ctx-ic plain" : "ctx-ic";
    b.textContent = glyph;
    b.title = title;
    if (fn) {
      b.onclick = () => {
        hideCtx();
        fn();
      };
    } else {
      b.disabled = true;
    }
    row.appendChild(b);
  };
  return { row, add };
}

/** Paste whatever the clipboard holds into `dest`. */
function pasteFromClipboardInto(dest: string): void {
  void (async () => {
    const clip = (await readClipboard()) ?? clipboard;
    if (!clip) {
      toast(t("clipEmpty"));
      return;
    }
    if (clip.cut) clipboard = null;
    markCut([]);
    await pasteInto(clip.paths, dest, clip.cut);
  })();
}

function showTreeCtxMenu(ev: MouseEvent, path: string): void {
  ev.preventDefault();
  ctxMenu.innerHTML = "";
  const mkItem = (label: string, fn: () => void) => {
    const d = document.createElement("div");
    d.className = "ctx-item";
    d.textContent = label;
    d.onclick = () => {
      hideCtx();
      fn();
    };
    ctxMenu.appendChild(d);
  };
  // Same icon row as the grid: cut, copy, paste into this folder, rename,
  // delete. The folder under the cursor is its own target.
  const { row, add } = makeIconRow();
  add(ICON_CUT, t("ctxCut"), () => clipSetPaths([path], true));
  add(ICON_COPY, t("ctxCopy"), () => clipSetPaths([path], false));
  add(ICON_PASTE, t("ctxPasteHere"), clipboard ? () => pasteFromClipboardInto(path) : null);
  add(ICON_RENAME, t("act_rename"), () => void renameFolder(path));
  add(ICON_DELETE, t("act_delete"), () => void deleteFolder(path));
  ctxMenu.appendChild(row);

  mkItem(t("ctxOpen"), () => void openDir(path));
  mkItem(t("ctxBrowseRecursive"), () => void browseRecursive(path));
  mkItem(
    pregenBusy ? t("pregenCancel") : t("pregenStart"),
    () => (pregenBusy ? cancelPregen() : startPregen(path)),
  );
  mkItem(isFavorite(path) ? t("favRemove") : t("favAdd"), () => toggleFavorite(path));
  mkItem(t("propsTitle"), () => void showProps(path));
  mkItem(t("copyPath"), () => copyToClipboard(path));
  mkItem(t("ctxReveal"), () => void invoke("reveal", { path }));
  positionCtx(ev);
}

// Segoe Fluent Icons glyphs, named so the code stays readable.
const ICON_CUT = "\ue8c6";
const ICON_COPY = "\ue8c8";
const ICON_PASTE = "\ue77f";
const ICON_RENAME = "\ue8ac";
const ICON_DELETE = "\ue74d";

const LABEL_COLORS = ["#e05555", "#e8a33d", "#e5d947", "#58c26a", "#4f9cf0", "#b06fd8"];

function buildCtx(e: Entry | null): void {
  ctxMenu.innerHTML = "";
  const sel = e ? selEntries() : [];
  const media = sel.filter((x) => x.kind !== "dir");

  // Win11-style single row of icon commands (Segoe Fluent Icons glyphs).
  const { row: icons, add: mkIcon } = makeIconRow();
  mkIcon(ICON_CUT, t("ctxCut"), sel.length ? () => clipSet(sel, true) : null);
  mkIcon(ICON_COPY, t("ctxCopy"), sel.length ? () => clipSet(sel, false) : null);
  mkIcon(ICON_PASTE, t("ctxPaste"), clipboard ? () => void clipPaste() : null);
  mkIcon(ICON_RENAME, t("act_rename"), e ? () => void renameEntry(e) : null);
  mkIcon(ICON_DELETE, t("act_delete"), sel.length ? () => void deleteEntries(sel) : null);
  mkIcon("↶", t("act_rotLeft"), media.length ? () => rotateTargets(-90) : null, true);
  mkIcon("↷", t("act_rotRight"), media.length ? () => rotateTargets(90) : null, true);
  ctxMenu.appendChild(icons);

  const mkItem = (label: string, fn: () => void) => {
    const d = document.createElement("div");
    d.className = "ctx-item";
    d.textContent = label;
    d.onclick = () => { hideCtx(); fn(); };
    ctxMenu.appendChild(d);
  };
  if (e && media.length) {
    // Rating stars + colour labels on one compact row.
    const stars = document.createElement("div");
    stars.className = "ctx-stars";
    for (let n = 1; n <= 5; n++) {
      const s = document.createElement("span");
      s.textContent = "★";
      s.className = metaOf(e.path).rating >= n ? "on" : "";
      s.onclick = () => {
        hideCtx();
        setMetaSel({ rating: metaOf(e.path).rating === n ? 0 : n });
      };
      stars.appendChild(s);
    }
    const colors = document.createElement("span");
    colors.className = "ctx-colors";
    for (const c of LABEL_COLORS) {
      const d = document.createElement("span");
      d.className = "ctx-dot";
      d.style.background = c;
      d.onclick = () => {
        hideCtx();
        setMetaSel({ color: metaOf(e.path).color === c ? "" : c });
      };
      colors.appendChild(d);
    }
    stars.appendChild(colors);
    ctxMenu.appendChild(stars);
  }

  if (e) {
    mkItem(t("ctxOpen"), () => activate(e));
    mkItem(t("ctxReveal"), () => void invoke("reveal", { path: e.path }));
    mkItem(t("copyPath"), () => copyToClipboard(sel.map((x) => x.path).join("\n")));
    mkItem(t("propsTitle"), () => void showProps(e.path));
    if (media.length) {
      mkItem(t("ctxCopyMoveTo"), () => openPicker(sel));
      mkItem(t("ctxTags"), () => {
        void (async () => {
          const v = await askText(t("ctxTagsPrompt"), metaOf(e.path).tags);
          if (v !== null) setMetaSel({ tags: v });
        })();
      });
      for (const ed of settings.editors) {
        mkItem(`${t("ctxOpenWith")} ${ed.name}`, () => {
          for (const x of media) void invoke("open_with", { path: x.path, exe: ed.path });
        });
      }
      mkItem(t("ctxAddEditor"), () => {
        void (async () => {
          const name = await askText(t("editorNamePrompt"));
          if (!name) return;
          const path = await askText(t("editorPathPrompt"));
          if (!path) return;
          settings.editors.push({ name, path });
          saveSettings();
        })();
      });
      mkItem(`${t("ctxConvert")} (${media.length})`, () => openConvert(media));
      if (sel.length > 1) mkItem(`${t("ctxRenameBatch")} (${sel.length})`, () => openRenameBatch(sel));
      const imgs = media.filter((x) => x.kind === "image" || x.kind === "raw");
      if (imgs.length >= 2 && imgs.length <= 4) {
        mkItem(`${t("ctxCompare")} (${imgs.length})`, () => openCompare(imgs));
      }
      if (imgs.length === 1) {
        mkItem(t("ctxWallpaper"), () => {
          void invoke("set_wallpaper", { path: imgs[0].path })
            .then(() => (statusbar.textContent = t("wallpaperSet")))
            .catch((err) => alert(String(err)));
        });
      }
    }
    if (e.kind === "dir") {
      mkItem(t("ctxBrowseRecursive"), () => void browseRecursive(e.path));
      mkItem(
        pregenBusy ? t("pregenCancel") : t("pregenStart"),
        () => (pregenBusy ? cancelPregen() : startPregen(e.path)),
      );
      mkItem(isFavorite(e.path) ? t("favRemove") : t("favAdd"), () => toggleFavorite(e.path));
    }
  } else {
    if (clipboard) mkItem(t("ctxPasteHere"), () => void clipPaste());
    if (currentPath) {
      mkItem(
        pregenBusy ? t("pregenCancel") : t("pregenStart"),
        () => (pregenBusy ? cancelPregen() : startPregen(currentPath)),
      );
      mkItem(t("ctxBrowseRecursive"), () => void browseRecursive(currentPath));
      mkItem(t("xmpImport"), () => void importXmp());
      mkItem(t("propsTitle"), () => void showProps(currentPath));
    }
  }
}

window.addEventListener("click", hideCtx);
document.addEventListener("scroll", hideCtx, true);
gridWrap.addEventListener("contextmenu", (ev) => {
  const tgt = ev.target as HTMLElement;
  if (tgt.closest(".cell") || tgt.closest("tr")) return;
  ev.stopPropagation();
  showCtxMenu(ev, null); // empty area: paste target
});
document.addEventListener("contextmenu", (ev) => {
  const tgt = ev.target as HTMLElement;
  if (tgt.tagName !== "INPUT" && tgt.tagName !== "TEXTAREA") ev.preventDefault();
  if (Date.now() - ctxShownAt > 50) hideCtx();
});

// ---------- Image viewer ----------
// Display = fit scale (from the ACDSee-style fit mode) x manual zoom.
let zoom = 1;
/** Real pixel dimensions of the image being loaded (header read),
 *  so the cached-thumbnail placeholder is shown at the FINAL size —
 *  no "real size then zoom" jump while the full image decodes. */
let pendingDims: { w: number; h: number } | null = null;

function effectiveDims(): { w: number; h: number } {
  if (viewerImg.dataset.ph === "1" && pendingDims) return pendingDims;
  return { w: viewerImg.naturalWidth, h: viewerImg.naturalHeight };
}

/** Rotation of the image on screen; 90/270 swap width and height. */
function viewerRot(): number {
  const e = mediaList()[viewerIndex];
  return e ? rotOf(e.path) : 0;
}

/** Display options of the ACTIVE viewing mode (windowed vs true fullscreen
 *  are configured independently in the Options "Visualisation" tab). */
function modeFit(): { mode: Settings["fitMode"]; reduce: boolean; enlarge: boolean } {
  return viewerFS
    ? { mode: settings.fsFitMode, reduce: settings.fsFitReduce, enlarge: settings.fsFitEnlarge }
    : { mode: settings.fitMode, reduce: settings.fitReduce, enlarge: settings.fitEnlarge };
}

/** Fit scale for the current image, mode and reduce/enlarge flags. */
function fitScale(): number {
  let { w: nw, h: nh } = effectiveDims();
  if (!nw || !nh) return 1;
  // A quarter-turn swaps the on-screen footprint.
  if (viewerRot() % 180 !== 0) [nw, nh] = [nh, nw];
  const mf = modeFit();
  // Placeholder with unknown real dims: always fill the screen area.
  const phUnknown = viewerImg.dataset.ph === "1" && !pendingDims;
  const rW = viewerScroll.clientWidth / nw;
  const rH = viewerScroll.clientHeight / nh;
  let s: number;
  switch (mf.mode) {
    case "fitW": s = rW; break;
    case "fitH": s = rH; break;
    case "actual": return phUnknown ? Math.min(rW, rH, 1) : 1;
    default: s = Math.min(rW, rH);
  }
  if (s < 1 && !mf.reduce && !phUnknown) s = 1;   // don't shrink large images
  if (s > 1 && !mf.enlarge && !phUnknown) s = 1;  // don't grow small images
  return s;
}

function applyViewerScale(): void {
  const { w: nw, h: nh } = effectiveDims();
  if (!nw || !nh) return;
  const s = fitScale() * zoom;
  const w = Math.round(nw * s);
  const h = Math.round(nh * s);
  viewerImg.style.width = `${w}px`;
  viewerImg.style.height = `${h}px`;
  // The wrapper carries the ROTATED footprint so centring and scrolling
  // stay correct; the image itself is rotated around its own centre.
  const rot = viewerRot();
  const [boxW, boxH] = rot % 180 === 0 ? [w, h] : [h, w];
  const rotEl = $<HTMLDivElement>("viewer-rot");
  rotEl.style.width = `${boxW}px`;
  rotEl.style.height = `${boxH}px`;
  viewerImg.style.transform = `translate(-50%, -50%) rotate(${rot}deg)`;
  const overflow =
    boxW > viewerScroll.clientWidth || boxH > viewerScroll.clientHeight;
  viewerScroll.classList.toggle("pannable", overflow);
}

/** Flash the current on-screen scale in the bottom-left corner. */
let zoomBadgeTimer = 0;
function showZoomBadge(): void {
  const el = $<HTMLDivElement>("zoom-badge");
  el.textContent = `${Math.round(fitScale() * zoom * 100)} %`;
  el.classList.remove("hidden");
  window.clearTimeout(zoomBadgeTimer);
  zoomBadgeTimer = window.setTimeout(() => el.classList.add("hidden"), 1500);
}

function setFitMode(m: Settings["fitMode"]): void {
  if (viewerFS) settings.fsFitMode = m;
  else settings.fitMode = m;
  saveSettings();
  zoom = 1;
  playerZoom = 1;
  applyViewerScale();
  applyPlayerFit();
  updateFitButtons();
  showZoomBadge();
}

/** Switch fit mode while keeping the point (cx, cy) under the cursor. */
function setFitModeAt(m: Settings["fitMode"], cx: number, cy: number): void {
  const oldScale = fitScale() * zoom;
  const rect = viewerScroll.getBoundingClientRect();
  const x = cx - rect.left + viewerScroll.scrollLeft;
  const y = cy - rect.top + viewerScroll.scrollTop;
  setFitMode(m);
  const k = (fitScale() * zoom) / oldScale;
  viewerScroll.scrollLeft = x * k - (cx - rect.left);
  viewerScroll.scrollTop = y * k - (cy - rect.top);
}
function updateFitButtons(): void {
  const mf = modeFit();
  document.querySelectorAll<HTMLButtonElement>("#viewer-tools .vt[data-fit]").forEach((b) => {
    b.classList.toggle("active", b.dataset.fit === mf.mode);
  });
  $<HTMLButtonElement>("vt-enlarge").classList.toggle("active", mf.enlarge);
}
// Windowed toolbar buttons, each toggleable in the options. The "p" keys are
// the video player's own toolbar, configurable on the same footing.
const VT_BUTTONS: [string, string][] = [
  ["fs", "act_fullscreen"],
  ["fit", "fitBoth"], ["fitW", "fitWidth"], ["fitH", "fitHeight"], ["actual", "fitActual"],
  ["enlarge", "enlargeSmall"], ["rotl", "act_rotLeft"], ["rotr", "act_rotRight"],
  ["strip", "act_strip"], ["info", "act_info"], ["print", "printTitle"],
  ["pspeed", "speed"], ["pmarkA", "markA"], ["pmarkB", "markB"],
  ["pabloop", "abLoopTip"], ["pcut", "cutExport"],
];
/** First key of the player group, where the options list gets a heading. */
const VT_PLAYER_FIRST = "pspeed";
const VT_PLAYER_IDS: Record<string, string> = {
  pspeed: "player-speed", pmarkA: "btn-mark-a", pmarkB: "btn-mark-b",
  pabloop: "btn-ab-loop", pcut: "btn-cut",
};
function vtElement(key: string): HTMLElement | null {
  if (["fit", "fitW", "fitH", "actual"].includes(key)) {
    return document.querySelector(`#viewer-tools .vt[data-fit="${key}"]`);
  }
  const pid = VT_PLAYER_IDS[key];
  return document.getElementById(pid ?? "vt-" + key);
}
function applyVtButtons(): void {
  for (const [key] of VT_BUTTONS) {
    vtElement(key)?.classList.toggle("hidden", settings.vtShow[key] === false);
  }
}
function buildVtOptions(): void {
  const box = $<HTMLDivElement>("vt-options");
  box.innerHTML = "";
  for (const [key, labelKey] of VT_BUTTONS) {
    if (key === VT_PLAYER_FIRST) {
      const h = document.createElement("h4");
      h.textContent = t("playerToolbar");
      box.appendChild(h);
    }
    const label = document.createElement("label");
    const span = document.createElement("span");
    span.textContent = t(labelKey);
    const cb = document.createElement("input");
    cb.type = "checkbox";
    cb.checked = settings.vtShow[key] !== false;
    cb.onchange = () => {
      settings.vtShow[key] = cb.checked;
      saveSettings();
      applyVtButtons();
    };
    label.append(span, cb);
    box.appendChild(label);
  }
}

/** Per-mode visibility of the prev/next and delete overlay buttons. */
function applyViewButtons(): void {
  const nav = viewerFS ? settings.fsNavBtnShow : settings.navBtnShow;
  const del = viewerFS ? settings.fsDelBtnShow : settings.delBtnShow;
  for (const ov of [viewer, player]) {
    ov.classList.toggle("v-hide-nav", !nav);
    ov.classList.toggle("v-hide-del", !del);
  }
}
function viewerZoom(factor: number): void {
  if (!player.classList.contains("hidden")) {
    // Videos zoom too: same keys, same limits, only the surface differs.
    playerZoom = Math.min(16, Math.max(0.05, playerZoom * factor));
    applyPlayerTransform();
    toast(`${Math.round(playerZoom * 100)} %`);
    return;
  }
  zoom = Math.min(16, Math.max(0.05, zoom * factor));
  applyViewerScale();
  showZoomBadge();
}

// True fullscreen: only the image and the close button remain visible.
// Click the image to enter; first ✕ leaves fullscreen, second ✕ closes.
let viewerFS = false;
/** The fullscreen class belongs to whichever overlay is on screen, so
 *  moving between an image and a video keeps the mode. */
// ---------- Display-mode history (mouse thumb buttons) ----------
// The three ways of looking at a file form a history of their own, browsed
// with the Back/Forward buttons of the mouse exactly like web pages.
type ViewMode = "grid" | "windowed" | "fullscreen";

function currentViewMode(): ViewMode {
  const open =
    !viewer.classList.contains("hidden") || !player.classList.contains("hidden");
  if (!open) return "grid";
  return viewerFS ? "fullscreen" : "windowed";
}

let viewHist: ViewMode[] = ["grid"];
let viewPos = 0;
/** Set while we are replaying the history, so it does not record itself. */
let viewReplaying = false;
let viewRecordTimer = 0;

/** Record the mode we ended up in, once the current gesture has settled.
 *  Deferring collapses the intermediate states a single action goes through
 *  (leaving fullscreen passes through "windowed" on its way to the grid). */
function scheduleViewRecord(): void {
  if (viewReplaying) return;
  window.clearTimeout(viewRecordTimer);
  viewRecordTimer = window.setTimeout(() => {
    const mode = currentViewMode();
    if (viewHist[viewPos] === mode) return;
    viewHist = viewHist.slice(0, viewPos + 1);
    viewHist.push(mode);
    if (viewHist.length > 50) viewHist.shift();
    viewPos = viewHist.length - 1;
  }, 0);
}

/** Put the app back into `mode` without touching the history. */
function applyViewMode(mode: ViewMode): void {
  viewReplaying = true;
  try {
    if (mode === "grid") {
      closeOverlay(true);
      return;
    }
    const open =
      !viewer.classList.contains("hidden") || !player.classList.contains("hidden");
    if (!open) {
      // Coming back from the grid: reopen whatever we were looking at.
      const media = mediaList();
      const e = media[viewerIndex] ?? media[0];
      if (!e) return;
      const idx = media.indexOf(e);
      if (e.kind === "video") openPlayer(idx);
      else openViewer(idx);
    }
    void setViewerFullscreen(mode === "fullscreen");
  } finally {
    // Let the DOM settle before listening again.
    window.setTimeout(() => (viewReplaying = false), 0);
  }
}

function viewHistoryGo(delta: number): void {
  const next = viewPos + delta;
  if (next < 0 || next >= viewHist.length) {
    // Nothing left in the display history: in the grid, the thumb buttons
    // keep their familiar meaning and walk the folder history instead.
    if (currentViewMode() === "grid") navGo(delta);
    return;
  }
  viewPos = next;
  applyViewMode(viewHist[viewPos]);
}

// Mouse buttons 3 and 4 are the thumb rest: Back and Forward.
for (const ev of ["mousedown", "mouseup", "auxclick"] as const) {
  window.addEventListener(
    ev,
    (e) => {
      if (e.button !== 3 && e.button !== 4) return;
      // The web view would otherwise navigate its own history and blank out.
      e.preventDefault();
      e.stopPropagation();
      if (ev === "mouseup") viewHistoryGo(e.button === 3 ? -1 : 1);
    },
    true,
  );
}

/** The filmstrip and the info panel sit outside both overlays: these two root
 *  classes tell them when to show (an overlay is up) and when to stay out of
 *  the way (true fullscreen). */
function updateOverlayState(): void {
  const open =
    !viewer.classList.contains("hidden") || !player.classList.contains("hidden");
  const root = document.documentElement.classList;
  root.toggle("overlay-open", open);
  root.toggle("overlay-fs", open && viewerFS);
  scheduleViewRecord();
}

function applyOverlayFS(): void {
  viewer.classList.toggle("true-fs", viewerFS);
  player.classList.toggle("true-fs", viewerFS);
  updateOverlayState();
  // Fullscreen swaps the bulky native controls for the compact glass bar.
  playerVideo.controls = !viewerFS;
  if (viewerFS) pokePbar();
  else player.classList.remove("hide-cursor");
  if (!viewerFS) viewer.classList.remove("hide-cursor");
}

async function setViewerFullscreen(on: boolean): Promise<void> {
  viewerFS = on;
  applyOverlayFS();
  // Each mode has its own fit/buttons configuration.
  zoom = 1;
  applyViewButtons();
  updateFitButtons();
  applyViewerScale();
  applyPlayerFit();
  try {
    await getCurrentWindow().setFullscreen(on);
  } catch {
    /* capability missing: CSS-only fullscreen still applies */
  }
}

function openViewer(idx: number): void {
  if (idx < 0) return;
  treeFocus = false;
  viewerIndex = idx;
  zoom = 1;
  viewer.classList.remove("hidden");
  applyOverlayFS();
  applyViewButtons();
  updateFitButtons();
  updateOverlayState();
  void showViewerImage();
}
// ---- Full-resolution preload ----------------------------------------
// Neighbouring images are fetched AND decoded ahead of time and held in
// RAM, so moving to the next one paints the real image immediately -
// no low-res thumbnail flash. Depth is configurable (Options > Viewing).
const hdUrls = new Map<string, string>();            // src path -> asset URL
const assetBust = new Map<string, number>();        // src path -> revision
const hdCache = new Map<string, HTMLImageElement>(); // src path -> decoded

async function resolveHd(e: Entry): Promise<string | null> {
  const known = hdUrls.get(e.path);
  if (known) return known;
  const full = (await invoke("full_image", {
    path: e.path,
    kind: e.kind,
    ext: e.ext,
  }).catch(() => null)) as string | null;
  if (!full) return null;
  const rev = assetBust.get(e.path);
  // A rotated file keeps its path: the revision defeats the HTTP cache.
  const url = convertFileSrc(full) + (rev ? `?v=${rev}` : "");
  hdUrls.set(e.path, url);
  return url;
}

function preloadHd(e: Entry): void {
  if (hdCache.has(e.path)) return;
  void resolveHd(e).then((url) => {
    if (!url || hdCache.has(e.path)) return;
    const img = new Image();
    hdCache.set(e.path, img);
    img.src = url;
    // decode() moves the bitmap into memory now instead of at paint time.
    void img.decode().catch(() => {});
  });
}

/** Keep only the images within the preload window around `center`. */
function trimHdCache(center: number, media: Entry[]): void {
  const keep = new Set<string>();
  const n = settings.preloadCount;
  for (let off = -n; off <= n; off++) {
    const m = media[center + off];
    if (m) keep.add(m.path);
  }
  for (const path of [...hdCache.keys()]) {
    if (!keep.has(path)) hdCache.delete(path);
  }
}

const APP_TITLE = "SpeedDisplay";

/** The media name belongs in the window title bar, not in a badge over the
 *  image. Called with no argument when going back to the grid. */
function setWindowTitle(text?: string): void {
  void getCurrentWindow().setTitle(text ? `${text}  —  ${APP_TITLE}` : APP_TITLE).catch(() => {});
}

async function showViewerImage(): Promise<void> {
  const media = mediaList();
  const e = media[viewerIndex];
  if (!e) return;
  viewerInfo.textContent = "";
  setWindowTitle(`${e.name}  —  ${fmtSize(e.size)}  (${viewerIndex + 1}/${media.length})`);
  if (!settings.keepZoom) zoom = 1;
  updateStrip();
  if (!$("exif-panel").classList.contains("hidden")) void fillExifPanel(e);

  // Already decoded in RAM? Paint the real image, skip the placeholder.
  const ready = hdCache.get(e.path);
  if (ready?.complete && ready.naturalWidth > 0) {
    pendingDims = null;
    viewerImg.dataset.ph = "";
    viewerImg.src = ready.src;
    applyViewerScale();
    $("viewer-loading").classList.add("hidden");
    preloadNeighbours();
    return;
  }

  // Show the cached thumbnail immediately, scaled to the FINAL display
  // size (real dimensions come from a header-only read), so the image
  // appears "already zoomed" instead of tiny-then-resized.
  pendingDims = null;
  viewerImg.dataset.ph = "1";
  void invoke("image_dims", { path: e.path })
    .then((d) => {
      const dims = d as [number, number] | null;
      if (dims && mediaList()[viewerIndex]?.path === e.path && viewerImg.dataset.ph === "1") {
        pendingDims = { w: dims[0], h: dims[1] };
        applyViewerScale();
      }
    })
    .catch(() => {});
  const th = thumbs.get(e.path);
  if (th) viewerImg.src = convertFileSrc(th);

  // Loading indicator only if decoding takes noticeable time.
  const spinTimer = window.setTimeout(
    () => $("viewer-loading").classList.remove("hidden"),
    150,
  );

  const url = await resolveHd(e);
  window.clearTimeout(spinTimer);
  // Guard against fast navigation while decoding.
  if (mediaList()[viewerIndex]?.path === e.path) {
    if (url) {
      viewerImg.dataset.ph = "";
      viewerImg.src = url;
    }
    $("viewer-loading").classList.add("hidden");
  }
  preloadNeighbours();
}
function preloadNeighbours(): void {
  const media = mediaList();
  const n = settings.preloadCount;
  if (n <= 0) {
    hdCache.clear();
    return;
  }
  trimHdCache(viewerIndex, media);
  // Forward first (the usual direction), then backward.
  for (let step = 1; step <= n; step++) {
    for (const off of [step, -step]) {
      const m = media[viewerIndex + off];
      if (m && (m.kind === "image" || m.kind === "raw")) preloadHd(m);
    }
  }
}
/** Corner toast, auto-dismissed. Used when the folder navigation wraps. */
let toastTimer = 0;
function toast(msg: string): void {
  const el = $<HTMLDivElement>("toast");
  el.textContent = msg;
  el.classList.remove("hidden", "fade");
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => {
    el.classList.add("fade");
    window.setTimeout(() => el.classList.add("hidden"), 400);
  }, 1600);
}

/** Wrapped past the last item (or before the first)? Tell the user. */
function notifyWrap(raw: number, len: number): void {
  if (raw >= len) toast(t("wrapToFirst"));
  else if (raw < 0) toast(t("wrapToLast"));
}

function viewerNav(delta: number): void {
  const media = mediaList();
  if (!media.length) return;
  let i = viewerIndex + delta;
  if (settings.loopNav) {
    notifyWrap(i, media.length);
    i = (i + media.length) % media.length;
  } else if (i < 0 || i >= media.length) return;
  viewerIndex = i;
  const e = media[i];
  if (e.kind === "video") {
    viewer.classList.add("hidden");
    openPlayer(i);
  } else {
    void showViewerImage();
  }
}
/** Back to the grid: put the selection on the medium we were looking at, so
 *  the thumbnails resume where the browsing left off. */
function focusViewedItem(): void {
  const e = mediaList()[viewerIndex];
  if (!e) return;
  const idx = visible.findIndex((v) => v.path === e.path);
  if (idx < 0) return;
  setFocus(idx);
  repaint();
  scrollToIndex(idx);
}

function closeViewer(force = false): void {
  // The close button steps out of true fullscreen first; Escape (force)
  // goes straight back to the thumbnails.
  if (viewerFS) {
    void setViewerFullscreen(false);
    if (!force) return;
  }
  // …then back to the thumbnails.
  viewer.classList.add("hidden");
  $("viewer-loading").classList.add("hidden");
  $("zoom-badge").classList.add("hidden");
  viewerImg.dataset.ph = "";
  viewerImg.src = "";
  updateOverlayState();
  if (player.classList.contains("hidden")) {
    setWindowTitle();
    focusViewedItem();
  }
}

// ---------- Video player ----------
/** Seconds of video below which resuming is pointless, and the tail inside
 *  which we consider the film watched and start over. */
const RESUME_MIN = 5;
const RESUME_TAIL = 10;

/** Store where the video currently playing was left. */
function saveVideoPos(): void {
  const e = playerEntry;
  if (!e || !playerVideo.src) return;
  const t0 = playerVideo.currentTime;
  const d = playerVideo.duration;
  // Watched to the end: forget the position so it plays from the top again.
  const keep = t0 >= RESUME_MIN && (!Number.isFinite(d) || t0 < d - RESUME_TAIL);
  const pos = keep ? t0 : 0;
  if (Math.abs(metaOf(e.path).pos - pos) < 1) return; // nothing new to write
  metaMap.set(e.path, { ...metaOf(e.path), pos });
  void invoke("set_video_pos", { path: e.path, parent: parentOf(e.path), pos }).catch(() => {});
}
/** The entry the player currently holds, whatever the grid does meanwhile. */
let playerEntry: Entry | null = null;

// A position is worth keeping even if the app is killed: write it on pause,
// every few seconds while playing, and when the window goes away.
playerVideo.addEventListener("pause", saveVideoPos);
let lastPosSave = 0;
playerVideo.addEventListener("timeupdate", () => {
  if (Date.now() - lastPosSave < 5000) return;
  lastPosSave = Date.now();
  saveVideoPos();
});
window.addEventListener("beforeunload", saveVideoPos);

function openPlayer(idx: number): void {
  if (idx < 0) return;
  // Leaving one video for another: keep the place we had in the first.
  saveVideoPos();
  treeFocus = false;
  viewerIndex = idx;
  const e = mediaList()[idx];
  playerEntry = e;
  player.classList.remove("hidden");
  playerZoom = 1;
  applyOverlayFS();
  applyPlayerFit();
  updateOverlayState();
  updateStrip();
  if (!$("exif-panel").classList.contains("hidden")) void fillExifPanel(e);
  abA = -1;
  abB = -1;
  abLoop = false;
  updateABButtons();
  playerVideo.loop = settings.videoLoop;
  playerInfo.textContent = t("loading");
  setWindowTitle(e.name);
  playerVideo.src = "";
  invoke("prepare_video", { path: e.path })
    .then((out) => {
      const { path, mode } = out as { path: string; mode: string };
      if (mediaList()[viewerIndex]?.path !== e.path) return;
      playerVideo.src = convertFileSrc(path);
      // Pick the film up where it was left. The seek has to wait for the
      // duration to be known, hence the one-shot loadedmetadata handler.
      const resume = metaOf(e.path).pos;
      if (resume >= RESUME_MIN) {
        playerVideo.addEventListener(
          "loadedmetadata",
          () => {
            const d = playerVideo.duration;
            if (!Number.isFinite(d) || resume < d - 1) {
              playerVideo.currentTime = resume;
              toast(`${t("resumedAt")} ${fmtClock(resume)}`);
            }
          },
          { once: true },
        );
      }
      playerInfo.textContent = "";
      setWindowTitle(`${e.name}  —  ${t("mode_" + mode)}`);
      // Loading a new resource resets playbackRate to 1: re-apply the choice
      // shown in the toolbar, which would otherwise lie.
      playerVideo.playbackRate = Number($<HTMLSelectElement>("player-speed").value);
      void playerVideo.play();
    })
    .catch((err) => {
      playerInfo.textContent = String(err);
    });
}
function playerNav(delta: number): void {
  const media = mediaList();
  if (!media.length) return;
  let i = viewerIndex + delta;
  if (settings.loopNav) {
    notifyWrap(i, media.length);
    i = (i + media.length) % media.length;
  } else if (i < 0 || i >= media.length) return;
  playerVideo.pause();
  saveVideoPos(); // the pause handler already fires, but be explicit
  const e = media[i];
  if (e.kind === "video") {
    openPlayer(i);
  } else {
    playerEntry = null;
    player.classList.add("hidden");
    openViewer(i);
  }
}
function closePlayer(force = false): void {
  // Same two-step exit as the image viewer.
  if (viewerFS) {
    void setViewerFullscreen(false);
    if (!force) return;
  }
  saveVideoPos(); // before the source goes away
  playerVideo.pause();
  playerVideo.src = "";
  playerEntry = null;
  player.classList.add("hidden");
  updateOverlayState();
  if (viewer.classList.contains("hidden")) {
    setWindowTitle();
    focusViewedItem();
  }
}

// ----- Compact floating transport (true fullscreen) -------------------
// In fullscreen the chunky native <video> controls are replaced by a slim
// glass bar floating over the bottom third of the picture.
const pbar = $<HTMLDivElement>("pbar");
const pbarProg = $<HTMLDivElement>("pbar-prog");
const pbarBuf = $<HTMLDivElement>("pbar-buf");
const pbarVolFill = $<HTMLDivElement>("pbar-vol-fill");
const pbarPlay = $<HTMLButtonElement>("pbar-play");
const pbarMute = $<HTMLButtonElement>("pbar-mute");

/** m:ss, or h:mm:ss past the hour. */
function fmtClock(sec: number): string {
  if (!Number.isFinite(sec) || sec < 0) sec = 0;
  const s = Math.floor(sec % 60);
  const m = Math.floor(sec / 60) % 60;
  const h = Math.floor(sec / 3600);
  const p2 = (n: number) => String(n).padStart(2, "0");
  return h ? `${h}:${p2(m)}:${p2(s)}` : `${m}:${p2(s)}`;
}

function updatePbar(): void {
  const d = playerVideo.duration;
  const pct = Number.isFinite(d) && d > 0 ? (playerVideo.currentTime / d) * 100 : 0;
  pbarProg.style.width = `${pct}%`;
  $("pbar-cur").textContent = fmtClock(playerVideo.currentTime);
  $("pbar-dur").textContent = fmtClock(d);
  pbarPlay.textContent = playerVideo.paused ? "▶" : "❚❚";
  const muted = playerVideo.muted || playerVideo.volume === 0;
  pbarMute.textContent = muted ? "🔇" : "🔉";
  pbarVolFill.style.width = `${(muted ? 0 : playerVideo.volume) * 100}%`;
  if (Number.isFinite(d) && d > 0 && playerVideo.buffered.length) {
    pbarBuf.style.width = `${(playerVideo.buffered.end(playerVideo.buffered.length - 1) / d) * 100}%`;
  } else {
    pbarBuf.style.width = "0";
  }
}
for (const ev of ["timeupdate", "progress", "play", "pause", "volumechange", "loadedmetadata", "durationchange"]) {
  playerVideo.addEventListener(ev, updatePbar);
}

/** Pointer position inside a track element as a 0..1 ratio. */
function trackRatio(el: HTMLElement, clientX: number): number {
  const r = el.getBoundingClientRect();
  return Math.min(1, Math.max(0, (clientX - r.left) / r.width));
}
/** Click-and-drag scrubbing on a track. */
function dragTrack(el: HTMLElement, apply: (ratio: number) => void): void {
  el.addEventListener("pointerdown", (ev) => {
    ev.preventDefault();
    el.setPointerCapture(ev.pointerId);
    apply(trackRatio(el, ev.clientX));
    const move = (m: PointerEvent) => apply(trackRatio(el, m.clientX));
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  });
}
dragTrack($("pbar-seek"), (r) => {
  const d = playerVideo.duration;
  if (Number.isFinite(d) && d > 0) playerVideo.currentTime = r * d;
});
dragTrack($("pbar-vol"), (r) => {
  playerVideo.muted = false;
  playerVideo.volume = r;
});
pbarPlay.onclick = () => (playerVideo.paused ? void playerVideo.play() : playerVideo.pause());
pbarMute.onclick = () => (playerVideo.muted = !playerVideo.muted);
$<HTMLButtonElement>("pbar-exit").onclick = () => void setViewerFullscreen(false);

// Idle fade: the bar and the cursor step aside shortly after the mouse stops.
let pbarIdleTimer = 0;
let pbarHover = false;
pbar.addEventListener("pointerenter", () => {
  pbarHover = true;
  pbar.classList.remove("idle");
});
pbar.addEventListener("pointerleave", () => {
  pbarHover = false;
  pokePbar();
});
function pokePbar(): void {
  pbar.classList.remove("idle");
  player.classList.remove("hide-cursor");
  window.clearTimeout(pbarIdleTimer);
  pbarIdleTimer = window.setTimeout(() => {
    if (pbarHover || playerVideo.paused) return;
    pbar.classList.add("idle");
    player.classList.add("hide-cursor");
  }, 2600);
}
player.addEventListener("pointermove", pokePbar);

// Mouse on the picture itself. In fullscreen the native controls are gone, so
// a click plays/pauses; a double-click steps straight back to the thumbnails,
// like Escape does. The click action is deferred so a double-click does not
// also toggle playback on its way out.
let playClickTimer = 0;
playerVideo.addEventListener("click", () => {
  if (!viewerFS) return; // windowed: the native controls own the clicks
  window.clearTimeout(playClickTimer);
  playClickTimer = window.setTimeout(() => {
    if (playerVideo.paused) void playerVideo.play();
    else playerVideo.pause();
  }, 220);
});
playerVideo.addEventListener("dblclick", () => {
  window.clearTimeout(playClickTimer);
  if (viewerFS) closePlayer(true); // fullscreen -> grid in one gesture
  else void setViewerFullscreen(true);
});
// Click beside the video (the black surround) closes, as in the image viewer.
player.addEventListener("click", (ev) => {
  if (ev.target === player) closePlayer();
});

// ----- Player extras: speed, A-B loop, lossless cut -----
let abA = -1;
let abB = -1;
let abLoop = false;

function updateABButtons(): void {
  $("btn-mark-a").textContent = abA >= 0 ? `A ${abA.toFixed(1)}s` : "A";
  $("btn-mark-b").textContent = abB >= 0 ? `B ${abB.toFixed(1)}s` : "B";
  $("btn-ab-loop").classList.toggle("active", abLoop);
}
$<HTMLSelectElement>("player-speed").onchange = (ev) => {
  playerVideo.playbackRate = Number((ev.target as HTMLSelectElement).value);
};
$<HTMLButtonElement>("btn-mark-a").onclick = () => {
  abA = playerVideo.currentTime;
  updateABButtons();
};
$<HTMLButtonElement>("btn-mark-b").onclick = () => {
  abB = playerVideo.currentTime;
  updateABButtons();
};
$<HTMLButtonElement>("btn-ab-loop").onclick = () => {
  abLoop = !abLoop;
  updateABButtons();
};
playerVideo.addEventListener("timeupdate", () => {
  if (abLoop && abA >= 0 && abB > abA && playerVideo.currentTime >= abB) {
    playerVideo.currentTime = abA;
  }
});
$<HTMLButtonElement>("btn-cut").onclick = () => {
  const e = mediaList()[viewerIndex];
  if (!e) return;
  const start = abA >= 0 ? abA : 0;
  const end = abB > start ? abB : playerVideo.duration;
  playerInfo.textContent = t("cutting");
  invoke("cut_video", { path: e.path, start, end })
    .then((out) => {
      playerInfo.textContent = `${t("cutDone")} ${out}`;
      void reloadDir();
    })
    .catch((err) => (playerInfo.textContent = String(err)));
};

// ---------- Events ----------
void listen<{ src: string; thumb: string; ok: boolean; generation: number }>("thumb", (ev) => {
  // Drop only OLDER generations. Events from the request we just issued can
  // arrive before its invoke() promise resolves (fast cache hits), so a
  // strict equality here would throw away perfectly fresh thumbnails.
  if (ev.payload.generation < currentGen) return;
  if (ev.payload.ok) {
    thumbs.set(ev.payload.src, ev.payload.thumb);
    scheduleGridPaint();
  }
});

// Coalesce thumb events into one repaint per frame.
let paintPending = false;
function scheduleGridPaint(): void {
  if (paintPending) return;
  paintPending = true;
  requestAnimationFrame(() => {
    paintPending = false;
    if (settings.view === "grid") layoutGrid();
  });
}

// ---------- Internal drag & drop ----------
// Tauri installs its own OS drop target for files coming from Explorer,
// which makes HTML5 drag-and-drop unreliable inside the web view. Dragging
// within the app is therefore driven by plain mouse events.
let dragCandidate: { x: number; y: number; paths: string[] } | null = null;
let dragging = false;
let dropTarget: { el: HTMLElement; path: string } | null = null;

function beginDragPaths(ev: MouseEvent, paths: string[]): void {
  if (ev.button !== 0 || !paths.length) return;
  dragCandidate = { x: ev.clientX, y: ev.clientY, paths };
}

function beginDragCandidate(ev: MouseEvent, e: Entry, idx: number): void {
  const list = selected.has(idx) ? selEntries() : [e];
  beginDragPaths(ev, list.map((x) => x.path));
}

/** A folder may not be dropped into itself or into its own subtree. */
function isDescendantOf(candidate: string, ancestor: string): boolean {
  const a = normPath(ancestor);
  const c = normPath(candidate);
  const sep = ancestor.includes("\\") ? "\\" : "/";
  return c === a || c.startsWith(a + sep);
}

function clearDropTarget(): void {
  dropTarget?.el.classList.remove("drop-target");
  dropTarget = null;
}

/** Folder under the cursor: tree row, favourite, or folder cell. */
function dropTargetAt(x: number, y: number): { el: HTMLElement; path: string } | null {
  const el = document.elementFromPoint(x, y) as HTMLElement | null;
  if (!el) return null;
  const row = el.closest<HTMLElement>(".tree-row, .fav-item");
  if (row?.title) return { el: row, path: row.title };
  const cell = el.closest<HTMLElement>(".cell.folder");
  if (cell?.dataset.path) return { el: cell, path: cell.dataset.path };
  // Details view: the folder rows of the table.
  const line = el.closest<HTMLElement>('tr[data-dir="1"]');
  if (line?.dataset.path) return { el: line, path: line.dataset.path };
  return null;
}

/** True for a moment after a drop, so the click that follows the mouse-up
 *  does not also navigate into the folder we just dropped onto. */
let justDropped = false;

function endDrag(): void {
  dragging = false;
  dragCandidate = null;
  $("drag-ghost").classList.add("hidden");
  clearDropTarget();
}

/** Scroll the sidebar when the cursor lingers near its top or bottom. */
function dragAutoScroll(y: number): void {
  const side = $("sidebar");
  const r = side.getBoundingClientRect();
  const zone = 40;
  if (y < r.top + zone) side.scrollTop -= 12;
  else if (y > r.bottom - zone) side.scrollTop += 12;
}

window.addEventListener("mousemove", (ev) => {
  if (!dragCandidate) return;
  if (!dragging) {
    const moved = Math.abs(ev.clientX - dragCandidate.x) + Math.abs(ev.clientY - dragCandidate.y);
    if (moved < 6) return; // a plain click, not a drag
    dragging = true;
    const ghost = $<HTMLDivElement>("drag-ghost");
    ghost.textContent =
      dragCandidate.paths.length === 1
        ? baseName(dragCandidate.paths[0])
        : `${dragCandidate.paths.length} ${t("filesSuffix")}`;
    ghost.classList.remove("hidden");
  }
  const ghost = $<HTMLDivElement>("drag-ghost");
  ghost.style.left = `${ev.clientX + 14}px`;
  ghost.style.top = `${ev.clientY + 14}px`;
  dragAutoScroll(ev.clientY);
  const hit = dropTargetAt(ev.clientX, ev.clientY);
  if (hit?.el !== dropTarget?.el) {
    clearDropTarget();
    if (hit && !dragCandidate.paths.some((p) => isDescendantOf(hit.path, p))) {
      hit.el.classList.add("drop-target");
      dropTarget = hit;
    }
  }
  // Ctrl copies, plain drag moves — the Explorer convention.
  ghost.textContent =
    (ev.ctrlKey ? "⧉ " : "→ ") +
    (dragCandidate.paths.length === 1
      ? baseName(dragCandidate.paths[0])
      : `${dragCandidate.paths.length} ${t("filesSuffix")}`);
});

window.addEventListener("mouseup", (ev) => {
  if (!dragging) {
    dragCandidate = null;
    return;
  }
  const target = dropTarget;
  const paths = dragCandidate?.paths ?? [];
  endDrag();
  if (!target && paths.length) {
    // Silence here used to look like a broken feature.
    toast(t("dropNoTarget"));
    return;
  }
  if (target && paths.length) {
    justDropped = true;
    // Swallow the click that Windows sends right after this mouse-up.
    window.setTimeout(() => (justDropped = false), 120);
    void pasteInto(paths, target.path, !ev.ctrlKey);
  }
});

// ---------- Rubber-band (lasso) selection in grid view ----------
let rubber: { x: number; y: number; el: HTMLDivElement; base: Set<number> } | null = null;

gridWrap.addEventListener("mousedown", (ev) => {
  treeFocus = false; // clicking the file pane takes the arrows back
  if (ev.button !== 0 || settings.view !== "grid") return;
  const tgt = ev.target as HTMLElement;
  if (tgt.closest(".cell") || tgt.closest(".jump") || tgt.closest("tr")) return;
  dragCandidate = null;
  const rect = gridWrap.getBoundingClientRect();
  const el = document.createElement("div");
  el.id = "rubber";
  gridSpacer.appendChild(el);
  rubber = {
    x: ev.clientX - rect.left + gridWrap.scrollLeft,
    y: ev.clientY - rect.top + gridWrap.scrollTop,
    el,
    base: ev.ctrlKey ? new Set(selected) : new Set<number>(),
  };
  ev.preventDefault();
});
window.addEventListener("mousemove", (ev) => {
  if (!rubber) return;
  const rect = gridWrap.getBoundingClientRect();
  const px = Math.max(0, ev.clientX - rect.left + gridWrap.scrollLeft);
  const py = Math.max(0, ev.clientY - rect.top + gridWrap.scrollTop);
  const x0 = Math.min(rubber.x, px);
  const x1 = Math.max(rubber.x, px);
  const y0 = Math.min(rubber.y, py);
  const y1 = Math.max(rubber.y, py);
  Object.assign(rubber.el.style, {
    left: `${x0}px`, top: `${y0}px`, width: `${x1 - x0}px`, height: `${y1 - y0}px`,
  });
  // Geometry hit-test (cells are virtualised, the DOM only holds a window).
  selected = new Set(rubber.base);
  const r0 = Math.max(0, Math.floor((y0 - GAP) / gridRowH));
  const r1 = Math.max(0, Math.floor(y1 / gridRowH));
  for (let r = r0; r <= r1; r++) {
    for (let c = 0; c < gridCols; c++) {
      const cx = GAP + c * (gridCellW + GAP);
      const cy = GAP + r * gridRowH;
      if (cx <= x1 && cx + gridCellW >= x0 && cy <= y1 && cy + gridCellW + 26 >= y0) {
        const idx = r * gridCols + c;
        if (idx < visible.length) selected.add(idx);
      }
    }
  }
  updateSelStatus();
  layoutGrid();
});
window.addEventListener("mouseup", () => {
  if (!rubber) return;
  rubber.el.remove();
  rubber = null;
  layoutGrid();
});

gridWrap.addEventListener("scroll", () => {
  if (settings.view === "grid") layoutGrid();
});
window.addEventListener("resize", () => {
  if (settings.view === "grid") layoutGrid();
});

$<HTMLButtonElement>("btn-up").onclick = goUp;
$<HTMLButtonElement>("btn-back").onclick = () => navGo(-1);
$<HTMLButtonElement>("btn-fwd").onclick = () => navGo(1);
// Mouse side buttons (back/forward), like in a browser.
window.addEventListener("mouseup", (ev) => {
  if (ev.button === 3) navGo(-1);
  else if (ev.button === 4) navGo(1);
});
$<HTMLButtonElement>("btn-fav").onclick = () => {
  if (currentPath) toggleFavorite(currentPath);
};
$<HTMLButtonElement>("btn-allfiles").onclick = () => {
  settings.showAllFiles = !settings.showAllFiles;
  saveSettings();
  $("btn-allfiles").classList.toggle("fav-on", settings.showAllFiles);
  refresh();
};
const filterText = $<HTMLInputElement>("filter-text");
function syncFilterClear(): void {
  $("filter-clear").classList.toggle("hidden", filterText.value === "");
}
filterText.oninput = () => {
  syncFilterClear();
  refresh();
};
// Escape empties the box as well, the way browsers do.
filterText.addEventListener("keydown", (ev) => {
  if (ev.key === "Escape" && filterText.value) {
    ev.stopPropagation();
    clearFilterText();
  }
});
function clearFilterText(): void {
  filterText.value = "";
  syncFilterClear();
  refresh();
  if (filters.recursive) void startRecursiveSearch();
}
$<HTMLButtonElement>("filter-clear").onclick = () => {
  clearFilterText();
  filterText.focus();
};
// Segmented media-type filter: Tout | Images | Vidéos.
function syncKindSeg(): void {
  document.querySelectorAll<HTMLButtonElement>("#kind-seg .seg").forEach((b) =>
    b.classList.toggle("active", b.dataset.kind === settings.filterKind),
  );
}
document.querySelectorAll<HTMLButtonElement>("#kind-seg .seg").forEach((b) => {
  b.onclick = () => {
    settings.filterKind = b.dataset.kind as Settings["filterKind"];
    saveSettings();
    syncKindSeg();
    refresh();
  };
});
$<HTMLSelectElement>("sort").onchange = () => {
  settings.sortOrder = $<HTMLSelectElement>("sort").value;
  saveSettings();
  syncSortUI();
  refresh();
  if (settings.sortOrder.startsWith("taken")) void ensureCaptureDates();
};

// Details view columns. `name` is always shown; the rest is user-chosen
// through a right-click on the header row.
interface Column {
  id: string;
  label: string;   // i18n key
  sort?: string;   // sort key, when the column can be ordered
  always?: boolean;
}
const COLUMNS: Column[] = [
  { id: "name", label: "name", sort: "name", always: true },
  { id: "size", label: "size", sort: "size" },
  { id: "modified", label: "modified", sort: "date" },
  { id: "taken", label: "takenCol", sort: "taken" },
  { id: "type", label: "type", sort: "type" },
  { id: "rating", label: "colRating", sort: "rating" },
  { id: "color", label: "colColor", sort: "color" },
  { id: "tags", label: "colTags", sort: "tags" },
  { id: "res", label: "colRes", sort: "res" },
  { id: "mpx", label: "colMpx", sort: "res" },
  { id: "ratio", label: "colRatio", sort: "ratio" },
];
/** True when a column needing pixel sizes is on screen or drives the sort. */
function needsDims(): boolean {
  return (
    settings.columns.res === true ||
    settings.columns.mpx === true ||
    settings.columns.ratio === true ||
    settings.sortOrder.startsWith("res-") ||
    settings.sortOrder.startsWith("ratio-")
  );
}
function shownColumns(): Column[] {
  return COLUMNS.filter((c) => c.always || settings.columns[c.id]);
}

/** Cell content of `col` for one entry. */
function columnValue(c: Column, e: Entry): string {
  const m = metaOf(e.path);
  switch (c.id) {
    case "size":
      return e.kind === "dir" ? "" : fmtSize(e.size);
    case "modified":
      return fmtDate(e.mtime);
    case "taken": {
      const tk = takenMap.get(e.path);
      return e.kind === "dir" || !tk ? "" : fmtDate(tk);
    }
    case "type":
      return e.ext.toUpperCase();
    case "rating":
      return e.kind === "dir"
        ? ""
        : (m.flag === "pick" ? "✓ " : m.flag === "reject" ? "✗ " : "") +
            "★".repeat(m.rating);
    case "color":
      return m.color ? "●" : "";
    case "tags":
      return m.tags;
    case "res": {
      const d = dimsMap.get(e.path);
      return d && d[0] ? `${d[0]} × ${d[1]}` : "";
    }
    case "mpx": {
      const d = dimsMap.get(e.path);
      if (!d || !d[0]) return "";
      const mp = (d[0] * d[1]) / 1e6;
      return `${mp.toFixed(mp < 10 ? 1 : 0).replace(".", ",")} Mpx`;
    }
    case "ratio": {
      const d = dimsMap.get(e.path);
      return d ? fmtRatio(d[0], d[1]) : "";
    }
    default:
      return e.name;
  }
}

/** (Re)build the header row from the chosen columns. */
function buildListHeader(): void {
  const tr = listTable.querySelector("thead tr")!;
  tr.innerHTML = "";
  const [key, dir] = settings.sortOrder.split("-");
  for (const c of shownColumns()) {
    const th = document.createElement("th");
    const label = t(c.label);
    th.textContent = c.sort === key ? `${label} ${dir === "asc" ? "↑" : "↓"}` : label;
    th.dataset.col = c.id;
    if (c.sort) {
      th.dataset.sort = c.sort;
      th.onclick = () => {
        const [curKey, curDir] = settings.sortOrder.split("-");
        settings.sortOrder = `${c.sort}-${curKey === c.sort && curDir === "asc" ? "desc" : "asc"}`;
        saveSettings();
        syncSortUI();
        refresh();
        if (c.sort === "taken") void ensureCaptureDates();
        if (c.sort === "res" || c.sort === "ratio") void ensureDims();
      };
    } else {
      th.style.cursor = "default";
    }
    tr.appendChild(th);
  }
}

/** Right-click on the header: tick the columns to display. */
function showColumnMenu(ev: MouseEvent): void {
  ev.preventDefault();
  ev.stopPropagation();
  ctxMenu.innerHTML = "";
  for (const c of COLUMNS) {
    if (c.always) continue;
    const on = !!settings.columns[c.id];
    const d = document.createElement("div");
    d.className = "ctx-item";
    d.textContent = `${on ? "☑" : "☐"}  ${t(c.label)}`;
    d.onclick = () => {
      hideCtx();
      settings.columns[c.id] = !on;
      saveSettings();
      buildListHeader();
      if (settings.view === "list") layoutList();
      if (c.id === "taken" && !on) void ensureCaptureDates();
      if (!on && ["res", "mpx", "ratio"].includes(c.id)) void ensureDims();
    };
    ctxMenu.appendChild(d);
  }
  positionCtx(ev);
}
listTable.querySelector("thead")!.addEventListener("contextmenu", showColumnMenu);
/** Reflect the current sort in the select and the column headers (↑/↓). */
function syncSortUI(): void {
  const [key, dir] = settings.sortOrder.split("-");
  const sel = $<HTMLSelectElement>("sort");
  if ([...sel.options].some((o) => o.value === settings.sortOrder)) {
    sel.value = settings.sortOrder;
  }
  buildListHeader();
}
function setView(v: Settings["view"]): void {
  if (settings.view === v) return;
  settings.view = v;
  saveSettings();
  updateViewButton();
  refresh();
}
$<HTMLButtonElement>("btn-view").onclick = () =>
  setView(settings.view === "grid" ? "list" : "grid");

// Sidebar splitter: drag to resize the folder panel, Explorer-style.
let splitDrag = false;
$("splitter").addEventListener("mousedown", (ev) => {
  splitDrag = true;
  ev.preventDefault();
});
window.addEventListener("mousemove", (ev) => {
  if (!splitDrag) return;
  settings.sidebarWidth = Math.min(520, Math.max(140, ev.clientX));
  document.documentElement.style.setProperty("--sidebar-w", `${settings.sidebarWidth}px`);
  if (settings.view === "grid") layoutGrid();
});
window.addEventListener("mouseup", () => {
  if (splitDrag) {
    splitDrag = false;
    saveSettings();
  }
});
$<HTMLInputElement>("thumb-size").oninput = (ev) => {
  settings.thumbSize = Number((ev.target as HTMLInputElement).value);
  if (settings.view === "grid") layoutGrid();
};
$<HTMLInputElement>("thumb-size").onchange = () => {
  saveSettings();
  refresh(); // re-request thumbs at the new resolution
};

// Viewer / player controls
$<HTMLButtonElement>("viewer-close").onclick = () => closeViewer();
$<HTMLButtonElement>("viewer-prev").onclick = () => viewerNav(-1);
$<HTMLButtonElement>("viewer-next").onclick = () => viewerNav(1);
$<HTMLButtonElement>("player-close").onclick = () => closePlayer();
$<HTMLButtonElement>("player-prev").onclick = () => playerNav(-1);
$<HTMLButtonElement>("player-next").onclick = () => playerNav(1);
// Same wheel behaviour as the image viewer: move to the next / previous
// media. Ctrl+wheel has no zoom to apply on a video, so it takes the
// volume instead.
player.addEventListener(
  "wheel",
  (ev) => {
    ev.preventDefault();
    const over = (id: string) => (ev.target as HTMLElement).closest(`#${id}`) !== null;
    const step = -Math.sign(ev.deltaY); // wheel up = forward / louder
    // Over the transport, the wheel drives that track; over the picture it
    // moves to the next medium, like the image viewer.
    if (over("pbar-seek")) {
      pokePbar();
      const d = playerVideo.duration;
      if (Number.isFinite(d) && d > 0) {
        const jump = step * settings.wheelSeekSec;
        playerVideo.currentTime = Math.min(d, Math.max(0, playerVideo.currentTime + jump));
      }
      return;
    }
    if (over("pbar-vol") || ev.ctrlKey) {
      playerVideo.muted = false;
      playerVideo.volume = Math.min(1, Math.max(0, playerVideo.volume + step * 0.05));
      if (!over("pbar-vol")) toast(`${t("volume")} ${Math.round(playerVideo.volume * 100)} %`);
      return;
    }
    if (over("pbar")) return; // the rest of the bar swallows the wheel
    playerNav(ev.deltaY > 0 ? 1 : -1);
  },
  { passive: false },
);

$<HTMLButtonElement>("viewer-del").onclick = () => void deleteCurrent();
$<HTMLButtonElement>("player-del").onclick = () => void deleteCurrent();
viewerImg.addEventListener("load", () => applyViewerScale());
window.addEventListener("resize", () => {
  if (!viewer.classList.contains("hidden")) applyViewerScale();
  if (!player.classList.contains("hidden")) applyPlayerFit();
});
// The intrinsic size is only known once the stream is opened.
playerVideo.addEventListener("loadedmetadata", () => applyPlayerFit());
// Wheel: navigate between images; hold Ctrl to zoom (ACDSee/XnView style).
viewer.addEventListener("wheel", (ev) => {
  ev.preventDefault();
  if (ev.ctrlKey) {
    viewerZoomAt(ev.deltaY < 0 ? 1.15 : 0.87, ev.clientX, ev.clientY);
  } else {
    viewerNav(ev.deltaY > 0 ? 1 : -1);
  }
}, { passive: false });

/// Zoom keeping the point under the cursor stationary.
function viewerZoomAt(factor: number, cx: number, cy: number): void {
  const oldScale = fitScale() * zoom;
  zoom = Math.min(16, Math.max(0.05, zoom * factor));
  const k = (fitScale() * zoom) / oldScale;
  const rect = viewerScroll.getBoundingClientRect();
  const x = cx - rect.left + viewerScroll.scrollLeft;
  const y = cy - rect.top + viewerScroll.scrollTop;
  applyViewerScale();
  viewerScroll.scrollLeft = x * k - (cx - rect.left);
  viewerScroll.scrollTop = y * k - (cy - rect.top);
  showZoomBadge();
}

// Right button in the viewer: 100 % centred on the cursor.
//  - held down  -> a peek: the previous fit comes back on release;
//  - clicked    -> it sticks, and the next right-click returns.
let preActualMode: Settings["fitMode"] | null = null;
let rightPeek: { mode: Settings["fitMode"]; at: number } | null = null;
/** Longer than this and the press counts as "held", not "clicked". */
const PEEK_HOLD_MS = 250;

viewer.addEventListener("contextmenu", (ev) => {
  ev.preventDefault(); // the menu itself is never wanted over the picture
  ev.stopPropagation();
});
viewer.addEventListener("mousedown", (ev) => {
  if (ev.button !== 2) return;
  ev.preventDefault();
  if (modeFit().mode === "actual") {
    // Already at 100 % from an earlier click: this one goes back.
    setFitModeAt(preActualMode ?? "fit", ev.clientX, ev.clientY);
    preActualMode = null;
    return;
  }
  preActualMode = modeFit().mode;
  rightPeek = { mode: preActualMode, at: Date.now() };
  setFitModeAt("actual", ev.clientX, ev.clientY);
});
window.addEventListener("mouseup", (ev) => {
  if (ev.button !== 2 || !rightPeek) return;
  if (Date.now() - rightPeek.at >= PEEK_HOLD_MS) {
    // It was a hold: restore the view it interrupted.
    setFitModeAt(rightPeek.mode, ev.clientX, ev.clientY);
    preActualMode = null;
  }
  rightPeek = null;
});

// ----- Fullscreen -----
async function toggleFullscreen(): Promise<void> {
  // While viewing, go through setViewerFullscreen: it also swaps the fit rules,
  // the button sets and (for videos) the native controls for the compact bar.
  const overlayOpen =
    !viewer.classList.contains("hidden") || !player.classList.contains("hidden");
  if (overlayOpen) {
    await setViewerFullscreen(!viewerFS);
    return;
  }
  const w = getCurrentWindow();
  const fs = await w.isFullscreen();
  await w.setFullscreen(!fs);
}

/** Manual zoom of the player, mirroring `zoom` for images. */
let playerZoom = 1;

/** Rotation and zoom of the video, applied on top of the fit sizing.
 *  A quarter turn swaps the visual width and height, so the result is scaled
 *  back down to keep the whole picture inside the window. */
function applyPlayerTransform(): void {
  const rot = ((viewerRot() % 360) + 360) % 360;
  const quarter = rot === 90 || rot === 270;
  let k = 1;
  const w = playerVideo.offsetWidth;
  const h = playerVideo.offsetHeight;
  if (quarter && w > 0 && h > 0) {
    const box = player.getBoundingClientRect();
    k = Math.min(box.width / h, box.height / w, 1);
  }
  playerVideo.style.transformOrigin = "center center";
  playerVideo.style.transform = `rotate(${rot}deg) scale(${k * playerZoom})`;
}

/** Size the video with the active mode's fit / reduce / enlarge rules,
 *  so a 1080p clip fills a larger screen instead of sitting in bars. */
function applyPlayerFit(): void {
  const mf = modeFit();
  const st = playerVideo.style;
  st.maxWidth = "";
  st.maxHeight = "";
  switch (mf.mode) {
    case "actual":
      st.width = "auto";
      st.height = "auto";
      st.maxWidth = "100%";
      st.maxHeight = "100%";
      st.objectFit = "contain";
      break;
    case "fitW":
      st.width = "100%";
      st.height = "auto";
      st.objectFit = "contain";
      break;
    case "fitH":
      st.width = "auto";
      st.height = "100%";
      st.objectFit = "contain";
      break;
    default:
      st.objectFit = "contain";
      if (mf.enlarge) {
        // Grow to the whole area; the aspect ratio is kept by object-fit.
        st.width = "100%";
        st.height = "100%";
      } else {
        st.width = "auto";
        st.height = "auto";
        st.maxWidth = "100%";
        st.maxHeight = "100%";
      }
  }
  applyPlayerTransform();
}

// ----- Filmstrip -----
function updateStrip(): void {
  const strip = $<HTMLDivElement>("strip");
  if (strip.classList.contains("hidden")) return;
  const media = mediaList();
  strip.innerHTML = "";
  const from = Math.max(0, viewerIndex - 40);
  const to = Math.min(media.length, viewerIndex + 41);
  for (let i = from; i < to; i++) {
    const e = media[i];
    const d = document.createElement("div");
    d.className = "strip-item" + (i === viewerIndex ? " current" : "");
    const th = thumbs.get(e.path);
    if (th) {
      const im = document.createElement("img");
      im.src = convertFileSrc(th);
      d.appendChild(im);
    } else {
      d.textContent = e.kind === "video" ? "🎬" : "🖼";
    }
    d.title = e.name;
    d.onclick = () => {
      viewerIndex = i;
      if (e.kind === "video") {
        viewer.classList.add("hidden");
        openPlayer(i);
      } else {
        void showViewerImage();
      }
    };
    strip.appendChild(d);
    if (i === viewerIndex) requestAnimationFrame(() => d.scrollIntoView({ inline: "center", block: "nearest" }));
  }
}

// ----- EXIF / info panel -----
async function fillExifPanel(e: Entry): Promise<void> {
  const box = $<HTMLDivElement>("exif-body");
  const base: [string, string][] = [
    [t("name"), e.name],
    [t("size"), fmtSize(e.size)],
    [t("modified"), fmtDate(e.mtime)],
    [t("type"), e.ext.toUpperCase()],
  ];
  const exif = (await invoke("read_exif", { path: e.path }).catch(() => [])) as [string, string][];
  box.innerHTML = "";
  for (const [k, v] of [...base, ...exif]) {
    const row = document.createElement("div");
    row.className = "exif-row";
    row.innerHTML = `<span>${k}</span><span></span>`;
    (row.lastElementChild as HTMLElement).textContent = v;
    box.appendChild(row);
  }
}
function toggleInfoPanel(): void {
  const p = $("exif-panel");
  p.classList.toggle("hidden");
  const e = mediaList()[viewerIndex];
  if (!p.classList.contains("hidden") && e) void fillExifPanel(e);
}
function toggleStrip(): void {
  const s = $("strip");
  s.classList.toggle("hidden");
  updateStrip();
}

// Drag to pan when the image overflows; click on the backdrop closes.
let panStart: { x: number; y: number; sl: number; st: number; moved: boolean } | null = null;
viewerScroll.addEventListener("mousedown", (ev) => {
  if (ev.button !== 0) return; // right button drives the 100 % peek
  panStart = {
    x: ev.clientX, y: ev.clientY,
    sl: viewerScroll.scrollLeft, st: viewerScroll.scrollTop,
    moved: false,
  };
});
window.addEventListener("mousemove", (ev) => {
  if (!panStart) return;
  const dx = ev.clientX - panStart.x;
  const dy = ev.clientY - panStart.y;
  if (Math.abs(dx) + Math.abs(dy) > 4) panStart.moved = true;
  viewerScroll.scrollLeft = panStart.sl - dx;
  viewerScroll.scrollTop = panStart.st - dy;
});
window.addEventListener("mouseup", (ev) => {
  if (!panStart) return;
  const moved = panStart.moved;
  const target = ev.target;
  panStart = null;
  if (moved) return;
  // A click ON the picture does nothing: double-click drives the modes.
  // A click on the backdrop steps out one level, like the close button.
  if (target === viewerScroll) closeViewer();
});

// Double-click mirrors the Escape shortcut: in fullscreen it goes straight
// back to the thumbnails, in windowed mode it goes fullscreen. Same gesture,
// same meaning as on a video.
viewerImg.addEventListener("dblclick", (ev) => {
  ev.preventDefault();
  if (viewerFS) closeViewer(true);
  else void setViewerFullscreen(true);
});

// Middle click: back to the grid from anywhere, even when the picture covers
// the whole screen and there is no backdrop left to click.
for (const overlay of [viewer, player]) {
  overlay.addEventListener("auxclick", (ev) => {
    if (ev.button !== 1) return;
    ev.preventDefault();
    closeOverlay(true);
  });
  // Chromium pans on middle-press; we want nothing of the sort.
  overlay.addEventListener("mousedown", (ev) => {
    if (ev.button === 1) ev.preventDefault();
  });
}
viewerImg.addEventListener("dragstart", (ev) => ev.preventDefault());

// True fullscreen on a still image: the cursor bows out once the mouse rests,
// exactly as it does over a video.
let viewerIdleTimer = 0;
viewer.addEventListener("pointermove", () => {
  viewer.classList.remove("hide-cursor");
  window.clearTimeout(viewerIdleTimer);
  if (!viewerFS) return;
  viewerIdleTimer = window.setTimeout(() => {
    if (viewerFS && !viewer.classList.contains("hidden")) viewer.classList.add("hide-cursor");
  }, 2600);
});

document.querySelectorAll<HTMLButtonElement>("#viewer-tools .vt[data-fit]").forEach((b) => {
  b.onclick = () => setFitMode(b.dataset.fit as Settings["fitMode"]);
});
$<HTMLButtonElement>("vt-fs").onclick = () => void setViewerFullscreen(true);
$<HTMLButtonElement>("vt-enlarge").onclick = () => {
  if (viewerFS) settings.fsFitEnlarge = !settings.fsFitEnlarge;
  else settings.fitEnlarge = !settings.fitEnlarge;
  saveSettings();
  applyViewerScale();
  applyPlayerFit();
  updateFitButtons();
};
$<HTMLButtonElement>("vt-rotl").onclick = () => rotateTargets(-90);
$<HTMLButtonElement>("vt-rotr").onclick = () => rotateTargets(90);
$<HTMLButtonElement>("vt-strip").onclick = toggleStrip;
$<HTMLButtonElement>("vt-info").onclick = toggleInfoPanel;
$<HTMLButtonElement>("vt-print").onclick = () => window.print();

// Settings modal
$<HTMLButtonElement>("btn-settings").onclick = () => {
  $("settings").classList.remove("hidden");
  void refreshCacheStats();
};
// Each settings tab has its own accent colour; switching animates the pane.
const TAB_COLORS: Record<string, string> = {
  general: "#ffb300",
  view: "#38bdf8",
  font: "#4ade80",
  keys: "#e879f9",
  cache: "#fb7185",
};
function applyTabAccent(tabId: string): void {
  $("settings-panel").style.setProperty("--tab-accent", TAB_COLORS[tabId] ?? "#ffb300");
}
document.querySelectorAll<HTMLButtonElement>("#settings-tabs .stab").forEach((tab) => {
  tab.onclick = () => {
    document.querySelectorAll<HTMLButtonElement>("#settings-tabs .stab").forEach((b) =>
      b.classList.toggle("active", b === tab),
    );
    document.querySelectorAll<HTMLDivElement>("#settings-panel > .spane").forEach((p) =>
      p.classList.toggle("hidden", p.id !== "pane-" + tab.dataset.tab),
    );
    applyTabAccent(tab.dataset.tab!);
    const pane = $("pane-" + tab.dataset.tab);
    pane.classList.remove("anim");
    void pane.offsetWidth; // restart the CSS animation
    pane.classList.add("anim");
    if (tab.dataset.tab === "cache") void refreshCacheStats();
  };
});
$<HTMLSelectElement>("set-cache-limit").onchange = (ev) => {
  settings.cacheLimitGb = Number((ev.target as HTMLSelectElement).value);
  saveSettings();
  enforceCacheLimit();
};
$<HTMLInputElement>("set-scrollbar").oninput = (ev) => {
  settings.scrollbarPct = Number((ev.target as HTMLInputElement).value);
  document.documentElement.style.setProperty(
    "--scrollbar-w",
    `${Math.round((10 * settings.scrollbarPct) / 100)}px`,
  );
  $("scrollbar-val").textContent = `${settings.scrollbarPct} %`;
};
$<HTMLInputElement>("set-scrollbar").onchange = () => saveSettings();
const bindCheck = (id: string, get: () => boolean, set: (v: boolean) => void, after?: () => void) => {
  const el = $<HTMLInputElement>(id);
  el.checked = get();
  el.onchange = () => {
    set(el.checked);
    saveSettings();
    after?.();
  };
};
bindCheck("set-loop", () => settings.loopNav, (v) => (settings.loopNav = v));
bindCheck("set-exifrot", () => settings.writeExifRotation,
  (v) => (settings.writeExifRotation = v));
bindCheck("set-xmp", () => settings.writeXmp, (v) => (settings.writeXmp = v));
$<HTMLInputElement>("set-preload").oninput = (ev) => {
  settings.preloadCount = Number((ev.target as HTMLInputElement).value);
  $("preload-val").textContent = String(settings.preloadCount);
};
$<HTMLInputElement>("set-wheelseek").oninput = (ev) => {
  settings.wheelSeekSec = Number((ev.target as HTMLInputElement).value);
  $("wheelseek-val").textContent = `${settings.wheelSeekSec} s`;
};
$<HTMLInputElement>("set-wheelseek").onchange = () => saveSettings();
$<HTMLInputElement>("set-preload").onchange = () => {
  saveSettings();
  preloadNeighbours();
};
bindCheck("set-keepzoom", () => settings.keepZoom, (v) => (settings.keepZoom = v));
bindCheck("set-hidden", () => settings.showHidden, (v) => (settings.showHidden = v), refresh);
bindCheck("set-autorefresh", () => settings.autoRefresh, (v) => (settings.autoRefresh = v));
bindCheck("set-videoloop", () => settings.videoLoop, (v) => {
  settings.videoLoop = v;
  playerVideo.loop = v;
});
bindCheck("set-navbtn-show", () => settings.navBtnShow, (v) => {
  settings.navBtnShow = v;
  applyViewButtons();
});
bindCheck("set-delbtn-show", () => settings.delBtnShow, (v) => {
  settings.delBtnShow = v;
  applyViewButtons();
});
bindCheck("set-open-fs", () => settings.openFullscreen, (v) => (settings.openFullscreen = v));
bindCheck("set-fs-reduce", () => settings.fsFitReduce, (v) => {
  settings.fsFitReduce = v;
  applyViewerScale();
});
bindCheck("set-fs-enlarge", () => settings.fsFitEnlarge, (v) => {
  settings.fsFitEnlarge = v;
  applyViewerScale();
  updateFitButtons();
});
bindCheck("set-fs-navbtn", () => settings.fsNavBtnShow, (v) => {
  settings.fsNavBtnShow = v;
  applyViewButtons();
});
bindCheck("set-fs-delbtn", () => settings.fsDelBtnShow, (v) => {
  settings.fsDelBtnShow = v;
  applyViewButtons();
});
$<HTMLInputElement>("set-navbtn-size").oninput = (ev) => {
  settings.navBtnPct = Number((ev.target as HTMLInputElement).value);
  document.documentElement.style.setProperty(
    "--nav-btn",
    `${Math.round((46 * settings.navBtnPct) / 100)}px`,
  );
  $("navbtn-val").textContent = `${settings.navBtnPct} %`;
};
$<HTMLInputElement>("set-navbtn-size").onchange = () => saveSettings();
$<HTMLInputElement>("set-custom-img").onchange = (ev) => {
  settings.customImgExts = (ev.target as HTMLInputElement).value;
  saveSettings();
  void refreshListing();
};
$<HTMLInputElement>("set-custom-vid").onchange = (ev) => {
  settings.customVidExts = (ev.target as HTMLInputElement).value;
  saveSettings();
  void refreshListing();
};
$<HTMLButtonElement>("btn-associate").onclick = () => {
  const exts = [
    "jpg", "jpeg", "png", "gif", "bmp", "webp", "tif", "tiff", "avif", "heic",
    "arw", "nef", "cr2", "cr3", "dng", "raf", "orf", "rw2",
    "mp4", "m4v", "mkv", "avi", "webm", "mov", "wmv", "mpg", "ts", "flv",
  ];
  void invoke("associate_files", { exts })
    .then(() => alert(t("associateDone")))
    .catch((err) => alert(String(err)));
};

// Floating navigation buttons over the grid (top-right and bottom-right):
// jump to top/bottom plus one-screen up/down next to each.
$<HTMLButtonElement>("btn-jump-top").onclick = () =>
  gridWrap.scrollTo({ top: 0, behavior: "smooth" });
$<HTMLButtonElement>("btn-jump-bottom").onclick = () =>
  gridWrap.scrollTo({ top: gridWrap.scrollHeight, behavior: "smooth" });
const pageScroll = (dir: 1 | -1) =>
  gridWrap.scrollBy({ top: dir * gridWrap.clientHeight * 0.9, behavior: "smooth" });
$<HTMLButtonElement>("btn-page-up-t").onclick = () => pageScroll(-1);
$<HTMLButtonElement>("btn-page-down-t").onclick = () => pageScroll(1);
$<HTMLButtonElement>("btn-page-up-b").onclick = () => pageScroll(-1);
$<HTMLButtonElement>("btn-page-down-b").onclick = () => pageScroll(1);
function updateJumpButtons(): void {
  const st = gridWrap.scrollTop;
  const max = gridWrap.scrollHeight - gridWrap.clientHeight;
  $("jump-top-row").classList.toggle("hidden", st < 60);
  $("jump-bottom-row").classList.toggle("hidden", max - st < 60);
}
gridWrap.addEventListener("scroll", updateJumpButtons);
window.addEventListener("resize", updateJumpButtons);
$<HTMLButtonElement>("settings-close").onclick = () => $("settings").classList.add("hidden");
$<HTMLSelectElement>("set-lang").onchange = (ev) => {
  settings.lang = (ev.target as HTMLSelectElement).value;
  saveSettings();
  applySettings();
  refresh();
};
function buildThemeOptions(): void {
  const sel = $<HTMLSelectElement>("set-theme");
  if (sel.options.length === THEMES.length) return;
  sel.innerHTML = "";
  for (const th of THEMES) {
    const o = document.createElement("option");
    o.value = th.id;
    o.textContent = th.label;
    sel.appendChild(o);
  }
}
function applyTheme(id: string): void {
  settings.theme = id;
  document.documentElement.dataset.theme = id;
  $<HTMLSelectElement>("set-theme").value = id;
  saveSettings();
}
/** Walk the theme list; the arrow keys do this natively on the select. */
function cycleTheme(step: number): void {
  const i = THEMES.findIndex((t) => t.id === settings.theme);
  const next = THEMES[((i < 0 ? 0 : i) + step + THEMES.length) % THEMES.length];
  applyTheme(next.id);
}
$<HTMLSelectElement>("set-theme").onchange = (ev) => {
  applyTheme((ev.target as HTMLSelectElement).value);
};
$<HTMLButtonElement>("theme-prev").onclick = () => cycleTheme(-1);
$<HTMLButtonElement>("theme-next").onclick = () => cycleTheme(1);
$<HTMLSelectElement>("set-preview").onchange = (ev) => {
  settings.previewMode = (ev.target as HTMLSelectElement).value as Settings["previewMode"];
  saveSettings();
  refresh();
};
$<HTMLSelectElement>("set-start").onchange = (ev) => {
  settings.startMode = (ev.target as HTMLSelectElement).value as Settings["startMode"];
  if (settings.startMode === "fixed" && !settings.startPath) {
    settings.startPath = currentPath;
    $<HTMLInputElement>("set-start-path").value = currentPath;
  }
  saveSettings();
  $("start-path-row").classList.toggle("hidden", settings.startMode !== "fixed");
};
$<HTMLInputElement>("set-start-path").onchange = (ev) => {
  settings.startPath = (ev.target as HTMLInputElement).value.trim();
  saveSettings();
};
$<HTMLButtonElement>("btn-start-current").onclick = () => {
  settings.startPath = currentPath;
  $<HTMLInputElement>("set-start-path").value = currentPath;
  saveSettings();
};
$<HTMLInputElement>("set-reduce").onchange = (ev) => {
  settings.fitReduce = (ev.target as HTMLInputElement).checked;
  saveSettings();
  applyViewerScale();
};
$<HTMLInputElement>("set-enlarge").onchange = (ev) => {
  settings.fitEnlarge = (ev.target as HTMLInputElement).checked;
  saveSettings();
  applyViewerScale();
  updateFitButtons();
};
$<HTMLInputElement>("set-font-size").oninput = (ev) => {
  settings.fontSize = Number((ev.target as HTMLInputElement).value);
  applyFont();
};
$<HTMLInputElement>("set-font-size").onchange = () => saveSettings();
$<HTMLSelectElement>("set-font-family").onchange = (ev) => {
  settings.fontFamily = (ev.target as HTMLSelectElement).value;
  applyFont();
  saveSettings();
};
$<HTMLSelectElement>("set-font-weight").onchange = (ev) => {
  settings.fontWeight = (ev.target as HTMLSelectElement).value as Settings["fontWeight"];
  applyFont();
  saveSettings();
};
bindCheck("set-clear-play-exit", () => settings.clearPlayOnExit, (v) => {
  settings.clearPlayOnExit = v;
  void invoke("set_clear_play_on_exit", { on: v }).catch(() => {});
});
$<HTMLButtonElement>("btn-clear-play").onclick = () => {
  void invoke("clear_play").then((freed) => {
    toast(`${t("freed")} ${fmtSize(freed as number)}`);
    void refreshCacheStats();
  });
};
$<HTMLButtonElement>("btn-clear-cache").onclick = () => {
  invoke("clear_cache").then(() => {
    thumbs.clear();
    previews.clear();
    void refreshCacheStats();
    refresh();
  });
};

// ---------- Configurable keyboard shortcuts ----------
// Every action can carry any number of key combos, editable in the settings.
const GRID_ACTIONS = [
  "open", "up", "gridPrev", "gridNext", "toggleView", "favorite",
  "settings", "search", "rename", "delete", "copy", "cut", "paste",
  "fullscreen", "pick", "reject", "rate1", "rate2", "rate3", "rate4", "rate5", "rate0",
  "navBack", "navForward", "selectAll", "newFolder",
  "rotLeft", "rotRight", "rotReset",
  "gridFirst", "gridLast", "selToStart", "selToEnd",
  "gridRowUp", "gridRowDown", "selLeft", "selRight", "selUp", "selDown",
  "undo", "properties", "copyPath", "addressBar", "viewList", "viewGrid",
] as const;
const VIEWER_ACTIONS = [
  "next", "prev", "close", "zoomIn", "zoomOut",
  "fit", "fitW", "fitH", "actual", "toggleEnlarge",
  "fullscreen", "info", "strip", "pick", "reject", "delete", "closeStep",
  "rotLeft", "rotRight", "rotReset", "actualSize", "fitScreen",
  // Ratings work on the medium on screen: setMetaTargets already handles the
  // "overlay open" case, only the key routing was missing.
  "rate0", "rate1", "rate2", "rate3", "rate4", "rate5",
] as const;
// Deduplicated: some actions live in both contexts (delete, fullscreen…).
const ALL_ACTIONS: readonly string[] = [...new Set<string>([...GRID_ACTIONS, ...VIEWER_ACTIONS])];

async function renameEntry(e: Entry): Promise<void> {
  const name = await askText(t("renamePrompt"), e.name, { selectStem: e.kind !== "dir" });
  if (name && name !== e.name) {
    await invoke("rename_file", { path: e.path, newName: name })
      .then((np) => {
        pushUndo({ kind: "rename", from: e.path, to: np as string });
        return refreshListing();
      })
      .catch((err) => alert(String(err)));
  }
}
async function deleteEntries(list: Entry[]): Promise<void> {
  if (!list.length) return;
  // No confirmation: files go to the recycle bin, always recoverable.
  for (const e of list) {
    await invoke("delete_file", { path: e.path }).catch((err) => alert(String(err)));
  }
  pushUndo({ kind: "delete", paths: list.map((e) => e.path) });
  statusbar.textContent = `🗑 ${list.length} ${t("filesSuffix")}`;
  dropFromListing(list.map((e) => e.path));
}
function deleteEntry(e: Entry): void {
  void deleteEntries([e]);
}
function overlayNav(delta: number): void {
  if (!viewer.classList.contains("hidden")) viewerNav(delta);
  else if (!player.classList.contains("hidden")) playerNav(delta);
}

/// Delete the media shown in the viewer/player, then show the next one.
async function deleteCurrent(): Promise<void> {
  const e = mediaList()[viewerIndex];
  if (!e) return;
  // No confirmation: recycle bin only, always recoverable.
  try {
    await invoke("delete_file", { path: e.path });
  } catch (err) {
    alert(String(err));
    return;
  }
  const inViewer = !viewer.classList.contains("hidden");
  const idx = viewerIndex;
  dropFromListing([e.path]);
  const media = mediaList();
  if (!media.length) {
    if (viewerFS) void setViewerFullscreen(false); // flag drops synchronously
    closeViewer(true);
    closePlayer(true);
    return;
  }
  viewerIndex = Math.min(idx, media.length - 1);
  const next = media[viewerIndex];
  if (next.kind === "video") {
    if (inViewer) viewer.classList.add("hidden");
    openPlayer(viewerIndex);
  } else {
    if (!inViewer) player.classList.add("hidden");
    viewer.classList.remove("hidden");
    void showViewerImage();
  }
}
function closeOverlay(force = false): void {
  if (!viewer.classList.contains("hidden")) closeViewer(force);
  else if (!player.classList.contains("hidden")) closePlayer(force);
}

const actionHandlers: Record<string, () => void> = {
  open: () => { if (selectedIndex >= 0) activate(visible[selectedIndex]); },
  up: goUp,
  gridPrev: () => moveFocusBy(-1),
  gridNext: () => moveFocusBy(1),
  gridRowUp: () => (treeFocus ? treeMove(-1) : moveFocusBy(-rowStep())),
  gridRowDown: () => (treeFocus ? treeMove(1) : moveFocusBy(rowStep())),
  selLeft: () => extendSelectionBy(-1),
  selRight: () => extendSelectionBy(1),
  selUp: () => extendSelectionBy(-rowStep()),
  selDown: () => extendSelectionBy(rowStep()),
  undo: () => void undoLast(),
  properties: () => {
    const e = visible[selectedIndex];
    void showProps(e ? e.path : currentPath);
  },
  copyPath: () => {
    const list = selEntries();
    copyToClipboard(list.length ? list.map((e) => e.path).join("\n") : currentPath);
  },
  addressBar: openAddressBar,
  gridFirst: () => { setFocus(0); repaint(); scrollToIndex(0); },
  gridLast: () => {
    setFocus(visible.length - 1);
    repaint();
    scrollToIndex(selectedIndex);
  },
  selToEnd: () => extendSelection(visible.length - 1),
  selToStart: () => extendSelection(0),
  toggleView: () => $<HTMLButtonElement>("btn-view").click(),
  viewList: () => setView("list"),
  viewGrid: () => setView("grid"),
  favorite: () => { if (currentPath) toggleFavorite(currentPath); },
  settings: () => $<HTMLButtonElement>("btn-settings").click(),
  search: () => $<HTMLInputElement>("filter-text").focus(),
  rename: () => {
    const e = visible[selectedIndex];
    if (e) renameEntry(e);
    // Nothing selected in the grid: rename the current folder (tree focus).
    else if (currentPath) void renameFolder(currentPath);
  },
  delete: () => {
    const overlayOpen =
      !viewer.classList.contains("hidden") || !player.classList.contains("hidden");
    if (overlayOpen) {
      void deleteCurrent();
      return;
    }
    const list = selEntries();
    if (list.length) void deleteEntries(list);
    // Nothing selected: delete the current folder (recycle bin).
    else if (currentPath) void deleteFolder(currentPath);
  },
  copy: () => clipSet(selEntries(), false),
  cut: () => clipSet(selEntries(), true),
  paste: () => void clipPaste(),
  next: () => overlayNav(1),
  prev: () => overlayNav(-1),
  close: () => closeOverlay(true), // Escape: back to the grid in one go
  closeStep: () => closeOverlay(false), // Enter: one step out at a time
  zoomIn: () => viewerZoom(1.25),
  zoomOut: () => viewerZoom(0.8),
  actualSize: () => setFitMode("actual"),
  fitScreen: () => {
    // Fit in both directions, shrinking AND growing as needed.
    if (viewerFS) {
      settings.fsFitReduce = true;
      settings.fsFitEnlarge = true;
    } else {
      settings.fitReduce = true;
      settings.fitEnlarge = true;
    }
    setFitMode("fit");
  },
  fit: () => setFitMode("fit"),
  fitW: () => setFitMode("fitW"),
  fitH: () => setFitMode("fitH"),
  actual: () => setFitMode("actual"),
  toggleEnlarge: () => $<HTMLButtonElement>("vt-enlarge").click(),
  fullscreen: () => void toggleFullscreen(),
  info: toggleInfoPanel,
  strip: toggleStrip,
  pick: () => setMetaTargets({ flag: "pick" }, true),
  reject: () => setMetaTargets({ flag: "reject" }, true),
  rate0: () => setMetaTargets({ rating: 0 }),
  rate1: () => setMetaTargets({ rating: 1 }),
  rate2: () => setMetaTargets({ rating: 2 }),
  rate3: () => setMetaTargets({ rating: 3 }),
  rate4: () => setMetaTargets({ rating: 4 }),
  rate5: () => setMetaTargets({ rating: 5 }),
  navBack: () => navGo(-1),
  navForward: () => navGo(1),
  rotLeft: () => rotateTargets(-90),
  rotRight: () => rotateTargets(90),
  rotReset: () => rotateTargets(0, true),
  selectAll: () => {
    selected = new Set(visible.map((_, i) => i));
    selAnchor = 0;
    if (selectedIndex < 0) selectedIndex = 0;
    updateSelStatus();
    repaint();
  },
  newFolder: () => void newFolder(),
};

/** Create a folder in the current directory and start renaming it. */
async function newFolder(): Promise<void> {
  if (!currentPath) return;
  const name = await askText(t("newFolderPrompt"), t("newFolderName"));
  if (!name || !name.trim()) return;
  try {
    const created = (await invoke("create_folder", {
      parent: currentPath,
      name: name.trim(),
    })) as string;
    await openDir(currentPath);
    const idx = visible.findIndex((e) => e.path === created);
    if (idx >= 0) {
      setFocus(idx);
      repaint();
    }
    pushUndo({ kind: "newFolder", path: created });
    await refreshTreeNode(currentPath); // show it in the left panel at once
    statusbar.textContent = `📁 ${baseName(created)}`;
  } catch (err) {
    alert(String(err));
  }
}

/** Rotate (display-only) the viewer image, or the whole grid selection.
 *  Nothing is written to the files: the angle lives in the metadata DB. */
// ----- Rotation progress (floating, non-blocking) -----
let rotToken = 0;
/** Below this, the batch finishes before a panel would even be noticed. */
const ROT_PANEL_MIN = 3;

function rotPanelShow(total: number): void {
  $("rot-panel").classList.remove("hidden");
  $("rot-title").textContent = t("rotRunning");
  $("rot-fill").style.width = "0%";
  $("rot-detail").textContent = `0 / ${total}`;
}
function rotPanelHide(): void {
  $("rot-panel").classList.add("hidden");
}
void listen<{ token: number; done: number; total: number; current: string }>(
  "rot-progress",
  (ev) => {
    if (ev.payload.token !== rotToken) return;
    const { done, total, current } = ev.payload;
    $("rot-fill").style.width = `${total ? Math.round((done / total) * 100) : 0}%`;
    $("rot-detail").textContent = `${done} / ${total} — ${current}`;
  },
);
void listen<{ token: number; count: number; cancelled: boolean }>("rot-done", (ev) => {
  if (ev.payload.token !== rotToken) return;
  rotPanelHide();
  if (ev.payload.cancelled) toast(t("rotCancelled"));
});
$<HTMLButtonElement>("rot-cancel").onclick = () => {
  void invoke("cancel_rotate").catch(() => {});
  rotPanelHide();
};

function rotateTargets(delta: number, reset = false): void {
  const overlayOpen =
    !viewer.classList.contains("hidden") || !player.classList.contains("hidden");
  const targets = (overlayOpen ? [mediaList()[viewerIndex]] : selEntries())
    .filter((e): e is Entry => !!e && e.kind !== "dir" && e.kind !== "other");
  if (!targets.length) return;

  const applyDbRotation = (list: Entry[]) => {
    for (const e of list) {
      const rot = reset ? 0 : (((rotOf(e.path) + delta) % 360) + 360) % 360;
      metaMap.set(e.path, { ...metaOf(e.path), rot });
      if (e.kind === "video" && !player.classList.contains("hidden")) applyPlayerTransform();
      void invoke("set_meta", {
        path: e.path,
        parent: currentPath,
        rating: null, color: null, flag: null, tags: null, rot,
      }).catch(() => {});
    }
  };

  // RAW files are displayed through the JPEG preview embedded by the
  // camera, which carries its OWN orientation: patching the container's
  // tag would not move those pixels. They always rotate through the
  // database (and the tag is still written, for other programs).
  const rawTargets = targets.filter((e) => e.kind === "raw");
  const exifTargets = targets.filter((e) => e.kind !== "raw");

  if (settings.writeExifRotation && !reset) {
    const quarters = delta > 0 ? 1 : -1;
    // RAW files always rotate through the database; the container tag is
    // written all the same, for other programs.
    applyDbRotation(rawTargets);

    // One single backend batch for both kinds: two concurrent runs would
    // fight over the cancellation counter, and one progress bar is enough.
    const all = [...rawTargets, ...exifTargets];
    if (all.length) {
      const token = ++rotToken;
      if (all.length >= ROT_PANEL_MIN) rotPanelShow(all.length);
      void invoke("rotate_exif", { paths: all.map((e) => e.path), quarters, token })
        .then((done) => {
          rotPanelHide();
          const patched = new Set(done as string[]);
          // The file itself now carries the angle: clear any display-only
          // rotation and drop the stale thumbnail / preloaded bitmap.
          for (const e of exifTargets.filter((x) => patched.has(x.path))) {
            if (rotOf(e.path) !== 0) {
              metaMap.set(e.path, { ...metaOf(e.path), rot: 0 });
              void invoke("set_meta", {
                path: e.path, parent: currentPath,
                rating: null, color: null, flag: null, tags: null, rot: 0,
              }).catch(() => {});
            }
            bustAsset(e.path);
          }
          // No tag to patch (PNG, exotic RAW): fall back to the database.
          const missed = exifTargets.filter((x) => !patched.has(x.path));
          applyDbRotation(missed);
          void reloadDir();
          if (overlayOpen) void showViewerImage();
          // A raw file's tag is patched all the same, but Adobe tools keep
          // their own orientation for raws and never read it back.
          if (rawTargets.length) toast(t("rotRawNote"));
          else if (missed.length) toast(t("rotDbOnly"));
        })
        .catch(() => {
          rotPanelHide();
          applyDbRotation(exifTargets);
        });
    } else {
      repaint();
      if (overlayOpen) {
        applyViewerScale();
        void showViewerImage();
      }
    }
    return;
  }

  applyDbRotation(targets);
  repaint();
  if (overlayOpen) applyViewerScale();
}

/** Forget every cached form of a file whose bytes just changed, and make
 *  the next asset URL unique so the web view cannot serve a stale copy. */
function bustAsset(path: string): void {
  thumbs.delete(path);
  hdCache.delete(path);
  hdUrls.delete(path);
  assetBust.set(path, (assetBust.get(path) ?? 0) + 1);
}

/// Meta change targeting the viewer image when an overlay is open,
/// otherwise the grid selection. `toggle` clears an identical flag.
function setMetaTargets(patch: Partial<Meta>, toggle = false): void {
  const overlayOpen =
    !viewer.classList.contains("hidden") || !player.classList.contains("hidden");
  const targets = overlayOpen ? [mediaList()[viewerIndex]].filter(Boolean) : selEntries();
  for (const e of targets) {
    if (!e || e.kind === "dir") continue;
    let p = { ...patch };
    if (toggle && patch.flag && metaOf(e.path).flag === patch.flag) p = { flag: "" };
    const cur = { ...metaOf(e.path), ...p };
    metaMap.set(e.path, cur);
    void invoke("set_meta", {
      path: e.path,
      parent: currentPath,
      rating: p.rating ?? null,
      color: p.color ?? null,
      flag: p.flag ?? null,
      tags: p.tags ?? null,
      rot: p.rot ?? null,
    }).catch(() => {});
    syncXmp(e.path);
  }
  repaint();
}

/** Normalised combo for a key event: "Ctrl+Shift+K", "ArrowRight", "Space"… */
function comboOf(ev: KeyboardEvent): string {
  const parts: string[] = [];
  if (ev.ctrlKey) parts.push("Ctrl");
  if (ev.altKey) parts.push("Alt");
  let k = ev.key === " " ? "Space" : ev.key;
  if (k.length === 1) {
    if (ev.shiftKey && /[a-z]/i.test(k)) parts.push("Shift");
    k = k.toUpperCase();
  } else if (ev.shiftKey) {
    parts.push("Shift");
  }
  parts.push(k);
  return parts.join("+");
}

document.addEventListener("keydown", (ev) => {
  if (["Control", "Shift", "Alt", "Meta"].includes(ev.key)) return;
  if (!ctxMenu.classList.contains("hidden")) {
    if (ev.key === "Escape") hideCtx();
    return;
  }
  // The text prompt is modal: nothing else may see a key while it is up.
  if (!$("input-modal").classList.contains("hidden")) {
    if (ev.key === "Escape") $<HTMLButtonElement>("input-cancel").click();
    return;
  }
  // Modal overlays: Escape closes, other keys pass through to inputs.
  for (const id of ["compare", "picker", "convert-modal", "rename-modal", "dup-modal"]) {
    if (!$(id).classList.contains("hidden")) {
      if (ev.key === "Escape") $(id).classList.add("hidden");
      return;
    }
  }
  // Settings modal: Escape closes, everything else behaves normally.
  if (!$("settings").classList.contains("hidden")) {
    if (ev.key === "Escape") $("settings").classList.add("hidden");
    return;
  }
  const overlayOpen =
    !viewer.classList.contains("hidden") || !player.classList.contains("hidden");
  if (!overlayOpen && (ev.target as HTMLElement).tagName === "INPUT") {
    if (ev.key === "Escape") (ev.target as HTMLElement).blur();
    return;
  }
  const combo = comboOf(ev);
  const ctx: readonly string[] = overlayOpen ? VIEWER_ACTIONS : GRID_ACTIONS;
  for (const id of ctx) {
    if ((settings.shortcuts[id] ?? []).includes(combo)) {
      ev.preventDefault();
      actionHandlers[id]();
      return;
    }
  }
});
function repaint(): void {
  settings.view === "grid" ? layoutGrid() : layoutList();
}

// ---------- Shortcut editor (settings modal) ----------
let recordTarget: string | null = null;

function buildShortcutEditor(): void {
  const box = $<HTMLDivElement>("shortcut-list");
  box.innerHTML = "";
  const q = $<HTMLInputElement>("sc-search").value.trim().toLowerCase();
  const shown = ALL_ACTIONS.filter((id) => {
    if (!q) return true;
    if (t("act_" + id).toLowerCase().includes(q)) return true;
    return (settings.shortcuts[id] ?? []).some((c) => c.toLowerCase().includes(q));
  });
  for (const id of shown) {
    const row = document.createElement("div");
    row.className = "sc-row";
    const label = document.createElement("span");
    label.className = "sc-label";
    label.textContent = t("act_" + id);
    const keys = document.createElement("span");
    keys.className = "sc-keys";
    for (const combo of settings.shortcuts[id] ?? []) {
      const chip = document.createElement("span");
      chip.className = "sc-chip";
      chip.textContent = combo;
      const rm = document.createElement("span");
      rm.className = "sc-rm";
      rm.textContent = "✕";
      rm.onclick = () => {
        settings.shortcuts[id] = (settings.shortcuts[id] ?? []).filter((c) => c !== combo);
        saveSettings();
        buildShortcutEditor();
      };
      chip.appendChild(rm);
      keys.appendChild(chip);
    }
    const add = document.createElement("button");
    add.className = "sc-add";
    add.textContent = recordTarget === id ? t("pressKey") : "+";
    add.onclick = () => {
      recordTarget = recordTarget === id ? null : id;
      buildShortcutEditor();
    };
    keys.appendChild(add);
    row.append(label, keys);
    box.appendChild(row);
  }
}

$<HTMLInputElement>("sc-search").oninput = () => buildShortcutEditor();

// Capture phase so recording wins over the global handler.
document.addEventListener(
  "keydown",
  (ev) => {
    if (!recordTarget) return;
    if (["Control", "Shift", "Alt", "Meta"].includes(ev.key)) return;
    ev.preventDefault();
    ev.stopPropagation();
    if (ev.key !== "Escape") {
      const combo = comboOf(ev);
      const list = settings.shortcuts[recordTarget] ?? [];
      if (!list.includes(combo)) list.push(combo);
      settings.shortcuts[recordTarget] = list;
      saveSettings();
    }
    recordTarget = null;
    buildShortcutEditor();
  },
  true,
);

// ---------- Folder picker (copy / move to…) ----------
let pickerItems: Entry[] = [];
let pickerDest = "";

function openPicker(list: Entry[]): void {
  pickerItems = list;
  pickerDest = "";
  $("picker").classList.remove("hidden");
  const tree = $<HTMLDivElement>("picker-tree");
  tree.innerHTML = "";
  void invoke("list_roots").then((roots) => {
    for (const r of roots as string[]) tree.appendChild(makePickerNode(r, r));
  });
}
function makePickerNode(path: string, name: string): HTMLDivElement {
  const wrap = document.createElement("div");
  wrap.className = "tree-node";
  const row = document.createElement("div");
  row.className = "tree-row";
  row.title = path;
  const twist = document.createElement("span");
  twist.className = "twist";
  twist.textContent = "▸";
  const label = document.createElement("span");
  label.className = "tree-label";
  label.textContent = `📁 ${name}`;
  row.append(twist, label);
  const kids = document.createElement("div");
  kids.className = "tree-children hidden";
  wrap.append(row, kids);
  let loaded = false;
  twist.onclick = async (ev) => {
    ev.stopPropagation();
    if (!loaded) {
      loaded = true;
      const list = (await invoke("list_dir", { path }).catch(() => [])) as Entry[];
      for (const d of list.filter((x) => x.kind === "dir" && (settings.showHidden || !x.hidden))) {
        kids.appendChild(makePickerNode(d.path, d.name));
      }
    }
    kids.classList.toggle("hidden");
    twist.textContent = kids.classList.contains("hidden") ? "▸" : "▾";
  };
  row.onclick = () => {
    pickerDest = path;
    $("picker-dest").textContent = path;
    document.querySelectorAll("#picker-tree .tree-row.active").forEach((n) => n.classList.remove("active"));
    row.classList.add("active");
  };
  return wrap;
}
async function pickerGo(cut: boolean): Promise<void> {
  if (!pickerDest || !pickerItems.length) return;
  $("picker").classList.add("hidden");
  await pasteInto(pickerItems.map((e) => e.path), pickerDest, cut);
}
$<HTMLButtonElement>("picker-copy").onclick = () => void pickerGo(false);
$<HTMLButtonElement>("picker-move").onclick = () => void pickerGo(true);
$<HTMLButtonElement>("picker-cancel").onclick = () => $("picker").classList.add("hidden");

// ---------- Batch convert ----------
let convertItems: Entry[] = [];
function openConvert(list: Entry[]): void {
  convertItems = list;
  $("convert-count").textContent = String(list.length);
  $("convert-progress").textContent = "";
  $("convert-modal").classList.remove("hidden");
}
$<HTMLButtonElement>("convert-go").onclick = () => {
  const format = $<HTMLSelectElement>("convert-format").value;
  const quality = Number($<HTMLInputElement>("convert-quality").value);
  const maxDim = Number($<HTMLInputElement>("convert-maxdim").value) || 0;
  $("convert-progress").textContent = "0 / " + convertItems.length;
  void invoke("convert_batch", {
    paths: convertItems.map((e) => e.path),
    format,
    maxDim,
    quality,
  })
    .then((n) => {
      $("convert-progress").textContent = `${t("convertDone")} : ${n}`;
      void reloadDir();
    })
    .catch((err) => ($("convert-progress").textContent = String(err)));
};
$<HTMLButtonElement>("convert-cancel").onclick = () => $("convert-modal").classList.add("hidden");
void listen<{ done: number; total: number }>("convert-progress", (ev) => {
  $("convert-progress").textContent = `${ev.payload.done} / ${ev.payload.total}`;
});

// ---------- Batch rename ----------
let renameItems: Entry[] = [];
function renamePreview(pattern: string): [string, string][] {
  return renameItems.map((e, i) => {
    const stem = e.name.replace(/\.[^.]+$/, "");
    const ext = e.ext ? "." + e.ext : "";
    const date = new Date(e.mtime).toISOString().slice(0, 10);
    const name =
      pattern
        .replaceAll("{n}", String(i + 1).padStart(3, "0"))
        .replaceAll("{nom}", stem)
        .replaceAll("{date}", date) + ext;
    return [e.path, name];
  });
}
function openRenameBatch(list: Entry[]): void {
  renameItems = list;
  $("rename-modal").classList.remove("hidden");
  updateRenamePreview();
}
function updateRenamePreview(): void {
  const pairs = renamePreview($<HTMLInputElement>("rename-pattern").value || "{nom}_{n}");
  $("rename-preview").textContent = pairs
    .slice(0, 3)
    .map(([, n]) => n)
    .join("\n") + (pairs.length > 3 ? "\n…" : "");
}
$<HTMLInputElement>("rename-pattern").oninput = updateRenamePreview;
$<HTMLButtonElement>("rename-go").onclick = () => {
  const pairs = renamePreview($<HTMLInputElement>("rename-pattern").value || "{nom}_{n}");
  void invoke("rename_batch", { pairs })
    .then((n) => {
      $("rename-modal").classList.add("hidden");
      statusbar.textContent = `${n} ${t("renamed")}`;
      void refreshListing();
    })
    .catch((err) => alert(String(err)));
};
$<HTMLButtonElement>("rename-cancel").onclick = () => $("rename-modal").classList.add("hidden");

// ---------- Compare (2-4 images side by side) ----------
let compareZoom = 1;
function openCompare(list: Entry[]): void {
  const box = $<HTMLDivElement>("compare-panes");
  box.innerHTML = "";
  compareZoom = 1;
  for (const e of list) {
    const pane = document.createElement("div");
    pane.className = "cmp-pane";
    const scroll = document.createElement("div");
    scroll.className = "cmp-scroll";
    const img = document.createElement("img");
    img.dataset.path = e.path;
    const th = thumbs.get(e.path);
    if (th) img.src = convertFileSrc(th);
    void invoke("full_image", { path: e.path, kind: e.kind, ext: e.ext })
      .then((p) => (img.src = convertFileSrc(p as string)))
      .catch(() => {});
    img.onload = () => applyCompareZoom();
    const cap = document.createElement("div");
    cap.className = "cmp-cap";
    cap.textContent = `${e.name} — ${fmtSize(e.size)}`;
    scroll.appendChild(img);
    pane.append(scroll, cap);
    box.appendChild(pane);
  }
  $("compare").classList.remove("hidden");
}
function applyCompareZoom(): void {
  document.querySelectorAll<HTMLImageElement>("#compare-panes img").forEach((img) => {
    if (!img.naturalWidth) return;
    const scroll = img.parentElement!;
    const fit = Math.min(
      scroll.clientWidth / img.naturalWidth,
      scroll.clientHeight / img.naturalHeight,
      1,
    );
    const s = fit * compareZoom;
    img.style.width = `${Math.round(img.naturalWidth * s)}px`;
    img.style.height = `${Math.round(img.naturalHeight * s)}px`;
  });
}
$("compare").addEventListener("wheel", (ev) => {
  ev.preventDefault();
  compareZoom = Math.min(16, Math.max(0.1, compareZoom * ((ev as WheelEvent).deltaY < 0 ? 1.15 : 0.87)));
  applyCompareZoom();
}, { passive: false });
$<HTMLButtonElement>("compare-close").onclick = () => $("compare").classList.add("hidden");

// ---------- Duplicates ----------
async function runDuplicates(): Promise<void> {
  const near = $<HTMLInputElement>("dup-near").checked;
  const items = mediaList().map((e) => ({ path: e.path, kind: e.kind, ext: e.ext }));
  if (!items.length) return;
  statusbar.textContent = t("dupSearching");
  const groups = (await invoke("find_duplicates", { items, near }).catch((err) => {
    alert(String(err));
    return [];
  })) as { paths: string[]; exact: boolean }[];
  statusbar.textContent = `${groups.length} ${t("dupGroups")}`;
  if (!groups.length) return;
  const box = $<HTMLDivElement>("dup-list");
  box.innerHTML = "";
  const byPath = new Map(visible.map((e) => [e.path, e]));
  groups.forEach((g, gi) => {
    const head = document.createElement("div");
    head.className = "dup-head";
    head.textContent = `${t("dupGroup")} ${gi + 1} — ${g.exact ? t("dupExact") : t("dupNear")}`;
    box.appendChild(head);
    g.paths.forEach((p, i) => {
      const e = byPath.get(p);
      const row = document.createElement("div");
      row.className = "dup-row";
      const cb = document.createElement("input");
      cb.type = "checkbox";
      cb.checked = i > 0; // keep the first by default
      cb.dataset.path = p;
      const im = document.createElement("img");
      const th = thumbs.get(p);
      if (th) im.src = convertFileSrc(th);
      const label = document.createElement("span");
      label.textContent = `${e?.name ?? p}  (${e ? fmtSize(e.size) : "?"})`;
      row.append(cb, im, label);
      box.appendChild(row);
    });
  });
  $("dup-modal").classList.remove("hidden");
}
$<HTMLButtonElement>("dup-delete").onclick = () => {
  const checked = [...document.querySelectorAll<HTMLInputElement>("#dup-list input:checked")];
  const byPath = new Map(visible.map((e) => [e.path, e]));
  const list = checked.map((c) => byPath.get(c.dataset.path!)).filter(Boolean) as Entry[];
  $("dup-modal").classList.add("hidden");
  void deleteEntries(list);
};
$<HTMLButtonElement>("dup-close").onclick = () => $("dup-modal").classList.add("hidden");

// ---------- Filter popover + saved searches + recursive search ----------
function readFilterUI(): void {
  filters.minSizeMb = Number($<HTMLInputElement>("f-min-size").value) || 0;
  filters.maxSizeMb = Number($<HTMLInputElement>("f-max-size").value) || 0;
  filters.dateFrom = $<HTMLInputElement>("f-date-from").value;
  filters.dateTo = $<HTMLInputElement>("f-date-to").value;
  filters.minRating = Number($<HTMLSelectElement>("f-rating").value);
  filters.color = $<HTMLSelectElement>("f-color").value;
  filters.flag = $<HTMLSelectElement>("f-flag").value;
  filters.tag = $<HTMLInputElement>("f-tag").value.trim();
}
function writeFilterUI(): void {
  $<HTMLInputElement>("f-min-size").value = filters.minSizeMb ? String(filters.minSizeMb) : "";
  $<HTMLInputElement>("f-max-size").value = filters.maxSizeMb ? String(filters.maxSizeMb) : "";
  $<HTMLInputElement>("f-date-from").value = filters.dateFrom;
  $<HTMLInputElement>("f-date-to").value = filters.dateTo;
  $<HTMLSelectElement>("f-rating").value = String(filters.minRating);
  $<HTMLSelectElement>("f-color").value = filters.color;
  $<HTMLSelectElement>("f-flag").value = filters.flag;
  $<HTMLInputElement>("f-tag").value = filters.tag;
  $<HTMLInputElement>("f-recursive").checked = filters.recursive;
}
function updateFilterButton(): void {
  const active =
    filters.minSizeMb > 0 || filters.maxSizeMb > 0 || !!filters.dateFrom || !!filters.dateTo ||
    filters.minRating > 0 || !!filters.color || !!filters.flag || !!filters.tag || filters.recursive;
  $("btn-filter").classList.toggle("fav-on", active);
}
$<HTMLButtonElement>("btn-filter").onclick = (ev) => {
  ev.stopPropagation();
  writeFilterUI();
  rebuildSavedSearches();
  $("filter-pop").classList.toggle("hidden");
};
$("filter-pop").onclick = (ev) => ev.stopPropagation();
window.addEventListener("click", () => $("filter-pop").classList.add("hidden"));

$<HTMLButtonElement>("f-apply").onclick = () => {
  readFilterUI();
  const wantRecursive = $<HTMLInputElement>("f-recursive").checked;
  $("filter-pop").classList.add("hidden");
  updateFilterButton();
  if (wantRecursive) void startRecursiveSearch();
  else {
    filters.recursive = false;
    void openDir(currentPath);
  }
};
$<HTMLButtonElement>("f-reset").onclick = () => {
  filters = { ...EMPTY_FILTERS };
  writeFilterUI();
  updateFilterButton();
  $("filter-pop").classList.add("hidden");
  void invoke("cancel_search").catch(() => {});
  void openDir(currentPath);
};

/** Show every media file of `root` and its subfolders in one flat view. */
async function browseRecursive(root: string): Promise<void> {
  if (!(await openDir(root))) return;
  await startRecursiveSearch();
  updateFilterButton();
}

async function startRecursiveSearch(): Promise<void> {
  filters.recursive = true;
  entries = [];
  refresh();
  statusbar.textContent = t("searching");
  const text = $<HTMLInputElement>("filter-text").value.toLowerCase();
  const kindF = settings.filterKind;
  // The token is ours and is known BEFORE the call: the walk starts emitting
  // immediately, and waiting for the reply to learn an id would silently drop
  // every batch found in the meantime.
  searchGen += 1;
  const token = searchGen;
  await invoke("search_dir", {
    token,
    root: currentPath,
    filter: {
      query: text,
      kind: kindF === "all" ? "" : kindF,
      min_size: Math.round(filters.minSizeMb * 1024 ** 2),
      max_size: Math.round(filters.maxSizeMb * 1024 ** 2),
      min_mtime: filters.dateFrom ? Date.parse(filters.dateFrom) : 0,
      max_mtime: filters.dateTo ? Date.parse(filters.dateTo) + 86_400_000 : 0,
      include_hidden: settings.showHidden,
    },
  });
}
// Repainting on every 50-item batch would sort and lay out the whole list
// dozens of times on a big tree; coalesce into one repaint per frame burst.
let searchPaint = 0;
void listen<{ generation: number; items: Entry[] }>("search-hit", (ev) => {
  if (ev.payload.generation !== searchGen || !filters.recursive) return;
  entries.push(...remapCustomExts(ev.payload.items));
  statusbar.textContent = `${t("searching")} ${entries.length}`;
  if (!searchPaint) {
    searchPaint = window.setTimeout(() => {
      searchPaint = 0;
      refresh();
    }, 150);
  }
});
void listen<{ generation: number }>("search-done", (ev) => {
  if (ev.payload.generation !== searchGen) return;
  window.clearTimeout(searchPaint);
  searchPaint = 0;
  refresh();
  statusbar.textContent = `${visible.length} ${t("items")} (${t("searchDone")})`;
  void loadMetaForParents(entries);
});

function rebuildSavedSearches(): void {
  const sel = $<HTMLSelectElement>("f-saved");
  sel.innerHTML = `<option value="">${t("savedSearches")}…</option>`;
  settings.savedSearches.forEach((s, i) => {
    const o = document.createElement("option");
    o.value = String(i);
    o.textContent = s.name;
    sel.appendChild(o);
  });
}
$<HTMLSelectElement>("f-saved").onchange = (ev) => {
  const i = Number((ev.target as HTMLSelectElement).value);
  const s = settings.savedSearches[i];
  if (!s) return;
  filters = { ...s.state };
  writeFilterUI();
};
$<HTMLButtonElement>("f-save").onclick = async () => {
  const name = await askText(t("saveSearchPrompt"));
  if (!name) return;
  readFilterUI();
  filters.recursive = $<HTMLInputElement>("f-recursive").checked;
  settings.savedSearches.push({ name, state: { ...filters } });
  saveSettings();
  rebuildSavedSearches();
};
$<HTMLButtonElement>("f-dup").onclick = () => {
  $("filter-pop").classList.add("hidden");
  void runDuplicates();
};

// ---------- Drag & drop from Explorer, auto-refresh, print ----------
void listen<{ paths: string[] }>("tauri://drag-drop", (ev) => {
  const paths = ev.payload.paths ?? [];
  if (!paths.length || !currentPath) return;
  void pasteInto(paths, currentPath, false);
});
void listen("dir-changed", () => {
  if (settings.autoRefresh) void reloadDir();
});

// A crash inside a listener used to die silently in the web view, taking every
// later interaction with it. Surface it instead: the status bar keeps the last
// error, and a toast makes it impossible to miss.
function reportError(what: string): void {
  statusbar.textContent = `⚠ ${what}`;
  try {
    toast(`⚠ ${what}`);
  } catch {
    /* the toast itself may be the casualty */
  }
}
window.addEventListener("error", (ev) => reportError(ev.message));
window.addEventListener("unhandledrejection", (ev) => {
  const r = ev.reason;
  reportError(typeof r === "string" ? r : (r?.message ?? String(r)));
});

// ---------- Boot ----------
(async () => {
  await loadSettings();
  enforceCacheLimit();
  await buildSidebar();
  void readClipboard(); // Explorer may already hold a file selection

  // Opened through a file association? Show that file directly.
  const startupFile = (await invoke("get_startup_file").catch(() => null)) as string | null;
  if (startupFile) {
    const sep = startupFile.includes("\\") ? "\\" : "/";
    const parent = startupFile.slice(0, startupFile.lastIndexOf(sep)) || startupFile;
    if (await openDir(parent)) {
      const idx = mediaList().findIndex((m) => m.path === startupFile);
      if (idx >= 0) {
        const e = mediaList()[idx];
        if (e.kind === "video") openPlayer(idx);
        else openViewer(idx);
      }
      return;
    }
  }

  const fallback =
    settings.lastPath ??
    ((await invoke("home_dirs")) as [string, string][])[0]?.[1] ??
    ((await invoke("list_roots")) as string[])[0];
  // Fixed start folder first (if configured and still valid), else resume.
  if (settings.startMode === "fixed" && settings.startPath) {
    if (await openDir(settings.startPath)) return;
  }
  if (fallback) await openDir(fallback);
})();
