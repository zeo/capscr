import { describe, expect, it, vi } from "vitest";
import type { AppConfig } from "./api";

vi.mock("./api", () => ({ api: { getConfig: () => new Promise(() => {}) } }));

const { applySavedConfig, config, mutateConfig } = await import("./store");

const draft = (quality: number) => ({ output: { quality } }) as unknown as AppConfig;

describe("applySavedConfig", () => {
  it("takes the server reply when nothing changed during the save", () => {
    const sent = draft(80);
    mutateConfig(sent);
    const saved = draft(80);
    expect(applySavedConfig(sent, saved)).toBe(true);
    expect(config()).toBe(saved);
  });

  it("keeps an edit made while the save was in flight", () => {
    const sent = draft(80);
    mutateConfig(sent);
    const edited = draft(60);
    mutateConfig(edited);
    expect(applySavedConfig(sent, draft(80))).toBe(false);
    expect(config()).toBe(edited);
  });
});
