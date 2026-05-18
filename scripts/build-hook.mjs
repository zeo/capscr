#!/usr/bin/env node
// Regenerate platform icons + installer artwork from icon-master.png if it
// exists, otherwise fall back to icons/icon.png. Runs before dev/build.

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, statSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");

const mode = process.argv[2];
if (!mode || (mode !== "--dev" && mode !== "--build")) {
  console.error("usage: build-hook.mjs --dev|--build");
  process.exit(1);
}

const master = resolve(root, "icons", "icon-master.png");
const iconPng = resolve(root, "icons", "icon.png");
const ico = resolve(root, "icons", "icon.ico");
const headerBmp = resolve(root, "icons", "installer-header.bmp");
const sidebarBmp = resolve(root, "icons", "installer-sidebar.bmp");

// source = master if present (preferred — kept high-res for sharp downscale),
// otherwise fall back to icons/icon.png (which `cargo tauri icon` overwrites
// at 512px, so it loses fidelity over time).
const source = existsSync(master) ? master : iconPng;

if (!existsSync(source)) {
  console.error(`icon source not found: ${source}`);
  process.exit(1);
}

const sourceMtime = statSync(source).mtimeMs;
const needs = (target) => !existsSync(target) || sourceMtime > statSync(target).mtimeMs;

if (needs(ico)) {
  console.log(`[capscr] regenerating platform icons from ${source}`);
  mkdirSync(resolve(root, "icons"), { recursive: true });
  const r = spawnSync(
    "cargo",
    ["tauri", "icon", source, "-o", resolve(root, "icons")],
    { cwd: root, stdio: "inherit", shell: true },
  );
  if (r.status !== 0) {
    console.error(`[capscr] icon generation failed (exit ${r.status})`);
    process.exit(r.status ?? 1);
  }
} else {
  console.log("[capscr] icons up-to-date, skipping regen");
}

// Installer BMPs: lanczos-downscale from master to exact NSIS dimensions.
// 150x57 header (icon on the left, NSIS draws title to the right of it).
// 164x314 sidebar (welcome page — icon centered, dark background fill below).
function genBmp(target, vf) {
  if (!needs(target)) return;
  console.log(`[capscr] regenerating ${target}`);
  const r = spawnSync(
    "ffmpeg",
    ["-y", "-i", source, "-vf", vf, "-frames:v", "1", "-update", "1", target],
    { stdio: ["ignore", "ignore", "inherit"], shell: false },
  );
  if (r.status !== 0) {
    console.error(`[capscr] ffmpeg failed for ${target} (exit ${r.status})`);
    process.exit(r.status ?? 1);
  }
}

genBmp(
  headerBmp,
  "scale=50:50:flags=lanczos+full_chroma_inp+full_chroma_int+accurate_rnd,pad=150:57:3:3:color=0x0d0d0d",
);
genBmp(
  sidebarBmp,
  "scale=164:164:flags=lanczos+full_chroma_inp+full_chroma_int+accurate_rnd,pad=164:314:0:75:color=0x0d0d0d",
);

const frontendCmd = mode === "--dev" ? "dev" : "build";
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const r = spawnSync(npm, ["run", frontendCmd], {
  cwd: resolve(root, "frontend"),
  stdio: "inherit",
  shell: true,
});
process.exit(r.status ?? 1);
