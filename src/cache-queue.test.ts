import { describe, expect, test } from "bun:test";
import { cacheTaskId, prioritizeCacheTasks, sectionCacheState, summarizeCacheProjects, type CacheTask } from "./cache-queue";

function task(overrides: Partial<CacheTask> = {}): CacheTask {
  const base: CacheTask = {
    id: "task",
    projectId: "current",
    projectTitle: "Current",
    projectOrder: 0,
    sectionIndex: 0,
    clipIndex: 0,
    text: "Text",
    voice: "af_bella",
    status: "queued",
  };
  return { ...base, ...overrides };
}

describe("cache queue", () => {
  test("gives playback, nearby current sections, then other projects priority", () => {
    const ordered = prioritizeCacheTasks([
      task({ id: "other", projectId: "other", projectOrder: 0 }),
      task({ id: "previous", sectionIndex: 1 }),
      task({ id: "next", sectionIndex: 3 }),
      task({ id: "current", sectionIndex: 2 }),
      task({ id: "playback", sectionIndex: 5, playbackOrder: 0 }),
    ], "current", 2);
    expect(ordered.map((item) => item.id)).toEqual(["playback", "current", "next", "previous", "other"]);
  });

  test("uses the task location and text as a stable rebuild key", () => {
    expect(cacheTaskId("project", "af_bella", 1, 2, "Hello")).toBe(cacheTaskId("project", "af_bella", 1, 2, "Hello"));
    expect(cacheTaskId("project", "af_bella", 1, 2, "Hello")).not.toBe(cacheTaskId("project", "af_bella", 1, 2, "Hello again"));
  });

  test("marks a section ready only when every clip is ready", () => {
    const tasks = [
      task({ id: "one", status: "ready" }),
      task({ id: "two", status: "caching", clipIndex: 1 }),
    ];
    expect(sectionCacheState(tasks, "current", 0)).toBe("caching");
    tasks[1].status = "ready";
    expect(sectionCacheState(tasks, "current", 0)).toBe("ready");
  });

  test("keeps failures visible without blocking later queued work", () => {
    const tasks = [task({ id: "failed", status: "failed" }), task({ id: "queued", sectionIndex: 1 })];
    expect(sectionCacheState(tasks, "current", 0)).toBe("failed");
    expect(prioritizeCacheTasks(tasks.filter((item) => item.status === "queued"), "current", 0)[0].id).toBe("queued");
  });

  test("collapses clips into article rows using the next incomplete task", () => {
    const summaries = summarizeCacheProjects([
      task({ id: "ready", status: "ready" }),
      task({ id: "pending", sectionIndex: 1, status: "queued" }),
      task({ id: "other", projectId: "other", projectTitle: "Other", status: "caching" }),
    ], "current", 0);
    expect(summaries.map(({ projectId }) => projectId)).toEqual(["current", "other"]);
    expect(summaries[0]).toMatchObject({ total: 2, ready: 1, queued: 1, nextTask: { id: "pending" } });
    expect(summaries[1]).toMatchObject({ total: 1, caching: 1, nextTask: { id: "other" } });
  });
});
