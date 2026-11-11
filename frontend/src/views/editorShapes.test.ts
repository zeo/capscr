import { describe, expect, it } from "vitest";
import {
  bboxOf,
  handleAt,
  handlesOf,
  hitTest,
  hitTestTop,
  moveOp,
  resizeOp,
  restyleOp,
  type MeasureText,
  type Op,
  type RectOp,
} from "./editorShapes";

const measure: MeasureText = (op) => ({ w: op.text.length * 10, h: op.text.split("\n").length * 20 });

const rect: RectOp = { kind: "rect", origin: { x: 10, y: 10 }, size: { w: 100, h: 50 }, color: "#ff0000", width: 3, fill: false };
const arrow: Op = { kind: "arrow", from: { x: 0, y: 0 }, to: { x: 100, y: 0 }, color: "#ff0000", width: 4 };
const text: Op = { kind: "text", origin: { x: 50, y: 50 }, text: "hello", color: "#ffffff", fontSize: 16 };
const step: Op = { kind: "step", center: { x: 200, y: 200 }, number: 1, color: "#ff0000", radius: 16 };
const pen: Op = { kind: "pen", points: [{ x: 0, y: 0 }, { x: 10, y: 10 }, { x: 20, y: 0 }], color: "#000000", width: 2 };
const highlight: Op = { kind: "highlight", from: { x: 0, y: 0 }, to: { x: 100, y: 0 }, color: "#ffff00", width: 3 };

describe("hitTest", () => {
  it("hits a line near its segment and misses past its tolerance", () => {
    expect(hitTest(arrow, { x: 50, y: 5 }, measure)).toBe(true);
    expect(hitTest(arrow, { x: 50, y: 20 }, measure)).toBe(false);
  });

  it("uses the wider rendered marker width for highlights", () => {
    expect(hitTest(highlight, { x: 50, y: 7 }, measure)).toBe(true);
  });

  it("covers boxes, text extents, steps and pen strokes", () => {
    expect(hitTest(rect, { x: 60, y: 30 }, measure)).toBe(true);
    expect(hitTest(rect, { x: 200, y: 30 }, measure)).toBe(false);
    expect(hitTest(text, { x: 95, y: 60 }, measure)).toBe(true);
    expect(hitTest(text, { x: 120, y: 60 }, measure)).toBe(false);
    expect(hitTest(step, { x: 210, y: 210 }, measure)).toBe(true);
    expect(hitTest(pen, { x: 15, y: 5 }, measure)).toBe(true);
    expect(hitTest(pen, { x: 10, y: 30 }, measure)).toBe(false);
  });

  it("returns the topmost hit", () => {
    const below: Op = { ...rect, origin: { x: 0, y: 0 } };
    expect(hitTestTop([below, rect], { x: 60, y: 30 }, measure)).toBe(1);
    expect(hitTestTop([below, rect], { x: 500, y: 500 }, measure)).toBeNull();
  });
});

describe("bboxOf", () => {
  it("normalizes boxes dragged up and left", () => {
    const flipped: Op = { ...rect, origin: { x: 110, y: 60 }, size: { w: -100, h: -50 } };
    expect(bboxOf(flipped, measure)).toEqual({ x: 6, y: 6, w: 108, h: 58 });
  });

  it("measures multi-line text through the injected measure", () => {
    expect(bboxOf({ ...text, text: "ab\ncd" } as Op, measure)).toEqual({ x: 46, y: 46, w: 58, h: 48 });
  });
});

describe("moveOp", () => {
  it("shifts every point of each kind", () => {
    expect(moveOp(arrow, 5, 6)).toMatchObject({ from: { x: 5, y: 6 }, to: { x: 105, y: 6 } });
    expect(moveOp(rect, 5, 6)).toMatchObject({ origin: { x: 15, y: 16 }, size: rect.size });
    expect(moveOp(step, -10, 0)).toMatchObject({ center: { x: 190, y: 200 } });
    expect(moveOp(pen, 1, 1)).toMatchObject({ points: [{ x: 1, y: 1 }, { x: 11, y: 11 }, { x: 21, y: 1 }] });
  });
});

describe("handles and resize", () => {
  it("gives boxes eight handles, lines two and the rest none", () => {
    expect(handlesOf(rect)).toHaveLength(8);
    expect(handlesOf(arrow).map((h) => h.handle)).toEqual(["from", "to"]);
    expect(handlesOf(text)).toEqual([]);
    expect(handlesOf(pen)).toEqual([]);
  });

  it("finds the handle under the pointer within the radius", () => {
    expect(handleAt(rect, { x: 111, y: 61 }, 4)).toBe("se");
    expect(handleAt(rect, { x: 60, y: 9 }, 4)).toBe("n");
    expect(handleAt(rect, { x: 60, y: 30 }, 4)).toBeNull();
  });

  it("moves only the dragged edges of a box", () => {
    expect(resizeOp(rect, "se", { x: 150, y: 100 })).toMatchObject({ origin: { x: 10, y: 10 }, size: { w: 140, h: 90 } });
    expect(resizeOp(rect, "n", { x: 999, y: 0 })).toMatchObject({ origin: { x: 10, y: 0 }, size: { w: 100, h: 60 } });
  });

  it("flips a box dragged past its opposite edge", () => {
    expect(resizeOp(rect, "e", { x: 0, y: 30 })).toMatchObject({ origin: { x: 0, y: 10 }, size: { w: 10, h: 50 } });
  });

  it("moves a single line endpoint", () => {
    expect(resizeOp(arrow, "to", { x: 50, y: 50 })).toMatchObject({ from: { x: 0, y: 0 }, to: { x: 50, y: 50 } });
  });
});

describe("restyleOp", () => {
  it("applies only fields the kind carries", () => {
    expect(restyleOp(rect, { color: "#00ff00", fontSize: 40, fill: true })).toEqual({ ...rect, color: "#00ff00", fill: true });
    expect(restyleOp(text, { width: 9, fontSize: 30 })).toEqual({ ...text, fontSize: 30 });
    expect(restyleOp(step, { radius: 24 })).toEqual({ ...step, radius: 24 });
  });

  it("maps the block setting onto a pixelate radius", () => {
    const blur: Op = { kind: "blur", origin: { x: 0, y: 0 }, size: { w: 10, h: 10 }, radius: 12 };
    expect(restyleOp(blur, { block: 20, color: "#ffffff" })).toEqual({ ...blur, radius: 20 });
  });

  it("returns the same object when nothing changes", () => {
    expect(restyleOp(rect, { color: rect.color, fontSize: 30 })).toBe(rect);
  });
});
