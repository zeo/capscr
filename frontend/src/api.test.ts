/// <reference types="vite/client" />
import { describe, expect, it } from "vitest";
import mainRs from "../../src/main.rs?raw";

const sources = Object.values(
  import.meta.glob<string>(["./**/*.{ts,tsx}", "!./**/*.test.{ts,tsx}"], {
    query: "?raw",
    import: "default",
    eager: true,
  }),
);

const callSites = sources.flatMap((text) => text.match(/\binvoke\s*[<(]/g) ?? []);
const invoked = sources.flatMap((text) =>
  [...text.matchAll(/\binvoke\b[^"(]*\(\s*"([a-z0-9_]+)"/g)].map((m) => m[1]),
);

const handlerBlock = mainRs.match(/generate_handler!\[([\s\S]*?)\]\)/)?.[1] ?? "";
const handlers = handlerBlock
  .split(/\r?\n/)
  .map((line) => line.trim().replace(/,$/, ""))
  .filter((line) => line && !line.startsWith("#["))
  .map((path) => path.split("::").pop()!);

describe("frontend to backend command contract", () => {
  it("finds every invoke call by a literal command name", () => {
    expect(invoked.length).toBeGreaterThan(50);
    expect(invoked).toHaveLength(callSites.length);
  });

  it("only invokes commands the backend registers", () => {
    expect(handlers.length).toBeGreaterThan(50);
    expect([...new Set(invoked)].filter((name) => !handlers.includes(name))).toEqual([]);
  });

  it("registers each command once", () => {
    expect(handlers.filter((name, i) => handlers.indexOf(name) !== i)).toEqual([]);
  });
});
