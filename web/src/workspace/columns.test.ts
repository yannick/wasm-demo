import { describe, expect, it } from "vitest";

import { template } from "./layout.js";

describe("workspace columns", () => {
  it("gives the chat the free space and the sheet its own width", () => {
    expect(template({ chat: true, sheet: true, side: true }, 720, 330)).toBe("minmax(400px, 1fr) auto minmax(360px, 720px) auto 330px");
  });

  it("lets the sheet fill when the chat is minimized", () => {
    expect(template({ chat: false, sheet: true, side: true }, 720, 330)).toBe("minmax(360px, 1fr) auto 330px");
  });

  it("lets a lone side panel fill the row", () => {
    expect(template({ chat: false, sheet: false, side: true }, 720, 330)).toBe("minmax(260px, 1fr)");
  });
});
