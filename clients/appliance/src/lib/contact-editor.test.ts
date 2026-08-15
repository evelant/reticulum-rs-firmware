import { describe, expect, test } from "bun:test";

import { contactSaveIntent } from "./contact-editor.ts";

describe("contact editor", () => {
  test("normalizes a newly entered destination and selects the saved contact", () => {
    expect(contactSaveIntent("Field relay", ` ${"AABB".repeat(8)} `, null)).toEqual({
      destination: "aabb".repeat(8),
      name: "Field relay",
      selectAfterSave: true,
    });
  });

  test("keeps the original destination and active conversation while renaming", () => {
    const existing = "12".repeat(16);

    expect(contactSaveIntent("Hilltop relay", "ff".repeat(16), existing)).toEqual({
      destination: existing,
      name: "Hilltop relay",
      selectAfterSave: false,
    });
  });

  test("preserves the user-entered name for the shared UTF-8 validation path", () => {
    expect(contactSaveIntent("  Relay A  ", "34".repeat(16), null).name).toBe("  Relay A  ");
  });
});
