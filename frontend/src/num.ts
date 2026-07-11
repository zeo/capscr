// reconcile a number input's shown value with what actually gets saved. the
// onInput handlers skip an out-of-range keystroke, which leaves a controlled
// input displaying text the store never took; call this on change (fires on
// blur/enter) to clamp the visible value into range and return the number to
// persist. an empty or unparseable field falls back to the current value.
export function commitNumber(
  el: HTMLInputElement,
  opts: { min: number; max: number; fallback: number; int?: boolean },
): number {
  let v = parseFloat(el.value);
  if (Number.isNaN(v)) v = opts.fallback;
  v = Math.min(opts.max, Math.max(opts.min, v));
  if (opts.int) v = Math.round(v);
  el.value = String(v);
  return v;
}
