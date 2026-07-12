import { onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";

// linux selection overlay. mirrors the win32 GDI selector's interaction
// model: drag = region, click = window under cursor, alt+click = color pick,
// shift = aspect snap, ctrl on release = keep open for arrow fine-tune,
// enter/space = commit (fullscreen when nothing is selected), esc or
// right-click = cancel, wheel = loupe zoom.

interface WindowRect {
  id: number;
  x: number;
  y: number;
  width: number;
  height: number;
}

interface SelectorContext {
  origin_x: number;
  origin_y: number;
  frame_width: number;
  frame_height: number;
  windows: WindowRect[];
}

const CLICK_THRESHOLD = 5;
const MAGNIFIER_SIZE = 120;
const ASPECT_TARGETS = [1, 16 / 9, 16 / 10, 4 / 3, 21 / 9];

export function Selector() {
  let canvas!: HTMLCanvasElement;
  let ctxInfo: SelectorContext | null = null;
  let frame: ImageBitmap | null = null;
  let frameData: ImageData | null = null;

  // all coordinates below are frame pixels (canvas backing store space)
  let cursorX = -1;
  let cursorY = -1;
  let startX = 0;
  let startY = 0;
  let endX = 0;
  let endY = 0;
  let mouseDown = false;
  let dragStarted = false;
  let hovered: WindowRect | null = null;
  let zoom = 8;
  let shiftHeld = false;
  let finished = false;
  let raf = 0;

  const finish = (outcome: Record<string, unknown>) => {
    if (finished) return;
    finished = true;
    invoke("selector_finish", { outcome }).catch(() => {});
  };

  const scale = () => {
    if (!ctxInfo) return { sx: 1, sy: 1 };
    return {
      sx: ctxInfo.frame_width / window.innerWidth,
      sy: ctxInfo.frame_height / window.innerHeight,
    };
  };

  const toFrame = (e: MouseEvent) => {
    const { sx, sy } = scale();
    return { x: Math.round(e.clientX * sx), y: Math.round(e.clientY * sy) };
  };

  // aspect-snap the end point against the start point, matching the win32
  // selector: keep width, recompute height for the nearest target ratio
  const snappedEnd = () => {
    if (!shiftHeld) return { ex: endX, ey: endY };
    const dx = endX - startX;
    const dy = endY - startY;
    const w = Math.abs(dx);
    const h = Math.abs(dy);
    if (w === 0 || h === 0) return { ex: endX, ey: endY };
    const ratio = w / h;
    let best = ASPECT_TARGETS[0];
    let bestDiff = Infinity;
    for (const t of ASPECT_TARGETS) {
      const diff = Math.abs(ratio - t);
      if (diff < bestDiff) {
        bestDiff = diff;
        best = t;
      }
    }
    const newH = Math.round(w / best);
    return { ex: endX, ey: startY + Math.sign(dy || 1) * newH };
  };

  const hasSelection = () => {
    const { ex, ey } = snappedEnd();
    return (
      dragStarted &&
      (Math.abs(ex - startX) > CLICK_THRESHOLD || Math.abs(ey - startY) > CLICK_THRESHOLD)
    );
  };

  const selectionRect = () => {
    const { ex, ey } = snappedEnd();
    const left = Math.min(startX, ex);
    const top = Math.min(startY, ey);
    return {
      left,
      top,
      width: Math.abs(ex - startX),
      height: Math.abs(ey - startY),
    };
  };

  const windowAt = (x: number, y: number): WindowRect | null => {
    if (!ctxInfo) return null;
    const vx = x + ctxInfo.origin_x;
    const vy = y + ctxInfo.origin_y;
    // list is z-ordered topmost first
    for (const w of ctxInfo.windows) {
      if (vx >= w.x && vx < w.x + w.width && vy >= w.y && vy < w.y + w.height) {
        return w;
      }
    }
    return null;
  };

  const pixelAt = (x: number, y: number): [number, number, number] => {
    if (!frameData || x < 0 || y < 0 || x >= frameData.width || y >= frameData.height) {
      return [0, 0, 0];
    }
    const i = (y * frameData.width + x) * 4;
    return [frameData.data[i], frameData.data[i + 1], frameData.data[i + 2]];
  };

  const draw = () => {
    raf = 0;
    if (!ctxInfo) return;
    const g = canvas.getContext("2d");
    if (!g) return;
    const W = canvas.width;
    const H = canvas.height;

    // dimmed freeze-frame base
    if (frame) {
      g.drawImage(frame, 0, 0);
    } else {
      g.fillStyle = "#000";
      g.fillRect(0, 0, W, H);
    }
    g.fillStyle = "rgba(0,0,0,0.7)";
    g.fillRect(0, 0, W, H);

    const punch = (x: number, y: number, w: number, h: number) => {
      if (!frame || w <= 0 || h <= 0) return;
      g.drawImage(frame, x, y, w, h, x, y, w, h);
    };

    const showSelection = mouseDown || hasSelection();
    if (showSelection) {
      const r = selectionRect();
      punch(r.left, r.top, r.width, r.height);
      g.strokeStyle = "#fff";
      g.lineWidth = 1;
      g.strokeRect(r.left + 0.5, r.top + 0.5, r.width, r.height);

      const label = `${r.width}x${r.height}`;
      g.font = "12px monospace";
      const tw = g.measureText(label).width;
      const tx = r.left + 5;
      const ty = r.top > 20 ? r.top - 6 : r.top + r.height + 15;
      g.fillStyle = "#000";
      g.fillRect(tx - 2, ty - 12, tw + 4, 16);
      g.fillStyle = "#fff";
      g.fillText(label, tx, ty);
    } else if (hovered && ctxInfo) {
      const left = hovered.x - ctxInfo.origin_x;
      const top = hovered.y - ctxInfo.origin_y;
      punch(left, top, hovered.width, hovered.height);
      g.strokeStyle = "#fff";
      g.lineWidth = 1;
      g.strokeRect(left + 0.5, top + 0.5, hovered.width, hovered.height);
    }

    if (cursorX >= 0 && cursorY >= 0) {
      // crosshair with a 20px gap around the cursor
      g.strokeStyle = "#808080";
      g.lineWidth = 1;
      g.beginPath();
      g.moveTo(0, cursorY + 0.5);
      g.lineTo(cursorX - 20, cursorY + 0.5);
      g.moveTo(cursorX + 20, cursorY + 0.5);
      g.lineTo(W, cursorY + 0.5);
      g.moveTo(cursorX + 0.5, 0);
      g.lineTo(cursorX + 0.5, cursorY - 20);
      g.moveTo(cursorX + 0.5, cursorY + 20);
      g.lineTo(cursorX + 0.5, H);
      g.stroke();

      // loupe, flipped away from the canvas edges
      if (frame) {
        let magX = cursorX + 30;
        let magY = cursorY + 30;
        if (magX + MAGNIFIER_SIZE > W) magX = Math.max(0, cursorX - MAGNIFIER_SIZE - 30);
        if (magY + MAGNIFIER_SIZE > H) magY = Math.max(0, cursorY - MAGNIFIER_SIZE - 30);
        const srcSize = Math.max(1, Math.floor(MAGNIFIER_SIZE / zoom));
        const srcX = Math.max(0, Math.min(cursorX - srcSize / 2, W - srcSize));
        const srcY = Math.max(0, Math.min(cursorY - srcSize / 2, H - srcSize));
        g.imageSmoothingEnabled = false;
        g.drawImage(frame, srcX, srcY, srcSize, srcSize, magX, magY, MAGNIFIER_SIZE, MAGNIFIER_SIZE);
        g.imageSmoothingEnabled = true;
        g.strokeStyle = "#fff";
        g.strokeRect(magX + 0.5, magY + 0.5, MAGNIFIER_SIZE, MAGNIFIER_SIZE);
        g.strokeStyle = "#808080";
        const cx = magX + MAGNIFIER_SIZE / 2;
        const cy = magY + MAGNIFIER_SIZE / 2;
        g.beginPath();
        g.moveTo(cx - 10, cy);
        g.lineTo(cx + 10, cy);
        g.moveTo(cx, cy - 10);
        g.lineTo(cx, cy + 10);
        g.stroke();

        const [pr, pg, pb] = pixelAt(cursorX, cursorY);
        const hex = `#${[pr, pg, pb].map((v) => v.toString(16).padStart(2, "0")).join("")}`;
        g.font = "11px monospace";
        const hw = g.measureText(hex).width;
        g.fillStyle = "#000";
        g.fillRect(magX, magY + MAGNIFIER_SIZE + 2, hw + 6, 15);
        g.fillStyle = "#fff";
        g.fillText(hex, magX + 3, magY + MAGNIFIER_SIZE + 13);
      }
    }
  };

  const schedule = () => {
    if (!raf) raf = requestAnimationFrame(draw);
  };

  const commitRegion = () => {
    if (!ctxInfo) return finish({ kind: "cancelled" });
    const r = selectionRect();
    finish({
      kind: "region",
      x: r.left + ctxInfo.origin_x,
      y: r.top + ctxInfo.origin_y,
      width: r.width,
      height: r.height,
    });
  };

  const onMouseMove = (e: MouseEvent) => {
    const p = toFrame(e);
    cursorX = p.x;
    cursorY = p.y;
    shiftHeld = e.shiftKey;
    if (mouseDown) {
      endX = p.x;
      endY = p.y;
    } else if (!hasSelection()) {
      hovered = windowAt(p.x, p.y);
    }
    schedule();
  };

  const onMouseDown = (e: MouseEvent) => {
    if (e.button === 2) return; // handled by contextmenu
    if (e.button !== 0) return;
    const p = toFrame(e);
    if (e.altKey) {
      const [r, g, b] = pixelAt(p.x, p.y);
      finish({ kind: "color", r, g, b });
      return;
    }
    startX = p.x;
    startY = p.y;
    endX = p.x;
    endY = p.y;
    mouseDown = true;
    dragStarted = true;
    schedule();
  };

  const onMouseUp = (e: MouseEvent) => {
    if (e.button !== 0 || !mouseDown) return;
    const p = toFrame(e);
    endX = p.x;
    endY = p.y;
    mouseDown = false;
    shiftHeld = e.shiftKey;
    const dx = Math.abs(endX - startX);
    const dy = Math.abs(endY - startY);
    if (dx <= CLICK_THRESHOLD && dy <= CLICK_THRESHOLD) {
      const w = windowAt(p.x, p.y);
      if (w) {
        finish({ kind: "window", id: w.id });
      } else {
        finish({ kind: "full_screen" });
      }
      return;
    }
    // ctrl on release keeps the overlay open for arrow fine-tune
    if (!e.ctrlKey) {
      commitRegion();
    } else {
      schedule();
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    shiftHeld = e.shiftKey;
    if (e.key === "Escape") {
      finish({ kind: "cancelled" });
      return;
    }
    if (e.key === "Enter" || e.key === " ") {
      if (hasSelection()) {
        commitRegion();
      } else {
        finish({ kind: "full_screen" });
      }
      return;
    }
    const arrows: Record<string, [number, number]> = {
      ArrowLeft: [-1, 0],
      ArrowRight: [1, 0],
      ArrowUp: [0, -1],
      ArrowDown: [0, 1],
    };
    const delta = arrows[e.key];
    if (!delta) return;
    e.preventDefault();
    const [dx, dy] = delta;
    if (e.ctrlKey && e.shiftKey) {
      // adjust the start (top-left) boundary
      if (dragStarted) {
        startX += dx;
        startY += dy;
      }
    } else if (e.ctrlKey) {
      // move the whole selection
      if (dragStarted) {
        startX += dx;
        startY += dy;
        endX += dx;
        endY += dy;
      }
    } else if (e.shiftKey) {
      // adjust the end (bottom-right) boundary, seeding at the cursor when
      // no selection exists yet
      if (!dragStarted) {
        startX = cursorX;
        startY = cursorY;
        endX = cursorX + dx;
        endY = cursorY + dy;
        dragStarted = true;
      } else {
        endX += dx;
        endY += dy;
      }
    } else {
      // bare arrows nudge the virtual cursor (the webview cannot warp the
      // real pointer, so the crosshair moves instead)
      cursorX += dx;
      cursorY += dy;
    }
    schedule();
  };

  const onKeyUp = (e: KeyboardEvent) => {
    shiftHeld = e.shiftKey;
    schedule();
  };

  const onWheel = (e: WheelEvent) => {
    zoom = e.deltaY < 0 ? Math.min(zoom + 1, 30) : Math.max(zoom - 1, 2);
    schedule();
  };

  const onContextMenu = (e: MouseEvent) => {
    e.preventDefault();
    finish({ kind: "cancelled" });
  };

  onMount(async () => {
    document.body.style.cursor = "crosshair";
    try {
      ctxInfo = await invoke<SelectorContext>("selector_context");
      canvas.width = ctxInfo.frame_width;
      canvas.height = ctxInfo.frame_height;
      const raw = await invoke<ArrayBuffer>("selector_frame");
      const bytes = new Uint8ClampedArray(raw);
      frameData = new ImageData(bytes, ctxInfo.frame_width, ctxInfo.frame_height);
      frame = await createImageBitmap(frameData);
    } catch (err) {
      console.error("selector boot failed", err);
      finish({ kind: "cancelled" });
      return;
    }
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    schedule();
  });

  onCleanup(() => {
    document.body.style.cursor = "";
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("keyup", onKeyUp);
    if (raf) cancelAnimationFrame(raf);
  });

  return (
    <canvas
      ref={canvas}
      class="selector-canvas"
      onMouseMove={onMouseMove}
      onMouseDown={onMouseDown}
      onMouseUp={onMouseUp}
      onWheel={onWheel}
      onContextMenu={onContextMenu}
    />
  );
}
