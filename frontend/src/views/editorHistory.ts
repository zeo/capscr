import type { Op } from "./editorShapes";

export interface Snapshot<B> {
  base: B;
  ops: Op[];
}

export interface History<B> {
  states: Snapshot<B>[];
  index: number;
  // -1 once the saved state has been dropped from the stack
  saved: number;
}

export const MAX_BASES = 20;

export const createHistory = <B>(base: B): History<B> => ({ states: [{ base, ops: [] }], index: 0, saved: 0 });

export const current = <B>(h: History<B>) => h.states[h.index];

export const canUndo = <B>(h: History<B>) => h.index > 0;

export const canRedo = <B>(h: History<B>) => h.index < h.states.length - 1;

export const isDirty = <B>(h: History<B>) => h.index !== h.saved;

// takes the snapshot that was written, which may no longer be the current
// state if edits landed while the save was in flight
export const markSaved = <B>(h: History<B>, written: Snapshot<B> = current(h)): History<B> => ({
  ...h,
  saved: h.states.indexOf(written),
});

export const undo = <B>(h: History<B>): History<B> => (canUndo(h) ? { ...h, index: h.index - 1 } : h);

export const redo = <B>(h: History<B>): History<B> => (canRedo(h) ? { ...h, index: h.index + 1 } : h);

// drops the redo branch, then the oldest states once more than maxBases
// distinct base images are held, since each base can be a full-size bitmap
export function push<B>(h: History<B>, next: Snapshot<B>, maxBases = MAX_BASES): History<B> {
  let states = [...h.states.slice(0, h.index + 1), next];
  let saved = h.saved > h.index ? -1 : h.saved;
  const bases = new Set(states.map((s) => s.base));
  for (let excess = bases.size - maxBases; excess > 0; excess--) {
    const oldest = states[0].base;
    const keep = states.findIndex((s) => s.base !== oldest);
    states = states.slice(keep);
    saved = saved >= keep ? saved - keep : -1;
  }
  return { states, index: states.length - 1, saved };
}
