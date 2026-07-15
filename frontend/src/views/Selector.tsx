import { onCleanup, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";

interface WindowRect {
  id: number;
  handle?: string;
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
  native_backdrop: boolean;
}

const CLICK_THRESHOLD = 5;
const MAGNIFIER_SIZE = 120;
const ASPECT_TARGETS = [1, 16 / 9, 16 / 10, 4 / 3, 21 / 9];
const DRAG_CHANNEL = "capscr-selector-drag";

interface SharedDrag {
  startX: number;
  startY: number;
  endX: number;
  endY: number;
  mouseDown: boolean;
  scaleX: number;
  scaleY: number;
  shiftHeld: boolean;
}

export function Selector() {
  let backdrop!: HTMLCanvasElement;
  let root!: HTMLDivElement;
  let outline!: HTMLDivElement;
  let sizeLabel!: HTMLDivElement;
  let loupe!: HTMLCanvasElement;
  let colorLabel!: HTMLDivElement;

  let ctxInfo: SelectorContext | null = null;
  let frame: ImageBitmap | null = null;
  let frameData: ImageData | null = null;
  let frameLoading: Promise<void> | null = null;
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
  let altHeld = false;
  let finished = false;
  let raf = 0;
  let selectionScaleX = 1;
  let selectionScaleY = 1;
  const dragChannel = new BroadcastChannel(DRAG_CHANNEL);

  const finish = (outcome: Record<string, unknown>) => {
    if (finished) return;
    finished = true;
    invoke("selector_finish", { outcome }).catch(() => {});
  };

  const scale = () => ({
    sx: (ctxInfo?.frame_width ?? window.innerWidth) / window.innerWidth,
    sy: (ctxInfo?.frame_height ?? window.innerHeight) / window.innerHeight,
  });

  const toFrame = (e: MouseEvent) => {
    const { sx, sy } = scale();
    return { x: Math.round(e.clientX * sx), y: Math.round(e.clientY * sy) };
  };

  const toDesktop = (e: MouseEvent) => ({
    x: e.clientX + (ctxInfo?.origin_x ?? 0),
    y: e.clientY + (ctxInfo?.origin_y ?? 0),
  });

  const shareDrag = () => {
    dragChannel.postMessage({
      startX,
      startY,
      endX,
      endY,
      mouseDown,
      scaleX: selectionScaleX,
      scaleY: selectionScaleY,
      shiftHeld,
    } satisfies SharedDrag);
  };

  const snappedEnd = () => {
    if (!shiftHeld) return { ex: endX, ey: endY };
    const dx = endX - startX;
    const dy = endY - startY;
    const width = Math.abs(dx);
    const height = Math.abs(dy);
    if (!width || !height) return { ex: endX, ey: endY };
    const ratio = width / height;
    const target = ASPECT_TARGETS.reduce((best, candidate) =>
      Math.abs(ratio - candidate) < Math.abs(ratio - best) ? candidate : best,
    );
    return { ex: endX, ey: startY + Math.sign(dy || 1) * Math.round(width / target) };
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
    return {
      left: Math.min(startX, ex),
      top: Math.min(startY, ey),
      width: Math.abs(ex - startX),
      height: Math.abs(ey - startY),
    };
  };

  const windowAt = (clientX: number, clientY: number) => {
    if (!ctxInfo) return null;
    const vx = clientX + ctxInfo.origin_x;
    const vy = clientY + ctxInfo.origin_y;
    return (
      ctxInfo.windows.find(
        (candidate) =>
          vx >= candidate.x &&
          vx < candidate.x + candidate.width &&
          vy >= candidate.y &&
          vy < candidate.y + candidate.height,
      ) ?? null
    );
  };

  const pixelAt = (x: number, y: number): [number, number, number] => {
    if (!frameData || x < 0 || y < 0 || x >= frameData.width || y >= frameData.height) {
      return [0, 0, 0];
    }
    const offset = (y * frameData.width + x) * 4;
    return [frameData.data[offset], frameData.data[offset + 1], frameData.data[offset + 2]];
  };

  const position = (element: HTMLElement, left: number, top: number, width: number, height: number) => {
    element.style.left = `${left}px`;
    element.style.top = `${top}px`;
    element.style.width = `${Math.max(0, width)}px`;
    element.style.height = `${Math.max(0, height)}px`;
  };

  const paintBackdrop = () => {
    if (!frame || !ctxInfo || ctxInfo.native_backdrop) return;
    backdrop.width = ctxInfo.frame_width;
    backdrop.height = ctxInfo.frame_height;
    backdrop.getContext("2d")?.drawImage(frame, 0, 0);
  };

  const loadFrame = () => {
    if (frame) return Promise.resolve();
    if (frameLoading) return frameLoading;
    frameLoading = (async () => {
      if (!ctxInfo) return;
      const raw = await invoke<ArrayBuffer>("selector_frame");
      frameData = new ImageData(
        new Uint8ClampedArray(raw),
        ctxInfo.frame_width,
        ctxInfo.frame_height,
      );
      frame = await createImageBitmap(frameData);
      paintBackdrop();
      schedule();
    })();
    return frameLoading;
  };

  const requestFrame = () => {
    void loadFrame().catch((error) => console.error("selector frame load failed", error));
  };

  const drawLoupe = (screenX: number, screenY: number) => {
    if (!frame || !ctxInfo) return;
    const { sx, sy } = scale();
    let left = screenX + 30;
    let top = screenY + 30;
    if (left + MAGNIFIER_SIZE > window.innerWidth) left = Math.max(0, screenX - MAGNIFIER_SIZE - 30);
    if (top + MAGNIFIER_SIZE + 19 > window.innerHeight) top = Math.max(0, screenY - MAGNIFIER_SIZE - 49);
    loupe.style.left = `${left}px`;
    loupe.style.top = `${top}px`;
    colorLabel.style.left = `${left}px`;
    colorLabel.style.top = `${top + MAGNIFIER_SIZE + 2}px`;

    const dpr = window.devicePixelRatio || 1;
    const size = Math.round(MAGNIFIER_SIZE * dpr);
    if (loupe.width !== size || loupe.height !== size) {
      loupe.width = size;
      loupe.height = size;
    }
    const sourceWidth = Math.max(1, Math.floor((MAGNIFIER_SIZE * sx) / zoom));
    const sourceHeight = Math.max(1, Math.floor((MAGNIFIER_SIZE * sy) / zoom));
    const sourceX = Math.max(0, Math.min(cursorX - sourceWidth / 2, ctxInfo.frame_width - sourceWidth));
    const sourceY = Math.max(0, Math.min(cursorY - sourceHeight / 2, ctxInfo.frame_height - sourceHeight));
    const g = loupe.getContext("2d");
    if (!g) return;
    g.imageSmoothingEnabled = false;
    g.drawImage(frame, sourceX, sourceY, sourceWidth, sourceHeight, 0, 0, size, size);
    g.strokeStyle = "#808080";
    g.lineWidth = dpr;
    g.beginPath();
    g.moveTo(size / 2 - 10 * dpr, size / 2);
    g.lineTo(size / 2 + 10 * dpr, size / 2);
    g.moveTo(size / 2, size / 2 - 10 * dpr);
    g.lineTo(size / 2, size / 2 + 10 * dpr);
    g.stroke();

    const color = pixelAt(cursorX, cursorY);
    colorLabel.textContent = `#${color.map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
  };

  const render = () => {
    raf = 0;
    if (!ctxInfo) return;
    const { sx, sy } = scale();
    if (cursorX >= 0 && cursorY >= 0) {
      const screenX = cursorX / sx;
      const screenY = cursorY / sy;
      const showLoupe = altHeld && frame !== null;
      loupe.style.display = showLoupe ? "block" : "none";
      colorLabel.style.display = showLoupe ? "block" : "none";
      if (showLoupe) drawLoupe(screenX, screenY);
    } else {
      [loupe, colorLabel].forEach((element) => (element.style.display = "none"));
    }

    let rect: { left: number; top: number; width: number; height: number } | null = null;
    let label = "";
    if (mouseDown || hasSelection()) {
      const selected = selectionRect();
      rect = {
        left: selected.left - ctxInfo.origin_x,
        top: selected.top - ctxInfo.origin_y,
        width: selected.width,
        height: selected.height,
      };
      label = `${Math.round(selected.width * selectionScaleX)}x${Math.round(selected.height * selectionScaleY)}`;
    } else if (hovered) {
      rect = {
        left: hovered.x - ctxInfo.origin_x,
        top: hovered.y - ctxInfo.origin_y,
        width: hovered.width,
        height: hovered.height,
      };
    }

    if (!rect) {
      [outline, sizeLabel].forEach((element) => {
        element.style.display = "none";
      });
      return;
    }

    const left = Math.max(0, rect.left);
    const top = Math.max(0, rect.top);
    const right = Math.min(window.innerWidth, rect.left + rect.width);
    const bottom = Math.min(window.innerHeight, rect.top + rect.height);
    outline.style.display = "block";
    position(outline, left, top, right - left, bottom - top);

    if (label) {
      sizeLabel.style.display = "block";
      sizeLabel.textContent = label;
      sizeLabel.style.left = `${left + 5}px`;
      sizeLabel.style.top = `${top > 24 ? top - 21 : bottom + 5}px`;
    } else {
      sizeLabel.style.display = "none";
    }
  };

  const schedule = () => {
    if (!raf) raf = requestAnimationFrame(render);
  };

  const commitRegion = () => {
    if (!ctxInfo) return finish({ kind: "cancelled" });
    const rect = selectionRect();
    finish({
      kind: "region",
      x: Math.round(rect.left),
      y: Math.round(rect.top),
      width: Math.max(1, Math.round(rect.width)),
      height: Math.max(1, Math.round(rect.height)),
    });
  };

  const onMouseMove = (e: MouseEvent) => {
    const point = toFrame(e);
    cursorX = point.x;
    cursorY = point.y;
    shiftHeld = e.shiftKey;
    altHeld = e.altKey;
    if (altHeld) requestFrame();
    if (mouseDown) {
      const desktop = toDesktop(e);
      endX = desktop.x;
      endY = desktop.y;
      shareDrag();
    } else if (!hasSelection()) {
      const nextHovered = windowAt(e.clientX, e.clientY);
      if (nextHovered === hovered && !altHeld) return;
      hovered = nextHovered;
    }
    schedule();
  };

  const onMouseLeave = () => {
    cursorX = -1;
    cursorY = -1;
    if (!mouseDown && !hasSelection()) hovered = null;
    schedule();
  };

  const onMouseDown = async (e: MouseEvent) => {
    if (e.button !== 0) return;
    const point = toFrame(e);
    if (e.altKey) {
      try {
        await loadFrame();
      } catch (error) {
        console.error("selector frame load failed", error);
        return;
      }
      const [r, g, b] = pixelAt(point.x, point.y);
      finish({ kind: "color", r, g, b });
      return;
    }
    const desktop = toDesktop(e);
    const { sx, sy } = scale();
    selectionScaleX = sx;
    selectionScaleY = sy;
    startX = desktop.x;
    startY = desktop.y;
    endX = desktop.x;
    endY = desktop.y;
    mouseDown = true;
    dragStarted = true;
    shareDrag();
    schedule();
  };

  const onMouseUp = (e: MouseEvent) => {
    if (e.button !== 0 || !mouseDown) return;
    const desktop = toDesktop(e);
    endX = desktop.x;
    endY = desktop.y;
    mouseDown = false;
    shiftHeld = e.shiftKey;
    altHeld = e.altKey;
    if (altHeld) requestFrame();
    shareDrag();
    if (Math.abs(endX - startX) <= CLICK_THRESHOLD && Math.abs(endY - startY) <= CLICK_THRESHOLD) {
      const target = windowAt(e.clientX, e.clientY);
      finish(
        target
          ? {
              kind: "window",
              id: target.id,
              handle: target.handle ?? null,
              x: target.x,
              y: target.y,
            }
          : { kind: "full_screen" },
      );
    } else if (!e.ctrlKey) {
      commitRegion();
    } else {
      schedule();
    }
  };

  const onKeyDown = (e: KeyboardEvent) => {
    shiftHeld = e.shiftKey;
    altHeld = e.altKey;
    if (altHeld) requestFrame();
    if (e.key === "Escape") return finish({ kind: "cancelled" });
    if (e.key === "Enter" || e.key === " ") {
      return hasSelection() ? commitRegion() : finish({ kind: "full_screen" });
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
    if (e.ctrlKey && e.shiftKey && dragStarted) {
      startX += dx;
      startY += dy;
    } else if (e.ctrlKey && dragStarted) {
      startX += dx;
      startY += dy;
      endX += dx;
      endY += dy;
    } else if (e.shiftKey) {
      if (!dragStarted) {
        const { sx, sy } = scale();
        startX = cursorX / sx + (ctxInfo?.origin_x ?? 0);
        startY = cursorY / sy + (ctxInfo?.origin_y ?? 0);
        endX = startX;
        endY = startY;
        dragStarted = true;
      }
      endX += dx;
      endY += dy;
    } else {
      cursorX += dx;
      cursorY += dy;
    }
    if (dragStarted) shareDrag();
    schedule();
  };

  const onKeyUp = (e: KeyboardEvent) => {
    shiftHeld = e.shiftKey;
    altHeld = e.altKey;
    schedule();
  };

  const onWheel = (e: WheelEvent) => {
    e.preventDefault();
    zoom = e.deltaY < 0 ? Math.min(zoom + 1, 30) : Math.max(zoom - 1, 2);
    schedule();
  };

  const onContextMenu = (e: MouseEvent) => {
    e.preventDefault();
    finish({ kind: "cancelled" });
  };

  const onResize = () => {
    paintBackdrop();
    schedule();
  };

  onMount(async () => {
    document.body.style.cursor = "crosshair";
    dragChannel.onmessage = (event: MessageEvent<SharedDrag>) => {
      const next = event.data;
      if (![next.startX, next.startY, next.endX, next.endY].every(Number.isFinite)) return;
      startX = next.startX;
      startY = next.startY;
      endX = next.endX;
      endY = next.endY;
      mouseDown = next.mouseDown;
      selectionScaleX = next.scaleX;
      selectionScaleY = next.scaleY;
      shiftHeld = next.shiftHeld;
      dragStarted = true;
      schedule();
    };
    try {
      ctxInfo = await invoke<SelectorContext>("selector_context");
      document.documentElement.classList.toggle("selector-native-backdrop", ctxInfo.native_backdrop);
      if (!ctxInfo.native_backdrop) await loadFrame();
      render();
      await invoke("selector_ready");
    } catch (error) {
      console.error("selector boot failed", error);
      finish({ kind: "cancelled" });
      return;
    }
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("resize", onResize);
    schedule();
  });

  onCleanup(() => {
    document.body.style.cursor = "";
    document.documentElement.classList.remove("selector-native-backdrop");
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("keyup", onKeyUp);
    window.removeEventListener("resize", onResize);
    frame?.close();
    dragChannel.close();
    if (raf) cancelAnimationFrame(raf);
  });

  return (
    <div
      ref={root}
      class="selector"
      onMouseMove={onMouseMove}
      onMouseLeave={onMouseLeave}
      onMouseDown={onMouseDown}
      onMouseUp={onMouseUp}
      onWheel={onWheel}
      onContextMenu={onContextMenu}
    >
      <canvas ref={backdrop} class="selector-backdrop" />
      <div ref={outline} class="selector-outline" />
      <div ref={sizeLabel} class="selector-label" />
      <canvas ref={loupe} class="selector-loupe" />
      <div ref={colorLabel} class="selector-color" />
    </div>
  );
}
