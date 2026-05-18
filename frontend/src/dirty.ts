import { createSignal } from "solid-js";

// Any view that holds unsaved edits to AppConfig flips this on edit and clears
// it on save. App.tsx checks before tab switches and on window close so the
// user doesn't silently lose work.
export const [configDirty, setConfigDirty] = createSignal(false);
