import { describe, expect, it } from "vitest";
import type { Op } from "./editorShapes";
import { canRedo, canUndo, createHistory, current, isDirty, markSaved, push, redo, undo, type History } from "./editorHistory";

const step = (n: number): Op => ({ kind: "step", center: { x: n, y: n }, number: n, color: "#ff0000", radius: 10 });

const withOps = (h: History<string>, ...ops: Op[]) => push(h, { base: current(h).base, ops: [...current(h).ops, ...ops] });

describe("editor history", () => {
  it("starts clean with nothing to undo", () => {
    const h = createHistory("a");
    expect(isDirty(h)).toBe(false);
    expect(canUndo(h)).toBe(false);
    expect(canRedo(h)).toBe(false);
  });

  it("walks shape edits and base edits through one stack", () => {
    let h = createHistory("a");
    h = withOps(h, step(1));
    h = push(h, { base: "cropped", ops: [] });
    h = withOps(h, step(2));
    expect(current(h)).toEqual({ base: "cropped", ops: [step(2)] });

    h = undo(undo(h));
    expect(current(h)).toEqual({ base: "a", ops: [step(1)] });
    h = redo(h);
    expect(current(h)).toEqual({ base: "cropped", ops: [] });
    h = redo(h);
    expect(current(h).ops).toEqual([step(2)]);
    expect(redo(h)).toBe(h);
  });

  it("drops the redo branch on a new edit", () => {
    let h = withOps(withOps(createHistory("a"), step(1)), step(2));
    h = withOps(undo(h), step(3));
    expect(canRedo(h)).toBe(false);
    expect(current(h).ops).toEqual([step(1), step(3)]);
  });

  it("tracks dirty against the saved state, including undo past it", () => {
    let h = withOps(createHistory("a"), step(1));
    expect(isDirty(h)).toBe(true);
    h = markSaved(h);
    expect(isDirty(h)).toBe(false);
    expect(isDirty(undo(h))).toBe(true);
    expect(isDirty(redo(undo(h)))).toBe(false);
  });

  it("marks the written snapshot even if edits landed during the save", () => {
    let h = withOps(createHistory("a"), step(1));
    const written = current(h);
    h = withOps(h, step(2));
    h = markSaved(h, written);
    expect(isDirty(h)).toBe(true);
    expect(isDirty(undo(h))).toBe(false);
  });

  it("forgets the saved state once its redo branch is replaced", () => {
    let h = markSaved(withOps(createHistory("a"), step(1)));
    h = withOps(undo(h), step(2));
    expect(h.saved).toBe(-1);
    expect(isDirty(h)).toBe(true);
    expect(isDirty(undo(h))).toBe(true);
  });

  it("caps distinct bases and drops every state of the oldest one", () => {
    let h = withOps(createHistory("b0"), step(1));
    for (let i = 1; i <= 3; i++) h = push(h, { base: `b${i}`, ops: [] }, 3);
    expect(new Set(h.states.map((s) => s.base))).toEqual(new Set(["b1", "b2", "b3"]));
    expect(h.index).toBe(h.states.length - 1);
    expect(h.saved).toBe(-1);
    expect(current(undo(undo(h))).base).toBe("b1");
    expect(canUndo(undo(undo(h)))).toBe(false);
  });
});
