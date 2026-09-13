import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import katex from "katex";
import MarkdownIt from "markdown-it";
import "katex/dist/katex.min.css";
import "./styles.css";
import { cacheTaskId, prioritizeCacheTasks, sectionCacheState, type CacheTask, type CacheTaskStatus } from "./cache-queue";
import { parsePlaybackPosition, readerShortcut, sectionPlaybackProgress, type PlaybackPosition } from "./playback";
import { deriveSpeechText, groupSpeechChunks, reconcileSections, splitMarkdownSections } from "./text";

type SpeechMode = "automatic" | "custom";
type Theme = "light" | "dark";
type CacheState = CacheTaskStatus;

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
  active: boolean;
  read: boolean;
}

interface ProjectMetadata {
  read: boolean;
  codex_url: string | null;
}

interface ProjectDocument {
  project_id: string;
  document: Document;
  metadata: ProjectMetadata;
}

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
const cacheReadyCount = required<HTMLElement>("#cache-ready-count");
const cacheReadyList = required<HTMLOListElement>("#cache-ready-list");
const themeToggle = required<HTMLButtonElement>("#theme-toggle");
const listenModeButton = required<HTMLButtonElement>("#listen-mode-button");
const editModeButton = required<HTMLButtonElement>("#edit-mode-button");
const sectionSummary = required<HTMLElement>("#section-summary");
const listenPane = required<HTMLElement>("#listen-pane");
const editPane = required<HTMLElement>("#edit-pane");
const sectionNavigator = required<HTMLElement>("#section-navigator");
const sectionNavigatorList = required<HTMLElement>("#section-navigator-list");
const markdownView = required<HTMLElement>("#markdown-view");
const markdownEditor = required<HTMLTextAreaElement>("#markdown-editor");
const narrationEditor = required<HTMLElement>("#narration-editor");
const playButton = required<HTMLButtonElement>("#play-button");
const stopButton = required<HTMLButtonElement>("#stop-button");
const voiceSelect = required<HTMLSelectElement>("#voice-select");
const speedSlider = required<HTMLInputElement>("#speed-slider");
const speedValue = required<HTMLOutputElement>("#speed-value");
const speedPresetButtons = Array.from(document.querySelectorAll<HTMLButtonElement>(".speed-preset"));
const activityStatus = required<HTMLElement>("#activity-status");
const audioPlayer = required<HTMLAudioElement>("#audio-player");

let currentProjectId = "";
let projects: ProjectSummary[] = [];
let currentDocument: Document = { title: "", sections: [] };
let currentMetadata: ProjectMetadata = { read: false, codex_url: null };
let runtimeStatus: RuntimeStatus = {
  model_ready: false,
  engine_ready: false,
  downloading: false,
  progress: null,
  backend: null,
  error: null,
};
let mode: "listen" | "edit" = "listen";
let activeSection = 0;
let playbackToken = 0;
let playbackPosition: PlaybackPosition | null = null;
let currentPlayback: PlaybackContext | null = null;
let saveTimer: number | undefined;
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

function loadPreferences(): Preferences {
  const defaults: Preferences = {
    theme: window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light",
    voice: DEFAULT_VOICE,
    speed: 1,
    textScale: DEFAULT_TEXT_SCALE,
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
  if (persist) savePreferences();
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
      if (!tasks.length) return { projectId: project.project_id, tasks, cached: [] as boolean[], error: "" };
      try {
        return {
          projectId: project.project_id,
          tasks,
          cached: await invoke<boolean[]>("audio_cache_status", { projectId: project.project_id, texts: tasks.map((task) => task.text), voice }),
          error: "",
        };
      } catch (error) {
        return { projectId: project.project_id, tasks, cached: [] as boolean[], error: String(error) };
      }
    }));
    if (generation !== cacheQueueBuildGeneration) return;
    statuses.forEach(({ tasks, cached, error }) => {
      tasks.forEach((task, index) => {
        if (cached[index]) {
          task.status = "ready";
          task.error = undefined;
        } else if (error) {
          task.status = "failed";
          task.error = error;
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
    else if (!isPlaying && cacheQueue.length && !queueTaskCount("failed")) setStatus("Audio queue ready");
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

function cacheTaskStateLabel(task: QueueTask): string {
  if (task.status === "caching") return "Caching now";
  if (task.status === "ready") return "Ready";
  if (task.status === "failed") return task.error ? `Failed: ${task.error}` : "Failed";
  return "Queued";
}

function renderCacheTask(list: HTMLOListElement, task: QueueTask): void {
  const row = document.createElement("li");
  row.className = `cache-task ${task.status}`;
  const main = document.createElement("button");
  main.type = "button";
  main.className = "cache-task-main";
  main.innerHTML = `<span class="cache-task-meta">${escapeHtml(task.projectTitle)} · Section ${task.sectionIndex + 1} · Clip ${task.clipIndex + 1}</span><span class="cache-task-preview">${escapeHtml(task.text)}</span><span class="cache-task-state">${escapeHtml(cacheTaskStateLabel(task))}</span>`;
  main.addEventListener("click", () => void navigateToCacheTask(task));
  row.appendChild(main);
  if (task.status === "failed") {
    const retry = document.createElement("button");
    retry.type = "button";
    retry.className = "cache-retry-button";
    retry.textContent = "Retry";
    retry.addEventListener("click", () => retryCacheTask(task.id));
    row.appendChild(retry);
  }
  list.appendChild(row);
}

function renderCacheQueue(): void {
  const ready = queueTaskCount("ready");
  const failed = queueTaskCount("failed");
  const caching = queueTaskCount("caching");
  cachePanelCount.textContent = `${ready}/${cacheQueue.length}`;
  cachePanelSummary.textContent = cacheQueue.length
    ? `${ready} ready · ${caching ? `${caching} caching · ` : ""}${queueTaskCount("queued")} queued${failed ? ` · ${failed} failed` : ""}`
    : "No audio to cache";
  cachePanelDot.className = `status-dot ${failed ? "error" : ready === cacheQueue.length && cacheQueue.length ? "ready" : "busy"}`;
  const active = cacheQueue.find((task) => task.id === activeCacheTaskId);
  cacheActiveItem.hidden = !active;
  cacheActiveItem.textContent = active ? `Caching now · ${active.projectTitle} · Section ${active.sectionIndex + 1} · ${active.text}` : "";
  cacheQueueList.replaceChildren();
  cacheReadyList.replaceChildren();
  cacheReadyCount.textContent = String(ready);
  prioritizedQueueTasks().forEach((task) => {
    renderCacheTask(task.status === "ready" ? cacheReadyList : cacheQueueList, task);
  });
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
  if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
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
  markdownView.querySelectorAll<HTMLElement>(".document-section").forEach((section) => {
    const index = Number(section.dataset.sectionIndex);
    const active = index === activeSection;
    section.classList.toggle("active", active);
    section.setAttribute("aria-pressed", String(active));
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
}

function renderListenView(): void {
  markdownView.innerHTML = "";
  sectionSummary.textContent = `${currentDocument.sections.length} section${currentDocument.sections.length === 1 ? "" : "s"}`;
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
    return;
  }
  currentDocument.sections.forEach((section, index) => {
    const sectionElement = document.createElement("section");
    const cacheState = currentSectionCacheState(index);
    sectionElement.className = `document-section${index === activeSection ? " active" : ""}`;
    sectionElement.dataset.sectionIndex = String(index);
    sectionElement.tabIndex = 0;
    sectionElement.setAttribute("role", "button");
    sectionElement.setAttribute("aria-pressed", String(index === activeSection));
    sectionElement.setAttribute("aria-label", `Read section ${index + 1}; ${cacheStateLabel(cacheState)}`);
    sectionElement.innerHTML = renderMarkdown(section.markdown);
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
      activateSection(index);
    });
    sectionElement.addEventListener("keydown", (event) => {
      if ((event.target as Element).closest("a[href]")) return;
      if (event.key === "Enter" || event.key === " ") {
        event.preventDefault();
        activateSection(index);
      }
    });
    markdownView.appendChild(sectionElement);
  });
  updatePlaybackVisuals();
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
      section.speech_text = textarea.value;
      section.speech_mode = "custom";
      clearPlaybackPosition(currentProjectId);
      invalidateCurrentProjectQueue();
      scheduleSave();
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
      scheduleCacheQueueRebuild();
    });
    wrapper.append(header, textarea, reset);
    narrationEditor.appendChild(wrapper);
  });
}

function renderEditors(): void {
  markdownEditor.value = currentDocument.sections.map((section) => section.markdown).join("\n\n");
  renderNarrationEditor();
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

function renderDocument(): void {
  renderProjectPicker();
  titleInput.value = currentDocument.title;
  titleInput.disabled = !currentProjectId;
  editModeButton.disabled = !currentProjectId;
  reloadFilesButton.disabled = !currentProjectId;
  if (!currentProjectId) {
    mode = "listen";
    listenPane.hidden = false;
    editPane.hidden = true;
    listenModeButton.classList.add("active");
    editModeButton.classList.remove("active");
    listenModeButton.setAttribute("aria-selected", "true");
    editModeButton.setAttribute("aria-selected", "false");
  }
  activeSection = Math.min(activeSection, Math.max(currentDocument.sections.length - 1, 0));
  renderListenView();
  renderEditors();
  renderCacheQueue();
  restoreButton.disabled = !hasPrevious;
}

async function setProjectRead(read: boolean): Promise<boolean> {
  if (!currentProjectId) return false;
  if (currentMetadata.read === read) return true;
  const projectId = currentProjectId;
  try {
    const metadata = await invoke<ProjectMetadata>("set_project_read", { projectId, read });
    if (projectId !== currentProjectId) return false;
    currentMetadata = metadata;
    const summary = projects.find((project) => project.project_id === projectId);
    if (summary) summary.read = metadata.read;
    renderProjectPicker();
    return true;
  } catch (error) {
    if (projectId === currentProjectId) setStatus(`Read status failed: ${String(error)}`);
    return false;
  }
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
    const saved = await invoke<ProjectDocument>("save_document", { projectId, document });
    if (saved.project_id === currentProjectId) {
      currentDocument = saved.document;
      currentMetadata = saved.metadata;
    }
    return true;
  } catch (error) {
    setStatus(`Save failed: ${String(error)}`);
    return false;
  }
}

function scheduleSave(): void {
  cancelScheduledSave();
  const projectId = currentProjectId;
  const document = cloneDocument(currentDocument);
  saveTimer = window.setTimeout(() => {
    saveTimer = undefined;
    if (!projectId) return;
    void invoke<ProjectDocument>("save_document", { projectId, document })
      .then((saved) => {
        if (saved.project_id === currentProjectId) {
          currentDocument = saved.document;
          currentMetadata = saved.metadata;
        }
        const summary = projects.find((project) => project.project_id === saved.project_id);
        if (summary) summary.title = saved.document.title;
        renderProjectPicker();
        setStatus("Saved");
      })
      .catch((error: unknown) => setStatus(`Save failed: ${String(error)}`));
  }, 300);
}

function updateMode(nextMode: "listen" | "edit"): void {
  if (!currentProjectId && nextMode === "edit") return;
  if (mode === nextMode) return;
  stopPlayback();
  mode = nextMode;
  listenModeButton.classList.toggle("active", mode === "listen");
  editModeButton.classList.toggle("active", mode === "edit");
  listenModeButton.setAttribute("aria-selected", String(mode === "listen"));
  editModeButton.setAttribute("aria-selected", String(mode === "edit"));
  listenPane.hidden = mode !== "listen";
  editPane.hidden = mode !== "edit";
  if (mode === "edit") markdownEditor.focus();
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
  currentMetadata = loaded?.metadata ?? { read: false, codex_url: null };
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
  if (!currentProjectId) return;
  deleteProjectMessage.textContent = `Delete “${currentDocument.title.trim() || "Untitled reading"}” and its saved files? This cannot be undone.`;
  deleteProjectDialog.hidden = false;
  confirmDeleteProjectButton.focus();
}

function closeDeleteProjectDialog(): void {
  deleteProjectDialog.hidden = true;
  deleteProjectButton.focus();
}

async function confirmDeleteCurrentProject(): Promise<void> {
  if (!currentProjectId) return;
  closeDeleteProjectDialog();
  const title = currentDocument.title.trim() || "Untitled reading";
  const deletedProjectId = currentProjectId;
  if (!(await flushPendingSave())) return;
  stopPlayback(false);
  cacheQueue = cacheQueue.filter((task) => task.projectId !== currentProjectId);
  renderCacheQueue();
  try {
    const next = await invoke<ProjectDocument | null>("delete_project", { projectId: currentProjectId });
    currentProjectId = next?.project_id ?? "";
    currentDocument = next?.document ?? { title: "", sections: [] };
    currentMetadata = next?.metadata ?? { read: false, codex_url: null };
    clearPlaybackPosition(deletedProjectId);
    restorePlaybackSelection();
    await Promise.all([loadProjects(), loadRecoveryStatus()]);
    renderDocument();
    setStatus(next ? `Deleted “${title}”; selected “${next.document.title}”` : `Deleted “${title}”; no projects remain`);
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
  } else if (isPlaying && audioPlayer.src) {
    void audioPlayer.play().catch((error: unknown) => setStatus(`Playback failed: ${String(error)}`));
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
audioPlayer.addEventListener("timeupdate", persistCurrentPlaybackPosition);
audioPlayer.addEventListener("pause", persistCurrentPlaybackPosition);
window.addEventListener("beforeunload", persistCurrentPlaybackPosition);

document.addEventListener("keydown", (event) => {
  if (event.defaultPrevented || event.isComposing) return;

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
themeToggle.addEventListener("click", () => {
  applyTheme(preferences.theme === "dark" ? "light" : "dark");
  savePreferences();
});
markdownEditor.addEventListener("input", updateSectionsFromMarkdown);
titleInput.addEventListener("input", () => {
  currentDocument.title = titleInput.value;
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
    mode = "listen";
    listenPane.hidden = false;
    editPane.hidden = true;
    listenModeButton.classList.add("active");
    editModeButton.classList.remove("active");
    listenModeButton.setAttribute("aria-selected", "true");
    editModeButton.setAttribute("aria-selected", "false");
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
