/// <reference types="vitest/config" />
import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const tauriConf = JSON.parse(
  readFileSync(
    resolve(dirname(fileURLToPath(import.meta.url)), "..", "tauri.conf.json"),
    "utf-8",
  ),
) as { version: string };

export default defineConfig({
  plugins: [solidPlugin()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  envPrefix: ["VITE_", "TAURI_"],
  define: {
    __APP_VERSION__: JSON.stringify(tauriConf.version),
  },
  // the tested editor modules are pure, so skip the dom the solid plugin asks for
  test: {
    environment: "node",
  },
  build: {
    target: "esnext",
    minify: "esbuild",
    sourcemap: false,
    rolldownOptions: {
      input: {
        // the recording bar loads its own minimal page so no shared boot
        // splash or hub chrome can ever paint behind the pill
        main: resolve(dirname(fileURLToPath(import.meta.url)), "index.html"),
        recbar: resolve(dirname(fileURLToPath(import.meta.url)), "recbar.html"),
      },
    },
  },
});
