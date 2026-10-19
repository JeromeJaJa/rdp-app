import { createContext } from "react";

/** Tiles register their element here; one IntersectionObserver serves the whole grid. */
export interface TileObserver {
  observe(el: Element, id: string): () => void;
}

export const TileObserverContext = createContext<TileObserver | null>(null);
