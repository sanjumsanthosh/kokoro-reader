export interface PlaybackPosition {
  sectionIndex: number;
  clipIndex: number;
  currentTime: number;
  progress: number;
  voice: string;
  clipId: string;
}

export interface ArticleTimingItem {
  id: string;
  duration: number | null;
}

export interface ArticleTiming {
  total: number | null;
  elapsed: number;
  remaining: number;
}

export type ReaderShortcut = "toggle" | "restart-section" | "next-section" | "previous-section" | "restart-document" | "jump-to-marker" | "rewind" | "forward";

const READER_SHORTCUTS: Record<string, ReaderShortcut> = {
  " ": "toggle",
  s: "restart-section",
  j: "next-section",
  k: "previous-section",
  "0": "restart-document",
  t: "jump-to-marker",
  ArrowLeft: "rewind",
  ArrowRight: "forward",
};

export function readerShortcut(key: string): ReaderShortcut | null {
  return READER_SHORTCUTS[key.length === 1 ? key.toLowerCase() : key] ?? null;
}

export function parsePlaybackPosition(value: string | null): PlaybackPosition | null {
  if (!value) return null;
  try {
    const position = JSON.parse(value) as Partial<PlaybackPosition>;
    return Number.isInteger(position.sectionIndex) && position.sectionIndex! >= 0
      && Number.isInteger(position.clipIndex) && position.clipIndex! >= 0
      && Number.isFinite(position.currentTime) && position.currentTime! >= 0
      && Number.isFinite(position.progress) && position.progress! >= 0 && position.progress! <= 1
      && typeof position.voice === "string" && position.voice.length > 0
      && typeof position.clipId === "string" && position.clipId.length > 0
      ? position as PlaybackPosition
      : null;
  } catch {
    return null;
  }
}

export function sectionPlaybackProgress(clipIndex: number, clipCount: number, currentTime: number, duration: number): number {
  if (clipCount <= 0) return 0;
  const clipProgress = duration > 0 ? Math.min(1, Math.max(0, currentTime / duration)) : 0;
  return Math.min(1, Math.max(0, (clipIndex + clipProgress) / clipCount));
}

export function articleTiming(
  items: readonly ArticleTimingItem[],
  clipId: string | null,
  currentTime: number,
  speed: number,
): ArticleTiming {
  if (items.some((item) => item.duration === null || !Number.isFinite(item.duration) || item.duration < 0)) {
    return { total: null, elapsed: 0, remaining: 0 };
  }
  const total = items.reduce((sum, item) => sum + item.duration!, 0);
  const clipIndex = clipId ? items.findIndex((item) => item.id === clipId) : -1;
  const elapsedAtOneX = clipIndex < 0
    ? 0
    : items.slice(0, clipIndex).reduce((sum, item) => sum + item.duration!, 0)
      + Math.min(Math.max(Number.isFinite(currentTime) ? currentTime : 0, 0), items[clipIndex].duration!);
  const playbackSpeed = Number.isFinite(speed) && speed > 0 ? speed : 1;
  const elapsed = elapsedAtOneX / playbackSpeed;
  const adjustedTotal = total / playbackSpeed;
  return {
    total: adjustedTotal,
    elapsed: Math.min(adjustedTotal, elapsed),
    remaining: Math.max(0, adjustedTotal - elapsed),
  };
}
