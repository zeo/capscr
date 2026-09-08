import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import {
  ArrowRight,
  Square,
  Type,
  Droplet,
  RotateCcw,
  RotateCw,
  Save,
  Copy,
  Upload,
  X,
  ZoomIn,
  ZoomOut,
  Maximize2,
  Circle,
  Minus,
  CircleDashed,
  Highlighter,
  MousePointer2,
  PenLine,
  Crop,
  FlipHorizontal2,
  FlipVertical2,
  Trash2,
} from "lucide-solid";
import { Titlebar } from "../components/Titlebar";

type Tool =
  | "select"
  | "arrow"
  | "rect"
  | "text"
  | "blur"
  | "step"
  | "line"
  | "ellipse"
  | "highlight"
  | "pen"
  | "crop";

interface Point {
  x: number;
  y: number;
}

interface ArrowOp {
  kind: "arrow";
  from: Point;
  to: Point;
  color: string;
  width: number;
}

interface RectOp {
  kind: "rect";
  origin: Point;
  size: { w: number; h: number };
  color: string;
  width: number;
  fill: boolean;
}

interface TextOp {
  kind: "text";
  origin: Point;
  text: string;
  color: string;
  fontSize: number;
}

interface BlurOp {
  kind: "blur";
  origin: Point;
  size: { w: number; h: number };
  radius: number;
}

interface StepOp {
  kind: "step";
  center: Point;
  number: number;
  color: string;
  radius: number;
}

interface LineOp {
  kind: "line";
  from: Point;
  to: Point;
  color: string;
  width: number;
}

interface EllipseOp {
  kind: "ellipse";
  origin: Point;
  size: { w: number; h: number };
  color: string;
  width: number;
  fill: boolean;
}

interface HighlightOp {
  kind: "highlight";
  from: Point;
  to: Point;
  color: string;
  width: number;
}

interface PenOp {
  kind: "pen";
  points: Point[];
  color: string;
  width: number;
}

interface EditorExitRequest {
  requestId: string;
}

type Op =
  | ArrowOp
  | RectOp
  | TextOp
  | BlurOp
  | StepOp
  | LineOp
  | EllipseOp
  | HighlightOp
  | PenOp;

interface CropDraft {
  kind: "crop";
  origin: Point;
  size: { w: number; h: number };
}

type Draft = Op | CropDraft;

const COLORS = ["#ef4444", "#f59e0b", "#10b981", "#3b82f6", "#a855f7", "#ffffff", "#000000"];

const TEXT_FONT = (size: number, bold = false) =>
  `${bold ? "bold " : ""}${size}px "Fira Code", "Hack", ui-monospace, monospace`;

export function Editor() {
  let canvasRef!: HTMLCanvasElement;
  let baseImage: HTMLImageElement | null = null;

  const [imagePath, setImagePath] = createSignal<string | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  const [tool, setTool] = createSignal<Tool>("arrow");
  const [color, setColor] = createSignal<string>(COLORS[0]);
  const [strokeWidth, setStrokeWidth] = createSignal(3);
  const [textSize, setTextSize] = createSignal(24);
  const [stepRadius, setStepRadius] = createSignal(16);
  const [fillShapes, setFillShapes] = createSignal(false);
  const [blurRadius, setBlurRadius] = createSignal(12);
  const [ops, setOps] = createSignal<Op[]>([]);
  const [redoStack, setRedoStack] = createSignal<Op[]>([]);
  const [selected, setSelected] = createSignal<number | null>(null);
  // snapshots of the baked canvas before each destructive base edit
  // (crop / rotate / flip), so ctrl+z can walk them back. capped at 5.
  const [baseUndo, setBaseUndo] = createSignal<string[]>([]);
  const [hasBaseEdit, setHasBaseEdit] = createSignal(false);
  // 1.0 = fit-to-wrap (handled by CSS max-width); >1.0 = explicit pixel scale.
  const [zoom, setZoom] = createSignal(1.0);
  const ZOOM_LEVELS = [0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];
  const [draft, setDraft] = createSignal<Draft | null>(null);
  const [textInputAt, setTextInputAt] = createSignal<Point | null>(null);
  const [textBuffer, setTextBuffer] = createSignal("");
  const [busy, setBusy] = createSignal<"save" | "copy" | "upload" | null>(null);
  const [status, setStatus] = createSignal<{ tone: string; msg: string } | null>(null);
  // tracks whether a paste replaced the canvas — paste doesn't add to ops[]
  // so isDirty() would otherwise return false, silently discarding the paste
  const [hasPastedContent, setHasPastedContent] = createSignal(false);
  const [isHdrSource, setIsHdrSource] = createSignal(false);
  const [hdrSidecarPath, setHdrSidecarPath] = createSignal<string | null>(null);

  let dragStart: Point | null = null;
  // active object move: the pointer grab point plus the op as it was,
  // so each mousemove re-derives position instead of accumulating drift
  let moveDrag: { start: Point; orig: Op } | null = null;
  let loadGeneration = 0;

  const win = getCurrentWindow();

  const resetEditState = () => {
    setOps([]);
    setRedoStack([]);
    setSelected(null);
    setBaseUndo([]);
    setHasBaseEdit(false);
    setDraft(null);
    setTextInputAt(null);
    setTextBuffer("");
    moveDrag = null;
    dragStart = null;
  };

  const loadImage = async (path: string) => {
    const generation = ++loadGeneration;
    const isCurrent = () => generation === loadGeneration;
    setLoaded(false);
    setStatus(null);
    resetEditState();
    setHasPastedContent(false);
    baseImage = null;
    setIsHdrSource(false);
    setHdrSidecarPath(null);

    const ext = path.split(".").pop()?.toLowerCase();
    if (ext === "gif") {
      setImagePath(path);
      setStatus({
        tone: "err",
        msg: "GIF editing isn't supported — would flatten frames. Close to keep the original.",
      });
      return;
    }

    setImagePath(path);

    let baseIsHdr = false;
    try {
      baseIsHdr = await api.isHdrCapture(path);
    } catch {
      // probe failure isn't fatal
    }
    if (!isCurrent()) return;

    const dot = path.lastIndexOf(".");
    const stem = dot > 0 ? path.slice(0, dot) : path;
    const sidecar = `${stem}.hdr.png`;
    let sidecarIsHdr = false;
    try {
      sidecarIsHdr = await api.isHdrCapture(sidecar);
    } catch {
      // sidecar absent
    }
    if (!isCurrent()) return;
    if (sidecarIsHdr) setHdrSidecarPath(sidecar);
    setIsHdrSource(baseIsHdr || sidecarIsHdr);

    const img = new Image();
    img.src = convertFileSrc(path);
    try {
      await img.decode();
    } catch (e) {
      if (isCurrent()) {
        setStatus({ tone: "err", msg: `image load failed: ${e}` });
      }
      return;
    }
    if (!isCurrent()) return;
    baseImage = img;
    canvasRef.width = img.naturalWidth;
    canvasRef.height = img.naturalHeight;
    setLoaded(true);
    setLoaded(true);
    if (isStagedSession()) {
      setStatus({ tone: "", msg: "annotate, then save to keep the capture." });
    }
    redraw();
  };
  onMount(async () => {
    const generation = loadGeneration;
    const path = await invoke<string | null>("get_editor_image_path");
    if (generation !== loadGeneration) return;
    if (!path) {
      setStatus({ tone: "err", msg: "no image path received from backend" });
      return;
    }
    await loadImage(path);
  });

  // when the editor window is reused for a new image, the backend emits this
  // event instead of opening a fresh window. Without a listener here the
  // canvas would keep showing the previous image while imagePath() points to
  // the new file — any save would overwrite the wrong file.
  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<string>("capscr://editor-load", async (e) => {
      if (isDirty()) {
        const ok = window.confirm(
          "Discard unsaved annotations and open a new image?",
        );
        if (!ok) return;
      }
      await loadImage(e.payload);
    })
      .then((stopListening) => {
        if (disposed) {
          stopListening();
        } else {
          unlisten = stopListening;
        }
      })
      .catch((error) => {
        if (!disposed) {
          setStatus({ tone: "err", msg: `editor listener failed: ${error}` });
        }
      });
    onCleanup(() => {
      disposed = true;
      unlisten?.();
    });
  });

  const stepZoom = (dir: 1 | -1) => {
    const cur = zoom();
    const idx = ZOOM_LEVELS.findIndex((z) => Math.abs(z - cur) < 0.01);
    const fallback = ZOOM_LEVELS.findIndex((z) => z >= cur);
    const start = idx >= 0 ? idx : Math.max(0, fallback);
    const next = Math.max(0, Math.min(ZOOM_LEVELS.length - 1, start + dir));
    setZoom(ZOOM_LEVELS[next]);
  };

  const onKeydown = (e: KeyboardEvent) => {
    if (textInputAt()) return;
    const mod = e.ctrlKey || e.metaKey;
    if (e.key === "z" && mod && !e.shiftKey) {
      e.preventDefault();
      undo();
    } else if ((e.key === "y" && mod) || (e.key === "z" && mod && e.shiftKey)) {
      e.preventDefault();
      redo();
    } else if (mod && (e.key === "=" || e.key === "+")) {
      e.preventDefault();
      stepZoom(1);
    } else if (mod && e.key === "-") {
      e.preventDefault();
      stepZoom(-1);
    } else if (mod && e.key === "0") {
      e.preventDefault();
      setZoom(1.0);
    } else if ((e.key === "Delete" || e.key === "Backspace") && selected() !== null) {
      e.preventDefault();
      deleteSelected();
    } else if (e.key.startsWith("Arrow") && selected() !== null) {
      e.preventDefault();
      const step = e.shiftKey ? 10 : 1;
      const dx = e.key === "ArrowLeft" ? -step : e.key === "ArrowRight" ? step : 0;
      const dy = e.key === "ArrowUp" ? -step : e.key === "ArrowDown" ? step : 0;
      nudgeSelected(dx, dy);
    } else if (e.key === "Escape") {
      if (selected() !== null) {
        setSelected(null);
        redraw();
      } else {
        void win.close();
      }
    } else if (e.key === "v" || e.key === "V" || e.key === "0") pickTool("select");
    else if (e.key === "1") pickTool("arrow");
    else if (e.key === "2") pickTool("rect");
    else if (e.key === "3") {
      pickTool("text");
    } else if (e.key === "4") pickTool("blur");
    else if (e.key === "5") pickTool("step");
    else if (e.key === "6") pickTool("line");
    else if (e.key === "7") pickTool("ellipse");
    else if (e.key === "8") pickTool("highlight");
    else if (e.key === "9") pickTool("pen");
    else if (e.key === "c" || e.key === "C") pickTool("crop");
  };

  const onWheelZoom = (e: WheelEvent) => {
    if (!(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    stepZoom(e.deltaY < 0 ? 1 : -1);
  };

  // replace the current canvas with a pasted clipboard image; users expect
  // paste to work. We accept any image/* type the browser decoded.
  const onPaste = async (e: ClipboardEvent) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (let i = 0; i < items.length; i++) {
      const it = items[i];
      if (!it.type.startsWith("image/")) continue;
      const blob = it.getAsFile();
      if (!blob) continue;
      e.preventDefault();
      const generation = ++loadGeneration;
      const isCurrent = () => generation === loadGeneration;
      const url = URL.createObjectURL(blob);
      try {
        const img = new Image();
        img.src = url;
        await img.decode();
        if (!isCurrent()) return;
        baseImage = img;
        canvasRef.width = img.naturalWidth;
        canvasRef.height = img.naturalHeight;
        resetEditState();
        setHasPastedContent(true);
        setIsHdrSource(false);
        setHdrSidecarPath(null);
        setLoaded(true);
        setStatus({
          tone: "ok",
          msg: "pasted from clipboard — save to write back to disk.",
        });
        redraw();
      } catch (err) {
        if (isCurrent()) {
          setStatus({ tone: "err", msg: `paste failed: ${err}` });
        }
      } finally {
        URL.revokeObjectURL(url);
      }
      return;
    }
    setStatus({ tone: "", msg: "paste: only images are accepted here — use the text tool to add text" });
  };

  // truthy when there's at least one committed edit, a base transform
  // (crop / rotate / flip), or a draft mid-drag. used by the dirty-state
  // guard so Escape / the close button warn before throwing away work.
  // annotate-before-save sessions stage under annotate-stage (see
  // cleanup_annotate_stage in commands.rs); nothing exists outside the
  // editor until Save mints the capture.
  const isStagedSession = () => (imagePath() ?? "").includes("annotate-stage");

  const isDirty = () =>
    ops().length > 0 ||
    draft() !== null ||
    hasPastedContent() ||
    hasBaseEdit() ||
    (textInputAt() !== null && textBuffer().trim().length > 0);

  const confirmDiscardEdits = () => {
    if (!isDirty()) return true;
    if (isStagedSession()) {
      return window.confirm(
        "Discard this annotation? Nothing has been saved yet — closing throws the capture away.",
      );
    }
    const msg = hasPastedContent() || hasBaseEdit()
      ? "Discard pasted image, transforms and unsaved annotations? Changes aren't written to disk until you press Save."
      : "Discard unsaved annotations? They aren't written back to disk until you press Save.";
    return window.confirm(msg);
  };

  const sendEditorExitDecision = (requestId: string, allowed: boolean) => {
    void win.emitTo(
      { kind: "Window", label: "hub" },
      "capscr://editor-exit-decision",
      { requestId, allowed },
    ).catch((error) => {
      setStatus({ tone: "err", msg: `close response failed: ${error}` });
    });
  };

  onMount(() => {
    window.addEventListener("keydown", onKeydown);
    window.addEventListener("paste", onPaste);
  });
  onCleanup(() => {
    window.removeEventListener("keydown", onKeydown);
    window.removeEventListener("paste", onPaste);
  });

  // intercept the close button on the titlebar. Without this, clicking X
  // discards unsaved annotations silently — the same data-loss footgun the
  // Settings tab already guards against.
  let appExitApproved = false;
  let pendingExitRequestId: string | null = null;
  let closeUnlisten: (() => void) | null = null;

  const registerCloseGuard = async () => {
    closeUnlisten = await win.onCloseRequested(async (ev) => {
      ev.preventDefault();
      if (!appExitApproved && !confirmDiscardEdits()) return;
      appExitApproved = true;
      if (await closeApprovedEditor()) return;
      const requestId = pendingExitRequestId;
      pendingExitRequestId = null;
      if (requestId) sendEditorExitDecision(requestId, false);
    });
  };

  const closeApprovedEditor = async () => {
    const stopCloseGuard = closeUnlisten;
    closeUnlisten = null;
    try {
      if (stopCloseGuard) await stopCloseGuard();
      await win.close();
      await new Promise((resolve) => setTimeout(resolve, 250));
      if (!(await Window.getByLabel("editor"))) return true;
      setStatus({ tone: "err", msg: "editor window did not close" });
    } catch (error) {
      setStatus({ tone: "err", msg: `editor close failed: ${error}` });
    }
    appExitApproved = false;
    try {
      await registerCloseGuard();
    } catch (error) {
      setStatus({ tone: "err", msg: `close listener failed: ${error}` });
    }
    return false;
  };

  onMount(() => {
    let disposed = false;
    void registerCloseGuard()
      .then(() => {
        if (!disposed) return;
        closeUnlisten?.();
        closeUnlisten = null;
      })
      .catch((error) => {
        if (!disposed) {
          setStatus({ tone: "err", msg: `close listener failed: ${error}` });
        }
      });
    onCleanup(() => {
      disposed = true;
      closeUnlisten?.();
      closeUnlisten = null;
    });
  });

  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void win.listen<EditorExitRequest>("capscr://editor-exit-request", async (event) => {
      try {
        if (isDirty()) {
          await win.show();
          await win.unminimize();
          await win.setFocus();
        }
        if (!confirmDiscardEdits()) {
          sendEditorExitDecision(event.payload.requestId, false);
          return;
        }
        appExitApproved = true;
        pendingExitRequestId = event.payload.requestId;
        await win.close();
        return;
      } catch (error) {
        if (!disposed) {
          setStatus({ tone: "err", msg: `editor close failed: ${error}` });
        }
      }
      appExitApproved = false;
      pendingExitRequestId = null;
      sendEditorExitDecision(event.payload.requestId, false);
    })
      .then((stopListening) => {
        if (disposed) {
          stopListening();
        } else {
          unlisten = stopListening;
        }
      })
      .catch((error) => {
        if (!disposed) {
          setStatus({ tone: "err", msg: `close listener failed: ${error}` });
        }
      });
    onCleanup(() => {
      disposed = true;
      unlisten?.();
    });
  });

  function pointFromEvent(e: MouseEvent): Point {
    const rect = canvasRef.getBoundingClientRect();
    const scaleX = canvasRef.width / rect.width;
    const scaleY = canvasRef.height / rect.height;
    return {
      x: (e.clientX - rect.left) * scaleX,
      y: (e.clientY - rect.top) * scaleY,
    };
  }

  function redraw() {
    if (!baseImage) return;
    const ctx = canvasRef.getContext("2d");
    if (!ctx) return;
    ctx.drawImage(baseImage, 0, 0);
    for (const op of ops()) {
      renderOp(ctx, op);
    }
    const d = draft();
    if (d) {
      if (d.kind === "crop") drawCropDraft(ctx, d);
      else renderOp(ctx, d);
    }
    const sel = selected();
    if (sel !== null) {
      const op = ops()[sel];
      if (op) drawSelection(ctx, op);
    }
  }

  function renderOp(ctx: CanvasRenderingContext2D, op: Op) {
    switch (op.kind) {
      case "arrow":
        drawArrow(ctx, op.from, op.to, op.color, op.width);
        break;
      case "rect": {
        if (op.fill) {
          ctx.save();
          ctx.globalAlpha = 0.28;
          ctx.fillStyle = op.color;
          ctx.fillRect(op.origin.x, op.origin.y, op.size.w, op.size.h);
          ctx.restore();
        }
        ctx.strokeStyle = op.color;
        ctx.lineWidth = op.width;
        ctx.lineJoin = "miter";
        ctx.strokeRect(op.origin.x, op.origin.y, op.size.w, op.size.h);
        break;
      }
      case "text":
        ctx.fillStyle = op.color;
        ctx.font = TEXT_FONT(op.fontSize);
        ctx.textBaseline = "top";
        // black shadow for legibility
        ctx.shadowColor = "rgba(0,0,0,0.85)";
        ctx.shadowBlur = 4;
        ctx.fillText(op.text, op.origin.x, op.origin.y);
        ctx.shadowBlur = 0;
        ctx.shadowColor = "transparent";
        break;
      case "blur":
        applyBlur(ctx, op);
        break;
      case "step":
        drawStep(ctx, op);
        break;
      case "line":
        ctx.strokeStyle = op.color;
        ctx.lineWidth = op.width;
        ctx.lineCap = "round";
        ctx.beginPath();
        ctx.moveTo(op.from.x, op.from.y);
        ctx.lineTo(op.to.x, op.to.y);
        ctx.stroke();
        break;
      case "ellipse": {
        const cx = op.origin.x + op.size.w / 2;
        const cy = op.origin.y + op.size.h / 2;
        const rx = Math.abs(op.size.w) / 2;
        const ry = Math.abs(op.size.h) / 2;
        ctx.beginPath();
        ctx.ellipse(cx, cy, rx, ry, 0, 0, Math.PI * 2);
        if (op.fill) {
          ctx.save();
          ctx.globalAlpha = 0.28;
          ctx.fillStyle = op.color;
          ctx.fill();
          ctx.restore();
        }
        ctx.strokeStyle = op.color;
        ctx.lineWidth = op.width;
        ctx.stroke();
        break;
      }
      case "highlight":
        // semi-transparent thick stroke, behaves like a text-marker pen
        ctx.save();
        ctx.globalAlpha = 0.35;
        ctx.globalCompositeOperation = "multiply";
        ctx.strokeStyle = op.color;
        ctx.lineWidth = op.width;
        ctx.lineCap = "butt";
        ctx.beginPath();
        ctx.moveTo(op.from.x, op.from.y);
        ctx.lineTo(op.to.x, op.to.y);
        ctx.stroke();
        ctx.restore();
        break;
      case "pen":
        drawPen(ctx, op.points, op.color, op.width);
        break;
    }
  }
  function drawStep(ctx: CanvasRenderingContext2D, op: StepOp) {
    const r = op.radius;
    ctx.beginPath();
    ctx.arc(op.center.x, op.center.y, r, 0, Math.PI * 2);
    ctx.fillStyle = op.color;
    ctx.fill();
    // contrasting outline so the pin reads on busy backgrounds
    ctx.strokeStyle = "rgba(0,0,0,0.6)";
    ctx.lineWidth = Math.max(1, r * 0.08);
    ctx.stroke();
    const label = String(op.number);
    // white text on the colored disk
    ctx.fillStyle = "#ffffff";
    ctx.font = TEXT_FONT(Math.round(r * 1.1), true);
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.shadowColor = "rgba(0,0,0,0.65)";
    ctx.shadowBlur = 3;
    ctx.fillText(label, op.center.x, op.center.y + 1);
    ctx.shadowBlur = 0;
    ctx.shadowColor = "transparent";
    ctx.textAlign = "start";
    ctx.textBaseline = "alphabetic";
  }

  function drawArrow(
    ctx: CanvasRenderingContext2D,
    a: Point,
    b: Point,
    col: string,
    w: number,
  ) {
    ctx.strokeStyle = col;
    ctx.fillStyle = col;
    ctx.lineWidth = w;
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(a.x, a.y);
    ctx.lineTo(b.x, b.y);
    ctx.stroke();

    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const len = Math.hypot(dx, dy);
    if (len < 1) return;
    const ux = dx / len;
    const uy = dy / len;
    const head = Math.max(10, w * 3);
    const wing = head * 0.5;
    const p1 = { x: b.x - ux * head + -uy * wing, y: b.y - uy * head + ux * wing };
    const p2 = { x: b.x - ux * head - -uy * wing, y: b.y - uy * head - ux * wing };
    ctx.beginPath();
    ctx.moveTo(b.x, b.y);
    ctx.lineTo(p1.x, p1.y);
    ctx.lineTo(p2.x, p2.y);
    ctx.closePath();
    ctx.fill();
  }

  function drawPen(ctx: CanvasRenderingContext2D, points: Point[], col: string, w: number) {
    if (points.length === 0) return;
    ctx.strokeStyle = col;
    ctx.fillStyle = col;
    ctx.lineWidth = w;
    ctx.lineCap = "round";
    ctx.lineJoin = "round";
    if (points.length === 1) {
      ctx.beginPath();
      ctx.arc(points[0].x, points[0].y, Math.max(1, w / 2), 0, Math.PI * 2);
      ctx.fill();
      return;
    }
    // midpoint quadratic smoothing so fast strokes don't come out jagged.
    // hit-testing still uses the raw segments, which hug this curve closely.
    ctx.beginPath();
    ctx.moveTo(points[0].x, points[0].y);
    for (let i = 1; i < points.length - 1; i++) {
      const mx = (points[i].x + points[i + 1].x) / 2;
      const my = (points[i].y + points[i + 1].y) / 2;
      ctx.quadraticCurveTo(points[i].x, points[i].y, mx, my);
    }
    const last = points[points.length - 1];
    ctx.lineTo(last.x, last.y);
    ctx.stroke();
  }

  function drawCropDraft(ctx: CanvasRenderingContext2D, d: CropDraft) {
    // dim everything outside the crop, dashed outline on the kept region
    ctx.save();
    ctx.fillStyle = "rgba(0,0,0,0.55)";
    const { x, y } = d.origin;
    const { w, h } = d.size;
    const cw = ctx.canvas.width;
    const ch = ctx.canvas.height;
    ctx.fillRect(0, 0, cw, y);
    ctx.fillRect(0, y + h, cw, ch - y - h);
    ctx.fillRect(0, y, x, h);
    ctx.fillRect(x + w, y, cw - x - w, h);
    ctx.restore();
    ctx.save();
    ctx.strokeStyle = "#ffffff";
    ctx.lineWidth = 1.5;
    ctx.setLineDash([8, 5]);
    ctx.shadowColor = "rgba(0,0,0,0.8)";
    ctx.shadowBlur = 4;
    ctx.strokeRect(x, y, w, h);
    ctx.restore();
  }

  function applyBlur(ctx: CanvasRenderingContext2D, op: BlurOp) {
    const canvasW = ctx.canvas.width;
    const canvasH = ctx.canvas.height;
    // clamp to canvas bounds so getImageData never reads outside — reads
    // outside return transparent black and corrupt the average at edges
    const x = Math.max(0, Math.min(Math.floor(op.origin.x), canvasW - 1));
    const y = Math.max(0, Math.min(Math.floor(op.origin.y), canvasH - 1));
    const w = Math.max(1, Math.min(Math.floor(op.size.w), canvasW - x));
    const h = Math.max(1, Math.min(Math.floor(op.size.h), canvasH - y));
    if (w <= 0 || h <= 0) return;

    // use a pixelate-style mosaic — it is faster and more privacy-safe than a true blur.
    const cell = Math.max(4, op.radius);
    const cols = Math.ceil(w / cell);
    const rows = Math.ceil(h / cell);
    for (let row = 0; row < rows; row++) {
      for (let col = 0; col < cols; col++) {
        const sx = x + col * cell;
        const sy = y + row * cell;
        const sw = Math.min(cell, w - col * cell);
        const sh = Math.min(cell, h - row * cell);
        const data = ctx.getImageData(sx, sy, sw, sh).data;
        let r = 0, g = 0, b = 0, n = 0;
        for (let i = 0; i < data.length; i += 4) {
          r += data[i];
          g += data[i + 1];
          b += data[i + 2];
          n++;
        }
        ctx.fillStyle = `rgb(${(r / n) | 0}, ${(g / n) | 0}, ${(b / n) | 0})`;
        ctx.fillRect(sx, sy, sw, sh);
      }
    }
  }

  // ---------- object selection: hit-testing, bounding boxes, moving ----------

  function distToSegment(p: Point, a: Point, b: Point): number {
    const dx = b.x - a.x;
    const dy = b.y - a.y;
    const lenSq = dx * dx + dy * dy;
    let t = 0;
    if (lenSq > 0) {
      t = Math.max(0, Math.min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / lenSq));
    }
    return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy));
  }

  function bboxOf(ctx: CanvasRenderingContext2D, op: Op): { x: number; y: number; w: number; h: number } {
    switch (op.kind) {
      case "arrow":
      case "line":
      case "highlight": {
        const pad = op.width / 2 + 4;
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
        const x = Math.min(op.origin.x, op.origin.x + op.size.w);
        const y = Math.min(op.origin.y, op.origin.y + op.size.h);
        return { x: x - 4, y: y - 4, w: Math.abs(op.size.w) + 8, h: Math.abs(op.size.h) + 8 };
      }
      case "pen": {
        let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
        for (const p of op.points) {
          minX = Math.min(minX, p.x);
          minY = Math.min(minY, p.y);
          maxX = Math.max(maxX, p.x);
          maxY = Math.max(maxY, p.y);
        }
        const pad = op.width / 2 + 4;
        return { x: minX - pad, y: minY - pad, w: maxX - minX + pad * 2, h: maxY - minY + pad * 2 };
      }
      case "text": {
        ctx.save();
        ctx.font = TEXT_FONT(op.fontSize);
        const w = ctx.measureText(op.text).width;
        ctx.restore();
        return { x: op.origin.x - 4, y: op.origin.y - 4, w: w + 8, h: op.fontSize * 1.25 + 8 };
      }
      case "step": {
        const pad = op.radius + 4;
        return { x: op.center.x - pad, y: op.center.y - pad, w: pad * 2, h: pad * 2 };
      }
    }
  }

  function hitTest(ctx: CanvasRenderingContext2D, op: Op, p: Point): boolean {
    switch (op.kind) {
      case "arrow":
      case "line":
      case "highlight":
        return distToSegment(p, op.from, op.to) <= Math.max(6, op.width / 2 + 3);
      case "pen": {
        const tol = Math.max(8, op.width / 2 + 2);
        const pts = op.points;
        if (pts.length === 1) return Math.hypot(p.x - pts[0].x, p.y - pts[0].y) <= tol;
        for (let i = 1; i < pts.length; i++) {
          if (distToSegment(p, pts[i - 1], pts[i]) <= tol) return true;
        }
        return false;
      }
      case "rect":
      case "blur": {
        const b = bboxOf(ctx, op);
        return p.x >= b.x && p.x <= b.x + b.w && p.y >= b.y && p.y <= b.y + b.h;
      }
      case "ellipse": {
        const cx = op.origin.x + op.size.w / 2;
        const cy = op.origin.y + op.size.h / 2;
        const rx = Math.abs(op.size.w) / 2 + 4;
        const ry = Math.abs(op.size.h) / 2 + 4;
        if (rx <= 0 || ry <= 0) return false;
        return ((p.x - cx) / rx) ** 2 + ((p.y - cy) / ry) ** 2 <= 1;
      }
      case "text": {
        const b = bboxOf(ctx, op);
        return p.x >= b.x && p.x <= b.x + b.w && p.y >= b.y && p.y <= b.y + b.h;
      }
      case "step":
        return Math.hypot(p.x - op.center.x, p.y - op.center.y) <= op.radius + 4;
    }
  }

  function hitTestTop(p: Point): number | null {
    const ctx = canvasRef.getContext("2d");
    if (!ctx) return null;
    const list = ops();
    for (let i = list.length - 1; i >= 0; i--) {
      if (hitTest(ctx, list[i], p)) return i;
    }
    return null;
  }

  function drawSelection(ctx: CanvasRenderingContext2D, op: Op) {
    const b = bboxOf(ctx, op);
    ctx.save();
    ctx.strokeStyle = "#ffffff";
    ctx.lineWidth = 1.5;
    ctx.setLineDash([7, 5]);
    ctx.shadowColor = "rgba(0,0,0,0.9)";
    ctx.shadowBlur = 4;
    ctx.strokeRect(b.x, b.y, b.w, b.h);
    ctx.restore();
  }

  function moveOp(op: Op, dx: number, dy: number): Op {
    switch (op.kind) {
      case "arrow":
      case "line":
      case "highlight":
        return {
          ...op,
          from: { x: op.from.x + dx, y: op.from.y + dy },
          to: { x: op.to.x + dx, y: op.to.y + dy },
        };
      case "rect":
      case "blur":
      case "ellipse":
        return { ...op, origin: { x: op.origin.x + dx, y: op.origin.y + dy } };
      case "text":
        return { ...op, origin: { x: op.origin.x + dx, y: op.origin.y + dy } };
      case "step":
        return { ...op, center: { x: op.center.x + dx, y: op.center.y + dy } };
      case "pen":
        return { ...op, points: op.points.map((p) => ({ x: p.x + dx, y: p.y + dy })) };
    }
  }

  function nudgeSelected(dx: number, dy: number) {
    const i = selected();
    if (i === null) return;
    const cur = ops();
    if (i < 0 || i >= cur.length) return;
    const next = [...cur];
    next[i] = moveOp(cur[i], dx, dy);
    setOps(next);
    redraw();
  }

  function deleteSelected() {
    const i = selected();
    if (i === null) return;
    const cur = ops();
    if (i < 0 || i >= cur.length) {
      setSelected(null);
      redraw();
      return;
    }
    setRedoStack([...redoStack(), cur[i]]);
    setOps(cur.filter((_, j) => j !== i));
    setSelected(null);
    redraw();
  }

  // ---------- destructive base edits: crop / rotate / flip ----------

  // the canvas already shows base + committed ops, so its bitmap is the
  // baked pre-edit state — snapshot it for undo before transforming.
  function bakedDataURL(): string {
    redraw();
    return canvasRef.toDataURL("image/png");
  }

  function pushBaseUndo(url: string) {
    setBaseUndo([...baseUndo(), url].slice(-5));
  }

  function replaceBaseWith(url: string, w: number, h: number): Promise<void> {
    const img = new Image();
    img.src = url;
    return img.decode().then(() => {
      baseImage = img;
      canvasRef.width = w;
      canvasRef.height = h;
      setOps([]);
      setRedoStack([]);
      setDraft(null);
      setSelected(null);
      moveDrag = null;
      dragStart = null;
      setHasBaseEdit(true);
      redraw();
    }).catch((e) => {
      setStatus({ tone: "err", msg: `transform failed: ${e}` });
    });
  }

  function applyCrop(origin: Point, size: { w: number; h: number }) {
    const cw = canvasRef.width;
    const ch = canvasRef.height;
    const x = Math.max(0, Math.min(Math.round(origin.x), cw - 1));
    const y = Math.max(0, Math.min(Math.round(origin.y), ch - 1));
    const w = Math.max(1, Math.min(Math.round(size.w), cw - x));
    const h = Math.max(1, Math.min(Math.round(size.h), ch - y));
    if (w < 10 || h < 10) {
      setStatus({ tone: "", msg: "crop region too small — drag a larger area." });
      setDraft(null);
      redraw();
      return;
    }
    const prev = bakedDataURL();
    const off = document.createElement("canvas");
    off.width = w;
    off.height = h;
    off.getContext("2d")!.drawImage(canvasRef, x, y, w, h, 0, 0, w, h);
    pushBaseUndo(prev);
    setRedoStack([]);
    setDraft(null);
    void replaceBaseWith(off.toDataURL("image/png"), w, h).then(() => {
      setStatus({ tone: "ok", msg: `cropped to ${w} × ${h} — ctrl+z walks it back.` });
    });
  }

  function rotateBase(dir: 1 | -1) {
    if (!baseImage || !loaded() || busy() !== null) return;
    if (textInputAt()) commitText();
    const prev = bakedDataURL();
    const off = document.createElement("canvas");
    off.width = canvasRef.height;
    off.height = canvasRef.width;
    const ctx = off.getContext("2d")!;
    ctx.translate(off.width / 2, off.height / 2);
    ctx.rotate(dir * Math.PI / 2);
    ctx.drawImage(canvasRef, -canvasRef.width / 2, -canvasRef.height / 2);
    pushBaseUndo(prev);
    setRedoStack([]);
    void replaceBaseWith(off.toDataURL("image/png"), off.width, off.height);
  }

  function flipBase(horizontal: boolean) {
    if (!baseImage || !loaded() || busy() !== null) return;
    if (textInputAt()) commitText();
    const prev = bakedDataURL();
    const off = document.createElement("canvas");
    off.width = canvasRef.width;
    off.height = canvasRef.height;
    const ctx = off.getContext("2d")!;
    ctx.translate(horizontal ? off.width : 0, horizontal ? 0 : off.height);
    ctx.scale(horizontal ? -1 : 1, horizontal ? 1 : -1);
    ctx.drawImage(canvasRef, 0, 0);
    pushBaseUndo(prev);
    setRedoStack([]);
    void replaceBaseWith(off.toDataURL("image/png"), off.width, off.height);
  }

  function restoreBaseSnapshot(url: string, rest: string[]) {
    const img = new Image();
    img.src = url;
    img.decode().then(() => {
      baseImage = img;
      canvasRef.width = img.naturalWidth;
      canvasRef.height = img.naturalHeight;
      setOps([]);
      setRedoStack([]);
      setDraft(null);
      setSelected(null);
      setBaseUndo(rest);
      setHasBaseEdit(rest.length > 0 || hasPastedContent());
      redraw();
    }).catch((e) => {
      setStatus({ tone: "err", msg: `undo failed: ${e}` });
    });
  }
  function onMouseDown(e: MouseEvent) {
    if (!loaded() || busy() !== null) return;
    if (textInputAt()) return;
    const p = pointFromEvent(e);
    const t = tool();
    if (t === "select") {
      const idx = hitTestTop(p);
      if (idx === null) {
        if (selected() !== null) {
          setSelected(null);
          redraw();
        }
        return;
      }
      setSelected(idx);
      moveDrag = { start: p, orig: ops()[idx] };
      redraw();
      return;
    }
    dragStart = p;
    if (t === "arrow") {
      setDraft({ kind: "arrow", from: p, to: p, color: color(), width: strokeWidth() });
    } else if (t === "rect") {
      setDraft({ kind: "rect", origin: p, size: { w: 0, h: 0 }, color: color(), width: strokeWidth(), fill: fillShapes() });
    } else if (t === "blur") {
      setDraft({ kind: "blur", origin: p, size: { w: 0, h: 0 }, radius: blurRadius() });
    } else if (t === "line") {
      setDraft({ kind: "line", from: p, to: p, color: color(), width: strokeWidth() });
    } else if (t === "ellipse") {
      setDraft({
        kind: "ellipse",
        origin: p,
        size: { w: 0, h: 0 },
        color: color(),
        width: strokeWidth(),
        fill: fillShapes(),
      });
    } else if (t === "highlight") {
      // 3x default stroke so the marker actually feels like a highlighter
      setDraft({
        kind: "highlight",
        from: p,
        to: p,
        color: color(),
        width: Math.max(8, strokeWidth() * 4),
      });
    } else if (t === "pen") {
      setDraft({ kind: "pen", points: [p], color: color(), width: strokeWidth() });
    } else if (t === "crop") {
      setDraft({ kind: "crop", origin: p, size: { w: 0, h: 0 } });
    } else if (t === "text") {
      setTextInputAt(p);
      setTextBuffer("");
      // focus the input after solid renders it
      setTimeout(() => {
        const el = document.getElementById("editor-text-input") as HTMLInputElement | null;
        el?.focus();
      }, 0);
    } else if (t === "step") {
      // click-to-drop, not drag. Auto-increment from the highest existing
      // step number so undo/redo stays consistent.
      const next = nextStepNumber();
      const op: StepOp = {
        kind: "step",
        center: p,
        number: next,
        color: color(),
        radius: stepRadius(),
      };
      setOps([...ops(), op]);
      setRedoStack([]);
      setSelected(ops().length - 1);
      dragStart = null;
      redraw();
    }
  }

  function nextStepNumber(): number {
    let max = 0;
    for (const op of ops()) {
      if (op.kind === "step" && op.number > max) max = op.number;
    }
    return max + 1;
  }

  function onMouseMove(e: MouseEvent) {
    if (moveDrag) {
      if (tool() !== "select") {
        moveDrag = null;
        return;
      }
      const p = pointFromEvent(e);
      const dx = p.x - moveDrag.start.x;
      const dy = p.y - moveDrag.start.y;
      if (dx === 0 && dy === 0) return;
      const idx = selected();
      if (idx === null) {
        moveDrag = null;
        return;
      }
      const next = [...ops()];
      next[idx] = moveOp(moveDrag.orig, dx, dy);
      setOps(next);
      redraw();
      return;
    }
    if (!dragStart) return;
    const p = pointFromEvent(e);
    const d = draft();
    if (!d) return;
    if (d.kind === "arrow" || d.kind === "line" || d.kind === "highlight") {
      setDraft({ ...d, to: p });
    } else if (d.kind === "pen") {
      const last = d.points[d.points.length - 1];
      // skip sub-pixel jitter so long strokes stay small in memory
      if (Math.hypot(p.x - last.x, p.y - last.y) < 2) return;
      setDraft({ ...d, points: [...d.points, p] });
    } else if (d.kind === "rect" || d.kind === "blur" || d.kind === "ellipse" || d.kind === "crop") {
      const ox = Math.min(dragStart.x, p.x);
      const oy = Math.min(dragStart.y, p.y);
      const w = Math.abs(p.x - dragStart.x);
      const h = Math.abs(p.y - dragStart.y);
      setDraft({ ...d, origin: { x: ox, y: oy }, size: { w, h } });
    }
    redraw();
  }

  function onMouseUp() {
    if (moveDrag) {
      moveDrag = null;
      return;
    }
    const d = draft();
    dragStart = null;
    if (!d) return;
    if (d.kind === "crop") {
      setDraft(null);
      applyCrop(d.origin, d.size);
      return;
    }
    // discard zero-size drafts
    if (d.kind === "arrow" || d.kind === "line" || d.kind === "highlight") {
      const dx = d.to.x - d.from.x;
      const dy = d.to.y - d.from.y;
      if (Math.hypot(dx, dy) < 3) {
        setDraft(null);
        redraw();
        return;
      }
    }
    if (d.kind === "rect" || d.kind === "blur" || d.kind === "ellipse") {
      if (d.size.w < 3 || d.size.h < 3) {
        setDraft(null);
        redraw();
        return;
      }
    }
    if (d.kind === "pen" && d.points.length === 0) {
      setDraft(null);
      redraw();
      return;
    }
    setOps([...ops(), d]);
    setRedoStack([]);
    setSelected(ops().length - 1);
    setDraft(null);
    redraw();
  }

  // switching tools commits pending text instead of stranding the input
  // box at a stale position while canvas input stays blocked behind it.
  function pickTool(t: Tool) {
    if (textInputAt()) commitText();
    setTool(t);
  }

  function commitText() {
    const at = textInputAt();
    const t = textBuffer().trim();
    if (at && t.length > 0) {
      setOps([
        ...ops(),
        { kind: "text", origin: at, text: t, color: color(), fontSize: textSize() },
      ]);
      setRedoStack([]);
      setSelected(ops().length - 1);
    }
    setTextInputAt(null);
    setTextBuffer("");
    redraw();
  }

  function cancelText() {
    setTextInputAt(null);
    setTextBuffer("");
  }

  function undo() {
    const cur = ops();
    if (cur.length > 0) {
      const last = cur[cur.length - 1];
      setOps(cur.slice(0, -1));
      setRedoStack([...redoStack(), last]);
      if (selected() !== null && selected()! >= cur.length - 1) setSelected(null);
      redraw();
      return;
    }
    // no ops left — walk back the last destructive base edit instead,
    // so crop / rotate / flip don't feel permanent.
    const snaps = baseUndo();
    if (snaps.length === 0) return;
    restoreBaseSnapshot(snaps[snaps.length - 1], snaps.slice(0, -1));
  }

  function redo() {
    const stack = redoStack();
    if (stack.length === 0) return;
    const next = stack[stack.length - 1];
    setRedoStack(stack.slice(0, -1));
    setOps([...ops(), next]);
    setSelected(ops().length - 1);
    redraw();
  }

  function targetMime(): string | null {
    const path = imagePath() ?? "";
    const ext = path.split(".").pop()?.toLowerCase() ?? "png";
    switch (ext) {
      case "jpg":
      case "jpeg":
        return "image/jpeg";
      case "webp":
        return "image/webp";
      case "png":
        return "image/png";
      // bmp / avif / jxl have no browser encoder — writing PNG bytes under
      // those extensions would silently corrupt the file, so refuse and let
      // copy / upload (which always export PNG) carry the image out instead.
      default:
        return null;
    }
  }

  function targetExt(): string {
    const path = imagePath() ?? "";
    return path.split(".").pop()?.toLowerCase() ?? "png";
  }

  async function exportBytes(mime: string): Promise<Uint8Array> {
    const blob: Blob = await new Promise((res, rej) => {
      canvasRef.toBlob(
        (b) => (b ? res(b) : rej(new Error(`toBlob(${mime}) failed`))),
        mime,
      );
    });
    const buf = await blob.arrayBuffer();
    return new Uint8Array(buf);
  }

  async function onSave() {
    // flush pending text into the canvas first — exportBytes reads pixels,
    // so an uncommitted input box would otherwise be silently dropped.
    if (textInputAt()) commitText();
    const path = imagePath();
    if (!path) return;
    const mime = targetMime();
    if (!mime) {
      setStatus({
        tone: "err",
        msg: `saving .${targetExt()} isn't supported in the editor — use copy or upload to export as PNG.`,
      });
      return;
    }
    setBusy("save");
    setStatus({ tone: "", msg: "writing..." });
    try {
      // encode in the source file's format so we don't write PNG bytes to a
      // .jpg path (which would silently corrupt the file for downstream
      // consumers that trust the extension).
      const bytes = await exportBytes(mime);
      await invoke<void>("save_edited_image", {
        bytes: Array.from(bytes),
        targetPath: path,
      });
      resetEditState();
      setHasPastedContent(false);
      setStatus({ tone: "ok", msg: "saved." });
      setTimeout(() => void win.close(), 400);
    } catch (e) {
      setStatus({ tone: "err", msg: `save failed: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  async function onCopy() {
    if (textInputAt()) commitText();
    setBusy("copy");
    setStatus({ tone: "", msg: "copying..." });
    try {
      // clipboard always wants PNG — that is what the Rust ClipboardManager
      // expects to decode.
      const bytes = await exportBytes("image/png");
      await invoke<void>("copy_edited_image_to_clipboard", {
        bytes: Array.from(bytes),
      });
      setStatus({ tone: "ok", msg: "copied to clipboard." });
    } catch (e) {
      setStatus({ tone: "err", msg: `copy failed: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  async function onUpload() {
    if (textInputAt()) commitText();
    setBusy("upload");
    setStatus({ tone: "", msg: "uploading..." });
    try {
      // upload path also expects PNG — the existing ImageUploader.upload
      // re-encodes, so we feed it canonical bytes.
      const bytes = await exportBytes("image/png");
      const result = await invoke<{ url: string; delete_url: string | null }>(
        "upload_edited_image",
        { bytes: Array.from(bytes) },
      );
      setStatus({ tone: "ok", msg: result.url });
    } catch (e) {
      setStatus({ tone: "err", msg: `upload failed: ${e}` });
    } finally {
      setBusy(null);
    }
  }
  return (
    <div class="editor">
      <Titlebar context="edit" />

      <div class="editor-toolbar">
        <div class="editor-tools">
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "select" }}
            onClick={() => pickTool("select")}
            title="select / move (v)"
          >
            <MousePointer2 size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "arrow" }}
            onClick={() => pickTool("arrow")}
            title="arrow (1)"
          >
            <ArrowRight size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "rect" }}
            onClick={() => pickTool("rect")}
            title="rectangle (2)"
          >
            <Square size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "text" }}
            onClick={() => pickTool("text")}
            title="text (3)"
          >
            <Type size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "blur" }}
            onClick={() => pickTool("blur")}
            title="pixelate (4)"
          >
            <Droplet size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "step" }}
            onClick={() => pickTool("step")}
            title="numbered step (5)"
          >
            <Circle size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "line" }}
            onClick={() => pickTool("line")}
            title="line (6)"
          >
            <Minus size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "ellipse" }}
            onClick={() => pickTool("ellipse")}
            title="ellipse (7)"
          >
            <CircleDashed size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "highlight" }}
            onClick={() => pickTool("highlight")}
            title="highlighter (8)"
          >
            <Highlighter size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "pen" }}
            onClick={() => pickTool("pen")}
            title="freehand pen (9)"
          >
            <PenLine size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "crop" }}
            onClick={() => pickTool("crop")}
            title="crop (c)"
          >
            <Crop size={14} stroke-width={1.5} />
          </button>
        </div>

        <div class="editor-colors">
          <For each={COLORS}>
            {(c) => (
              <button
                type="button"
                class="swatch"
                classList={{ "is-active": color() === c }}
                style={{ background: c }}
                onClick={() => setColor(c)}
                aria-label={c}
              />
            )}
          </For>
        </div>

        <div class="editor-controls">
          <Show
            when={
              tool() === "arrow" ||
              tool() === "rect" ||
              tool() === "line" ||
              tool() === "ellipse" ||
              tool() === "highlight" ||
              tool() === "pen"
            }
          >
            <label class="ctrl">
              <span>stroke</span>
              <input
                type="range"
                min={1}
                max={20}
                value={strokeWidth()}
                onInput={(e) => setStrokeWidth(parseInt(e.currentTarget.value))}
              />
              <span class="ctrl-val">{strokeWidth()}</span>
            </label>
          </Show>
          <Show when={tool() === "rect" || tool() === "ellipse"}>
            <label class="ctrl">
              <input
                type="checkbox"
                checked={fillShapes()}
                onChange={(e) => setFillShapes(e.currentTarget.checked)}
              />
              <span>fill</span>
            </label>
          </Show>
          <Show when={tool() === "text"}>
            <label class="ctrl">
              <span>size</span>
              <input
                type="range"
                min={10}
                max={72}
                value={textSize()}
                onInput={(e) => setTextSize(parseInt(e.currentTarget.value))}
              />
              <span class="ctrl-val">{textSize()}</span>
            </label>
          </Show>
          <Show when={tool() === "blur"}>
            <label class="ctrl">
              <span>block</span>
              <input
                type="range"
                min={4}
                max={40}
                value={blurRadius()}
                onInput={(e) => setBlurRadius(parseInt(e.currentTarget.value))}
              />
              <span class="ctrl-val">{blurRadius()}</span>
            </label>
          </Show>
          <Show when={tool() === "step"}>
            <label class="ctrl">
              <span>size</span>
              <input
                type="range"
                min={8}
                max={48}
                value={stepRadius()}
                onInput={(e) => setStepRadius(parseInt(e.currentTarget.value))}
              />
              <span class="ctrl-val">{stepRadius()}</span>
            </label>
          </Show>
        </div>

        <div class="editor-actions">
          <button
            class="btn"
            data-variant="ghost"
            onClick={undo}
            title="ctrl+z"
            disabled={ops().length === 0 && baseUndo().length === 0}
          >
            <RotateCcw size={12} stroke-width={1.5} />
            undo
          </button>
          <button
            class="btn"
            data-variant="ghost"
            onClick={redo}
            title="ctrl+y"
            disabled={redoStack().length === 0}
          >
            <RotateCw size={12} stroke-width={1.5} />
            redo
          </button>
          <Show when={selected() !== null}>
            <button
              class="btn"
              data-variant="ghost"
              onClick={deleteSelected}
              title="del"
            >
              <Trash2 size={12} stroke-width={1.5} />
              delete
            </button>
          </Show>
          <span class="editor-zoom-group">
            <button
              class="btn"
              data-variant="ghost"
              data-size="xs"
              onClick={() => stepZoom(-1)}
              title="ctrl+-"
              disabled={zoom() <= ZOOM_LEVELS[0]}
            >
              <ZoomOut size={11} stroke-width={1.5} />
            </button>
            <button
              class="btn"
              data-variant="ghost"
              data-size="xs"
              onClick={() => setZoom(1.0)}
              title="ctrl+0"
            >
              <Maximize2 size={11} stroke-width={1.5} />
              {Math.round(zoom() * 100)}%
            </button>
            <button
              class="btn"
              data-variant="ghost"
              data-size="xs"
              onClick={() => stepZoom(1)}
              title="ctrl+="
              disabled={zoom() >= ZOOM_LEVELS[ZOOM_LEVELS.length - 1]}
            >
              <ZoomIn size={11} stroke-width={1.5} />
            </button>
          </span>
          <span class="editor-transform-group">
            <button
              class="btn"
              data-variant="ghost"
              data-size="xs"
              onClick={(ev) => rotateBase(ev.shiftKey ? -1 : 1)}
              title="rotate 90° clockwise (shift-click for counter-clockwise)"
              disabled={busy() !== null || !loaded()}
            >
              <RotateCw size={11} stroke-width={1.5} />
            </button>
            <button
              class="btn"
              data-variant="ghost"
              data-size="xs"
              onClick={() => flipBase(true)}
              title="flip horizontal"
              disabled={busy() !== null || !loaded()}
            >
              <FlipHorizontal2 size={11} stroke-width={1.5} />
            </button>
            <button
              class="btn"
              data-variant="ghost"
              data-size="xs"
              onClick={() => flipBase(false)}
              title="flip vertical"
              disabled={busy() !== null || !loaded()}
            >
              <FlipVertical2 size={11} stroke-width={1.5} />
            </button>
          </span>
          <button class="btn" onClick={onSave} disabled={busy() !== null || !loaded()}>
            <Save size={12} stroke-width={1.5} />
            save
          </button>
          <button class="btn" data-variant="ghost" onClick={onCopy} disabled={busy() !== null || !loaded()}>
            <Copy size={12} stroke-width={1.5} />
            copy
          </button>
          <button class="btn" data-variant="ghost" onClick={onUpload} disabled={busy() !== null || !loaded()}>
            <Upload size={12} stroke-width={1.5} />
            upload
          </button>
          <button class="btn" data-variant="ghost" onClick={() => void win.close()}>
            <X size={12} stroke-width={1.5} />
            close
          </button>
        </div>
      </div>

      <Show when={isHdrSource()}>
        <div class="editor-hdr-banner" role="status">
          <span class="editor-hdr-glyph">▮</span>
          <span class="editor-hdr-text">
            <strong>HDR capture detected.</strong>
            <Show
              when={hdrSidecarPath()}
              fallback={
                <span>
                  {" "}canvas annotates in SDR; saving from here will flatten the HDR file
                  to SDR. close the editor without saving to keep the original HDR.
                </span>
              }
            >
              <span>
                {" "}capscr kept the HDR original as <code>{hdrSidecarPath()!.split(/[\\/]/).pop()}</code>{" "}
                — annotations save into the SDR copy; the HDR sidecar is untouched.
              </span>
            </Show>
          </span>
        </div>
      </Show>

      <div class="editor-canvas-wrap" onWheel={onWheelZoom}>
        <Show
          when={loaded()}
          fallback={
            <div class="editor-loading">
              <span class="lede">loading capture...</span>
            </div>
          }
        >
          <div
            class="editor-canvas-scroll"
            style={{ width: `${canvasRef ? canvasRef.width * zoom() : 0}px` }}
          >
            <canvas
              ref={canvasRef!}
              class="editor-canvas"
              data-tool={tool()}
              style={{
                width: `${canvasRef ? canvasRef.width * zoom() : 0}px`,
                height: `${canvasRef ? canvasRef.height * zoom() : 0}px`,
                "max-width": "none",
              }}
              onMouseDown={onMouseDown}
              onMouseMove={onMouseMove}
              onMouseUp={onMouseUp}
              onMouseLeave={onMouseUp}
            />
            <Show when={textInputAt()}>
              <div
                class="editor-text-input-wrap"
                style={(() => {
                  const at = textInputAt()!;
                  const rect = canvasRef.getBoundingClientRect();
                  const scaleX = rect.width / canvasRef.width;
                  const scaleY = rect.height / canvasRef.height;
                  return {
                    left: `${at.x * scaleX}px`,
                    top: `${at.y * scaleY}px`,
                  };
                })()}
              >
                <input
                  id="editor-text-input"
                  type="text"
                  class="editor-text-input"
                  value={textBuffer()}
                  onInput={(e) => setTextBuffer(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") {
                      e.preventDefault();
                      e.stopPropagation();
                      commitText();
                    } else if (e.key === "Escape") {
                      e.preventDefault();
                      e.stopPropagation();
                      cancelText();
                    }
                  }}
                  onBlur={commitText}
                  placeholder="type, enter to commit"
                />
              </div>
            </Show>
          </div>
        </Show>
      </div>

      <footer class="editor-foot">
        <Show when={status()}>
          <span
            class="flash"
            data-tone={status()!.tone}
            role={status()!.tone === "err" ? "alert" : "status"}
            aria-live={status()!.tone === "err" ? "assertive" : "polite"}
            aria-atomic="true"
          >
            {status()!.msg}
          </span>
        </Show>
        <span class="grow" />
        <Show when={selected() !== null}>
          <span class="muted">shape {selected()! + 1} of {ops().length} — drag or arrow keys to move, del to remove, esc to deselect</span>
        </Show>
        <Show when={tool() === "crop"}>
          <span class="muted">drag a region — releasing applies the crop</span>
        </Show>
        <Show when={isStagedSession()}>
          <span class="muted">unsaved capture — save to keep it</span>
        </Show>
        <Show when={loaded() && baseImage}>
          <span class="muted">
            {baseImage!.naturalWidth} × {baseImage!.naturalHeight}
          </span>
        </Show>
      </footer>
    </div>
  );
}
