import { describe, expect, it } from "vitest";
import {
  computeColumnCount,
  computeGridWindow,
  gridWindowEquals,
  TILE_GAP,
  TILE_MIN_WIDTH,
} from "./historyGrid";

function expectExactHeightIdentity(
  win: ReturnType<typeof computeGridWindow>,
  gap = TILE_GAP,
) {
  if (win.totalRows === 0) {
    expect(win.totalHeight).toBe(0);
    expect(win.topSpacer).toBe(0);
    expect(win.bottomSpacer).toBe(0);
    return;
  }
  const renderedRows = win.endRow - win.startRow;
  const renderedHeight = renderedRows * win.rowHeight + (renderedRows - 1) * gap;
  expect(win.topSpacer + renderedHeight + win.bottomSpacer).toBeCloseTo(
    win.totalHeight,
    6,
  );
}

describe("computeColumnCount", () => {
  it("matches css repeat(auto-fill, minmax(180px, 1fr)) with 1px gap", () => {
    expect(computeColumnCount(0)).toBe(1);
    expect(computeColumnCount(-50)).toBe(1);
    expect(computeColumnCount(179)).toBe(1);
    expect(computeColumnCount(180)).toBe(1);
    expect(computeColumnCount(360)).toBe(1);
    expect(computeColumnCount(361)).toBe(2);
    expect(computeColumnCount(541)).toBe(2);
    expect(computeColumnCount(542)).toBe(3);
    expect(computeColumnCount(723)).toBe(4);
    expect(computeColumnCount(904)).toBe(5);
  });
});

describe("computeGridWindow", () => {
  it("returns an empty window for 0 items", () => {
    const win = computeGridWindow({
      itemCount: 0,
      containerWidth: 904,
      viewportHeight: 600,
      scrollTop: 0,
      gridOffsetTop: 140,
      rowHeight: 165,
    });
    expect(win.columns).toBe(5);
    expect(win.totalRows).toBe(0);
    expect(win.startIndex).toBe(0);
    expect(win.endIndex).toBe(0);
    expectExactHeightIdentity(win);
  });

  it("renders all items with zero spacers for a small list", () => {
    const win = computeGridWindow({
      itemCount: 7,
      containerWidth: 723,
      viewportHeight: 600,
      scrollTop: 0,
      gridOffsetTop: 140,
      rowHeight: 165,
      overscanRows: 3,
    });
    expect(win.columns).toBe(4);
    expect(win.totalRows).toBe(2);
    expect(win.startRow).toBe(0);
    expect(win.endRow).toBe(2);
    expect(win.startIndex).toBe(0);
    expect(win.endIndex).toBe(7);
    expect(win.topSpacer).toBe(0);
    expect(win.bottomSpacer).toBe(0);
    expect(win.totalHeight).toBe(2 * 165 + 1);
    expectExactHeightIdentity(win);
  });

  it("windows a 6,148-item list at top, middle, and bottom scroll", () => {
    const base = {
      itemCount: 6148,
      containerWidth: 723,
      viewportHeight: 664,
      gridOffsetTop: 140,
      rowHeight: 165,
      minTileWidth: TILE_MIN_WIDTH,
      gap: TILE_GAP,
      overscanRows: 3,
    };

    const top = computeGridWindow({ ...base, scrollTop: 0 });
    expect(top.columns).toBe(4);
    expect(top.totalRows).toBe(1537);
    expect(top.startRow).toBe(0);
    expect(top.topSpacer).toBe(0);
    expect(top.endRow).toBe(7);
    expect(top.endIndex - top.startIndex).toBe(28);
    expect(top.bottomSpacer).toBe((1537 - 7) * 166);
    expectExactHeightIdentity(top);

    const mid = computeGridWindow({ ...base, scrollTop: 100_140 });
    expect(mid.startRow).toBe(599);
    expect(mid.endRow).toBe(610);
    expect(mid.startIndex).toBe(599 * 4);
    expect(mid.endIndex).toBe(610 * 4);
    expect(mid.endIndex - mid.startIndex).toBe(44);
    expect(mid.topSpacer).toBe(599 * 166);
    expect(mid.bottomSpacer).toBe((1537 - 610) * 166);
    expectExactHeightIdentity(mid);

    const bottomScroll = base.gridOffsetTop + top.totalHeight - base.viewportHeight;
    const bottom = computeGridWindow({ ...base, scrollTop: bottomScroll });
    expect(bottom.endRow).toBe(1537);
    expect(bottom.endIndex).toBe(6148);
    expect(bottom.bottomSpacer).toBe(0);
    expect(bottom.startRow).toBeGreaterThan(1525);
    expectExactHeightIdentity(bottom);

    // overscrolled past end still clamps cleanly to the trailing rows
    const pastBottom = computeGridWindow({ ...base, scrollTop: top.totalHeight + 50_000 });
    expect(pastBottom.endRow).toBe(1537);
    expect(pastBottom.endIndex).toBe(6148);
    expect(pastBottom.bottomSpacer).toBe(0);
    expectExactHeightIdentity(pastBottom);
  });

  it("recomputes columns, total rows, and slice on container resize", () => {
    const narrow = computeGridWindow({
      itemCount: 6000,
      containerWidth: 542,
      viewportHeight: 600,
      scrollTop: 5000,
      gridOffsetTop: 120,
      rowHeight: 164,
      overscanRows: 2,
    });
    const wide = computeGridWindow({
      itemCount: 6000,
      containerWidth: 1085,
      viewportHeight: 600,
      scrollTop: 5000,
      gridOffsetTop: 120,
      rowHeight: 164,
      overscanRows: 2,
    });

    expect(narrow.columns).toBe(3);
    expect(narrow.totalRows).toBe(2000);
    expect(wide.columns).toBe(6);
    expect(wide.totalRows).toBe(1000);
    expect(wide.totalHeight).toBeLessThan(narrow.totalHeight);
    expect(wide.startIndex).toBe(wide.startRow * 6);
    expect(wide.endIndex - wide.startIndex).toBeLessThanOrEqual(60);
    expectExactHeightIdentity(narrow);
    expectExactHeightIdentity(wide);
  });

  it("treats intra-row scroll offsets as equal windows", () => {
    const a = computeGridWindow({
      itemCount: 1000,
      containerWidth: 723,
      viewportHeight: 600,
      scrollTop: 1000,
      gridOffsetTop: 140,
      rowHeight: 165,
      overscanRows: 3,
    });
    const b = computeGridWindow({
      itemCount: 1000,
      containerWidth: 723,
      viewportHeight: 600,
      scrollTop: 1020,
      gridOffsetTop: 140,
      rowHeight: 165,
      overscanRows: 3,
    });
    expect(gridWindowEquals(a, b)).toBe(true);
  });
});
