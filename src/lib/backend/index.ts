import type { Backend } from "./types";
import { isTauri } from "../env";

export type { Backend, Unsubscribe } from "./types";

let instance: Backend | null = null;

/** Lazily picks the Tauri backend inside the app, the mock in a plain browser. */
export async function getBackend(): Promise<Backend> {
  if (instance) return instance;
  instance = isTauri()
    ? new (await import("./tauri")).TauriBackend()
    : new (await import("./mock")).MockBackend();
  return instance;
}
