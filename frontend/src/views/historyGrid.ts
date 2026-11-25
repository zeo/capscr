export const TILE_MIN_WIDTH = 180;
export const TILE_GAP = 1;
export const DEFAULT_OVERSCAN_ROWS = 3;
export const TILE_ASPECT_RATIO = 16 / 10;
export const TILE_META_FALLBACK_HEIGHT = 51;

export interface GridWindowInput {
  itemCount: number;
  containerWidth: number;
  viewportHeight: number;
  scrollTop: number;
  gridOffsetTop: number;
  rowHeight: number;
  minTileWidth?: number;
  gap?: number;
  overscanRows?: number;
}

export interface GridWindow {
  columns: number;
  totalRows: number;
  rowHeight: number;
  rowStride: number;
  startRow: number;
  endRow: number;
  startIndex: number;
  endIndex: number;
  topSpacer: number;
  bottomSpacer: number;
  totalHeight: number;
}

// matches css repeat(auto-fill, minmax(180px, 1fr)) with 1px gap
export function computeColumnCount(
  containerWidth: number,
  minTileWidth = TILE_MIN_WIDTH,
  gap = TILE_GAP,
): number {
  if (!Number.isFinite(containerWidth) || containerWidth <= 0) return 1;
  const unit = minTileWidth + gap;
  if (!Number.isFinite(unit) || unit <= 0) return 1;
  return Math.max(1, Math.floor((containerWidth + gap) / unit));
}

export function estimateRowHeight(
  containerWidth: number,
  columns: number,
  gap = TILE_GAP,
  metaHeight = TILE_META_FALLBACK_HEIGHT,
): number {
  const cols = Math.max(1, Math.floor(columns));
  const tileWidth =
    Number.isFinite(containerWidth) && containerWidth > 0
      ? Math.max(1, (containerWidth - gap * (cols - 1)) / cols)
      : TILE_MIN_WIDTH;
  return tileWidth / TILE_ASPECT_RATIO + metaHeight;
}

export function computeGridWindow(input: GridWindowInput): GridWindow {
  const count = Number.isFinite(input.itemCount)
    ? Math.max(0, Math.floor(input.itemCount))
    : 0;
  const minTileWidth = input.minTileWidth ?? TILE_MIN_WIDTH;
  const gap = input.gap ?? TILE_GAP;
  const overscan = Math.max(
    0,
    Math.floor(input.overscanRows ?? DEFAULT_OVERSCAN_ROWS),
  );
  const columns = computeColumnCount(input.containerWidth, minTileWidth, gap);
  const rowHeight =
    Number.isFinite(input.rowHeight) && input.rowHeight > 0
      ? input.rowHeight
      : estimateRowHeight(input.containerWidth, columns, gap);
  const rowStride = rowHeight + gap;

  if (count === 0) {
    return {
      columns,
      totalRows: 0,
      rowHeight,
      rowStride,
      startRow: 0,
      endRow: 0,
      startIndex: 0,
      endIndex: 0,
      topSpacer: 0,
      bottomSpacer: 0,
      totalHeight: 0,
    };
  }

  const totalRows = Math.ceil(count / columns);
  const totalHeight = totalRows * rowHeight + (totalRows - 1) * gap;
  const scrollTop = Number.isFinite(input.scrollTop) ? input.scrollTop : 0;
  const gridOffsetTop = Number.isFinite(input.gridOffsetTop)
    ? input.gridOffsetTop
    : 0;
  const viewportHeight =
    Number.isFinite(input.viewportHeight) && input.viewportHeight > 0
      ? input.viewportHeight
      : 0;

  const viewTop = scrollTop - gridOffsetTop;
  const viewBottom = viewTop + viewportHeight;
  const clampedTop = Math.max(0, Math.min(viewTop, Math.max(0, totalHeight - 1)));
  const rawStartRow = Math.min(totalRows - 1, Math.floor(clampedTop / rowStride));
  const rawEndRow =
    viewBottom <= 0
      ? 0
      : Math.min(totalRows, Math.max(rawStartRow + 1, Math.ceil(viewBottom / rowStride)));

  const startRow = Math.max(0, rawStartRow - overscan);
  const endRow = Math.min(
    totalRows,
    Math.max(startRow + 1, rawEndRow + overscan),
  );
  const startIndex = startRow * columns;
  const endIndex = Math.min(count, endRow * columns);
  const topSpacer = startRow * rowStride;
  const bottomSpacer = (totalRows - endRow) * rowStride;

  return {
    columns,
    totalRows,
    rowHeight,
    rowStride,
    startRow,
    endRow,
    startIndex,
    endIndex,
    topSpacer,
    bottomSpacer,
    totalHeight,
  };
}

export function gridWindowEquals(a: GridWindow, b: GridWindow): boolean {
  return (
    a.columns === b.columns &&
    a.totalRows === b.totalRows &&
    a.startRow === b.startRow &&
    a.endRow === b.endRow &&
    a.startIndex === b.startIndex &&
    a.endIndex === b.endIndex &&
    a.topSpacer === b.topSpacer &&
    a.bottomSpacer === b.bottomSpacer &&
    a.totalHeight === b.totalHeight
  );
}
