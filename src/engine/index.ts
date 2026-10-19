import { isTauri } from "../lib/env";
import type { EngineModule } from "./types";
import { createTauriEngine } from "./tauri";
import { createMockEngine } from "./mock";

export type { Engine, RemoteSurfaceProps, RemoteThumbnailProps } from "./types";

const mod: EngineModule = isTauri() ? createTauriEngine() : createMockEngine();

export const engine = mod.engine;
export const RemoteSurface = mod.RemoteSurface;
export const RemoteThumbnail = mod.RemoteThumbnail;
