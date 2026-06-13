#!/usr/bin/env node
// build the frontend before dev/build, and (opt-in) regenerate platform icons
// from icon-master.png. icon regen is gated behind CAPSCR_REGEN_ICONS=1 so a
// normal build never clobbers the committed, max-compat icons

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
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


// source = master if present (preferred — kept high-res for sharp downscale),
// otherwise fall back to icons/icon.png
const source = existsSync(master) ? master : iconPng;

// regeneration is opt-in: `cargo tauri icon` overwrites icon.png at 512px and
// emits an all-png icon.ico that windows fails to render at small sizes, so a
// normal build keeps the committed icons untouched
const regenRequested =
  process.env.CAPSCR_REGEN_ICONS === "1" || process.argv.includes("--regen-icons");

if (!existsSync(ico) || regenRequested) {
  if (!existsSync(source)) {
    console.error(`icon source not found: ${source}`);
    process.exit(1);
  }
  console.log(`[capscr] regenerating platform icons from ${source}`);
  mkdirSync(resolve(root, "icons"), { recursive: true });
  // prefer the npm-global `tauri` binary (what tauri-action installs in CI);
  // fall back to `cargo tauri` which is what local `cargo install tauri-cli`
  // provides
  const candidates = [
    ["tauri", ["icon", source, "-o", resolve(root, "icons")]],
    ["cargo", ["tauri", "icon", source, "-o", resolve(root, "icons")]],
  ];
  let lastSuccess = false;
  let lastStatus = 1;
  for (const [bin, args] of candidates) {
    const r = spawnSync(bin, args, {
      cwd: root,
      stdio: "inherit",
      shell: true,
    });
    lastStatus = r.status ?? 1;
    if (r.status === 0) {
      lastSuccess = true;
      break;
    }
    // any non-zero exit (incl. cmd's 9009 / "not recognized" / 101 for no-such-cargo-subcommand)
    // → fall through to the next candidate; we only stop on success.
    if (r.error?.code === "ENOENT") continue;
  }
  if (!lastSuccess) {
    console.error(`[capscr] icon generation failed (exit ${lastStatus})`);
    process.exit(lastStatus);
  }
} else {
  console.log(
    "[capscr] using committed icons; set CAPSCR_REGEN_ICONS=1 to rebuild from icon-master.png",
  );
}



const frontendCmd = mode === "--dev" ? "dev" : "build";
const npm = process.platform === "win32" ? "npm.cmd" : "npm";
const r = spawnSync(npm, ["run", frontendCmd], {
  cwd: resolve(root, "frontend"),
  stdio: "inherit",
  shell: true,
});
process.exit(r.status ?? 1);
