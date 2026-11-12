// the ratios a shift-drag snaps to; src/overlay/selection.rs keeps the same
// list and both read src/overlay/fixtures/aspect.json in their tests
const ASPECT_RATIOS = [1, 16 / 9, 16 / 10, 4 / 3, 21 / 9];

export const aspectTarget = (width: number, height: number): number | null => {
  if (width <= 0 || height <= 0) return null;
  const ratio = width / height;
  return ASPECT_RATIOS.reduce((best, candidate) =>
    Math.abs(ratio - candidate) < Math.abs(ratio - best) ? candidate : best,
  );
};
