import { createResource, createSignal } from "solid-js";
import { api, type AppConfig, type CaptureTask } from "./api";

export const [config, { mutate: mutateConfig, refetch: refetchConfig }] = createResource<AppConfig>(api.getConfig);

export type CaptureTaskSaveState =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "saved"; count: number }
  | { kind: "error"; message: string };

export const [captureTaskSaveState, setCaptureTaskSaveState] =
  createSignal<CaptureTaskSaveState>({ kind: "idle" });

let queuedCaptureTasks: CaptureTask[] | undefined;
let failedCaptureTasks: CaptureTask[] | undefined;
let captureTaskSaveLoop: Promise<void> | null = null;

const mergeCaptureTasks = (captureTasks: CaptureTask[]) => {
  const current = config();
  if (current) mutateConfig({ ...current, capture_tasks: captureTasks });
};

const flushCaptureTasks = async () => {
  while (queuedCaptureTasks) {
    const candidate = queuedCaptureTasks;
    queuedCaptureTasks = undefined;
    setCaptureTaskSaveState({ kind: "saving" });
    try {
      const saved = await api.setCaptureTasks(candidate);
      if (!queuedCaptureTasks) {
        failedCaptureTasks = undefined;
        mergeCaptureTasks(saved);
        setCaptureTaskSaveState({ kind: "saved", count: saved.length });
      }
    } catch (error) {
      if (!queuedCaptureTasks) {
        failedCaptureTasks = candidate;
        setCaptureTaskSaveState({ kind: "error", message: String(error) });
      }
    }
  }
};

const enqueueCaptureTasks = (
  captureTasks: CaptureTask[],
  optimistic: boolean,
) => {
  if (optimistic) mergeCaptureTasks(captureTasks);
  failedCaptureTasks = undefined;
  queuedCaptureTasks = captureTasks;
  if (!captureTaskSaveLoop) {
    captureTaskSaveLoop = flushCaptureTasks().finally(() => {
      captureTaskSaveLoop = null;
      if (queuedCaptureTasks) queueCaptureTasks(queuedCaptureTasks);
    });
  }
};

export const queueCaptureTasks = (captureTasks: CaptureTask[]) => {
  enqueueCaptureTasks(captureTasks, true);
};

export const retryCaptureTaskSave = () => {
  if (failedCaptureTasks) queueCaptureTasks(failedCaptureTasks);
};

export const discardFailedCaptureTaskSave = () => {
  failedCaptureTasks = undefined;
  setCaptureTaskSaveState({ kind: "idle" });
};

export const waitForCaptureTaskSaves = async () => {
  while (captureTaskSaveLoop) await captureTaskSaveLoop;
};

export const saveCaptureTasks = async (captureTasks: CaptureTask[]) => {
  enqueueCaptureTasks(captureTasks, false);
  await waitForCaptureTaskSaves();
  const state = captureTaskSaveState();
  if (state.kind === "error") throw new Error(state.message);
};
