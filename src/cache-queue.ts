export type CacheTaskStatus = "queued" | "caching" | "ready" | "failed";

export interface CacheTask {
  id: string;
  projectId: string;
  projectTitle: string;
  projectOrder: number;
  sectionIndex: number;
  clipIndex: number;
  text: string;
  voice: string;
  status: CacheTaskStatus;
  playbackOrder?: number;
}

export function cacheTaskId(projectId: string, voice: string, sectionIndex: number, clipIndex: number, text: string): string {
  return JSON.stringify([projectId, voice, sectionIndex, clipIndex, text]);
}

export function prioritizeCacheTasks(
  tasks: readonly CacheTask[],
  currentProjectId: string,
  activeSection: number,
): CacheTask[] {
  return [...tasks].sort((left, right) => compareTaskPriority(left, right, currentProjectId, activeSection));
}

export function sectionCacheState(tasks: readonly CacheTask[], projectId: string, sectionIndex: number): CacheTaskStatus {
  const sectionTasks = tasks.filter((task) => task.projectId === projectId && task.sectionIndex === sectionIndex);
  if (!sectionTasks.length || sectionTasks.every((task) => task.status === "ready")) return "ready";
  if (sectionTasks.some((task) => task.status === "caching")) return "caching";
  if (sectionTasks.some((task) => task.status === "failed")) return "failed";
  return "queued";
}

function compareTaskPriority(left: CacheTask, right: CacheTask, currentProjectId: string, activeSection: number): number {
  const leftPriority = taskPriority(left, currentProjectId, activeSection);
  const rightPriority = taskPriority(right, currentProjectId, activeSection);
  for (let index = 0; index < leftPriority.length; index += 1) {
    const difference = leftPriority[index] - rightPriority[index];
    if (difference) return difference;
  }
  return left.id.localeCompare(right.id);
}

function taskPriority(task: CacheTask, currentProjectId: string, activeSection: number): number[] {
  if (task.playbackOrder !== undefined) return [0, task.playbackOrder, 0, 0];
  if (task.projectId === currentProjectId) {
    if (task.sectionIndex >= activeSection) return [1, task.sectionIndex - activeSection, task.clipIndex, 0];
    return [2, activeSection - task.sectionIndex, task.clipIndex, 0];
  }
  return [3, task.projectOrder, task.sectionIndex, task.clipIndex];
}
