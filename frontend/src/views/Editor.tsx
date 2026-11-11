import { createEffect, createMemo, createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { getCurrentWindow, Window } from "@tauri-apps/api/window";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import {
  ArrowRight,
  Square,
  Type,
  Droplet,
  Undo2,
  Redo2,
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
import {
  bboxOf,
  handleAt,
  handlesOf,
  clipBox,
  hitTestTop,
  markerWidth,
  moveOp,
  resizeOp,
  restyleOp,
  styleOf,
  TEXT_LINE_HEIGHT,
  type BlurOp,
  type Handle,
  type Op,
  type Point,
  type StepOp,
  type Style,
  type TextOp,
} from "./editorShapes";
import * as hist from "./editorHistory";

type Tool = "select" | Op["kind"] | "crop";

interface EditorExitRequest {
  requestId: string;
}

interface CropDraft {
  kind: "crop";
  origin: Point;
  size: { w: number; h: number };
}

type Draft = Op | CropDraft;

type Base = HTMLImageElement | HTMLCanvasElement;

type Drag =
  | { kind: "draw"; start: Point }
  | { kind: "move"; start: Point; index: number; orig: Op }
  | { kind: "resize"; handle: Handle; index: number; orig: Op };

const TOOLS: { id: Tool; title: string; key: string; icon: typeof ArrowRight }[] = [
  { id: "select", title: "select / move", key: "v", icon: MousePointer2 },
  { id: "arrow", title: "arrow", key: "1", icon: ArrowRight },
  { id: "rect", title: "rectangle", key: "2", icon: Square },
  { id: "text", title: "text", key: "3", icon: Type },
  { id: "blur", title: "pixelate", key: "4", icon: Droplet },
  { id: "step", title: "numbered step", key: "5", icon: Circle },
  { id: "line", title: "line", key: "6", icon: Minus },
  { id: "ellipse", title: "ellipse", key: "7", icon: CircleDashed },
  { id: "highlight", title: "highlighter", key: "8", icon: Highlighter },
  { id: "pen", title: "freehand pen", key: "9", icon: PenLine },
  { id: "crop", title: "crop", key: "c", icon: Crop },
];

const STROKE_KINDS: Tool[] = ["arrow", "rect", "line", "ellipse", "highlight", "pen"];

const COLORS = ["#ef4444", "#f59e0b", "#10b981", "#3b82f6", "#a855f7", "#ffffff", "#000000"];

const ZOOM_LEVELS = [0.25, 0.5, 0.75, 1, 1.5, 2, 3, 4, 6, 8];

// css px, divided by the zoom so handles keep their on-screen size
const HANDLE_SIZE = 8;

const HANDLE_CURSORS: Record<Handle, string> = {
  nw: "nwse-resize",
  se: "nwse-resize",
  ne: "nesw-resize",
  sw: "nesw-resize",
  n: "ns-resize",
  s: "ns-resize",
  e: "ew-resize",
  w: "ew-resize",
  from: "crosshair",
  to: "crosshair",
};

const TEXT_FAMILY = `"Fira Code", "Hack", ui-monospace, monospace`;

const TEXT_FONT = (size: number, bold = false) => `${bold ? "bold " : ""}${size}px ${TEXT_FAMILY}`;

const PREFS_KEY = "capscr.editor.prefs";

interface Prefs {
  tool: Tool;
  color: string;
  strokeWidth: number;
  textSize: number;
}

function readPrefs(): Partial<Prefs> {
  let raw: unknown;
  try {
    raw = JSON.parse(localStorage.getItem(PREFS_KEY) ?? "{}");
  } catch (e) {
    if (e instanceof SyntaxError) return {};
    throw e;
  }
  if (typeof raw !== "object" || raw === null) return {};
  const p = raw as Record<string, unknown>;
  const num = (v: unknown, min: number, max: number) =>
    typeof v === "number" && v >= min && v <= max ? v : undefined;
  return {
    tool: TOOLS.find((t) => t.id === p.tool && t.id !== "crop")?.id,
    color: typeof p.color === "string" && /^#[0-9a-f]{6}$/i.test(p.color) ? p.color : undefined,
    strokeWidth: num(p.strokeWidth, 1, 20),
    textSize: num(p.textSize, 10, 72),
  };
}

const baseSize = (b: Base) =>
  b instanceof HTMLImageElement ? { w: b.naturalWidth, h: b.naturalHeight } : { w: b.width, h: b.height };

function blankCanvas(w: number, h: number) {
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  return canvas;
}

const replaceAt = (list: Op[], index: number, op: Op) => list.map((o, i) => (i === index ? op : o));

export function Editor() {
  let canvasRef!: HTMLCanvasElement;
  let wrapRef!: HTMLDivElement;
  const measureCtx = document.createElement("canvas").getContext("2d")!;
  const prefs = readPrefs();

  const [imagePath, setImagePath] = createSignal<string | null>(null);
  const [history, setHistory] = createSignal<hist.History<Base> | null>(null);
  // uncommitted ops while a move, resize or slider drag is in flight
  const [liveOps, setLiveOps] = createSignal<Op[] | null>(null);
  const [tool, setTool] = createSignal<Tool>(prefs.tool ?? "arrow");
  const [color, setColor] = createSignal(prefs.color ?? COLORS[0]);
  const [strokeWidth, setStrokeWidth] = createSignal(prefs.strokeWidth ?? 3);
  const [textSize, setTextSize] = createSignal(prefs.textSize ?? 24);
  const [stepRadius, setStepRadius] = createSignal(16);
  const [fillShapes, setFillShapes] = createSignal(false);
  const [blurRadius, setBlurRadius] = createSignal(12);
  const [selected, setSelected] = createSignal<number | null>(null);
  const [zoom, setZoom] = createSignal<"fit" | number>("fit");
  const [wrapSize, setWrapSize] = createSignal({ w: 0, h: 0 });
  const [draft, setDraft] = createSignal<Draft | null>(null);
  const [textEdit, setTextEdit] = createSignal<{ at: Point; index: number | null } | null>(null);
  const [textBuffer, setTextBuffer] = createSignal("");
  const [hoverCursor, setHoverCursor] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal<"save" | "copy" | "upload" | null>(null);
  const [status, setStatus] = createSignal<{ tone: string; msg: string } | null>(null);
  const [isHdrSource, setIsHdrSource] = createSignal(false);
  const [hdrSidecarPath, setHdrSidecarPath] = createSignal<string | null>(null);

  let drag: Drag | null = null;
  let loadGeneration = 0;

  const win = getCurrentWindow();

  const loaded = () => history() !== null;
  const snapshot = () => {
    const h = history();
    return h ? hist.current(h) : null;
  };
  const committedOps = () => snapshot()?.ops ?? [];
  const ops = () => liveOps() ?? committedOps();
  const size = createMemo(
    () => {
      const snap = snapshot();
      return snap ? baseSize(snap.base) : { w: 0, h: 0 };
    },
    { w: 0, h: 0 },
    { equals: (a, b) => a.w === b.w && a.h === b.h },
  );
  const scale = () => {
    const z = zoom();
    if (z !== "fit") return z;
    const { w, h } = size();
    const wrap = wrapSize();
    if (!w || !h || !wrap.w || !wrap.h) return 1;
    return Math.min(1, wrap.w / w, wrap.h / h);
  };
  const selectedOp = () => {
    const i = selected();
    return i === null ? null : (ops()[i] ?? null);
  };
  const styleKind = () => selectedOp()?.kind ?? tool();

  const measureText = (op: TextOp) => {
    measureCtx.font = TEXT_FONT(op.fontSize);
    const lines = op.text.split("\n");
    return {
      w: Math.max(...lines.map((line) => measureCtx.measureText(line).width)),
      h: lines.length * op.fontSize * TEXT_LINE_HEIGHT,
    };
  };

  createEffect(() =>
    localStorage.setItem(
      PREFS_KEY,
      JSON.stringify({ tool: tool(), color: color(), strokeWidth: strokeWidth(), textSize: textSize() }),
    ),
  );

  const resetSession = () => {
    setHistory(null);
    setLiveOps(null);
    setSelected(null);
    setDraft(null);
    setTextEdit(null);
    setTextBuffer("");
    setZoom("fit");
    drag = null;
  };

  const loadImage = async (path: string) => {
    const generation = ++loadGeneration;
    const isCurrent = () => generation === loadGeneration;
    resetSession();
    setStatus(null);
    setIsHdrSource(false);
    setHdrSidecarPath(null);
    setImagePath(path);

    const ext = path.split(".").pop()?.toLowerCase();
    if (ext === "gif") {
      setStatus({
        tone: "err",
        msg: "GIF editing isn't supported because it would flatten the frames. Close to keep the original.",
      });
      return;
    }

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
    // without cors the asset-protocol image taints the canvas and export throws
    img.crossOrigin = "anonymous";
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
    setHistory(hist.createHistory<Base>(img));
    if (isStagedSession()) {
      setStatus({ tone: "", msg: "annotate, then save to keep the capture." });
    }
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
  // the new file, so any save would overwrite the wrong file.
  onMount(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<string>("capscr://editor-load", async (e) => {
      if (isDirty()) {
        const ok = window.confirm("Discard unsaved annotations and open a new image?");
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

  onMount(() => {
    const observer = new ResizeObserver(([entry]) =>
      setWrapSize({ w: entry.contentRect.width, h: entry.contentRect.height }),
    );
    observer.observe(wrapRef);
    onCleanup(() => observer.disconnect());
  });

  // repaint at most once per frame; pointer moves can fire several times
  // between frames and a pixelate pass over a 4k region is not free
  let frame = 0;
  createEffect(() => {
    const snap = snapshot();
    if (!snap) return;
    const editing = textEdit()?.index ?? null;
    const scene = {
      base: snap.base,
      ops: ops().filter((_, i) => i !== editing),
      draft: draft(),
      selection: editing === null ? selectedOp() : null,
      size: size(),
      scale: scale(),
    };
    cancelAnimationFrame(frame);
    frame = requestAnimationFrame(() => {
      if (canvasRef.width !== scene.size.w || canvasRef.height !== scene.size.h) {
        canvasRef.width = scene.size.w;
        canvasRef.height = scene.size.h;
      }
      const ctx = canvasRef.getContext("2d");
      if (!ctx) return;
      paint(ctx, scene.base, scene.ops);
      if (scene.draft?.kind === "crop") drawCropDraft(ctx, scene.draft);
      else if (scene.draft) renderOp(ctx, scene.draft);
      if (scene.selection) drawSelection(ctx, scene.selection, scene.scale);
    });
  });
  onCleanup(() => cancelAnimationFrame(frame));

  const stepZoom = (dir: 1 | -1) => {
    const cur = scale();
    const next =
      dir > 0 ? ZOOM_LEVELS.find((z) => z > cur + 1e-3) : [...ZOOM_LEVELS].reverse().find((z) => z < cur - 1e-3);
    if (next !== undefined) setZoom(next);
  };

  // keeps the image point under the cursor in place while the zoom changes
  const onWheelZoom = (e: WheelEvent) => {
    if (!(e.ctrlKey || e.metaKey) || !loaded()) return;
    e.preventDefault();
    const before = canvasRef.getBoundingClientRect();
    const fx = (e.clientX - before.left) / before.width;
    const fy = (e.clientY - before.top) / before.height;
    stepZoom(e.deltaY < 0 ? 1 : -1);
    const after = canvasRef.getBoundingClientRect();
    wrapRef.scrollLeft += after.left + fx * after.width - e.clientX;
    wrapRef.scrollTop += after.top + fy * after.height - e.clientY;
  };

  const onKeydown = (e: KeyboardEvent) => {
    if (textEdit() || busy() !== null) return;
    const mod = e.ctrlKey || e.metaKey;
    if (e.target instanceof HTMLInputElement && !mod) return;
    const key = e.key.toLowerCase();
    if (mod) {
      // leave ctrl+c alone when the user selected text, such as an upload url
      if (key === "c" && window.getSelection()?.toString()) return;
      const actions: Record<string, () => void> = {
        z: () => stepHistory(e.shiftKey ? 1 : -1),
        y: () => stepHistory(1),
        s: () => void onSave(),
        c: () => void onCopy(),
        "=": () => stepZoom(1),
        "+": () => stepZoom(1),
        "-": () => stepZoom(-1),
        "0": () => setZoom("fit"),
        "1": () => setZoom(1),
      };
      const action = actions[key];
      if (!action) return;
      e.preventDefault();
      action();
    } else if ((key === "delete" || key === "backspace") && selected() !== null) {
      e.preventDefault();
      deleteSelected();
    } else if (key.startsWith("arrow") && selected() !== null) {
      e.preventDefault();
      const step = e.shiftKey ? 10 : 1;
      const dx = key === "arrowleft" ? -step : key === "arrowright" ? step : 0;
      const dy = key === "arrowup" ? -step : key === "arrowdown" ? step : 0;
      nudgeSelected(dx, dy);
    } else if (key === "escape") {
      if (selected() !== null) setSelected(null);
      else void win.close();
    } else {
      const picked = key === "0" ? "select" : TOOLS.find((t) => t.key === key)?.id;
      if (picked) pickTool(picked);
    }
  };

  // a pasted image becomes a new base in the history, so ctrl+z brings back
  // what was there before
  const onPaste = async (e: ClipboardEvent) => {
    if (textEdit()) return;
    const items = e.clipboardData?.items;
    if (!items) return;
    for (let i = 0; i < items.length; i++) {
      const it = items[i];
      if (!it.type.startsWith("image/")) continue;
      const blob = it.getAsFile();
      if (!blob) continue;
      e.preventDefault();
      const generation = ++loadGeneration;
      const url = URL.createObjectURL(blob);
      try {
        const img = new Image();
        img.src = url;
        await img.decode();
        if (generation !== loadGeneration) return;
        if (history()) pushBase(img);
        else setHistory({ ...hist.createHistory<Base>(img), saved: -1 });
        setStatus({ tone: "ok", msg: "pasted from clipboard. save to write it to disk." });
      } catch (err) {
        if (generation === loadGeneration) {
          setStatus({ tone: "err", msg: `paste failed: ${err}` });
        }
      } finally {
        URL.revokeObjectURL(url);
      }
      return;
    }
    setStatus({ tone: "", msg: "paste: only images are accepted here. use the text tool to add text." });
  };

  // annotate-before-save sessions stage under annotate-stage (see
  // cleanup_annotate_stage in commands.rs); nothing exists outside the
  // editor until Save mints the capture
  const isStagedSession = () => (imagePath() ?? "").includes("annotate-stage");

  const isDirty = () => {
    const h = history();
    return (
      (h !== null && hist.isDirty(h)) ||
      draft() !== null ||
      liveOps() !== null ||
      (textEdit() !== null && textBuffer().trim().length > 0)
    );
  };

  const confirmDiscardEdits = () => {
    if (!isDirty()) return true;
    return window.confirm(
      isStagedSession()
        ? "Discard this annotation? Nothing has been saved yet, so closing throws the capture away."
        : "Discard unsaved changes? They aren't written to disk until you press Save.",
    );
  };

  const sendEditorExitDecision = (requestId: string, allowed: boolean) => {
    void win
      .emitTo({ kind: "Window", label: "hub" }, "capscr://editor-exit-decision", { requestId, allowed })
      .catch((error) => {
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

  // intercept the close button on the titlebar so unsaved annotations are
  // not discarded silently
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
    void win
      .listen<EditorExitRequest>("capscr://editor-exit-request", async (event) => {
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
    const { w, h } = size();
    return {
      x: (e.clientX - rect.left) * (w / rect.width),
      y: (e.clientY - rect.top) * (h / rect.height),
    };
  }

  function paint(ctx: CanvasRenderingContext2D, base: Base, list: Op[]) {
    ctx.clearRect(0, 0, ctx.canvas.width, ctx.canvas.height);
    ctx.drawImage(base, 0, 0);
    for (const op of list) renderOp(ctx, op);
  }

  // base plus committed shapes, without selection chrome or drafts
  function bake(): HTMLCanvasElement {
    const out = blankCanvas(size().w, size().h);
    paint(out.getContext("2d")!, snapshot()!.base, ops());
    return out;
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
      case "text": {
        ctx.save();
        ctx.fillStyle = op.color;
        ctx.font = TEXT_FONT(op.fontSize);
        ctx.textBaseline = "top";
        // black shadow for legibility
        ctx.shadowColor = "rgba(0,0,0,0.85)";
        ctx.shadowBlur = 4;
        op.text.split("\n").forEach((line, i) => {
          ctx.fillText(line, op.origin.x, op.origin.y + i * op.fontSize * TEXT_LINE_HEIGHT);
        });
        ctx.restore();
        break;
      }
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
        ctx.lineWidth = markerWidth(op.width);
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
    ctx.save();
    ctx.beginPath();
    ctx.arc(op.center.x, op.center.y, r, 0, Math.PI * 2);
    ctx.fillStyle = op.color;
    ctx.fill();
    // contrasting outline so the pin reads on busy backgrounds
    ctx.strokeStyle = "rgba(0,0,0,0.6)";
    ctx.lineWidth = Math.max(1, r * 0.08);
    ctx.stroke();
    ctx.fillStyle = "#ffffff";
    ctx.font = TEXT_FONT(Math.round(r * 1.1), true);
    ctx.textAlign = "center";
    ctx.textBaseline = "middle";
    ctx.shadowColor = "rgba(0,0,0,0.65)";
    ctx.shadowBlur = 3;
    ctx.fillText(String(op.number), op.center.x, op.center.y + 1);
    ctx.restore();
  }

  function drawArrow(ctx: CanvasRenderingContext2D, a: Point, b: Point, col: string, w: number) {
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
    ctx.beginPath();
    ctx.moveTo(b.x, b.y);
    ctx.lineTo(b.x - ux * head - uy * wing, b.y - uy * head + ux * wing);
    ctx.lineTo(b.x - ux * head + uy * wing, b.y - uy * head - ux * wing);
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
    // hit-testing still uses the raw segments, which hug this curve closely
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
    const { x, y } = d.origin;
    const { w, h } = d.size;
    const cw = ctx.canvas.width;
    const ch = ctx.canvas.height;
    ctx.save();
    ctx.fillStyle = "rgba(0,0,0,0.55)";
    ctx.fillRect(0, 0, cw, y);
    ctx.fillRect(0, y + h, cw, ch - y - h);
    ctx.fillRect(0, y, x, h);
    ctx.fillRect(x + w, y, cw - x - w, h);
    ctx.strokeStyle = "#ffffff";
    ctx.lineWidth = 1.5;
    ctx.setLineDash([8, 5]);
    ctx.shadowColor = "rgba(0,0,0,0.8)";
    ctx.shadowBlur = 4;
    ctx.strokeRect(x, y, w, h);
    ctx.restore();
  }

  function applyBlur(ctx: CanvasRenderingContext2D, op: BlurOp) {
    // clip to the canvas so getImageData never reads outside; reads
    // outside return transparent black and corrupt the average at edges
    const { x, y, w, h } = clipBox({ ...op.origin, ...op.size }, ctx.canvas.width, ctx.canvas.height);
    if (w <= 0 || h <= 0) return;

    // a pixelate mosaic is faster and more privacy-safe than a true blur.
    // single read-modify-write: per-cell getImageData calls stall for
    // tens of thousands of round trips on fullscreen regions
    const cell = Math.max(4, op.radius);
    const src = ctx.getImageData(x, y, w, h);
    const data = src.data;
    const out = ctx.createImageData(w, h);
    const dst = out.data;
    for (let cy = 0; cy < h; cy += cell) {
      const y1 = Math.min(cy + cell, h);
      for (let cx = 0; cx < w; cx += cell) {
        const x1 = Math.min(cx + cell, w);
        let r = 0, g = 0, b = 0, a = 0, n = 0;
        for (let py = cy; py < y1; py++) {
          for (let px = cx; px < x1; px++) {
            const i = (py * w + px) * 4;
            r += data[i];
            g += data[i + 1];
            b += data[i + 2];
            a += data[i + 3];
            n++;
          }
        }
        r = (r / n) | 0;
        g = (g / n) | 0;
        b = (b / n) | 0;
        a = (a / n) | 0;
        for (let py = cy; py < y1; py++) {
          for (let px = cx; px < x1; px++) {
            const i = (py * w + px) * 4;
            dst[i] = r;
            dst[i + 1] = g;
            dst[i + 2] = b;
            dst[i + 3] = a;
          }
        }
      }
    }
    ctx.putImageData(out, x, y);
  }

  function drawSelection(ctx: CanvasRenderingContext2D, op: Op, s: number) {
    const b = bboxOf(op, measureText);
    ctx.save();
    ctx.strokeStyle = "#ffffff";
    ctx.lineWidth = 1.5 / s;
    ctx.setLineDash([7 / s, 5 / s]);
    ctx.shadowColor = "rgba(0,0,0,0.9)";
    ctx.shadowBlur = 4;
    ctx.strokeRect(b.x, b.y, b.w, b.h);
    ctx.setLineDash([]);
    ctx.shadowBlur = 0;
    ctx.fillStyle = "#ffffff";
    ctx.strokeStyle = "#1d4f91";
    ctx.lineWidth = 1 / s;
    const half = HANDLE_SIZE / s / 2;
    for (const { at } of handlesOf(op)) {
      ctx.fillRect(at.x - half, at.y - half, half * 2, half * 2);
      ctx.strokeRect(at.x - half, at.y - half, half * 2, half * 2);
    }
    ctx.restore();
  }

  function pushOps(list: Op[]) {
    const h = history();
    setLiveOps(null);
    if (!h || list === hist.current(h).ops) return;
    setHistory(hist.push(h, { base: hist.current(h).base, ops: list }));
  }

  function pushBase(base: Base) {
    const h = history();
    if (!h) return;
    setLiveOps(null);
    setSelected(null);
    setHistory(hist.push(h, { base, ops: [] }));
  }

  function stepHistory(dir: 1 | -1) {
    const h = history();
    if (!h) return;
    drag = null;
    setLiveOps(null);
    setDraft(null);
    setSelected(null);
    setStatus(null);
    setHistory(dir < 0 ? hist.undo(h) : hist.redo(h));
  }

  // selecting a shape loads its style into the toolbar, so the controls
  // show what the shape uses and edit it in place
  function selectShape(index: number) {
    const op = ops()[index];
    if (!op) return;
    const s = styleOf(op);
    if (s.color !== undefined) setColor(s.color);
    if (s.width !== undefined) setStrokeWidth(s.width);
    if (s.fontSize !== undefined) setTextSize(s.fontSize);
    if (s.radius !== undefined) setStepRadius(s.radius);
    if (s.fill !== undefined) setFillShapes(s.fill);
    if (s.block !== undefined) setBlurRadius(s.block);
    setSelected(index);
  }

  // live updates while a slider or the color picker is dragged, one history
  // entry when it is released
  function applyStyle(patch: Partial<Style>, commit = true) {
    const i = selected();
    const op = selectedOp();
    if (i === null || !op) return;
    const next = restyleOp(op, patch);
    const list = next === op ? ops() : replaceAt(ops(), i, next);
    if (commit) pushOps(list);
    else if (next !== op) setLiveOps(list);
  }

  function nudgeSelected(dx: number, dy: number) {
    const i = selected();
    const op = selectedOp();
    if (i === null || !op) return;
    pushOps(replaceAt(committedOps(), i, moveOp(op, dx, dy)));
  }

  function deleteSelected() {
    const i = selected();
    setSelected(null);
    if (i === null) return;
    pushOps(committedOps().filter((_, j) => j !== i));
  }

  function transformBase(w: number, h: number, draw: (ctx: CanvasRenderingContext2D, src: HTMLCanvasElement) => void) {
    if (!loaded() || busy() !== null) return;
    if (textEdit()) commitText();
    const src = bake();
    const out = blankCanvas(w, h);
    draw(out.getContext("2d")!, src);
    pushBase(out);
  }

  function applyCrop(origin: Point, cropSize: { w: number; h: number }) {
    const { x, y, w, h } = clipBox({ ...origin, ...cropSize }, size().w, size().h);
    if (w < 10 || h < 10) {
      setStatus({ tone: "", msg: "crop region too small. drag a larger area." });
      return;
    }
    transformBase(w, h, (ctx, src) => ctx.drawImage(src, x, y, w, h, 0, 0, w, h));
    setStatus({ tone: "ok", msg: `cropped to ${w} × ${h}. ctrl+z undoes it.` });
  }

  function rotateBase(dir: 1 | -1) {
    const { w, h } = size();
    transformBase(h, w, (ctx, src) => {
      ctx.translate(h / 2, w / 2);
      ctx.rotate((dir * Math.PI) / 2);
      ctx.drawImage(src, -w / 2, -h / 2);
    });
  }

  function flipBase(horizontal: boolean) {
    const { w, h } = size();
    transformBase(w, h, (ctx, src) => {
      ctx.translate(horizontal ? w : 0, horizontal ? 0 : h);
      ctx.scale(horizontal ? -1 : 1, horizontal ? 1 : -1);
      ctx.drawImage(src, 0, 0);
    });
  }

  function nextStepNumber(): number {
    return committedOps().reduce((max, op) => (op.kind === "step" ? Math.max(max, op.number) : max), 0) + 1;
  }

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0 || !loaded() || busy() !== null || textEdit()) return;
    const p = pointFromEvent(e);
    const t = tool();
    canvasRef.setPointerCapture(e.pointerId);

    const sel = selected();
    const selOp = selectedOp();
    const handle = selOp && handleAt(selOp, p, HANDLE_SIZE / scale());
    if (sel !== null && selOp && handle) {
      drag = { kind: "resize", handle, index: sel, orig: selOp };
      return;
    }

    if (t === "select" || t === "text") {
      const idx = hitTestTop(ops(), p, measureText);
      const hit = idx === null ? null : ops()[idx];
      if (t === "text") {
        if (idx !== null && hit?.kind === "text") openTextEditor(hit.origin, idx);
        else openTextEditor(p, null);
        return;
      }
      if (idx === null || !hit) {
        setSelected(null);
        return;
      }
      selectShape(idx);
      drag = { kind: "move", start: p, index: idx, orig: hit };
      return;
    }

    setSelected(null);
    if (t === "step") {
      const list = committedOps();
      pushOps([...list, { kind: "step", center: p, number: nextStepNumber(), color: color(), radius: stepRadius() }]);
      selectShape(list.length);
      return;
    }
    drag = { kind: "draw", start: p };
    const stroke = { color: color(), width: strokeWidth() };
    const box = { origin: p, size: { w: 0, h: 0 } };
    if (t === "arrow" || t === "line" || t === "highlight") setDraft({ kind: t, from: p, to: p, ...stroke });
    else if (t === "rect" || t === "ellipse") setDraft({ kind: t, ...box, ...stroke, fill: fillShapes() });
    else if (t === "blur") setDraft({ kind: "blur", ...box, radius: blurRadius() });
    else if (t === "pen") setDraft({ kind: "pen", points: [p], ...stroke });
    else if (t === "crop") setDraft({ kind: "crop", ...box });
  }

  function updateHoverCursor(p: Point) {
    const op = selectedOp();
    const handle = op && handleAt(op, p, HANDLE_SIZE / scale());
    if (handle) setHoverCursor(HANDLE_CURSORS[handle]);
    else if (tool() === "select" && hitTestTop(ops(), p, measureText) !== null) setHoverCursor("move");
    else setHoverCursor(null);
  }

  function onPointerMove(e: PointerEvent) {
    if (!loaded()) return;
    const p = pointFromEvent(e);
    const active = drag;
    if (!active) {
      updateHoverCursor(p);
      return;
    }
    if (active.kind === "move") {
      const moved = moveOp(active.orig, p.x - active.start.x, p.y - active.start.y);
      setLiveOps(replaceAt(committedOps(), active.index, moved));
      return;
    }
    if (active.kind === "resize") {
      setLiveOps(replaceAt(committedOps(), active.index, resizeOp(active.orig, active.handle, p)));
      return;
    }
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
      const { start } = active;
      setDraft({
        ...d,
        origin: { x: Math.min(start.x, p.x), y: Math.min(start.y, p.y) },
        size: { w: Math.abs(p.x - start.x), h: Math.abs(p.y - start.y) },
      });
    }
  }

  function onPointerUp() {
    const active = drag;
    drag = null;
    if (!active) return;
    if (active.kind !== "draw") {
      const live = liveOps();
      if (live) pushOps(live);
      return;
    }
    const d = draft();
    setDraft(null);
    if (!d) return;
    if (d.kind === "crop") {
      applyCrop(d.origin, d.size);
      return;
    }
    const tooSmall =
      d.kind === "arrow" || d.kind === "line" || d.kind === "highlight"
        ? Math.hypot(d.to.x - d.from.x, d.to.y - d.from.y) < 3
        : d.kind === "rect" || d.kind === "blur" || d.kind === "ellipse"
          ? d.size.w < 3 || d.size.h < 3
          : false;
    if (tooSmall) return;
    const list = committedOps();
    pushOps([...list, d]);
    selectShape(list.length);
  }

  function onPointerCancel() {
    drag = null;
    setDraft(null);
    setLiveOps(null);
  }

  function onDoubleClick(e: MouseEvent) {
    if (tool() !== "select" || !loaded() || textEdit()) return;
    const idx = hitTestTop(ops(), pointFromEvent(e), measureText);
    const hit = idx === null ? null : ops()[idx];
    if (idx !== null && hit?.kind === "text") openTextEditor(hit.origin, idx);
  }

  // switching tools commits pending text instead of stranding the input
  // box at a stale position while canvas input stays blocked behind it
  function pickTool(t: Tool) {
    if (textEdit()) commitText();
    setTool(t);
  }

  function openTextEditor(at: Point, index: number | null) {
    const op = index === null ? null : ops()[index];
    if (op?.kind === "text") {
      setColor(op.color);
      setTextSize(op.fontSize);
    }
    setSelected(null);
    setTextBuffer(op?.kind === "text" ? op.text : "");
    setTextEdit({ at, index });
  }

  function commitText() {
    const edit = textEdit();
    if (!edit) return;
    const text = textBuffer().trim();
    setTextEdit(null);
    setTextBuffer("");
    const list = committedOps();
    const op: TextOp = { kind: "text", origin: edit.at, text, color: color(), fontSize: textSize() };
    if (edit.index === null) {
      if (!text) return;
      pushOps([...list, op]);
      selectShape(list.length);
      return;
    }
    if (!text) {
      pushOps(list.filter((_, i) => i !== edit.index));
      return;
    }
    const prev = list[edit.index];
    const unchanged =
      prev?.kind === "text" && prev.text === text && prev.color === op.color && prev.fontSize === op.fontSize;
    if (!unchanged) pushOps(replaceAt(list, edit.index, op));
    selectShape(edit.index);
  }

  function cancelText() {
    setTextEdit(null);
    setTextBuffer("");
  }

  function targetMime(): string | null {
    switch (targetExt()) {
      case "jpg":
      case "jpeg":
        return "image/jpeg";
      case "webp":
        return "image/webp";
      case "png":
        return "image/png";
      // bmp / avif / jxl have no browser encoder; writing PNG bytes under
      // those extensions would silently corrupt the file, so refuse and let
      // copy / upload (which always export PNG) carry the image out instead
      default:
        return null;
    }
  }

  function targetExt(): string {
    return (imagePath() ?? "").split(".").pop()?.toLowerCase() ?? "png";
  }

  async function exportBytes(mime: string): Promise<Uint8Array> {
    const canvas = bake();
    const blob = await new Promise<Blob>((resolve, reject) => {
      canvas.toBlob((b) => (b ? resolve(b) : reject(new Error(`toBlob(${mime}) failed`))), mime);
    });
    return new Uint8Array(await blob.arrayBuffer());
  }

  async function onSave() {
    if (!loaded() || busy() !== null) return;
    // flush pending text first; exportBytes reads the committed shapes, so
    // an open text box would otherwise be dropped
    if (textEdit()) commitText();
    const path = imagePath();
    if (!path) return;
    const mime = targetMime();
    if (!mime) {
      setStatus({
        tone: "err",
        msg: `saving .${targetExt()} isn't supported in the editor. use copy or upload to export as PNG.`,
      });
      return;
    }
    const exported = snapshot()!;
    setBusy("save");
    setStatus({ tone: "", msg: "writing..." });
    try {
      // encode in the source file's format so a .jpg path never receives
      // PNG bytes that downstream tools would misread
      const bytes = await exportBytes(mime);
      const savedPath = await invoke<string>("save_edited_image", bytes, {
        headers: { "x-capscr-target": encodeURIComponent(path) },
      });
      // a new image may have been opened while the write was in flight
      if (imagePath() !== path) return;
      // staged sessions mint their real file here; point at it so a
      // second save overwrites the capture instead of the dead stage
      setImagePath(savedPath);
      const h = history();
      if (h) setHistory(hist.markSaved(h, exported));
      setStatus({ tone: "ok", msg: "saved." });
    } catch (e) {
      setStatus({ tone: "err", msg: `save failed: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  async function onCopy() {
    if (!loaded() || busy() !== null) return;
    if (textEdit()) commitText();
    setBusy("copy");
    setStatus({ tone: "", msg: "copying..." });
    try {
      // clipboard always wants PNG, which the Rust ClipboardManager decodes
      await invoke<void>("copy_edited_image_to_clipboard", await exportBytes("image/png"));
      setStatus({ tone: "ok", msg: "copied to clipboard." });
    } catch (e) {
      setStatus({ tone: "err", msg: `copy failed: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  async function onUpload() {
    if (!loaded() || busy() !== null) return;
    if (textEdit()) commitText();
    setBusy("upload");
    setStatus({ tone: "", msg: "uploading..." });
    try {
      const result = await invoke<{ url: string; delete_url: string | null }>(
        "upload_edited_image",
        await exportBytes("image/png"),
      );
      setStatus({ tone: "ok", msg: result.url });
    } catch (e) {
      setStatus({ tone: "err", msg: `upload failed: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  const idle = () => busy() === null && loaded();
  const textLines = () => textBuffer().split("\n");

  const range = (
    label: string,
    min: number,
    max: number,
    value: () => number,
    set: (v: number) => void,
    patch: (v: number) => Partial<Style>,
  ) => (
    <label class="ctrl">
      <span>{label}</span>
      <input
        type="range"
        min={min}
        max={max}
        value={value()}
        onInput={(e) => {
          const v = parseInt(e.currentTarget.value);
          set(v);
          applyStyle(patch(v), false);
        }}
        onChange={(e) => applyStyle(patch(parseInt(e.currentTarget.value)))}
      />
      <span class="ctrl-val">{value()}</span>
    </label>
  );

  return (
    <div class="editor">
      <Titlebar context="edit" />

      <div class="editor-toolbar">
        <div class="editor-row">
          <div class="editor-tools">
            <For each={TOOLS}>
              {(t) => (
                <button
                  type="button"
                  class="tool"
                  classList={{ "is-active": tool() === t.id }}
                  onClick={() => pickTool(t.id)}
                  title={`${t.title} (${t.key})`}
                  aria-label={t.title}
                  aria-pressed={tool() === t.id}
                >
                  <t.icon size={14} stroke-width={1.5} />
                </button>
              )}
            </For>
          </div>

          <div class="editor-actions">
            <span class="editor-group">
              <button
                class="btn"
                data-variant="ghost"
                data-size="xs"
                onClick={() => stepHistory(-1)}
                title="undo (ctrl+z)"
                aria-label="undo"
                disabled={!idle() || !hist.canUndo(history()!)}
              >
                <Undo2 size={12} stroke-width={1.5} />
              </button>
              <button
                class="btn"
                data-variant="ghost"
                data-size="xs"
                onClick={() => stepHistory(1)}
                title="redo (ctrl+y)"
                aria-label="redo"
                disabled={!idle() || !hist.canRedo(history()!)}
              >
                <Redo2 size={12} stroke-width={1.5} />
              </button>
            </span>
            <span class="editor-group">
              <button
                class="btn"
                data-variant="ghost"
                data-size="xs"
                onClick={() => rotateBase(-1)}
                title="rotate 90° counter-clockwise"
                aria-label="rotate counter-clockwise"
                disabled={!idle()}
              >
                <RotateCcw size={12} stroke-width={1.5} />
              </button>
              <button
                class="btn"
                data-variant="ghost"
                data-size="xs"
                onClick={() => rotateBase(1)}
                title="rotate 90° clockwise"
                aria-label="rotate clockwise"
                disabled={!idle()}
              >
                <RotateCw size={12} stroke-width={1.5} />
              </button>
              <button
                class="btn"
                data-variant="ghost"
                data-size="xs"
                onClick={() => flipBase(true)}
                title="flip horizontal"
                aria-label="flip horizontal"
                disabled={!idle()}
              >
                <FlipHorizontal2 size={12} stroke-width={1.5} />
              </button>
              <button
                class="btn"
                data-variant="ghost"
                data-size="xs"
                onClick={() => flipBase(false)}
                title="flip vertical"
                aria-label="flip vertical"
                disabled={!idle()}
              >
                <FlipVertical2 size={12} stroke-width={1.5} />
              </button>
            </span>
            <span class="editor-group">
              <button
                class="btn"
                data-variant="ghost"
                data-size="xs"
                onClick={() => stepZoom(-1)}
                title="zoom out (ctrl+-)"
                aria-label="zoom out"
                disabled={!loaded() || scale() <= ZOOM_LEVELS[0]}
              >
                <ZoomOut size={12} stroke-width={1.5} />
              </button>
              <button
                class="btn editor-zoom-label"
                data-variant="ghost"
                data-size="xs"
                onClick={() => setZoom(zoom() === "fit" ? 1 : "fit")}
                title={zoom() === "fit" ? "actual size (ctrl+1)" : "fit to window (ctrl+0)"}
                disabled={!loaded()}
              >
                <Maximize2 size={11} stroke-width={1.5} />
                {zoom() === "fit" ? "fit " : ""}
                {Math.round(scale() * 100)}%
              </button>
              <button
                class="btn"
                data-variant="ghost"
                data-size="xs"
                onClick={() => stepZoom(1)}
                title="zoom in (ctrl+=)"
                aria-label="zoom in"
                disabled={!loaded() || scale() >= ZOOM_LEVELS[ZOOM_LEVELS.length - 1]}
              >
                <ZoomIn size={12} stroke-width={1.5} />
              </button>
            </span>
            <button class="btn" onClick={onSave} disabled={!idle()} title="save (ctrl+s)">
              <Save size={12} stroke-width={1.5} />
              save
            </button>
            <button class="btn" data-variant="ghost" onClick={onCopy} disabled={!idle()} title="copy image (ctrl+c)">
              <Copy size={12} stroke-width={1.5} />
              copy
            </button>
            <button class="btn" data-variant="ghost" onClick={onUpload} disabled={!idle()}>
              <Upload size={12} stroke-width={1.5} />
              upload
            </button>
            <button class="btn" data-variant="ghost" onClick={() => void win.close()}>
              <X size={12} stroke-width={1.5} />
              close
            </button>
          </div>
        </div>

        <div class="editor-row">
          <Show when={styleKind() !== "blur" && styleKind() !== "crop"}>
            <div class="editor-colors">
              <For each={COLORS}>
                {(c) => (
                  <button
                    type="button"
                    class="swatch"
                    classList={{ "is-active": color() === c }}
                    style={{ background: c }}
                    onClick={() => {
                      setColor(c);
                      applyStyle({ color: c });
                    }}
                    aria-label={c}
                  />
                )}
              </For>
              <input
                type="color"
                class="swatch swatch-custom"
                classList={{ "is-active": !COLORS.includes(color()) }}
                value={color()}
                title="custom color"
                aria-label="custom color"
                onInput={(e) => {
                  setColor(e.currentTarget.value);
                  applyStyle({ color: e.currentTarget.value }, false);
                }}
                onChange={(e) => applyStyle({ color: e.currentTarget.value })}
              />
            </div>
          </Show>

          <div class="editor-controls">
            <Show when={STROKE_KINDS.includes(styleKind())}>
              {range("stroke", 1, 20, strokeWidth, setStrokeWidth, (width) => ({ width }))}
            </Show>
            <Show when={styleKind() === "rect" || styleKind() === "ellipse"}>
              <label class="ctrl">
                <input
                  type="checkbox"
                  checked={fillShapes()}
                  onChange={(e) => {
                    setFillShapes(e.currentTarget.checked);
                    applyStyle({ fill: e.currentTarget.checked });
                  }}
                />
                <span>fill</span>
              </label>
            </Show>
            <Show when={styleKind() === "text"}>
              {range("size", 10, 72, textSize, setTextSize, (fontSize) => ({ fontSize }))}
            </Show>
            <Show when={styleKind() === "blur"}>
              {range("block", 4, 40, blurRadius, setBlurRadius, (block) => ({ block }))}
            </Show>
            <Show when={styleKind() === "step"}>
              {range("size", 8, 48, stepRadius, setStepRadius, (radius) => ({ radius }))}
            </Show>
            <Show when={tool() === "crop"}>
              <span class="muted">drag a region; releasing applies the crop</span>
            </Show>
            <Show when={tool() === "select" && selected() === null}>
              <span class="muted">click a shape to select it, double-click text to edit it</span>
            </Show>
          </div>

          <Show when={selected() !== null}>
            <span class="editor-selection">
              <span class="muted">
                shape {selected()! + 1} of {ops().length} · drag to move, handles resize, arrows nudge
              </span>
              <button class="btn" data-variant="ghost" data-size="xs" onClick={deleteSelected} title="delete (del)">
                <Trash2 size={12} stroke-width={1.5} />
                delete
              </button>
            </span>
          </Show>
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
                {" "}capscr kept the HDR original as <code>{hdrSidecarPath()!.split(/[\\/]/).pop()}</code>.
                annotations save into the SDR copy; the HDR sidecar is untouched.
              </span>
            </Show>
          </span>
        </div>
      </Show>

      <div class="editor-canvas-wrap" ref={wrapRef} onWheel={onWheelZoom}>
        <div class="editor-canvas-scroll" hidden={!loaded()}>
          <canvas
            ref={canvasRef}
            class="editor-canvas"
            data-tool={tool()}
            style={{
              width: `${size().w * scale()}px`,
              height: `${size().h * scale()}px`,
              cursor: hoverCursor() ?? undefined,
              "image-rendering": scale() >= 1 ? "pixelated" : "auto",
            }}
            onPointerDown={onPointerDown}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={onPointerCancel}
            onDblClick={onDoubleClick}
          />
          <Show when={textEdit()}>
            {(edit) => (
              <textarea
                ref={(el) =>
                  setTimeout(() => {
                    el.focus();
                    el.selectionStart = el.selectionEnd = el.value.length;
                  })
                }
                class="editor-text-input"
                rows={textLines().length}
                spellcheck={false}
                style={{
                  left: `${edit().at.x * scale()}px`,
                  top: `${edit().at.y * scale()}px`,
                  width: `${Math.max(6, ...textLines().map((line) => line.length)) + 1}ch`,
                  color: color(),
                  "font-size": `${textSize() * scale()}px`,
                  "font-family": TEXT_FAMILY,
                  "line-height": String(TEXT_LINE_HEIGHT),
                }}
                value={textBuffer()}
                onInput={(e) => setTextBuffer(e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && !e.shiftKey) {
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
            )}
          </Show>
        </div>
        <Show when={!loaded()}>
          <div class="editor-loading">
            <span class="lede">{status()?.tone === "err" ? status()!.msg : "loading capture..."}</span>
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
        <Show when={isStagedSession()}>
          <span class="muted">unsaved capture, save to keep it</span>
        </Show>
        <Show when={loaded()}>
          <span class="muted">
            {size().w} × {size().h}
          </span>
        </Show>
      </footer>
    </div>
  );
}
