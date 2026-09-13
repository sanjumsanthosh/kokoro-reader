export interface PlaybackPosition {
  sectionIndex: number;
  clipIndex: number;
  currentTime: number;
  progress: number;
  voice: string;
  clipId: string;
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
