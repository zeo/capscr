import { invoke as call, type InvokeOptions } from "@ceri/app";

export { appWindow, dialog, fileSrc, listen, opener, windowLabel, type Unlisten } from "@ceri/app";

// the views show a failed command's message as it is
export function invoke<T = unknown>(command: string, args?: unknown, options?: InvokeOptions): Promise<T> {
  return call<T>(command, args, options).catch((e: unknown) => Promise.reject(e instanceof Error ? e.message : e));
}
