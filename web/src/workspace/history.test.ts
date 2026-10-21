import { describe, expect, it } from "vitest";

import { historyStep } from "./history.js";

describe("prompt history keys", () => {
  it("steps back from an empty prompt and forward to empty again", () => {
    expect(historyStep(-1, 1, 3, false, true)).toBe(0);
    expect(historyStep(0, 1, 3, true, false)).toBe(1);
    expect(historyStep(2, 1, 3, true, false)).toBeNull();
    expect(historyStep(0, -1, 3, true, false)).toBe(-1);
  });

  it("leaves the keys to the caret in a prompt being written or edited", () => {
    expect(historyStep(-1, 1, 3, false, false)).toBeNull();
    expect(historyStep(1, 1, 3, false, false)).toBeNull();
  });
});
