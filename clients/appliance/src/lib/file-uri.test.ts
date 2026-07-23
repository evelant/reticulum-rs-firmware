import { describe, expect, test } from "bun:test";

import { nativePathFromFileUri } from "./file-uri.ts";

describe("Expo file URI conversion", () => {
  test("decodes an app-private absolute path without changing Unicode", () => {
    expect(
      nativePathFromFileUri(
        "file:///var/mobile/Containers/Data/Application/ABC/Documents/Reticulum%20%E2%9C%93.sqlite3",
      ),
    ).toBe("/var/mobile/Containers/Data/Application/ABC/Documents/Reticulum ✓.sqlite3");
  });

  test("rejects non-file schemes and remote file authorities", () => {
    expect(() => nativePathFromFileUri("content://documents/chat.sqlite3")).toThrow(
      "must use the file scheme",
    );
    expect(() => nativePathFromFileUri("file://example.test/chat.sqlite3")).toThrow(
      "must not contain a remote authority",
    );
  });

  test("rejects null bytes after percent decoding", () => {
    expect(() => nativePathFromFileUri("file:///tmp/chat%00.sqlite3")).toThrow(
      "must not contain a null byte",
    );
  });
});
