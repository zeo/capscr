import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { invoke } from "@tauri-apps/api/core";
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
} from "lucide-solid";
import { Titlebar } from "../components/Titlebar";

type Tool = "arrow" | "rect" | "text" | "blur";

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

type Op = ArrowOp | RectOp | TextOp | BlurOp;

const COLORS = ["#ef4444", "#f59e0b", "#10b981", "#3b82f6", "#a855f7", "#ffffff", "#000000"];

export function Editor() {
  let canvasRef!: HTMLCanvasElement;
  let baseImage: HTMLImageElement | null = null;

  const [imagePath, setImagePath] = createSignal<string | null>(null);
  const [loaded, setLoaded] = createSignal(false);
  const [tool, setTool] = createSignal<Tool>("arrow");
  const [color, setColor] = createSignal<string>(COLORS[0]);
  const [strokeWidth, setStrokeWidth] = createSignal(3);
  const [textSize, setTextSize] = createSignal(24);
  const [ops, setOps] = createSignal<Op[]>([]);
  const [redoStack, setRedoStack] = createSignal<Op[]>([]);
  // 1.0 = fit-to-wrap (handled by CSS max-width); >1.0 = explicit pixel scale.
  const [zoom, setZoom] = createSignal(1.0);
  const ZOOM_LEVELS = [0.5, 0.75, 1.0, 1.5, 2.0, 3.0, 4.0];
  const [draft, setDraft] = createSignal<Op | null>(null);
  const [textInputAt, setTextInputAt] = createSignal<Point | null>(null);
  const [textBuffer, setTextBuffer] = createSignal("");
  const [busy, setBusy] = createSignal<"save" | "copy" | "upload" | null>(null);
  const [status, setStatus] = createSignal<{ tone: string; msg: string } | null>(null);

  let dragStart: Point | null = null;

  const win = getCurrentWindow();

  onMount(async () => {
    const path = await invoke<string | null>("get_editor_image_path");
    if (!path) {
      setStatus({ tone: "err", msg: "no image path received from backend" });
      return;
    }

    // Block GIF editing — canvas can only sample the first frame and we'd
    // export PNG bytes over the .gif file, destroying the animation. Until
    // we have a real frame-aware GIF editor, refuse to load.
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

    const img = new Image();
    img.src = `asset://localhost/${encodeURIComponent(path)}`;
    try {
      await img.decode();
    } catch (e) {
      setStatus({ tone: "err", msg: `image load failed: ${e}` });
      return;
    }
    baseImage = img;
    canvasRef.width = img.naturalWidth;
    canvasRef.height = img.naturalHeight;
    setLoaded(true);
    redraw();
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
    } else if (e.key === "Escape") {
      void win.close();
    } else if (e.key === "1") setTool("arrow");
    else if (e.key === "2") setTool("rect");
    else if (e.key === "3") {
      setTool("text");
    } else if (e.key === "4") setTool("blur");
  };

  const onWheelZoom = (e: WheelEvent) => {
    if (!(e.ctrlKey || e.metaKey)) return;
    e.preventDefault();
    stepZoom(e.deltaY < 0 ? 1 : -1);
  };

  // Replace the current canvas with a pasted clipboard image. ShareX has
  // this; users expect it. We accept any image/* type the browser decoded.
  const onPaste = async (e: ClipboardEvent) => {
    const items = e.clipboardData?.items;
    if (!items) return;
    for (let i = 0; i < items.length; i++) {
      const it = items[i];
      if (!it.type.startsWith("image/")) continue;
      const blob = it.getAsFile();
      if (!blob) continue;
      e.preventDefault();
      const url = URL.createObjectURL(blob);
      try {
        const img = new Image();
        img.src = url;
        await img.decode();
        baseImage = img;
        canvasRef.width = img.naturalWidth;
        canvasRef.height = img.naturalHeight;
        setOps([]);
        setRedoStack([]);
        setLoaded(true);
        setStatus({
          tone: "ok",
          msg: "pasted from clipboard — save to write back to disk.",
        });
        redraw();
      } catch (err) {
        setStatus({ tone: "err", msg: `paste failed: ${err}` });
      } finally {
        URL.revokeObjectURL(url);
      }
      return;
    }
  };

  onMount(() => {
    window.addEventListener("keydown", onKeydown);
    window.addEventListener("paste", onPaste);
  });
  onCleanup(() => {
    window.removeEventListener("keydown", onKeydown);
    window.removeEventListener("paste", onPaste);
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
    if (d) renderOp(ctx, d);
  }

  function renderOp(ctx: CanvasRenderingContext2D, op: Op) {
    switch (op.kind) {
      case "arrow":
        drawArrow(ctx, op.from, op.to, op.color, op.width);
        break;
      case "rect":
        ctx.strokeStyle = op.color;
        ctx.lineWidth = op.width;
        ctx.lineJoin = "miter";
        ctx.strokeRect(op.origin.x, op.origin.y, op.size.w, op.size.h);
        break;
      case "text":
        ctx.fillStyle = op.color;
        ctx.font = `${op.fontSize}px "Fira Code", "Hack", ui-monospace, monospace`;
        ctx.textBaseline = "top";
        // black shadow for legibility
        ctx.shadowColor = "rgba(0,0,0,0.85)";
        ctx.shadowBlur = 4;
        ctx.fillText(op.text, op.origin.x, op.origin.y);
        ctx.shadowBlur = 0;
        break;
      case "blur":
        applyBlur(ctx, op);
        break;
    }
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

  function applyBlur(ctx: CanvasRenderingContext2D, op: BlurOp) {
    const w = Math.max(1, Math.floor(op.size.w));
    const h = Math.max(1, Math.floor(op.size.h));
    const x = Math.floor(op.origin.x);
    const y = Math.floor(op.origin.y);
    if (w <= 0 || h <= 0) return;

    // Use a pixelate-style mosaic — it's faster and more privacy-safe than a true blur.
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

  function onMouseDown(e: MouseEvent) {
    if (!loaded()) return;
    if (textInputAt()) return;
    const p = pointFromEvent(e);
    const t = tool();
    dragStart = p;
    if (t === "arrow") {
      setDraft({ kind: "arrow", from: p, to: p, color: color(), width: strokeWidth() });
    } else if (t === "rect") {
      setDraft({ kind: "rect", origin: p, size: { w: 0, h: 0 }, color: color(), width: strokeWidth() });
    } else if (t === "blur") {
      setDraft({ kind: "blur", origin: p, size: { w: 0, h: 0 }, radius: 12 });
    } else if (t === "text") {
      setTextInputAt(p);
      setTextBuffer("");
      // focus the input after solid renders it
      setTimeout(() => {
        const el = document.getElementById("editor-text-input") as HTMLInputElement | null;
        el?.focus();
      }, 0);
    }
  }

  function onMouseMove(e: MouseEvent) {
    if (!dragStart) return;
    const p = pointFromEvent(e);
    const d = draft();
    if (!d) return;
    if (d.kind === "arrow") {
      setDraft({ ...d, to: p });
    } else if (d.kind === "rect" || d.kind === "blur") {
      const ox = Math.min(dragStart.x, p.x);
      const oy = Math.min(dragStart.y, p.y);
      const w = Math.abs(p.x - dragStart.x);
      const h = Math.abs(p.y - dragStart.y);
      setDraft({ ...d, origin: { x: ox, y: oy }, size: { w, h } });
    }
    redraw();
  }

  function onMouseUp() {
    const d = draft();
    dragStart = null;
    if (!d) return;
    // discard zero-size drafts
    if (d.kind === "arrow") {
      const dx = d.to.x - d.from.x;
      const dy = d.to.y - d.from.y;
      if (Math.hypot(dx, dy) < 3) {
        setDraft(null);
        redraw();
        return;
      }
    }
    if (d.kind === "rect" || d.kind === "blur") {
      if (d.size.w < 3 || d.size.h < 3) {
        setDraft(null);
        redraw();
        return;
      }
    }
    setOps([...ops(), d]);
    setRedoStack([]);
    setDraft(null);
    redraw();
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
    if (cur.length === 0) return;
    const last = cur[cur.length - 1];
    setOps(cur.slice(0, -1));
    setRedoStack([...redoStack(), last]);
    redraw();
  }

  function redo() {
    const stack = redoStack();
    if (stack.length === 0) return;
    const next = stack[stack.length - 1];
    setRedoStack(stack.slice(0, -1));
    setOps([...ops(), next]);
    redraw();
  }

  function targetMime(): string {
    const path = imagePath() ?? "";
    const ext = path.split(".").pop()?.toLowerCase() ?? "png";
    switch (ext) {
      case "jpg":
      case "jpeg":
        return "image/jpeg";
      case "webp":
        return "image/webp";
      // bmp and gif aren't first-class encoders in browsers; fall through to png
      // to avoid silently writing wrong bytes to an extension we can't honour.
      default:
        return "image/png";
    }
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
    const path = imagePath();
    if (!path) return;
    setBusy("save");
    setStatus({ tone: "", msg: "writing..." });
    try {
      // Encode in the source file's format so we don't write PNG bytes to a
      // .jpg path (which would silently corrupt the file for downstream
      // consumers that trust the extension).
      const bytes = await exportBytes(targetMime());
      await invoke<void>("save_edited_image", {
        bytes: Array.from(bytes),
        targetPath: path,
      });
      setStatus({ tone: "ok", msg: "saved." });
      setTimeout(() => void win.close(), 400);
    } catch (e) {
      setStatus({ tone: "err", msg: `save failed: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  async function onCopy() {
    setBusy("copy");
    setStatus({ tone: "", msg: "copying..." });
    try {
      // Clipboard always wants PNG — that's what the Rust ClipboardManager
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
    setBusy("upload");
    setStatus({ tone: "", msg: "uploading..." });
    try {
      // Upload path also expects PNG — the existing ImageUploader.upload
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
            classList={{ "is-active": tool() === "arrow" }}
            onClick={() => setTool("arrow")}
            title="arrow (1)"
          >
            <ArrowRight size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "rect" }}
            onClick={() => setTool("rect")}
            title="rectangle (2)"
          >
            <Square size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "text" }}
            onClick={() => setTool("text")}
            title="text (3)"
          >
            <Type size={14} stroke-width={1.5} />
          </button>
          <button
            type="button"
            class="tool"
            classList={{ "is-active": tool() === "blur" }}
            onClick={() => setTool("blur")}
            title="pixelate (4)"
          >
            <Droplet size={14} stroke-width={1.5} />
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
          <Show when={tool() !== "text" && tool() !== "blur"}>
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
        </div>

        <div class="editor-actions">
          <button
            class="btn"
            data-variant="ghost"
            onClick={undo}
            title="ctrl+z"
            disabled={ops().length === 0}
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
          <button class="btn" onClick={onSave} disabled={busy() !== null}>
            <Save size={12} stroke-width={1.5} />
            save
          </button>
          <button class="btn" data-variant="ghost" onClick={onCopy} disabled={busy() !== null}>
            <Copy size={12} stroke-width={1.5} />
            copy
          </button>
          <button class="btn" data-variant="ghost" onClick={onUpload} disabled={busy() !== null}>
            <Upload size={12} stroke-width={1.5} />
            upload
          </button>
          <button class="btn" data-variant="ghost" onClick={() => void win.close()}>
            <X size={12} stroke-width={1.5} />
            close
          </button>
        </div>
      </div>

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
                      commitText();
                    } else if (e.key === "Escape") {
                      e.preventDefault();
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
          <span class="flash" data-tone={status()!.tone}>
            {status()!.msg}
          </span>
        </Show>
        <span class="grow" />
        <Show when={loaded() && baseImage}>
          <span class="muted">
            {baseImage!.naturalWidth} × {baseImage!.naturalHeight}
          </span>
        </Show>
      </footer>
    </div>
  );
}
