import { expect, it } from "vitest";
import fixtureText from "../../../src/overlay/fixtures/aspect.json?raw";
import { aspectTarget } from "./selection";

type Case = { width: number; height: number; snap: [number, number] | null };

it("snaps like the native selectors", () => {
  for (const { width, height, snap } of JSON.parse(fixtureText) as Case[]) {
    expect(aspectTarget(width, height), `${width}x${height}`).toBe(snap ? snap[0] / snap[1] : null);
  }
});
