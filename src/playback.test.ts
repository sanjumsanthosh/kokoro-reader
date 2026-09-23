import { describe, expect, test } from "bun:test";
import { articleTiming, parsePlaybackPosition, readerShortcut, sectionPlaybackProgress } from "./playback";

describe("playback position", () => {
  test("validates saved positions", () => {
    const saved = { sectionIndex: 2, clipIndex: 1, currentTime: 4.5, progress: 0.75, voice: "af_bella", clipId: "clip" };
    expect(parsePlaybackPosition(JSON.stringify(saved))).toEqual(saved);
    expect(parsePlaybackPosition('{"sectionIndex":-1}')).toBeNull();
  });

  test("maps the current clip onto the whole section", () => {
    expect(sectionPlaybackProgress(1, 2, 5, 10)).toBe(0.75);
  });

  test("calculates whole-article timing at the selected speed", () => {
    const items = [
      { id: "one", duration: 10 },
      { id: "two", duration: 20 },
      { id: "three", duration: 30 },
    ];
    expect(articleTiming(items, "two", 5, 2)).toEqual({ total: 30, elapsed: 7.5, remaining: 22.5 });
  });

  test("waits for every clip duration before reporting an article total", () => {
    expect(articleTiming([{ id: "one", duration: null }], null, 0, 1)).toEqual({
      total: null,
      elapsed: 0,
      remaining: 0,
    });
  });

  test("recognizes the reader shortcuts", () => {
    expect([" ", "s", "j", "k", "0", "t", "ArrowLeft", "ArrowRight"].map(readerShortcut)).toEqual([
      "toggle", "restart-section", "next-section", "previous-section", "restart-document", "jump-to-marker", "rewind", "forward",
    ]);
    expect(readerShortcut("x")).toBeNull();
  });
});
