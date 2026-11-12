import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AppConfig, CaptureTask } from "./api";

const setCaptureTasks = vi.hoisted(() => vi.fn());
vi.mock("./api", () => ({ api: { getConfig: () => new Promise(() => {}), setCaptureTasks } }));

const {
  applySavedConfig,
  captureTaskSaveState,
  config,
  discardFailedCaptureTaskSave,
  mutateConfig,
  queueCaptureTasks,
  retryCaptureTaskSave,
  saveCaptureTasks,
  waitForCaptureTaskSaves,
} = await import("./store");

const draft = (quality: number) => ({ output: { quality }, capture_tasks: [] }) as unknown as AppConfig;
const tasks = (...ids: string[]) => ids.map((id) => ({ id }) as unknown as CaptureTask);
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

let replies: { resolve: (saved: CaptureTask[]) => void; reject: (error: Error) => void }[];

beforeEach(async () => {
  await waitForCaptureTaskSaves();
  discardFailedCaptureTaskSave();
  replies = [];
  setCaptureTasks.mockReset();
  setCaptureTasks.mockImplementation(
    () => new Promise<CaptureTask[]>((resolve, reject) => replies.push({ resolve, reject })),
  );
  mutateConfig(draft(80));
});

describe("applySavedConfig", () => {
  it("takes the server reply when nothing changed during the save", () => {
    const sent = draft(80);
    mutateConfig(sent);
    const saved = draft(80);
    expect(applySavedConfig(sent, saved)).toBe(true);
    expect(config()).toBe(saved);
  });

  it("keeps an edit made while the save was in flight", () => {
    const sent = draft(80);
    mutateConfig(sent);
    const edited = draft(60);
    mutateConfig(edited);
    expect(applySavedConfig(sent, draft(80))).toBe(false);
    expect(config()).toBe(edited);
  });
});

describe("capture task saves", () => {
  it("sends only the newest list queued behind an in-flight save", async () => {
    const [a, b, c] = [tasks("a"), tasks("a", "b"), tasks("a", "b", "c")];
    queueCaptureTasks(a);
    queueCaptureTasks(b);
    queueCaptureTasks(c);
    expect(config()?.capture_tasks).toBe(c);
    replies[0].resolve(a);
    await settle();
    expect(config()?.capture_tasks).toBe(c);
    replies[1].resolve(c);
    await waitForCaptureTaskSaves();
    expect(setCaptureTasks.mock.calls).toEqual([[a], [c]]);
    expect(captureTaskSaveState()).toEqual({ kind: "saved", count: 3 });
  });

  it("re-sends a failed list on retry", async () => {
    const a = tasks("a");
    queueCaptureTasks(a);
    replies[0].reject(new Error("disk full"));
    await waitForCaptureTaskSaves();
    expect(captureTaskSaveState()).toEqual({ kind: "error", message: "Error: disk full" });
    retryCaptureTaskSave();
    replies[1].resolve(a);
    await waitForCaptureTaskSaves();
    expect(setCaptureTasks.mock.calls).toEqual([[a], [a]]);
    expect(captureTaskSaveState()).toEqual({ kind: "saved", count: 1 });
  });

  it("forgets a discarded failure", async () => {
    queueCaptureTasks(tasks("a"));
    replies[0].reject(new Error("disk full"));
    await waitForCaptureTaskSaves();
    discardFailedCaptureTaskSave();
    retryCaptureTaskSave();
    await waitForCaptureTaskSaves();
    expect(setCaptureTasks).toHaveBeenCalledTimes(1);
    expect(captureTaskSaveState()).toEqual({ kind: "idle" });
  });

  it("settles an explicit save only after the backend replies", async () => {
    const a = tasks("a");
    const before = config()?.capture_tasks;
    let done = false;
    const save = saveCaptureTasks(a).then(() => (done = true));
    await settle();
    expect(done).toBe(false);
    expect(config()?.capture_tasks).toBe(before);
    replies[0].resolve(a);
    await save;
    expect(config()?.capture_tasks).toBe(a);

    const failing = saveCaptureTasks(tasks("b"));
    replies[1].reject(new Error("denied"));
    await expect(failing).rejects.toThrow("denied");
  });
});
