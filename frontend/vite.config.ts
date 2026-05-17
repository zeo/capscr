import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

const pkg = JSON.parse(
  readFileSync(
    resolve(dirname(fileURLToPath(import.meta.url)), "package.json"),
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
    __APP_VERSION__: JSON.stringify(pkg.version),
  },
  build: {
    target: "esnext",
    minify: "esbuild",
    sourcemap: false,
  },
});
