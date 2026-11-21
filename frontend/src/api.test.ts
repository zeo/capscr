/// <reference types="vite/client" />
import { describe, expect, it } from "vitest";
import mainRs from "../../src/main.rs?raw";

// host.ts wraps invoke itself and names no command
const sources = Object.values(
  import.meta.glob<string>(["./**/*.{ts,tsx}", "!./**/*.test.{ts,tsx}", "!./host.ts"], {
    query: "?raw",
    import: "default",
    eager: true,
  }),
);

const callSites = sources.flatMap((text) => text.match(/\binvoke\s*[<(]/g) ?? []);
const invoked = sources.flatMap((text) =>
  [...text.matchAll(/\binvoke\b[^"(]*\(\s*"([a-z0-9_]+)"/g)].map((m) => m[1]),
);

const handlers = [...mainRs.matchAll(/json_command\(\s*"([a-z0-9_]+)"/g)].map((m) => m[1]);

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
