export interface Point {
  x: number;
  y: number;
}

export interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

interface Stroke {
  color: string;
  width: number;
}

export interface ArrowOp extends Stroke {
  kind: "arrow";
  from: Point;
  to: Point;
}

export interface LineOp extends Stroke {
  kind: "line";
  from: Point;
  to: Point;
}

// width holds the stroke setting; the marker renders wider, see markerWidth
export interface HighlightOp extends Stroke {
  kind: "highlight";
  from: Point;
  to: Point;
}

export interface RectOp extends Stroke {
  kind: "rect";
  origin: Point;
  size: { w: number; h: number };
  fill: boolean;
}

export interface EllipseOp extends Stroke {
  kind: "ellipse";
  origin: Point;
  size: { w: number; h: number };
  fill: boolean;
}

export interface BlurOp {
  kind: "blur";
  origin: Point;
  size: { w: number; h: number };
  radius: number;
}

export interface TextOp {
  kind: "text";
  origin: Point;
  text: string;
  color: string;
  fontSize: number;
}

export interface StepOp {
  kind: "step";
  center: Point;
  number: number;
  color: string;
  radius: number;
}

export interface PenOp extends Stroke {
  kind: "pen";
  points: Point[];
}

export type Op =
  | ArrowOp
  | RectOp
  | TextOp
  | BlurOp
  | StepOp
  | LineOp
  | EllipseOp
  | HighlightOp
  | PenOp;

export type Handle = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w" | "from" | "to";

export interface Style {
  color: string;
  width: number;
  fontSize: number;
  radius: number;
  fill: boolean;
  block: number;
}

export type MeasureText = (op: TextOp) => { w: number; h: number };

export const TEXT_LINE_HEIGHT = 1.25;

export const markerWidth = (width: number) => Math.max(8, width * 4);

const renderedWidth = (op: ArrowOp | LineOp | HighlightOp | PenOp) =>
  op.kind === "highlight" ? markerWidth(op.width) : op.width;

export function distToSegment(p: Point, a: Point, b: Point): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const lenSq = dx * dx + dy * dy;
  const t = lenSq > 0 ? Math.max(0, Math.min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / lenSq)) : 0;
  return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy));
}

function normalized(origin: Point, size: { w: number; h: number }): Box {
  return {
    x: Math.min(origin.x, origin.x + size.w),
    y: Math.min(origin.y, origin.y + size.h),
    w: Math.abs(size.w),
    h: Math.abs(size.h),
  };
}

// whole pixels of a box that fall inside a w x h image; empty boxes come back with w or h <= 0
export function clipBox(b: Box, w: number, h: number): Box {
  const x = Math.max(0, Math.floor(b.x));
  const y = Math.max(0, Math.floor(b.y));
  return { x, y, w: Math.min(w, Math.ceil(b.x + b.w)) - x, h: Math.min(h, Math.ceil(b.y + b.h)) - y };
}

const inside = (p: Point, b: Box) => p.x >= b.x && p.x <= b.x + b.w && p.y >= b.y && p.y <= b.y + b.h;

export function bboxOf(op: Op, measure: MeasureText): Box {
  switch (op.kind) {
    case "arrow":
    case "line":
    case "highlight": {
      const pad = renderedWidth(op) / 2 + 4;
      return {
        x: Math.min(op.from.x, op.to.x) - pad,
        y: Math.min(op.from.y, op.to.y) - pad,
        w: Math.abs(op.to.x - op.from.x) + pad * 2,
        h: Math.abs(op.to.y - op.from.y) + pad * 2,
      };
    }
    case "rect":
    case "blur":
    case "ellipse": {
      const b = normalized(op.origin, op.size);
      return { x: b.x - 4, y: b.y - 4, w: b.w + 8, h: b.h + 8 };
    }
    case "pen": {
      const xs = op.points.map((p) => p.x);
      const ys = op.points.map((p) => p.y);
      const pad = op.width / 2 + 4;
      const minX = Math.min(...xs);
      const minY = Math.min(...ys);
      return { x: minX - pad, y: minY - pad, w: Math.max(...xs) - minX + pad * 2, h: Math.max(...ys) - minY + pad * 2 };
    }
    case "text": {
      const { w, h } = measure(op);
      return { x: op.origin.x - 4, y: op.origin.y - 4, w: w + 8, h: h + 8 };
    }
    case "step": {
      const pad = op.radius + 4;
      return { x: op.center.x - pad, y: op.center.y - pad, w: pad * 2, h: pad * 2 };
    }
  }
}

export function hitTest(op: Op, p: Point, measure: MeasureText): boolean {
  switch (op.kind) {
    case "arrow":
    case "line":
    case "highlight":
      return distToSegment(p, op.from, op.to) <= Math.max(6, renderedWidth(op) / 2 + 3);
    case "pen": {
      const tol = Math.max(8, op.width / 2 + 2);
      const pts = op.points;
      if (pts.length === 1) return Math.hypot(p.x - pts[0].x, p.y - pts[0].y) <= tol;
      return pts.some((pt, i) => i > 0 && distToSegment(p, pts[i - 1], pt) <= tol);
    }
    case "ellipse": {
      const b = normalized(op.origin, op.size);
      const rx = b.w / 2 + 4;
      const ry = b.h / 2 + 4;
      return ((p.x - b.x - b.w / 2) / rx) ** 2 + ((p.y - b.y - b.h / 2) / ry) ** 2 <= 1;
    }
    case "step":
      return Math.hypot(p.x - op.center.x, p.y - op.center.y) <= op.radius + 4;
    case "rect":
    case "blur":
    case "text":
      return inside(p, bboxOf(op, measure));
  }
}

export function hitTestTop(ops: Op[], p: Point, measure: MeasureText): number | null {
  for (let i = ops.length - 1; i >= 0; i--) {
    if (hitTest(ops[i], p, measure)) return i;
  }
  return null;
}

export function moveOp(op: Op, dx: number, dy: number): Op {
  const shift = (p: Point) => ({ x: p.x + dx, y: p.y + dy });
  switch (op.kind) {
    case "arrow":
    case "line":
    case "highlight":
      return { ...op, from: shift(op.from), to: shift(op.to) };
    case "rect":
    case "blur":
    case "ellipse":
    case "text":
      return { ...op, origin: shift(op.origin) };
    case "step":
      return { ...op, center: shift(op.center) };
    case "pen":
      return { ...op, points: op.points.map(shift) };
  }
}

export function handlesOf(op: Op): { handle: Handle; at: Point }[] {
  switch (op.kind) {
    case "arrow":
    case "line":
    case "highlight":
      return [
        { handle: "from", at: op.from },
        { handle: "to", at: op.to },
      ];
    case "rect":
    case "blur":
    case "ellipse": {
      const { x, y, w, h } = normalized(op.origin, op.size);
      const mx = x + w / 2;
      const my = y + h / 2;
      return [
        { handle: "nw", at: { x, y } },
        { handle: "n", at: { x: mx, y } },
        { handle: "ne", at: { x: x + w, y } },
        { handle: "e", at: { x: x + w, y: my } },
        { handle: "se", at: { x: x + w, y: y + h } },
        { handle: "s", at: { x: mx, y: y + h } },
        { handle: "sw", at: { x, y: y + h } },
        { handle: "w", at: { x, y: my } },
      ];
    }
    default:
      return [];
  }
}

export function handleAt(op: Op, p: Point, radius: number): Handle | null {
  const hit = handlesOf(op).find(({ at }) => Math.hypot(p.x - at.x, p.y - at.y) <= radius);
  return hit ? hit.handle : null;
}

// resizes from the op as it was when the drag began, so dragging past the
// opposite edge flips the box instead of collapsing it
export function resizeOp(op: Op, handle: Handle, p: Point): Op {
  switch (op.kind) {
    case "arrow":
    case "line":
    case "highlight":
      if (handle === "from") return { ...op, from: p };
      if (handle === "to") return { ...op, to: p };
      return op;
    case "rect":
    case "blur":
    case "ellipse": {
      if (handle === "from" || handle === "to") return op;
      const b = normalized(op.origin, op.size);
      let x0 = b.x;
      let y0 = b.y;
      let x1 = b.x + b.w;
      let y1 = b.y + b.h;
      if (handle.includes("w")) x0 = p.x;
      if (handle.includes("e")) x1 = p.x;
      if (handle.includes("n")) y0 = p.y;
      if (handle.includes("s")) y1 = p.y;
      return {
        ...op,
        origin: { x: Math.min(x0, x1), y: Math.min(y0, y1) },
        size: { w: Math.abs(x1 - x0), h: Math.abs(y1 - y0) },
      };
    }
    default:
      return op;
  }
}

export function styleOf(op: Op): Partial<Style> {
  switch (op.kind) {
    case "arrow":
    case "line":
    case "highlight":
    case "pen":
      return { color: op.color, width: op.width };
    case "rect":
    case "ellipse":
      return { color: op.color, width: op.width, fill: op.fill };
    case "text":
      return { color: op.color, fontSize: op.fontSize };
    case "step":
      return { color: op.color, radius: op.radius };
    case "blur":
      return { block: op.radius };
  }
}

// applies only the fields this kind of op carries; returns the same object
// when nothing changes so callers can skip a no-op history entry
export function restyleOp(op: Op, patch: Partial<Style>): Op {
  const current = styleOf(op);
  const changed = (Object.keys(current) as (keyof Style)[]).filter(
    (key) => patch[key] !== undefined && patch[key] !== current[key],
  );
  if (changed.length === 0) return op;
  if (op.kind === "blur") return { ...op, radius: patch.block ?? op.radius };
  const next: Record<string, unknown> = { ...op };
  for (const key of changed) next[key] = patch[key];
  return next as unknown as Op;
}
