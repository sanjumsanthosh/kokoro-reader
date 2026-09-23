import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { LogicalPosition } from "@tauri-apps/api/dpi";
import { listen } from "@tauri-apps/api/event";
import { Menu } from "@tauri-apps/api/menu";
import { openUrl, revealItemInDir } from "@tauri-apps/plugin-opener";
import katex from "katex";
import MarkdownIt from "markdown-it";
import "katex/dist/katex.min.css";
import "./styles.css";
import { cacheTaskId, prioritizeCacheTasks, sectionCacheState, summarizeCacheProjects, type CacheTask, type CacheTaskStatus } from "./cache-queue";
import { articleTiming, parsePlaybackPosition, readerShortcut, sectionPlaybackProgress, type PlaybackPosition } from "./playback";
import { bestMarkdownPassage, deriveSpeechText, groupSpeechChunks, literalTextMatches, narrationHighlights, reconcileSections, speechChunkRanges, splitMarkdownSections, type TextMatch } from "./text";

type SpeechMode = "automatic" | "custom";
type Theme = "light" | "dark";
type ArticleTimerMode = "elapsed" | "remaining";
type CacheState = CacheTaskStatus;
type ViewMode = "listen" | "edit" | "articles" | "storage";
type ArticleFilter = "all" | "read" | "unread";
type ArticleSort = "created_at" | "read_at";

interface DocumentSection {
  markdown: string;
  speech_text: string;
  speech_mode: SpeechMode;
}

interface Document {
  title: string;
  sections: DocumentSection[];
}

interface ProjectSummary {
  project_id: string;
  title: string;
  updated_at: number;
  created_at: number;
  active: boolean;
  read: boolean;
  read_at: number | null;
}

interface ProjectMetadata {
  read: boolean;
  created_at: number | null;
  read_at: number | null;
  codex_url: string | null;
}

interface StorageArea { path: string; bytes: number; exists: boolean; }
interface ProjectCacheUsage { project_id: string; title: string; bytes: number; cached_clips: number; }
interface StorageStats {
  total_bytes: number;
  models: StorageArea;
  audio_cache: StorageArea;
  project_files: StorageArea;
  app_data: StorageArea;
  projects: ProjectCacheUsage[];
}

interface ProjectDocument {
  project_id: string;
  document: Document;
  metadata: ProjectMetadata;
  revision: string;
}

interface TextRange { start_utf16: number; end_utf16: number; }
interface SectionDiagnostic {
  code: string;
  severity: "error" | "warning";
  message: string;
  source_range: TextRange | null;
  narration_range: TextRange | null;
}
interface CoverageBlockResult {
  kind: string;
  label: string;
  source_range: TextRange;
  narration_range: TextRange | null;
  shared_terms: string[];
  missing_terms: string[];
  covered: boolean;
}
interface SectionPrecheckResult {
  section_index: number;
  spoken_grounding: number;
  visual_coverage: number;
  shared_terms: number;
  visual_terms: number;
  narration_terms: number;
  required_visual_shared_terms: number;
  required_grounded_shared_terms: number;
  diagnostics: SectionDiagnostic[];
  coverage_blocks: CoverageBlockResult[];
  ready: boolean;
}
interface SectionsPrecheckResult { sections: SectionPrecheckResult[]; ready_for_send: boolean; }

interface RuntimeStatus {
  model_ready: boolean;
  engine_ready: boolean;
  downloading: boolean;
  progress: DownloadProgress | null;
  backend: string | null;
  error: string | null;
}

interface DownloadProgress {
  asset: string;
  downloaded_bytes: number;
  total_bytes: number | null;
}

interface AudioAsset {
  path: string;
  duration_ms: number;
  cache_hit: boolean;
}

interface QueueTask extends CacheTask {
  asset?: AudioAsset;
  duration?: number;
  error?: string;
  waiters: Array<{ resolve: (asset: AudioAsset) => void; reject: (error: Error) => void; playbackToken?: number }>;
}

interface PlaybackContext {
  projectId: string;
  voice: string;
  sectionIndex: number;
  clipIndex: number;
  clipCount: number;
  clipId: string;
  duration: number;
}

interface PlaybackQueueItem {
  sectionIndex: number;
  clipIndex: number;
  clipCount: number;
  utterance: string;
  id: string;
}

const DEFAULT_VOICE = "af_bella";
const VOICES = new Set(["af_bella", "af_nicole", "am_fenrir"]);
const PREFERENCES_KEY = "kokoro-reader-preferences";
const PLAYBACK_KEY_PREFIX = "kokoro-reader-playback:";
const DEFAULT_TEXT_SCALE = 1;
const MIN_TEXT_SCALE = 0.8;
const MAX_TEXT_SCALE = 1.5;
const TEXT_SCALE_STEP = 0.1;

interface Preferences {
  theme: Theme;
  voice: string;
  speed: number;
  textScale: number;
  articleTimerMode: ArticleTimerMode;
}

const markdown = new MarkdownIt({
  html: false,
  breaks: true,
  linkify: true,
  typographer: true,
});

function required<T extends Element>(selector: string): T {
  const element = document.querySelector<T>(selector);
  if (!element) throw new Error(`Kokoro Reader UI is missing ${selector}`);
  return element;
}

const titleInput = required<HTMLInputElement>("#title-input");
const projectSelect = required<HTMLSelectElement>("#project-select");
const deleteProjectButton = required<HTMLButtonElement>("#delete-project-button");
const markReadButton = required<HTMLButtonElement>("#mark-read-button");
const backToCodexButton = required<HTMLButtonElement>("#back-to-codex-button");
const deleteProjectDialog = required<HTMLElement>("#delete-project-dialog");
const deleteProjectMessage = required<HTMLElement>("#delete-project-message");
const cancelDeleteProjectButton = required<HTMLButtonElement>("#cancel-delete-project-button");
const confirmDeleteProjectButton = required<HTMLButtonElement>("#confirm-delete-project-button");
const restoreButton = required<HTMLButtonElement>("#restore-button");
const reloadFilesButton = required<HTMLButtonElement>("#reload-files-button");
const modelBanner = required<HTMLElement>("#model-banner");
const modelStatusDot = required<HTMLElement>("#model-status-dot");
const modelStatusTitle = required<HTMLElement>("#model-status-title");
const modelStatusDetail = required<HTMLElement>("#model-status-detail");
const modelProgressRow = required<HTMLElement>("#model-progress-row");
const modelProgress = required<HTMLProgressElement>("#model-progress");
const modelProgressLabel = required<HTMLElement>("#model-progress-label");
const downloadButton = required<HTMLButtonElement>("#download-button");
const runtimePill = required<HTMLElement>("#runtime-pill");
const runtimeStatusDot = required<HTMLElement>("#runtime-status-dot");
const runtimeStatusLabel = required<HTMLElement>("#runtime-status-label");
const cachePanelButton = required<HTMLButtonElement>("#cache-panel-button");
const cachePanelDot = required<HTMLElement>("#cache-panel-dot");
const cachePanelCount = required<HTMLElement>("#cache-panel-count");
const cachePanelBackdrop = required<HTMLElement>("#cache-panel-backdrop");
const closeCachePanelButton = required<HTMLButtonElement>("#close-cache-panel-button");
const cachePanelSummary = required<HTMLElement>("#cache-panel-summary");
const cacheActiveItem = required<HTMLElement>("#cache-active-item");
const cacheQueueList = required<HTMLOListElement>("#cache-queue-list");
const themeToggle = required<HTMLButtonElement>("#theme-toggle");
const listenModeButton = required<HTMLButtonElement>("#listen-mode-button");
const editModeButton = required<HTMLButtonElement>("#edit-mode-button");
const articlesModeButton = required<HTMLButtonElement>("#articles-mode-button");
const storageModeButton = required<HTMLButtonElement>("#storage-mode-button");
const sectionSummary = required<HTMLElement>("#section-summary");
const workspaceToolbar = required<HTMLElement>(".workspace-toolbar");
const articleTimer = required<HTMLButtonElement>("#article-timer");
const articleTimerValue = required<HTMLElement>("#article-timer-value");
const searchButton = required<HTMLButtonElement>("#search-button");
const listenPane = required<HTMLElement>("#listen-pane");
const searchBar = required<HTMLElement>("#search-bar");
const searchInput = required<HTMLInputElement>("#search-input");
const searchResultCount = required<HTMLElement>("#search-result-count");
const searchPreviousButton = required<HTMLButtonElement>("#search-previous-button");
const searchNextButton = required<HTMLButtonElement>("#search-next-button");
const searchCloseButton = required<HTMLButtonElement>("#search-close-button");
const editPane = required<HTMLElement>("#edit-pane");
const articlesPane = required<HTMLElement>("#articles-pane");
const storagePane = required<HTMLElement>("#storage-pane");
const articlesFilter = required<HTMLSelectElement>("#articles-filter");
const articlesSelectAll = required<HTMLInputElement>("#articles-select-all");
const articlesTableBody = required<HTMLTableSectionElement>("#articles-table-body");
const deleteSelectedButton = required<HTMLButtonElement>("#delete-selected-button");
const refreshStorageButton = required<HTMLButtonElement>("#refresh-storage-button");
const storageTotal = required<HTMLElement>("#storage-total");
const storageAreas = required<HTMLElement>("#storage-areas");
const storageProjectList = required<HTMLOListElement>("#storage-project-list");
const sectionNavigator = required<HTMLElement>("#section-navigator");
const sectionNavigatorList = required<HTMLElement>("#section-navigator-list");
const markdownView = required<HTMLElement>("#markdown-view");
const markdownEditor = required<HTMLTextAreaElement>("#markdown-editor");
const narrationEditor = required<HTMLElement>("#narration-editor");
const alignmentStatus = required<HTMLElement>("#alignment-status");
const diagnosticsPanel = required<HTMLElement>("#diagnostics-panel");
const draftConflict = required<HTMLElement>("#draft-conflict");
const reloadDiskButton = required<HTMLButtonElement>("#reload-disk-button");
const keepEditsButton = required<HTMLButtonElement>("#keep-edits-button");
const playButton = required<HTMLButtonElement>("#play-button");
const stopButton = required<HTMLButtonElement>("#stop-button");
const voiceSelect = required<HTMLSelectElement>("#voice-select");
const speedSlider = required<HTMLInputElement>("#speed-slider");
const speedValue = required<HTMLOutputElement>("#speed-value");
const speedPresetButtons = Array.from(document.querySelectorAll<HTMLButtonElement>(".speed-preset"));
const activityStatus = required<HTMLElement>("#activity-status");
const playerBar = required<HTMLElement>(".player-bar");
const audioPlayer = required<HTMLAudioElement>("#audio-player");

let currentProjectId = "";
let currentRevision = "";
let projects: ProjectSummary[] = [];
let currentDocument: Document = { title: "", sections: [] };
let currentMetadata: ProjectMetadata = { read: false, created_at: null, read_at: null, codex_url: null };
let runtimeStatus: RuntimeStatus = {
  model_ready: false,
  engine_ready: false,
  downloading: false,
  progress: null,
  backend: null,
  error: null,
};
let mode: ViewMode = "listen";
let activeSection = 0;
let playbackToken = 0;
let playbackPosition: PlaybackPosition | null = null;
let currentPlayback: PlaybackContext | null = null;
let saveTimer: number | undefined;
let saveInFlight = false;
let editorDirty = false;
let pendingExternalRevision = "";
let validationTimer: number | undefined;
let validationGeneration = 0;
let sectionDiagnostics: SectionPrecheckResult[] = [];
let isPlaying = false;
let hasPrevious = false;
let preferences = loadPreferences();
let cacheQueue: QueueTask[] = [];
let cacheQueueBuildTimer: number | undefined;
let cacheQueueBuildGeneration = 0;
let cacheQueueWorkerRunning = false;
let cacheQueueBuilding = false;
let activeCacheTaskId = "";
let cachePanelOpen = false;
let searchQuery = "";
let searchActiveIndex = 0;
let searchMatchGroups: HTMLElement[][] = [];
let searchReturnFocus: HTMLElement | null = null;
let storageStats: StorageStats | null = null;
let articleFilter: ArticleFilter = "all";
let articleSort: ArticleSort = "created_at";
let articleSortDescending = true;
const selectedArticleIds = new Set<string>();
let editTarget: { sectionIndex: number; clipIndex: number } | null = null;
let pendingDeleteProjectIds: string[] = [];

function loadPreferences(): Preferences {
  const defaults: Preferences = {
    theme: window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light",
    voice: DEFAULT_VOICE,
    speed: 1,
    textScale: DEFAULT_TEXT_SCALE,
    articleTimerMode: "elapsed",
  };
  try {
    const stored = window.localStorage.getItem(PREFERENCES_KEY);
    if (!stored) return defaults;
    const parsed: unknown = JSON.parse(stored);
    if (!parsed || typeof parsed !== "object") return defaults;
    const values = parsed as Partial<Preferences>;
    return {
      theme: values.theme === "light" || values.theme === "dark" ? values.theme : defaults.theme,
      voice: typeof values.voice === "string" && VOICES.has(values.voice) ? values.voice : defaults.voice,
      speed: typeof values.speed === "number" && Number.isFinite(values.speed) && values.speed >= 0.5 && values.speed <= 2
        ? Math.round(values.speed * 10) / 10
        : defaults.speed,
      textScale: typeof values.textScale === "number" && Number.isFinite(values.textScale)
        && values.textScale >= MIN_TEXT_SCALE && values.textScale <= MAX_TEXT_SCALE
        ? Math.round(values.textScale * 10) / 10
        : defaults.textScale,
      articleTimerMode: values.articleTimerMode === "elapsed" || values.articleTimerMode === "remaining"
        ? values.articleTimerMode
        : defaults.articleTimerMode,
    };
  } catch (error) {
    console.warn("Unable to load saved reader preferences", error);
    return defaults;
  }
}

function savePreferences(): void {
  try {
    window.localStorage.setItem(PREFERENCES_KEY, JSON.stringify(preferences));
  } catch (error) {
    console.warn("Unable to save reader preferences", error);
  }
}

function playbackStorageKey(projectId: string): string {
  return `${PLAYBACK_KEY_PREFIX}${projectId}`;
}

function loadPlaybackPosition(projectId: string): PlaybackPosition | null {
  if (!projectId) return null;
  try {
    return parsePlaybackPosition(window.localStorage.getItem(playbackStorageKey(projectId)));
  } catch (error) {
    console.warn("Unable to load saved playback position", error);
    return null;
  }
}

function storePlaybackPosition(projectId: string, position: PlaybackPosition): void {
  if (projectId === currentProjectId) playbackPosition = position;
  try {
    window.localStorage.setItem(playbackStorageKey(projectId), JSON.stringify(position));
  } catch (error) {
    console.warn("Unable to save playback position", error);
  }
}

function clearPlaybackPosition(projectId: string): void {
  if (!projectId) return;
  if (projectId === currentProjectId) playbackPosition = null;
  try {
    window.localStorage.removeItem(playbackStorageKey(projectId));
  } catch (error) {
    console.warn("Unable to clear playback position", error);
  }
}

function restorePlaybackSelection(): void {
  playbackPosition = loadPlaybackPosition(currentProjectId);
  if (playbackPosition) {
    const utterance = groupSpeechChunks(currentDocument.sections[playbackPosition.sectionIndex]?.speech_text ?? "")[playbackPosition.clipIndex];
    if (!utterance || cacheTaskId(currentProjectId, playbackPosition.voice, playbackPosition.sectionIndex, playbackPosition.clipIndex, utterance) !== playbackPosition.clipId) {
      clearPlaybackPosition(currentProjectId);
    }
  }
  activeSection = Math.min(playbackPosition?.sectionIndex ?? 0, Math.max(currentDocument.sections.length - 1, 0));
}

function applyTheme(theme: Theme): void {
  preferences.theme = theme;
  document.documentElement.dataset.theme = theme;
  themeToggle.setAttribute("aria-pressed", String(theme === "dark"));
  themeToggle.setAttribute("aria-label", `Switch to ${theme === "dark" ? "light" : "dark"} mode`);
  themeToggle.querySelector("span")!.textContent = theme === "dark" ? "Light mode" : "Dark mode";
  document.querySelector<HTMLMetaElement>('meta[name="theme-color"]')?.setAttribute("content", theme === "dark" ? "#18191d" : "#f5f5f7");
}

function setTextScale(value: number, persist = true): void {
  const textScale = Math.min(MAX_TEXT_SCALE, Math.max(MIN_TEXT_SCALE, Math.round(value * 10) / 10));
  preferences.textScale = textScale;
  document.documentElement.style.setProperty("--reader-text-scale", String(textScale));
  if (persist) savePreferences();
}

function adjustTextScale(delta: number): void {
  setTextScale(preferences.textScale + delta);
  setStatus(`Text size ${Math.round(preferences.textScale * 100)}%`);
}

function setSpeed(value: number, persist = true): void {
  const speed = Math.min(2, Math.max(0.5, Math.round(value * 10) / 10));
  preferences.speed = speed;
  speedSlider.value = String(speed);
  speedValue.textContent = `${speed.toFixed(1)}×`;
  applyPlaybackSpeed();
  speedPresetButtons.forEach((button) => {
    const active = Number(button.dataset.speed) === speed;
    button.classList.toggle("active", active);
    button.setAttribute("aria-pressed", String(active));
  });
  renderArticleTimer();
  if (persist) savePreferences();
}

function toggleArticleTimer(): void {
  preferences.articleTimerMode = preferences.articleTimerMode === "elapsed" ? "remaining" : "elapsed";
  savePreferences();
  renderArticleTimer();
}

function applyPlaybackSpeed(): void {
  const speed = Number(speedSlider.value);
  audioPlayer.defaultPlaybackRate = speed;
  audioPlayer.playbackRate = speed;
}

function selectedVoice(): string {
  return VOICES.has(voiceSelect.value) ? voiceSelect.value : DEFAULT_VOICE;
}

function cacheStateLabel(state: CacheState): string {
  if (state === "ready") return "Audio ready to play";
  if (state === "caching") return "Caching audio";
  if (state === "failed") return "Audio caching failed";
  return "Audio queued";
}

function currentSectionCacheState(sectionIndex: number): CacheState {
  if (!currentProjectId) return "queued";
  const relevant = cacheQueue.filter((task) => task.projectId === currentProjectId && task.voice === selectedVoice());
  if (!relevant.some((task) => task.sectionIndex === sectionIndex)) {
    return groupSpeechChunks(currentDocument.sections[sectionIndex]?.speech_text ?? "").length ? "queued" : "ready";
  }
  return sectionCacheState(relevant, currentProjectId, sectionIndex);
}

function invalidateCurrentProjectQueue(): void {
  cacheQueue = cacheQueue.filter((task) => task.projectId !== currentProjectId);
  renderCacheQueue();
  renderListenView();
}

function scheduleCacheQueueRebuild(delay = 600): void {
  cacheQueueBuildGeneration += 1;
  if (cacheQueueBuildTimer !== undefined) window.clearTimeout(cacheQueueBuildTimer);
  const generation = cacheQueueBuildGeneration;
  cacheQueueBuildTimer = window.setTimeout(() => {
    cacheQueueBuildTimer = undefined;
    void rebuildCacheQueue(generation);
  }, delay);
}

async function rebuildCacheQueue(generation: number): Promise<void> {
  if (generation !== cacheQueueBuildGeneration || cacheQueueBuilding) return;
  cacheQueueBuilding = true;
  try {
    const voice = selectedVoice();
    const storedProjects = await invoke<ProjectDocument[]>("list_project_documents");
    if (generation !== cacheQueueBuildGeneration) return;
    const projectDocuments = storedProjects.map((project) => project.project_id === currentProjectId
      ? { ...project, document: cloneDocument(currentDocument) }
      : project);
    const previous = new Map(cacheQueue.map((task) => [task.id, task]));
    const next: QueueTask[] = [];
    projectDocuments.forEach((project, projectOrder) => {
      project.document.sections.forEach((section, sectionIndex) => {
        groupSpeechChunks(section.speech_text).forEach((text, clipIndex) => {
          const id = cacheTaskId(project.project_id, voice, sectionIndex, clipIndex, text);
          const existing = previous.get(id);
          next.push({
            id,
            projectId: project.project_id,
            projectTitle: project.document.title.trim() || "Untitled reading",
            projectOrder,
            sectionIndex,
            clipIndex,
            text,
            voice,
            status: existing?.status === "failed" || existing?.status === "caching" ? existing.status : "queued",
            playbackOrder: existing?.playbackOrder,
            asset: existing?.asset,
            duration: existing?.duration,
            error: existing?.error,
            waiters: existing?.waiters ?? [],
          });
        });
      });
    });
    previous.forEach((task) => {
      if (!next.some((candidate) => candidate.id === task.id)) {
        task.waiters.forEach(({ reject }) => reject(new Error("Audio queue item changed")));
      }
    });

    const statuses = await Promise.all(projectDocuments.map(async (project) => {
      const tasks = next.filter((task) => task.projectId === project.project_id);
      if (!tasks.length) return { projectId: project.project_id, tasks, durations: [] as Array<number | null>, error: "" };
      try {
        return {
          projectId: project.project_id,
          tasks,
          durations: await invoke<Array<number | null>>("audio_cache_status", { projectId: project.project_id, texts: tasks.map((task) => task.text), voice }),
          error: "",
        };
      } catch (error) {
        return { projectId: project.project_id, tasks, durations: [] as Array<number | null>, error: String(error) };
      }
    }));
    if (generation !== cacheQueueBuildGeneration) return;
    statuses.forEach(({ tasks, durations, error }) => {
      tasks.forEach((task, index) => {
        const duration = durations[index];
        if (typeof duration === "number" && Number.isFinite(duration) && duration >= 0) {
          task.status = "ready";
          task.duration = duration / 1000;
          task.error = undefined;
        } else if (error) {
          task.status = "failed";
          task.error = error;
        } else {
          task.duration = undefined;
        }
      });
    });
    cacheQueue = next;
    renderCacheQueue();
    renderListenView();
    void runCacheQueueWorker();
  } finally {
    cacheQueueBuilding = false;
    if (generation !== cacheQueueBuildGeneration) void rebuildCacheQueue(cacheQueueBuildGeneration);
  }
}

function queueTaskCount(status?: CacheTaskStatus): number {
  return cacheQueue.filter((task) => !status || task.status === status).length;
}

function prioritizedQueueTasks(): QueueTask[] {
  return prioritizeCacheTasks(cacheQueue, currentProjectId, activeSection) as QueueTask[];
}

function notifyCacheStatus(message: string): void {
  if (!isPlaying) setStatus(message);
}

async function runCacheQueueWorker(): Promise<void> {
  if (cacheQueueWorkerRunning || cacheQueueBuilding || !runtimeStatus.engine_ready || runtimeStatus.error) return;
  cacheQueueWorkerRunning = true;
  try {
    while (runtimeStatus.engine_ready && !runtimeStatus.error) {
      const task = prioritizedQueueTasks().find((candidate) => candidate.status === "queued");
      if (!task) break;
      task.status = "caching";
      activeCacheTaskId = task.id;
      renderCacheQueue();
      renderListenView();
      notifyCacheStatus(`Caching ${queueTaskCount("ready") + 1} of ${cacheQueue.length} · Section ${task.sectionIndex + 1}`);
      try {
        const asset = await synthesize(task.text, task.voice, task.projectId);
        const current = cacheQueue.find((candidate) => candidate.id === task.id);
        if (current) {
          current.status = "ready";
          current.asset = asset;
          current.duration = asset.duration_ms / 1000;
          current.error = undefined;
          current.waiters.splice(0).forEach(({ resolve }) => resolve(asset));
          notifyCacheStatus(`Cached ${queueTaskCount("ready")} of ${cacheQueue.length}`);
        }
      } catch (error) {
        const current = cacheQueue.find((candidate) => candidate.id === task.id);
        if (current) {
          const message = String(error);
          current.status = "failed";
          current.error = message;
          current.waiters.splice(0).forEach(({ reject }) => reject(new Error(message)));
          notifyCacheStatus("Cache failed; continuing");
        }
      } finally {
        activeCacheTaskId = "";
        renderCacheQueue();
        renderListenView();
      }
    }
  } finally {
    cacheQueueWorkerRunning = false;
    if (cacheQueue.some((task) => task.status === "queued")) void runCacheQueueWorker();
    else {
      if (!isPlaying && cacheQueue.length && !queueTaskCount("failed")) setStatus("Audio queue ready");
      if (mode === "articles" || mode === "storage") void refreshStorageStats();
    }
  }
}

function retryCacheTask(taskId: string): void {
  const task = cacheQueue.find((candidate) => candidate.id === taskId);
  if (!task) return;
  task.status = "queued";
  task.error = undefined;
  renderCacheQueue();
  renderListenView();
  void runCacheQueueWorker();
}

function clearPlaybackQueue(): void {
  cacheQueue.forEach((task) => {
    task.playbackOrder = undefined;
    task.waiters.splice(0).forEach(({ reject }) => reject(new Error("Playback stopped")));
  });
}

function waitForQueueTask(task: QueueTask, token: number): Promise<AudioAsset> {
  if (task.status === "ready" && task.asset) return Promise.resolve(task.asset);
  if (task.status === "failed") return Promise.reject(new Error(task.error ?? "Audio caching failed"));
  if (task.status === "ready") task.status = "queued";
  return new Promise((resolve, reject) => task.waiters.push({ resolve, reject, playbackToken: token }));
}

function requestPlaybackAssets(taskIds: string[], token: number): Promise<AudioAsset>[] {
  const tasks = taskIds.map((id) => cacheQueue.find((task) => task.id === id)).filter((task): task is QueueTask => Boolean(task));
  tasks.forEach((task, index) => {
    task.playbackOrder = index;
  });
  const assets = tasks.map((task) => waitForQueueTask(task, token));
  renderCacheQueue();
  renderListenView();
  void runCacheQueueWorker();
  return assets;
}

function setStatus(message: string): void {
  activityStatus.textContent = message;
}

function renderCacheProject(list: HTMLOListElement, summary: ReturnType<typeof summarizeCacheProjects>[number]): void {
  const row = document.createElement("li");
  const state = summary.caching ? "caching" : summary.failed ? "failed" : summary.ready === summary.total ? "ready" : "queued";
  row.className = `cache-task ${state}`;
  const main = document.createElement("button");
  main.type = "button";
  main.className = "cache-task-main";
  const parts = [`${summary.ready}/${summary.total} cached`];
  if (summary.caching) parts.push("caching");
  if (summary.queued) parts.push(`${summary.queued} pending`);
  if (summary.failed) parts.push(`${summary.failed} failed`);
  main.innerHTML = `<span class="cache-task-preview">${escapeHtml(summary.projectTitle)}</span><span class="cache-task-state">${parts.join(" · ")}</span>`;
  main.addEventListener("click", () => void navigateToCacheTask(summary.nextTask as QueueTask));
  row.appendChild(main);
  if (summary.failed) {
    const retry = document.createElement("button");
    retry.type = "button";
    retry.className = "cache-retry-button";
    retry.textContent = "Retry failed";
    retry.addEventListener("click", () => {
      cacheQueue.filter((task) => task.projectId === summary.projectId && task.status === "failed").forEach((task) => retryCacheTask(task.id));
    });
    row.appendChild(retry);
  }
  list.appendChild(row);
}

function renderCacheQueue(): void {
  const ready = queueTaskCount("ready");
  const failed = queueTaskCount("failed");
  const caching = queueTaskCount("caching");
  const summaries = summarizeCacheProjects(cacheQueue, currentProjectId, activeSection);
  const complete = summaries.filter((summary) => summary.ready === summary.total).length;
  cachePanelCount.textContent = `${complete}/${summaries.length}`;
  cachePanelSummary.textContent = cacheQueue.length
    ? `${ready}/${cacheQueue.length} clips ready${caching ? ` · ${caching} caching` : ""}${queueTaskCount("queued") ? ` · ${queueTaskCount("queued")} pending` : ""}${failed ? ` · ${failed} failed` : ""}`
    : "No audio to cache";
  cachePanelDot.className = `status-dot ${failed ? "error" : complete === summaries.length && summaries.length ? "ready" : "busy"}`;
  const active = cacheQueue.find((task) => task.id === activeCacheTaskId);
  cacheActiveItem.hidden = !active;
  cacheActiveItem.textContent = active ? `Caching now · ${active.projectTitle} · Section ${active.sectionIndex + 1} · ${active.text}` : "";
  cacheQueueList.replaceChildren();
  summaries.forEach((summary) => renderCacheProject(cacheQueueList, summary));
  renderArticleTimer();
}

function openCachePanel(): void {
  cachePanelOpen = true;
  cachePanelBackdrop.hidden = false;
  cachePanelButton.setAttribute("aria-expanded", "true");
  closeCachePanelButton.focus();
}

function closeCachePanel(): void {
  if (!cachePanelOpen) return;
  cachePanelOpen = false;
  cachePanelBackdrop.hidden = true;
  cachePanelButton.setAttribute("aria-expanded", "false");
  cachePanelButton.focus();
}

async function navigateToCacheTask(task: QueueTask): Promise<void> {
  closeCachePanel();
  stopPlayback(false);
  if (task.projectId !== currentProjectId) await switchProject(task.projectId);
  if (task.projectId !== currentProjectId) return;
  activeSection = task.sectionIndex;
  updateMode("listen");
  renderListenView();
  renderCacheQueue();
  window.requestAnimationFrame(() => {
    markdownView.querySelector<HTMLElement>(`[data-section-index="${task.sectionIndex}"]`)?.scrollIntoView({ behavior: "smooth", block: "center" });
  });
  setStatus(`Ready to play Section ${task.sectionIndex + 1}`);
  void runCacheQueueWorker();
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

function formatDate(timestamp: number | null): string {
  return timestamp ? new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(timestamp * 1000)) : "—";
}

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => {
    const entities: Record<string, string> = {
      "&": "&amp;",
      "<": "&lt;",
      ">": "&gt;",
      '"': "&quot;",
      "'": "&#39;",
    };
    return entities[character];
  });
}

function formatArticleTime(seconds: number): string {
  const totalSeconds = Math.max(0, Math.round(seconds));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const remainder = totalSeconds % 60;
  return hours
    ? `${hours}:${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`
    : `${minutes}:${String(remainder).padStart(2, "0")}`;
}

function renderArticleTimer(): void {
  const hasArticle = Boolean(currentProjectId && currentDocument.sections.length);
  articleTimer.disabled = !hasArticle;
  if (!hasArticle) {
    articleTimerValue.textContent = "—";
    articleTimer.setAttribute("aria-pressed", "false");
    articleTimer.title = "Article timer unavailable";
    articleTimer.setAttribute("aria-label", "Article timer unavailable");
    return;
  }

  const items = cacheQueue
    .filter((task) => task.projectId === currentProjectId && task.voice === selectedVoice())
    .sort((left, right) => left.sectionIndex - right.sectionIndex || left.clipIndex - right.clipIndex)
    .map((task) => ({
      id: task.id,
      duration: task.status === "ready" && task.duration !== undefined ? task.duration : null,
    }));
  const expectedClipCount = currentDocument.sections.reduce((count, section) => count + groupSpeechChunks(section.speech_text).length, 0);
  const position = playbackPosition?.voice === selectedVoice() ? playbackPosition : null;
  const timing = items.length === expectedClipCount
    ? articleTiming(items, position?.clipId ?? null, position?.currentTime ?? 0, preferences.speed)
    : { total: null, elapsed: 0, remaining: 0 };

  if (timing.total === null) {
    articleTimerValue.textContent = "Calculating…";
    articleTimer.setAttribute("aria-pressed", String(preferences.articleTimerMode === "remaining"));
    articleTimer.title = "Calculating article timing from cached audio";
    articleTimer.setAttribute("aria-label", "Article timing is being calculated");
    return;
  }

  const showingRemaining = preferences.articleTimerMode === "remaining";
  const primary = showingRemaining ? timing.remaining : timing.elapsed;
  articleTimerValue.textContent = `${formatArticleTime(primary)} / ${formatArticleTime(timing.total)}`;
  articleTimer.setAttribute("aria-pressed", String(showingRemaining));
  const nextMode = showingRemaining ? "elapsed" : "remaining";
  articleTimer.title = showingRemaining ? "Showing time remaining; click to show time covered" : "Showing time covered; click to show time remaining";
  articleTimer.setAttribute("aria-label", `${showingRemaining ? "Time remaining" : "Time covered"} ${formatArticleTime(primary)} of ${formatArticleTime(timing.total)}. Click to show ${nextMode} time`);
}

interface TextNodeRange {
  node: Text;
  start: number;
  end: number;
}

function collectTextNodes(element: HTMLElement): TextNodeRange[] {
  const nodes: TextNodeRange[] = [];
  const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
  let offset = 0;
  let current: Node | null;
  while ((current = walker.nextNode())) {
    const node = current as Text;
    const parent = node.parentElement;
    if (!parent || parent.closest(".section-play-button, .cache-indicator, .playback-marker, mark, .katex-mathml, [hidden], [aria-hidden='true']")) continue;
    if (!node.data) continue;
    nodes.push({ node, start: offset, end: offset + node.data.length });
    offset += node.data.length;
  }
  return nodes;
}

function highlightTextRanges(nodes: TextNodeRange[], ranges: TextMatch[], className: string, onMark?: (mark: HTMLElement, rangeIndex: number) => void): void {
  nodes.forEach(({ node, start: nodeStart, end: nodeEnd }) => {
    const overlaps = ranges
      .map((range, index) => ({ range, index }))
      .filter(({ range }) => range.start < nodeEnd && range.end > nodeStart);
    if (!overlaps.length) return;
    const fragment = document.createDocumentFragment();
    let cursor = 0;
    overlaps.forEach(({ range, index }) => {
      const start = Math.max(range.start, nodeStart) - nodeStart;
      const end = Math.min(range.end, nodeEnd) - nodeStart;
      if (start > cursor) fragment.appendChild(document.createTextNode(node.data.slice(cursor, start)));
      const mark = document.createElement("mark");
      mark.className = className;
      mark.textContent = node.data.slice(start, end);
      fragment.appendChild(mark);
      onMark?.(mark, index);
      cursor = end;
    });
    if (cursor < node.data.length) fragment.appendChild(document.createTextNode(node.data.slice(cursor)));
    node.replaceWith(fragment);
  });
}

function clearSearchHighlights(): void {
  markdownView.querySelectorAll<HTMLElement>("mark.search-match").forEach((mark) => {
    mark.replaceWith(document.createTextNode(mark.textContent ?? ""));
  });
  markdownView.normalize();
  searchMatchGroups = [];
}

function clearNarrationHighlights(): void {
  markdownView.querySelectorAll<HTMLElement>("mark.narration-match").forEach((mark) => {
    mark.replaceWith(document.createTextNode(mark.textContent ?? ""));
  });
  markdownView.normalize();
}

function highlightNarrationTerms(element: HTMLElement, terms: string[]): void {
  const expression = new RegExp(`\\b(${terms.map((term) => term.replace(/[.*+?^${}()|[\\]\\]/g, "\\$&")).join("|")})\\b`, "giu");
  const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
  const nodes: Text[] = [];
  let current: Node | null;
  while ((current = walker.nextNode())) {
    const node = current as Text;
    if (node.parentElement?.closest("mark, .section-play-button, .cache-indicator, .playback-marker")) continue;
    nodes.push(node);
  }
  nodes.forEach((node) => {
    expression.lastIndex = 0;
    if (!expression.test(node.data)) return;
    expression.lastIndex = 0;
    const fragment = document.createDocumentFragment();
    let cursor = 0;
    for (const match of node.data.matchAll(expression)) {
      const start = match.index ?? 0;
      if (start > cursor) fragment.appendChild(document.createTextNode(node.data.slice(cursor, start)));
      const mark = document.createElement("mark");
      mark.className = "narration-match";
      mark.textContent = match[0];
      fragment.appendChild(mark);
      cursor = start + match[0].length;
    }
    if (cursor < node.data.length) fragment.appendChild(document.createTextNode(node.data.slice(cursor)));
    node.replaceWith(fragment);
  });
}

function renderNarrationHighlights(): void {
  clearNarrationHighlights();
  if (!searchBar.hidden) return;
  const isSavedPositionInActiveSection = playbackPosition?.sectionIndex === activeSection;
  const clipIndex = isSavedPositionInActiveSection ? playbackPosition!.clipIndex : 0;
  const section = markdownView.querySelector<HTMLElement>(`.document-section[data-section-index="${activeSection}"]`);
  const speech = groupSpeechChunks(currentDocument.sections[activeSection]?.speech_text ?? "")[clipIndex];
  if (!section || !speech) return;
  const blocks = Array.from(section.querySelectorAll<HTMLElement>("h1, h2, h3, h4, h5, h6, p, li, blockquote, pre"));
  const blockNodes = blocks.map((block) => collectTextNodes(block));
  narrationHighlights(blockNodes.map((nodes) => nodes.map(({ node }) => node.data).join("")), speech).forEach((match) => {
    if (match.mode === "exact") highlightTextRanges(blockNodes[match.blockIndex], match.ranges, "narration-match");
    else highlightNarrationTerms(blocks[match.blockIndex], match.terms);
  });
}

function updateSearchResultUi(scrollToActive = false): void {
  const count = searchMatchGroups.length;
  if (!count) {
    searchActiveIndex = 0;
    searchResultCount.textContent = searchQuery ? "No matches" : "0/0";
    searchResultCount.setAttribute("aria-label", searchQuery ? "No matches" : "No search results");
    return;
  }

  searchActiveIndex = Math.min(searchActiveIndex, count - 1);
  searchMatchGroups.forEach((group, index) => {
    group.forEach((mark) => mark.classList.toggle("active", index === searchActiveIndex));
  });
  searchResultCount.textContent = `${searchActiveIndex + 1}/${count}`;
  searchResultCount.setAttribute("aria-label", `Search result ${searchActiveIndex + 1} of ${count}`);
  if (scrollToActive) {
    searchMatchGroups[searchActiveIndex][0]?.scrollIntoView({ behavior: "smooth", block: "center" });
  }
}

function renderSearchHighlights(scrollToActive = false): void {
  clearNarrationHighlights();
  clearSearchHighlights();
  if (searchBar.hidden || !searchQuery) {
    updateSearchResultUi();
    return;
  }

  markdownView.querySelectorAll<HTMLElement>(".document-section").forEach((section) => {
    const nodes = collectTextNodes(section);
    const source = nodes.map(({ node }) => node.data).join("");
    const matches = literalTextMatches(source, searchQuery);
    matches.forEach(() => searchMatchGroups.push([]));
    const groupStart = searchMatchGroups.length - matches.length;
    highlightTextRanges(nodes, matches, "search-match", (mark, index) => {
      mark.dataset.searchIndex = String(groupStart + index);
      searchMatchGroups[groupStart + index].push(mark);
    });
  });
  updateSearchResultUi(scrollToActive);
}

function navigateSearch(direction: 1 | -1): void {
  if (!searchMatchGroups.length) return;
  searchActiveIndex = (searchActiveIndex + direction + searchMatchGroups.length) % searchMatchGroups.length;
  updateSearchResultUi(true);
}

function openSearch(): void {
  if (!currentDocument.sections.length) return;
  if (searchBar.hidden) {
    searchReturnFocus = mode === "edit"
      ? searchButton
      : document.activeElement instanceof HTMLElement ? document.activeElement : searchButton;
    if (mode === "edit") updateMode("listen");
    searchBar.hidden = false;
    searchButton.setAttribute("aria-expanded", "true");
  }
  renderSearchHighlights(true);
  searchInput.focus();
  searchInput.select();
}

function closeSearch(): void {
  searchBar.hidden = true;
  searchButton.setAttribute("aria-expanded", "false");
  clearSearchHighlights();
  updateSearchResultUi();
  renderNarrationHighlights();
  const returnFocus = searchReturnFocus;
  searchReturnFocus = null;
  if (returnFocus?.isConnected) returnFocus.focus();
}

function renderMarkdown(source: string): string {
  const fencedCode: string[] = [];
  const inlineCode: string[] = [];
  const mathBlocks: Array<{ tex: string; display: boolean }> = [];
  let protectedSource = source.replace(/(^|\n)(```[\s\S]*?```|~~~[\s\S]*?~~~)/g, (_match, prefix: string, block: string) => {
    const body = block.replace(/^(```|~~~)[^\n]*\n?/, "").replace(/(```|~~~)\s*$/, "");
    const token = `KOKORO_FENCE_${fencedCode.length}_TOKEN`;
    fencedCode.push(`<pre><code>${escapeHtml(body)}</code></pre>`);
    return `${prefix}${token}\n`;
  });
  protectedSource = protectedSource.replace(/`([^`\n]+)`/g, (_match, code: string) => {
    const token = `KOKORO_INLINE_${inlineCode.length}_TOKEN`;
    inlineCode.push(`<code>${escapeHtml(code)}</code>`);
    return token;
  });
  protectedSource = protectedSource.replace(/\$\$([\s\S]+?)\$\$/g, (_match, tex: string) => {
    const token = `KOKORO_MATH_${mathBlocks.length}_TOKEN`;
    mathBlocks.push({ tex, display: true });
    return token;
  });
  protectedSource = protectedSource.replace(/\$([^$\n]+?)\$/g, (_match, tex: string) => {
    const token = `KOKORO_MATH_${mathBlocks.length}_TOKEN`;
    mathBlocks.push({ tex, display: false });
    return token;
  });

  let html = markdown.render(protectedSource);
  fencedCode.forEach((code, index) => {
    html = html.replace(`KOKORO_FENCE_${index}_TOKEN`, code);
  });
  inlineCode.forEach((code, index) => {
    html = html.replace(`KOKORO_INLINE_${index}_TOKEN`, code);
  });
  mathBlocks.forEach(({ tex, display }, index) => {
    const rendered = katex.renderToString(tex.trim(), {
      displayMode: display,
      throwOnError: false,
      trust: false,
      output: "htmlAndMathml",
    });
    html = html.replace(`KOKORO_MATH_${index}_TOKEN`, rendered);
  });
  return html;
}

function activateSection(index: number, scrollIntoView = false, resume = true): void {
  const togglesCurrentAudio = resume && index === activeSection && isPlaying && Boolean(audioPlayer.src);
  activeSection = index;
  renderListenView();
  if (scrollIntoView) {
    window.requestAnimationFrame(() => {
      markdownView.querySelector<HTMLElement>(`[data-section-index="${index}"]`)?.scrollIntoView({ behavior: "smooth", block: "center" });
    });
  }
  if (togglesCurrentAudio) togglePlayback();
  else void playFromSection(index, resume);
}

function setNavigatorFocus(index: number | null): void {
  if (index === null) sectionNavigator.removeAttribute("data-focus-index");
  else sectionNavigator.dataset.focusIndex = String(index);
  const center = index ?? activeSection;
  sectionNavigatorList.querySelectorAll<HTMLElement>(".section-navigator-item").forEach((item) => {
    const itemIndex = Number(item.dataset.sectionIndex);
    const distance = Math.abs(itemIndex - center);
    const width = 8 + Math.round(24 * Math.exp(-(distance * distance) / 3.2));
    item.style.setProperty("--navigator-line-width", `${width}px`);
    item.classList.toggle("focused", index === itemIndex);
    item.classList.toggle("active", itemIndex === activeSection);
    if (itemIndex === activeSection) item.setAttribute("aria-current", "true");
    else item.removeAttribute("aria-current");
  });
}

function moveSection(offset: number): void {
  const next = Math.min(currentDocument.sections.length - 1, Math.max(0, activeSection + offset));
  if (next === activeSection) {
    setStatus(offset < 0 ? "Already at first section" : "Already at last section");
    return;
  }
  activateSection(next, true);
}

function jumpToPlaybackMarker(): void {
  const sectionIndex = playbackPosition?.sectionIndex;
  if (sectionIndex === undefined) {
    setStatus("No saved playback position");
    return;
  }
  const section = markdownView.querySelector<HTMLElement>(`[data-section-index="${sectionIndex}"]`);
  if (!section) {
    setStatus("Saved playback position is unavailable");
    return;
  }
  section.scrollIntoView({ behavior: "smooth", block: "center" });
  setStatus(`Jumped to saved position in Section ${sectionIndex + 1}`);
}

function renderSectionNavigator(): void {
  sectionNavigatorList.replaceChildren();
  sectionNavigator.hidden = currentDocument.sections.length === 0;
  sectionNavigator.setAttribute("aria-label", `Document sections. Current section ${activeSection + 1} of ${currentDocument.sections.length}`);
  currentDocument.sections.forEach((section, index) => {
    const preview = section.speech_text.trim().replace(/\s+/g, " ") || `Section ${index + 1}`;
    const button = document.createElement("button");
    button.type = "button";
    button.className = `section-navigator-item${index === activeSection ? " active" : ""}`;
    button.dataset.sectionIndex = String(index);
    button.setAttribute("aria-label", `Section ${index + 1}: ${preview}`);
    button.title = `Section ${index + 1}: ${preview}`;
    if (index === activeSection) button.setAttribute("aria-current", "true");
    const lineWrap = document.createElement("span");
    lineWrap.className = "section-navigator-line-wrap";
    const line = document.createElement("span");
    line.className = "section-navigator-line";
    line.style.width = `${Math.min(34, Math.max(12, preview.length / 2))}px`;
    lineWrap.appendChild(line);
    button.appendChild(lineWrap);
    button.addEventListener("mouseenter", () => setNavigatorFocus(index));
    button.addEventListener("focus", () => setNavigatorFocus(index));
    button.addEventListener("click", () => activateSection(index, true));
    sectionNavigatorList.appendChild(button);
  });
  setNavigatorFocus(null);
}

function updatePlaybackVisuals(): void {
  renderArticleTimer();
  markdownView.querySelectorAll<HTMLElement>(".document-section").forEach((section) => {
    const index = Number(section.dataset.sectionIndex);
    const active = index === activeSection;
    section.classList.toggle("active", active);
    const sectionPlayButton = section.querySelector<HTMLButtonElement>(".section-play-button");
    const playing = active && isPlaying && !audioPlayer.paused;
    if (sectionPlayButton) {
      sectionPlayButton.classList.toggle("playing", playing);
      sectionPlayButton.setAttribute("aria-label", `${playing ? "Pause" : "Play"} section ${index + 1}`);
      sectionPlayButton.title = `${playing ? "Pause" : "Play"} section ${index + 1}`;
      const icon = sectionPlayButton.querySelector<SVGPathElement>("path");
      icon?.setAttribute("d", playing ? "M6 5h3v10H6zM11 5h3v10h-3z" : "m7 4 9 6-9 6V4Z");
    }
    const marker = section.querySelector<HTMLElement>(".playback-marker");
    const showMarker = active && playbackPosition?.sectionIndex === index;
    if (marker) {
      marker.hidden = !showMarker;
      if (showMarker) {
        const top = 14 + Math.max(0, section.offsetHeight - 28) * playbackPosition!.progress;
        marker.style.top = `${top}px`;
      }
    }
  });
  const focusIndex = sectionNavigator.dataset.focusIndex;
  setNavigatorFocus(focusIndex === undefined ? null : Number(focusIndex));
  sectionNavigator.setAttribute("aria-label", `Document sections. Current section ${activeSection + 1} of ${currentDocument.sections.length}`);
  renderNarrationHighlights();
}

function renderListenView(): void {
  markdownView.innerHTML = "";
  sectionSummary.textContent = `${currentDocument.sections.length} section${currentDocument.sections.length === 1 ? "" : "s"}`;
  renderArticleTimer();
  renderSectionNavigator();
  if (!currentDocument.sections.length) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    const noProjects = !currentProjectId;
    empty.innerHTML = `
      <div class="empty-state-inner">
        <div class="empty-state-icon" aria-hidden="true"><svg width="22" height="22" viewBox="0 0 20 20"><path d="M4 4.5h12v11H4zM7 8h6M7 11h4" /></svg></div>
        <strong>${noProjects ? "No reading projects" : "Your reading is empty"}</strong>
        <p>${noProjects ? "Send content from Codex to create the next isolated project." : "Open Edit and paste Markdown to create a document for Kokoro."}</p>
      </div>`;
    markdownView.appendChild(empty);
    renderSearchHighlights();
    return;
  }
  currentDocument.sections.forEach((section, index) => {
    const sectionElement = document.createElement("section");
    const cacheState = currentSectionCacheState(index);
    sectionElement.className = `document-section${index === activeSection ? " active" : ""}`;
    sectionElement.dataset.sectionIndex = String(index);
    sectionElement.innerHTML = renderMarkdown(section.markdown);
    const sectionPlayButton = document.createElement("button");
    sectionPlayButton.type = "button";
    sectionPlayButton.className = "section-play-button";
    sectionPlayButton.setAttribute("aria-label", `Play section ${index + 1}`);
    sectionPlayButton.title = `Play section ${index + 1}`;
    sectionPlayButton.innerHTML = `<svg width="15" height="15" viewBox="0 0 20 20" aria-hidden="true"><path d="m7 4 9 6-9 6V4Z" /></svg>`;
    sectionPlayButton.addEventListener("click", (event) => {
      event.stopPropagation();
      activateSection(index);
    });
    sectionElement.appendChild(sectionPlayButton);
    const cacheIndicator = document.createElement("span");
    cacheIndicator.className = `cache-indicator ${cacheState}`;
    cacheIndicator.setAttribute("role", "img");
    cacheIndicator.setAttribute("aria-label", cacheStateLabel(cacheState));
    cacheIndicator.title = cacheStateLabel(cacheState);
    sectionElement.appendChild(cacheIndicator);
    const playbackMarker = document.createElement("span");
    playbackMarker.className = "playback-marker";
    playbackMarker.hidden = true;
    playbackMarker.setAttribute("aria-hidden", "true");
    sectionElement.appendChild(playbackMarker);
    sectionElement.addEventListener("click", (event) => {
      const link = (event.target as Element).closest<HTMLAnchorElement>("a[href]");
      if (link && sectionElement.contains(link)) {
        event.preventDefault();
        const url = new URL(link.href);
        if (url.protocol === "http:" || url.protocol === "https:") {
          void openUrl(url).catch(() => setStatus("Could not open link in your browser"));
        }
        return;
      }
    });
    markdownView.appendChild(sectionElement);
  });
  updatePlaybackVisuals();
  renderSearchHighlights();
}

function renderNarrationEditor(): void {
  narrationEditor.innerHTML = "";
  currentDocument.sections.forEach((section, index) => {
    const wrapper = document.createElement("div");
    wrapper.className = "narration-section";
    const header = document.createElement("div");
    header.className = "narration-section-header";
    const label = document.createElement("label");
    label.textContent = `Section ${index + 1}`;
    label.htmlFor = `narration-${index}`;
    const speechMode = document.createElement("span");
    speechMode.className = `speech-mode${section.speech_mode === "custom" ? " custom" : ""}`;
    speechMode.textContent = section.speech_mode === "custom" ? "Custom" : "Automatic";
    header.append(label, speechMode);
    const textarea = document.createElement("textarea");
    textarea.id = `narration-${index}`;
    textarea.value = section.speech_text;
    textarea.dataset.sectionIndex = String(index);
    textarea.addEventListener("input", () => {
      clearEditTarget();
      editorDirty = true;
      section.speech_text = textarea.value;
      section.speech_mode = "custom";
      clearPlaybackPosition(currentProjectId);
      invalidateCurrentProjectQueue();
      scheduleSave();
      scheduleValidation();
      renderListenView();
      scheduleCacheQueueRebuild();
    });
    const reset = document.createElement("button");
    reset.type = "button";
    reset.className = "reset-button";
    reset.textContent = section.speech_mode === "custom" ? "Reset to automatic" : "Automatic narration";
    reset.disabled = section.speech_mode === "automatic";
    reset.addEventListener("click", () => {
      section.speech_text = deriveSpeechText(section.markdown);
      section.speech_mode = "automatic";
      clearPlaybackPosition(currentProjectId);
      invalidateCurrentProjectQueue();
      renderNarrationEditor();
      renderListenView();
      scheduleSave();
      editorDirty = true;
      scheduleValidation();
      scheduleCacheQueueRebuild();
    });
    wrapper.append(header, textarea, reset);
    narrationEditor.appendChild(wrapper);
  });
}

function renderEditors(): void {
  markdownEditor.value = currentDocument.sections.map((section) => section.markdown).join("\n\n<!-- kokoro-reader-section -->\n\n");
  renderNarrationEditor();
  scheduleValidation();
}

function projectLabel(project: ProjectSummary): string {
  const title = project.title.trim() || "Untitled reading";
  const status = project.read ? " · ✓ Read" : "";
  return `${title}${status} · ${project.project_id.slice(-8)}`;
}

function renderProjectMetadata(): void {
  const hasProject = Boolean(currentProjectId);
  markReadButton.disabled = !hasProject;
  markReadButton.textContent = currentMetadata.read ? "Mark unread" : "Mark read";
  markReadButton.setAttribute("aria-pressed", String(currentMetadata.read));
  backToCodexButton.hidden = !currentMetadata.codex_url;
  backToCodexButton.disabled = !currentMetadata.codex_url;
}

function renderProjectPicker(): void {
  projectSelect.replaceChildren();
  projects.forEach((project) => {
    const option = document.createElement("option");
    option.value = project.project_id;
    option.textContent = projectLabel(project);
    option.title = project.project_id;
    projectSelect.appendChild(option);
  });
  projectSelect.value = currentProjectId;
  projectSelect.disabled = !currentProjectId;
  deleteProjectButton.disabled = !currentProjectId;
  renderProjectMetadata();
}

function articleRows(): ProjectSummary[] {
  return projects
    .filter((project) => articleFilter === "all" || (articleFilter === "read" ? project.read : !project.read))
    .sort((left, right) => {
      const leftValue = articleSort === "created_at" ? left.created_at : left.read_at ?? -1;
      const rightValue = articleSort === "created_at" ? right.created_at : right.read_at ?? -1;
      return (leftValue - rightValue) * (articleSortDescending ? -1 : 1);
    });
}

function projectCacheUsage(projectId: string): ProjectCacheUsage | undefined {
  return storageStats?.projects.find((usage) => usage.project_id === projectId);
}

function renderArticles(): void {
  const rows = articleRows();
  articlesTableBody.replaceChildren();
  articlesSelectAll.checked = rows.length > 0 && rows.every((project) => selectedArticleIds.has(project.project_id));
  articlesSelectAll.indeterminate = rows.some((project) => selectedArticleIds.has(project.project_id)) && !articlesSelectAll.checked;
  deleteSelectedButton.disabled = selectedArticleIds.size === 0;
  document.querySelectorAll<HTMLButtonElement>("[data-article-sort]").forEach((button) => {
    const active = button.dataset.articleSort === articleSort;
    button.classList.toggle("active", active);
    button.textContent = `${button.dataset.articleSort === "created_at" ? "Date added" : "Marked read"}${active ? (articleSortDescending ? " ↓" : " ↑") : ""}`;
  });
  if (!rows.length) {
    const row = document.createElement("tr");
    row.innerHTML = `<td colspan="7" class="table-empty">No ${articleFilter === "all" ? "articles" : articleFilter} articles.</td>`;
    articlesTableBody.appendChild(row);
    return;
  }
  rows.forEach((project) => {
    const row = document.createElement("tr");
    row.className = project.read ? "read" : "unread";
    const usage = projectCacheUsage(project.project_id);
    const selector = document.createElement("input");
    selector.type = "checkbox";
    selector.checked = selectedArticleIds.has(project.project_id);
    selector.setAttribute("aria-label", `Select ${project.title}`);
    selector.addEventListener("change", () => {
      if (selector.checked) selectedArticleIds.add(project.project_id);
      else selectedArticleIds.delete(project.project_id);
      renderArticles();
    });
    const selectionCell = document.createElement("td");
    selectionCell.appendChild(selector);
    row.appendChild(selectionCell);
    row.insertAdjacentHTML("beforeend", `<td class="article-title">${escapeHtml(project.title.trim() || "Untitled reading")}</td><td><span class="read-status ${project.read ? "read" : "unread"}">${project.read ? "Read" : "Unread"}</span></td><td>${formatDate(project.created_at)}</td><td class="read-date">${formatDate(project.read_at)}</td><td>${usage ? formatBytes(usage.bytes) : "—"}</td>`);
    const actions = document.createElement("td");
    actions.className = "article-actions";
    const open = document.createElement("button");
    open.className = "quiet-button";
    open.textContent = "Open";
    open.addEventListener("click", () => void switchProject(project.project_id).then(() => updateMode("listen")));
    const read = document.createElement("button");
    read.className = "quiet-button";
    read.textContent = project.read ? "Unread" : "Read";
    read.addEventListener("click", () => void setArticleRead(project.project_id, !project.read));
    const remove = document.createElement("button");
    remove.className = "quiet-button article-delete";
    remove.textContent = "Delete";
    remove.addEventListener("click", () => openDeleteProjectDialog([project.project_id]));
    actions.append(open, read, remove);
    row.appendChild(actions);
    articlesTableBody.appendChild(row);
  });
}

function renderStorage(): void {
  if (!storageStats) return;
  storageTotal.textContent = `${formatBytes(storageStats.total_bytes)} used by Kokoro Reader`;
  storageAreas.replaceChildren();
  const areas: Array<[string, StorageArea]> = [["Models", storageStats.models], ["Audio cache", storageStats.audio_cache], ["Project files", storageStats.project_files], ["App data", storageStats.app_data]];
  areas.forEach(([label, area]) => {
    const card = document.createElement("section");
    card.className = "storage-area";
    card.innerHTML = `<strong>${label}</strong><span>${formatBytes(area.bytes)}</span><code>${escapeHtml(area.path)}</code>`;
    const reveal = document.createElement("button");
    reveal.className = "quiet-button";
    reveal.textContent = "Reveal in Finder";
    reveal.disabled = !area.exists;
    reveal.addEventListener("click", () => void revealItemInDir(area.path).catch(() => setStatus("Could not reveal this folder")));
    card.appendChild(reveal);
    storageAreas.appendChild(card);
  });
  storageProjectList.replaceChildren();
  storageStats.projects.slice(0, 5).forEach((usage) => {
    const item = document.createElement("li");
    item.innerHTML = `<span>${escapeHtml(usage.title.trim() || "Untitled reading")}</span><span>${formatBytes(usage.bytes)} · ${usage.cached_clips} clips</span>`;
    item.addEventListener("click", () => void switchProject(usage.project_id).then(() => updateMode("listen")));
    storageProjectList.appendChild(item);
  });
}

async function refreshStorageStats(): Promise<void> {
  refreshStorageButton.disabled = true;
  try {
    storageStats = await invoke<StorageStats>("storage_stats");
    renderStorage();
    renderArticles();
  } catch (error) {
    storageTotal.textContent = `Storage unavailable: ${String(error)}`;
  } finally {
    refreshStorageButton.disabled = false;
  }
}

function renderDocument(): void {
  renderProjectPicker();
  titleInput.value = currentDocument.title;
  titleInput.disabled = !currentProjectId;
  editModeButton.disabled = !currentProjectId;
  reloadFilesButton.disabled = !currentProjectId;
  searchButton.disabled = !currentProjectId || !currentDocument.sections.length;
  if (searchButton.disabled && !searchBar.hidden) closeSearch();
  if (!currentProjectId && (mode === "listen" || mode === "edit")) mode = "listen";
  activeSection = Math.min(activeSection, Math.max(currentDocument.sections.length - 1, 0));
  renderListenView();
  renderEditors();
  renderCacheQueue();
  renderArticles();
  renderStorage();
  restoreButton.disabled = !hasPrevious;
}

async function setArticleRead(projectId: string, read: boolean): Promise<boolean> {
  if (!projectId) return false;
  const summary = projects.find((project) => project.project_id === projectId);
  if (summary?.read === read) return true;
  try {
    const metadata = await invoke<ProjectMetadata>("set_project_read", { projectId, read });
    if (projectId === currentProjectId) currentMetadata = metadata;
    if (summary) {
      summary.read = metadata.read;
      summary.read_at = metadata.read_at;
    }
    renderProjectPicker();
    renderArticles();
    return true;
  } catch (error) {
    setStatus(`Read status failed: ${String(error)}`);
    return false;
  }
}

async function setProjectRead(read: boolean): Promise<boolean> {
  return setArticleRead(currentProjectId, read);
}

async function openCodexOrigin(): Promise<void> {
  const url = currentMetadata.codex_url;
  if (!url) return;
  try {
    await openUrl(url);
    setStatus("Opened in Codex");
  } catch {
    setStatus("Could not open Codex task");
  }
}

function updateSectionsFromMarkdown(): void {
  const blocks = splitMarkdownSections(markdownEditor.value);
  currentDocument.sections = reconcileSections(blocks, currentDocument.sections);
  activeSection = Math.min(activeSection, Math.max(currentDocument.sections.length - 1, 0));
  clearPlaybackPosition(currentProjectId);
  invalidateCurrentProjectQueue();
  renderNarrationEditor();
  renderListenView();
  scheduleSave();
  scheduleCacheQueueRebuild();
}

function cloneDocument(source: Document): Document {
  return {
    title: source.title,
    sections: source.sections.map((section) => ({ ...section })),
  };
}

function cancelScheduledSave(): void {
  if (saveTimer !== undefined) {
    window.clearTimeout(saveTimer);
    saveTimer = undefined;
  }
}

async function flushPendingSave(): Promise<boolean> {
  if (saveTimer === undefined || !currentProjectId) return true;
  cancelScheduledSave();
  const projectId = currentProjectId;
  const document = cloneDocument(currentDocument);
  try {
    saveInFlight = true;
    const saved = await invoke<ProjectDocument>("save_document", { projectId, document, expectedRevision: currentRevision });
    if (saved.project_id === currentProjectId) {
      currentDocument = saved.document;
      currentMetadata = saved.metadata;
      currentRevision = saved.revision;
      editorDirty = false;
    }
    return true;
  } catch (error) {
    if (String(error).startsWith("STALE_DRAFT:")) showDraftConflict(pendingExternalRevision);
    setStatus(`Save failed: ${String(error)}`);
    return false;
  } finally {
    saveInFlight = false;
  }
}

function scheduleSave(): void {
  cancelScheduledSave();
  const projectId = currentProjectId;
  const document = cloneDocument(currentDocument);
  saveTimer = window.setTimeout(() => {
    saveTimer = undefined;
    if (!projectId) return;
    saveInFlight = true;
    void invoke<ProjectDocument>("save_document", { projectId, document, expectedRevision: currentRevision })
      .then((saved) => {
        if (saved.project_id === currentProjectId) {
          currentDocument = saved.document;
          currentMetadata = saved.metadata;
          currentRevision = saved.revision;
          editorDirty = false;
        }
        const summary = projects.find((project) => project.project_id === saved.project_id);
        if (summary) summary.title = saved.document.title;
        renderProjectPicker();
        setStatus("Saved");
      })
      .catch((error: unknown) => {
        if (String(error).startsWith("STALE_DRAFT:")) showDraftConflict(pendingExternalRevision);
        setStatus(`Save failed: ${String(error)}`);
      })
      .finally(() => { saveInFlight = false; });
  }, 300);
}

function renderDiagnostics(): void {
  const ready = sectionDiagnostics.length > 0 && sectionDiagnostics.every((section) => section.ready);
  alignmentStatus.className = `alignment-status${ready ? " ready" : " error"}`;
  alignmentStatus.textContent = sectionDiagnostics.length ? (ready ? "Ready to transfer" : "Needs narration work") : "Checking alignment…";
  diagnosticsPanel.replaceChildren();
  sectionDiagnostics.flatMap((section) => section.diagnostics.map((diagnostic) => ({ section, diagnostic }))).forEach(({ section, diagnostic }) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `diagnostic-item ${diagnostic.severity}`;
    button.textContent = `Section ${section.section_index}: ${diagnostic.message}`;
    button.addEventListener("click", () => focusDiagnostic(section, diagnostic));
    diagnosticsPanel.appendChild(button);
  });
}

function focusDiagnostic(section: SectionPrecheckResult, diagnostic: SectionDiagnostic): void {
  const index = section.section_index - 1;
  const narration = narrationEditor.querySelector<HTMLTextAreaElement>(`#narration-${index}`);
  if (diagnostic.narration_range && narration) {
    narration.setSelectionRange(diagnostic.narration_range.start_utf16, diagnostic.narration_range.end_utf16);
    narration.scrollIntoView({ block: "center" });
  }
  if (diagnostic.source_range) {
    const offset = currentDocument.sections.slice(0, index).reduce((total, item) => total + item.markdown.length + "\n\n<!-- kokoro-reader-section -->\n\n".length, 0);
    markdownEditor.setSelectionRange(offset + diagnostic.source_range.start_utf16, offset + diagnostic.source_range.end_utf16);
    markdownEditor.focus();
  } else narration?.focus();
}

function scheduleValidation(): void {
  if (validationTimer !== undefined) window.clearTimeout(validationTimer);
  const generation = ++validationGeneration;
  validationTimer = window.setTimeout(() => {
    validationTimer = undefined;
    const sections = currentDocument.sections.map((section, index) => ({ sectionIndex: index + 1, markdown: section.markdown, speechText: section.speech_text }));
    void invoke<SectionsPrecheckResult>("precheck_sections", { sections })
      .then((result) => {
        if (generation !== validationGeneration) return;
        sectionDiagnostics = result.sections;
        renderDiagnostics();
      })
      .catch((error: unknown) => {
        if (generation !== validationGeneration) return;
        alignmentStatus.className = "alignment-status error";
        alignmentStatus.textContent = `Validation failed: ${String(error)}`;
      });
  }, 250);
}

function showDraftConflict(revision: string): void {
  pendingExternalRevision = revision;
  keepEditsButton.hidden = !editorDirty && !saveInFlight;
  draftConflict.hidden = false;
}

async function reloadDiskDraft(): Promise<void> {
  const reloaded = await invoke<ProjectDocument>("reload_shared_document", { projectId: currentProjectId });
  currentDocument = reloaded.document;
  currentMetadata = reloaded.metadata;
  currentRevision = reloaded.revision;
  editorDirty = false;
  pendingExternalRevision = "";
  draftConflict.hidden = true;
  renderDocument();
  setStatus("Reloaded external draft");
}

function updateMode(nextMode: ViewMode): void {
  if (!currentProjectId && nextMode === "edit") return;
  if (mode === nextMode) return;
  if (mode === "edit" && nextMode !== "edit") clearEditTarget();
  if (nextMode !== "listen" && !searchBar.hidden) closeSearch();
  stopPlayback();
  mode = nextMode;
  const tabs: Array<[HTMLButtonElement, ViewMode]> = [[listenModeButton, "listen"], [editModeButton, "edit"], [articlesModeButton, "articles"], [storageModeButton, "storage"]];
  tabs.forEach(([button, tab]) => {
    button.classList.toggle("active", mode === tab);
    button.setAttribute("aria-selected", String(mode === tab));
  });
  listenPane.hidden = mode !== "listen";
  editPane.hidden = mode !== "edit";
  articlesPane.hidden = mode !== "articles";
  storagePane.hidden = mode !== "storage";
  workspaceToolbar.hidden = mode === "articles" || mode === "storage";
  playerBar.hidden = mode === "articles" || mode === "storage";
  if (mode === "edit") applyEditTarget();
  if (mode === "articles") {
    renderArticles();
    void refreshStorageStats();
  }
  if (mode === "storage") void refreshStorageStats();
}

function applyEditTarget(): void {
  narrationEditor.querySelectorAll(".narration-section.edit-target").forEach((element) => element.classList.remove("edit-target"));
  markdownEditor.classList.remove("edit-target");
  if (!editTarget) {
    markdownEditor.focus();
    return;
  }
  const section = currentDocument.sections[editTarget.sectionIndex];
  if (!section) return;
  const narrationRange = speechChunkRanges(section.speech_text)[editTarget.clipIndex] ?? { start: 0, end: section.speech_text.length };
  const narrationTextarea = narrationEditor.querySelector<HTMLTextAreaElement>(`#narration-${editTarget.sectionIndex}`);
  narrationTextarea?.setSelectionRange(narrationRange.start, narrationRange.end);
  narrationTextarea?.closest(".narration-section")?.classList.add("edit-target");
  narrationTextarea?.scrollIntoView({ block: "center" });
  const markdownStart = currentDocument.sections.slice(0, editTarget.sectionIndex)
    .reduce((offset, item) => offset + item.markdown.length + 2, 0);
  const passage = bestMarkdownPassage(section.markdown, narrationRange.text);
  markdownEditor.setSelectionRange(markdownStart + passage.start, markdownStart + passage.end);
  markdownEditor.classList.add("edit-target");
  markdownEditor.focus();
}

function clearEditTarget(): void {
  editTarget = null;
  markdownEditor.classList.remove("edit-target");
  narrationEditor.querySelectorAll(".narration-section.edit-target").forEach((element) => element.classList.remove("edit-target"));
}

function contextEditTarget(sectionIndex: number | null): { sectionIndex: number; clipIndex: number } | null {
  if (!currentProjectId) return null;
  if (currentPlayback?.projectId === currentProjectId) return { sectionIndex: currentPlayback.sectionIndex, clipIndex: currentPlayback.clipIndex };
  if (playbackPosition?.voice === selectedVoice()) return { sectionIndex: playbackPosition.sectionIndex, clipIndex: playbackPosition.clipIndex };
  return { sectionIndex: sectionIndex ?? activeSection, clipIndex: 0 };
}

async function openReaderContextMenu(event: MouseEvent): Promise<void> {
  event.preventDefault();
  const sectionIndex = Number((event.target as Element).closest<HTMLElement>("[data-section-index]")?.dataset.sectionIndex);
  const target = contextEditTarget(Number.isFinite(sectionIndex) ? sectionIndex : null);
  const menu = await Menu.new({ items: [
    { text: "Edit current passage", enabled: Boolean(target), action: () => {
      if (!target) return;
      editTarget = target;
      updateMode("edit");
    } },
    { text: "Reload", action: () => window.location.reload() },
  ] });
  await menu.popup(new LogicalPosition(event.clientX, event.clientY));
}

function updatePlayerControls(): void {
  const ready = runtimeStatus.engine_ready && currentDocument.sections.length > 0;
  playButton.disabled = !ready;
  stopButton.disabled = !isPlaying;
  playButton.textContent = isPlaying && !audioPlayer.paused ? "Pause" : "Play";
  playButton.setAttribute("aria-label", playButton.textContent);
  speedValue.textContent = `${Number(speedSlider.value).toFixed(1)}×`;
}

function renderRuntimeStatus(): void {
  const setupVisible = !runtimeStatus.model_ready || Boolean(runtimeStatus.error);
  const statusState = runtimeStatus.error
    ? "error"
    : runtimeStatus.downloading || (runtimeStatus.model_ready && !runtimeStatus.engine_ready)
      ? "busy"
      : runtimeStatus.engine_ready
        ? "ready"
        : "setup";

  modelBanner.hidden = !setupVisible;
  runtimePill.classList.toggle("busy", statusState === "busy");
  runtimePill.classList.toggle("error", statusState === "error");
  runtimePill.classList.toggle("ready", statusState === "ready");
  runtimeStatusDot.className = `status-dot ${statusState}`;
  modelStatusDot.className = `status-dot ${statusState}`;
  downloadButton.disabled = runtimeStatus.downloading || runtimeStatus.engine_ready;

  if (runtimeStatus.downloading && runtimeStatus.progress) {
    const progress = runtimeStatus.progress;
    modelStatusTitle.textContent = `Downloading ${progress.asset}`;
    modelStatusDetail.textContent = "Speech stays offline after this one-time download.";
    modelProgressRow.hidden = false;
    if (progress.total_bytes && progress.total_bytes > 0) {
      modelProgress.max = progress.total_bytes;
      modelProgress.value = progress.downloaded_bytes;
      modelProgress.classList.remove("indeterminate");
      modelProgressLabel.textContent = `${formatBytes(progress.downloaded_bytes)} of ${formatBytes(progress.total_bytes)}`;
    } else {
      modelProgress.removeAttribute("value");
      modelProgress.classList.add("indeterminate");
      modelProgressLabel.textContent = `${formatBytes(progress.downloaded_bytes)} downloaded`;
    }
    downloadButton.textContent = "Downloading…";
    runtimeStatusLabel.textContent = "Downloading";
  } else if (runtimeStatus.error) {
    modelStatusTitle.textContent = runtimeStatus.model_ready
      ? "Speech engine needs attention"
      : "Model setup needs attention";
    modelStatusDetail.textContent = runtimeStatus.error;
    modelProgressRow.hidden = true;
    downloadButton.textContent = runtimeStatus.model_ready ? "Retry engine" : "Retry download";
    runtimeStatusLabel.textContent = "Model error";
  } else if (runtimeStatus.engine_ready) {
    modelStatusTitle.textContent = "Ready for offline reading";
    modelStatusDetail.textContent = runtimeStatus.backend ?? "Local inference engine ready";
    modelProgressRow.hidden = true;
    downloadButton.textContent = "Model ready";
    runtimeStatusLabel.textContent = runtimeStatus.backend ?? "Ready";
  } else if (runtimeStatus.model_ready) {
    modelStatusTitle.textContent = "Preparing local engine";
    modelStatusDetail.textContent = "The model is installed; warming the inference session…";
    modelProgressRow.hidden = true;
    downloadButton.textContent = "Preparing…";
    runtimeStatusLabel.textContent = "Preparing";
  } else {
    modelStatusTitle.textContent = "Set up offline speech";
    modelStatusDetail.textContent = "Download Kokoro once. Your documents and audio stay on this Mac.";
    modelProgressRow.hidden = true;
    downloadButton.textContent = "Download model";
    runtimeStatusLabel.textContent = "Setup required";
  }
  updatePlayerControls();
}

async function loadDocument(): Promise<void> {
  const loaded = await invoke<ProjectDocument | null>("get_document");
  currentProjectId = loaded?.project_id ?? "";
  currentDocument = loaded?.document ?? { title: "", sections: [] };
  currentMetadata = loaded?.metadata ?? { read: false, created_at: null, read_at: null, codex_url: null };
  currentRevision = loaded?.revision ?? "";
  editorDirty = false;
  pendingExternalRevision = "";
  draftConflict.hidden = true;
  restorePlaybackSelection();
  renderDocument();
}

async function loadProjects(): Promise<void> {
  projects = await invoke<ProjectSummary[]>("list_projects");
  renderProjectPicker();
}

async function loadRuntimeStatus(): Promise<void> {
  runtimeStatus = await invoke<RuntimeStatus>("runtime_status");
  renderRuntimeStatus();
}

async function loadRecoveryStatus(): Promise<void> {
  hasPrevious = currentProjectId
    ? await invoke<boolean>("has_previous", { projectId: currentProjectId })
    : false;
  restoreButton.disabled = !hasPrevious;
}

async function downloadModel(): Promise<void> {
  setStatus(runtimeStatus.model_ready ? "Restarting speech engine…" : "Downloading model…");
  downloadButton.disabled = true;
  try {
    await invoke("download_model");
    await loadRuntimeStatus();
    setStatus("Model ready");
  } catch (error) {
    setStatus(`Model setup failed: ${String(error)}`);
    await loadRuntimeStatus();
  }
}

async function switchProject(projectId: string): Promise<void> {
  if (!projectId || projectId === currentProjectId) return;
  if (!(await flushPendingSave())) {
    projectSelect.value = currentProjectId;
    return;
  }
  stopPlayback(false);
  try {
    const loaded = await invoke<ProjectDocument>("select_project", { projectId });
    currentProjectId = loaded.project_id;
    currentDocument = loaded.document;
    currentMetadata = loaded.metadata;
    currentRevision = loaded.revision;
    editorDirty = false;
    if (!currentDocument.sections.length) {
      currentDocument.sections = [{ markdown: "", speech_text: "", speech_mode: "automatic" }];
    }
    restorePlaybackSelection();
    await loadRecoveryStatus();
    renderDocument();
    setStatus("Project loaded");
    scheduleCacheQueueRebuild();
  } catch (error) {
    projectSelect.value = currentProjectId;
    setStatus(`Project load failed: ${String(error)}`);
  }
}

async function deleteCurrentProject(): Promise<void> {
  if (currentProjectId) openDeleteProjectDialog([currentProjectId]);
}

function openDeleteProjectDialog(projectIds: string[]): void {
  pendingDeleteProjectIds = [...new Set(projectIds)];
  if (!pendingDeleteProjectIds.length) return;
  deleteProjectMessage.textContent = pendingDeleteProjectIds.length === 1
    ? `Delete this article and its saved files? This cannot be undone.`
    : `Delete ${pendingDeleteProjectIds.length} selected articles and their saved files? This cannot be undone.`;
  deleteProjectDialog.hidden = false;
  confirmDeleteProjectButton.focus();
}

function closeDeleteProjectDialog(): void {
  deleteProjectDialog.hidden = true;
  deleteProjectButton.focus();
}

async function confirmDeleteCurrentProject(): Promise<void> {
  const projectIds = pendingDeleteProjectIds;
  if (!projectIds.length) return;
  closeDeleteProjectDialog();
  pendingDeleteProjectIds = [];
  if (!(await flushPendingSave())) return;
  stopPlayback(false);
  cacheQueue = cacheQueue.filter((task) => !projectIds.includes(task.projectId));
  projectIds.forEach(clearPlaybackPosition);
  projectIds.forEach((projectId) => selectedArticleIds.delete(projectId));
  renderCacheQueue();
  try {
    const next = projectIds.length === 1
      ? await invoke<ProjectDocument | null>("delete_project", { projectId: projectIds[0] })
      : await invoke<ProjectDocument | null>("delete_projects", { projectIds });
    currentProjectId = next?.project_id ?? "";
    currentDocument = next?.document ?? { title: "", sections: [] };
    currentMetadata = next?.metadata ?? { read: false, created_at: null, read_at: null, codex_url: null };
    restorePlaybackSelection();
    await Promise.all([loadProjects(), loadRecoveryStatus()]);
    renderDocument();
    setStatus(next ? `Deleted ${projectIds.length} article${projectIds.length === 1 ? "" : "s"}; selected “${next.document.title}”` : `Deleted ${projectIds.length} article${projectIds.length === 1 ? "" : "s"}; no articles remain`);
    void refreshStorageStats();
    scheduleCacheQueueRebuild();
  } catch (error) {
    setStatus(`Project delete failed: ${String(error)}`);
  }
}

function persistCurrentPlaybackPosition(): void {
  if (!currentPlayback || currentPlayback.projectId !== currentProjectId) return;
  const duration = Number.isFinite(audioPlayer.duration) && audioPlayer.duration > 0
    ? audioPlayer.duration
    : currentPlayback.duration;
  const currentTime = Number.isFinite(audioPlayer.currentTime) ? audioPlayer.currentTime : 0;
  storePlaybackPosition(currentPlayback.projectId, {
    sectionIndex: currentPlayback.sectionIndex,
    clipIndex: currentPlayback.clipIndex,
    currentTime,
    progress: sectionPlaybackProgress(currentPlayback.clipIndex, currentPlayback.clipCount, currentTime, duration),
    voice: currentPlayback.voice,
    clipId: currentPlayback.clipId,
  });
  updatePlaybackVisuals();
}

function stopPlayback(resumeCache = true): void {
  persistCurrentPlaybackPosition();
  clearPlaybackQueue();
  playbackToken += 1;
  isPlaying = false;
  currentPlayback = null;
  audioPlayer.pause();
  audioPlayer.currentTime = 0;
  audioPlayer.removeAttribute("src");
  audioPlayer.load();
  setStatus("Stopped");
  updatePlayerControls();
  updatePlaybackVisuals();
  if (resumeCache) void runCacheQueueWorker();
}

async function synthesize(text: string, voice: string, projectId: string): Promise<AudioAsset> {
  return invoke<AudioAsset>("synthesize_sentence", { projectId, text, voice });
}

function playAsset(asset: AudioAsset, item: PlaybackQueueItem, projectId: string, voice: string, token: number, resumeAt = 0): Promise<void> {
  return new Promise((resolve, reject) => {
    if (token !== playbackToken) {
      resolve();
      return;
    }
    activeSection = item.sectionIndex;
    audioPlayer.pause();
    audioPlayer.src = convertFileSrc(asset.path);
    applyPlaybackSpeed();
    audioPlayer.preservesPitch = true;
    (audioPlayer as HTMLAudioElement & { webkitPreservesPitch?: boolean }).webkitPreservesPitch = true;
    currentPlayback = {
      projectId,
      voice,
      sectionIndex: item.sectionIndex,
      clipIndex: item.clipIndex,
      clipCount: item.clipCount,
      clipId: item.id,
      duration: asset.duration_ms / 1000,
    };
    updatePlaybackVisuals();
    const startPlayback = (): void => {
      if (token !== playbackToken) {
        resolve();
        return;
      }
      const duration = Number.isFinite(audioPlayer.duration) ? audioPlayer.duration : currentPlayback?.duration ?? 0;
      applyPlaybackSpeed();
      audioPlayer.currentTime = Math.min(resumeAt, Math.max(0, duration - 0.05));
      persistCurrentPlaybackPosition();
      void audioPlayer.play().then(() => {
        isPlaying = true;
        setStatus(asset.cache_hit ? "Playing from cache" : "Playing");
        updatePlayerControls();
        updatePlaybackVisuals();
      }).catch(failed);
    };
    const finished = (): void => {
      audioPlayer.removeEventListener("ended", finished);
      audioPlayer.removeEventListener("error", failed);
      audioPlayer.removeEventListener("loadedmetadata", startPlayback);
      resolve();
    };
    const failed = (): void => {
      audioPlayer.removeEventListener("ended", finished);
      audioPlayer.removeEventListener("error", failed);
      audioPlayer.removeEventListener("loadedmetadata", startPlayback);
      reject(new Error("Audio playback failed"));
    };
    audioPlayer.addEventListener("ended", finished, { once: true });
    audioPlayer.addEventListener("error", failed, { once: true });
    if (audioPlayer.readyState >= HTMLMediaElement.HAVE_METADATA) startPlayback();
    else {
      audioPlayer.addEventListener("loadedmetadata", startPlayback, { once: true });
      audioPlayer.load();
    }
  });
}

async function playFromSection(startIndex: number, resume = true): Promise<void> {
  if (!runtimeStatus.engine_ready || !currentDocument.sections.length) return;
  stopPlayback(false);
  const token = playbackToken;
  const voice = selectedVoice();
  const projectId = currentProjectId;
  if (!projectId) return;
  let queue: PlaybackQueueItem[] = [];
  currentDocument.sections.slice(startIndex).forEach((section, offset) => {
    const utterances = groupSpeechChunks(section.speech_text);
    utterances.forEach((utterance, clipIndex) => {
      const sectionIndex = startIndex + offset;
      queue.push({
        sectionIndex,
        clipIndex,
        clipCount: utterances.length,
        utterance,
        id: cacheTaskId(projectId, voice, sectionIndex, clipIndex, utterance),
      });
    });
  });
  if (!queue.length) {
    setStatus("Nothing to read");
    void runCacheQueueWorker();
    return;
  }
  let resumeAt = 0;
  if (!resume) clearPlaybackPosition(projectId);
  if (resume && playbackPosition?.sectionIndex === startIndex && playbackPosition.voice === voice) {
    const resumeIndex = queue.findIndex((item) => item.id === playbackPosition?.clipId);
    if (resumeIndex >= 0) {
      resumeAt = playbackPosition.currentTime;
      queue = queue.slice(resumeIndex);
    }
  }
  const taskIds = queue.map((item) => item.id);
  const assets = requestPlaybackAssets(taskIds, token);
  if (assets.length !== queue.length) {
    clearPlaybackQueue();
    setStatus("Preparing audio queue…");
    scheduleCacheQueueRebuild(0);
    return;
  }
  isPlaying = true;
  updatePlayerControls();
  try {
    for (let index = 0; index < queue.length; index += 1) {
      const item = queue[index];
      setStatus(index === 0 ? "Preparing first section…" : "Preparing next section…");
      const asset = await assets[index];
      if (token !== playbackToken) return;
      await playAsset(asset, item, projectId, voice, token, index === 0 ? resumeAt : 0);
      if (token !== playbackToken) return;
    }
    currentPlayback = null;
    clearPlaybackPosition(projectId);
    updatePlaybackVisuals();
    const shouldMarkRead = startIndex === 0;
    const markedRead = shouldMarkRead && await setProjectRead(true);
    if (token !== playbackToken) return;
    isPlaying = false;
    setStatus(markedRead ? "Finished · marked read" : shouldMarkRead ? "Finished · read status not saved" : "Finished");
  } catch (error) {
    if (token === playbackToken) {
      isPlaying = false;
      setStatus(`Playback failed: ${String(error)}`);
    }
  } finally {
    if (token === playbackToken) {
      isPlaying = false;
      currentPlayback = null;
      clearPlaybackQueue();
      updatePlayerControls();
      updatePlaybackVisuals();
      void runCacheQueueWorker();
    }
  }
}

function togglePlayback(): void {
  if (isPlaying && !audioPlayer.paused) {
    audioPlayer.pause();
    setStatus("Paused");
    updatePlaybackVisuals();
  } else if (isPlaying && audioPlayer.src) {
    void audioPlayer.play()
      .then(() => updatePlaybackVisuals())
      .catch((error: unknown) => setStatus(`Playback failed: ${String(error)}`));
  } else {
    void playFromSection(activeSection);
  }
  updatePlayerControls();
}

function seekAudio(seconds: number): void {
  if (!audioPlayer.src || audioPlayer.readyState === 0) return;
  const currentTime = Number.isFinite(audioPlayer.currentTime) ? audioPlayer.currentTime : 0;
  const target = Math.max(0, currentTime + seconds);
  const duration = Number.isFinite(audioPlayer.duration) ? audioPlayer.duration : target;
  audioPlayer.currentTime = Math.min(target, duration);
  persistCurrentPlaybackPosition();
  setStatus(seconds < 0 ? "Rewound 5 seconds" : "Forward 5 seconds");
}

function isInteractiveTarget(target: EventTarget | null): boolean {
  return target instanceof HTMLElement
    && Boolean(target.closest("input, textarea, select, button, a, [contenteditable='true']"));
}

applyTheme(preferences.theme);
setTextScale(preferences.textScale, false);
voiceSelect.value = preferences.voice;
setSpeed(preferences.speed, false);
sectionNavigator.addEventListener("mouseleave", () => setNavigatorFocus(null));

playButton.addEventListener("click", () => {
  togglePlayback();
});
searchButton.addEventListener("click", openSearch);
searchInput.addEventListener("input", () => {
  searchQuery = searchInput.value;
  searchActiveIndex = 0;
  renderSearchHighlights(true);
});
searchInput.addEventListener("keydown", (event) => {
  if (event.key === "Enter") {
    event.preventDefault();
    navigateSearch(event.shiftKey ? -1 : 1);
  } else if (event.key === "Escape") {
    event.preventDefault();
    closeSearch();
  }
});
searchPreviousButton.addEventListener("click", () => navigateSearch(-1));
searchNextButton.addEventListener("click", () => navigateSearch(1));
searchCloseButton.addEventListener("click", closeSearch);
audioPlayer.addEventListener("timeupdate", persistCurrentPlaybackPosition);
audioPlayer.addEventListener("pause", persistCurrentPlaybackPosition);
window.addEventListener("beforeunload", persistCurrentPlaybackPosition);

document.addEventListener("keydown", (event) => {
  if (event.defaultPrevented || event.isComposing) return;

  const findCommand = (event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === "f";
  if (findCommand) {
    event.preventDefault();
    openSearch();
    return;
  }
  if (event.key === "Escape" && !searchBar.hidden) {
    event.preventDefault();
    closeSearch();
    return;
  }

  const command = event.metaKey && !event.ctrlKey && !event.altKey;
  if (command && (event.key === "+" || event.key === "=")) {
    event.preventDefault();
    adjustTextScale(TEXT_SCALE_STEP);
    return;
  }
  if (command && (event.key === "-" || event.key === "_")) {
    event.preventDefault();
    adjustTextScale(-TEXT_SCALE_STEP);
    return;
  }
  if (command && event.key === "0") {
    event.preventDefault();
    setTextScale(DEFAULT_TEXT_SCALE);
    setStatus(`Text size ${Math.round(DEFAULT_TEXT_SCALE * 100)}%`);
    return;
  }
  if (event.metaKey || event.ctrlKey || event.altKey || isInteractiveTarget(event.target)) return;

  const shortcut = readerShortcut(event.key);
  if (!shortcut) return;
  event.preventDefault();
  if (shortcut === "toggle") togglePlayback();
  else if (shortcut === "restart-section") activateSection(activeSection, true, false);
  else if (shortcut === "next-section") moveSection(1);
  else if (shortcut === "previous-section") moveSection(-1);
  else if (shortcut === "restart-document") activateSection(0, true, false);
  else if (shortcut === "jump-to-marker") jumpToPlaybackMarker();
  else if (shortcut === "rewind") seekAudio(-5);
  else seekAudio(5);
});

stopButton.addEventListener("click", () => stopPlayback());
cachePanelButton.addEventListener("click", () => {
  if (cachePanelOpen) closeCachePanel();
  else openCachePanel();
});
closeCachePanelButton.addEventListener("click", closeCachePanel);
cachePanelBackdrop.addEventListener("click", (event) => {
  if (event.target === cachePanelBackdrop) closeCachePanel();
});
projectSelect.addEventListener("change", () => {
  void switchProject(projectSelect.value);
});
deleteProjectButton.addEventListener("click", () => {
  void deleteCurrentProject();
});
markReadButton.addEventListener("click", () => {
  const read = !currentMetadata.read;
  void setProjectRead(read).then((updated) => {
    if (updated) setStatus(read ? "Marked read" : "Marked unread");
  });
});
backToCodexButton.addEventListener("click", () => {
  void openCodexOrigin();
});
cancelDeleteProjectButton.addEventListener("click", closeDeleteProjectDialog);
confirmDeleteProjectButton.addEventListener("click", () => {
  void confirmDeleteCurrentProject();
});
deleteProjectDialog.addEventListener("click", (event) => {
  if (event.target === deleteProjectDialog) closeDeleteProjectDialog();
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && cachePanelOpen) {
    event.preventDefault();
    closeCachePanel();
    return;
  }
  if (event.key === "Escape" && !deleteProjectDialog.hidden) {
    event.preventDefault();
    closeDeleteProjectDialog();
  }
});
listenModeButton.addEventListener("click", () => updateMode("listen"));
editModeButton.addEventListener("click", () => updateMode("edit"));
articlesModeButton.addEventListener("click", () => updateMode("articles"));
storageModeButton.addEventListener("click", () => updateMode("storage"));
articlesFilter.addEventListener("change", () => {
  articleFilter = articlesFilter.value as ArticleFilter;
  selectedArticleIds.clear();
  renderArticles();
});
articlesSelectAll.addEventListener("change", () => {
  articleRows().forEach((project) => {
    if (articlesSelectAll.checked) selectedArticleIds.add(project.project_id);
    else selectedArticleIds.delete(project.project_id);
  });
  renderArticles();
});
document.querySelectorAll<HTMLButtonElement>("[data-article-sort]").forEach((button) => {
  button.addEventListener("click", () => {
    const sort = button.dataset.articleSort as ArticleSort;
    articleSortDescending = articleSort === sort ? !articleSortDescending : true;
    articleSort = sort;
    renderArticles();
  });
});
deleteSelectedButton.addEventListener("click", () => openDeleteProjectDialog([...selectedArticleIds]));
refreshStorageButton.addEventListener("click", () => void refreshStorageStats());
markdownView.addEventListener("contextmenu", (event) => void openReaderContextMenu(event));
themeToggle.addEventListener("click", () => {
  applyTheme(preferences.theme === "dark" ? "light" : "dark");
  savePreferences();
});
markdownEditor.addEventListener("input", () => {
  clearEditTarget();
  editorDirty = true;
  updateSectionsFromMarkdown();
});
titleInput.addEventListener("input", () => {
  currentDocument.title = titleInput.value;
  editorDirty = true;
  scheduleSave();
  scheduleValidation();
});
reloadDiskButton.addEventListener("click", () => void reloadDiskDraft().catch((error: unknown) => setStatus(`Reload failed: ${String(error)}`)));
keepEditsButton.addEventListener("click", () => {
  currentRevision = pendingExternalRevision || currentRevision;
  pendingExternalRevision = "";
  draftConflict.hidden = true;
  scheduleSave();
});
voiceSelect.addEventListener("change", () => {
  preferences.voice = VOICES.has(voiceSelect.value) ? voiceSelect.value : DEFAULT_VOICE;
  voiceSelect.value = preferences.voice;
  savePreferences();
  stopPlayback();
  cacheQueue = [];
  renderCacheQueue();
  renderListenView();
  scheduleCacheQueueRebuild(0);
});
speedSlider.addEventListener("input", () => {
  setSpeed(Number(speedSlider.value));
});
articleTimer.addEventListener("click", toggleArticleTimer);
speedPresetButtons.forEach((button) => {
  button.addEventListener("click", () => setSpeed(Number(button.dataset.speed)));
});
downloadButton.addEventListener("click", () => void downloadModel());
restoreButton.addEventListener("click", () => {
  stopPlayback(false);
  void invoke<ProjectDocument>("restore_previous", { projectId: currentProjectId })
    .then((restored) => {
      currentProjectId = restored.project_id;
      currentDocument = restored.document;
      currentMetadata = restored.metadata;
      currentRevision = restored.revision;
      editorDirty = false;
      restorePlaybackSelection();
      renderDocument();
      void loadRecoveryStatus();
      setStatus("Previous document restored");
      scheduleCacheQueueRebuild();
    })
    .catch((error: unknown) => setStatus(`Restore failed: ${String(error)}`));
});
reloadFilesButton.addEventListener("click", () => {
  stopPlayback(false);
  void invoke<ProjectDocument>("reload_shared_document", { projectId: currentProjectId })
    .then((reloaded) => {
      currentProjectId = reloaded.project_id;
      currentDocument = reloaded.document;
      currentMetadata = reloaded.metadata;
      currentRevision = reloaded.revision;
      editorDirty = false;
      restorePlaybackSelection();
      renderDocument();
      void loadRecoveryStatus();
      setStatus("Reloaded shared files");
      scheduleCacheQueueRebuild();
    })
    .catch((error: unknown) => setStatus(`Reload failed: ${String(error)}`));
});

void listen<DownloadProgress>("model-progress", (event) => {
  runtimeStatus.downloading = true;
  runtimeStatus.progress = event.payload;
  renderRuntimeStatus();
});
void listen<RuntimeStatus>("runtime-updated", (event) => {
  runtimeStatus = event.payload;
  if (runtimeStatus.error) {
    stopPlayback();
    setStatus(runtimeStatus.error);
  }
  renderRuntimeStatus();
  if (runtimeStatus.engine_ready) {
    void runCacheQueueWorker();
    scheduleCacheQueueRebuild(0);
  }
});
void listen("document-updated", () => {
  cancelScheduledSave();
  stopPlayback(false);
  void loadDocument().then(() => Promise.all([loadProjects(), loadRecoveryStatus()])).then(() => {
    if (mode !== "listen") updateMode("listen");
    setStatus("Document received from Codex");
    scheduleCacheQueueRebuild();
  });
});

void loadDocument()
  .then(() => Promise.all([loadProjects(), loadRuntimeStatus(), loadRecoveryStatus()]))
  .then(() => scheduleCacheQueueRebuild(0))
  .catch((error: unknown) => {
    setStatus(`Startup failed: ${String(error)}`);
  });
