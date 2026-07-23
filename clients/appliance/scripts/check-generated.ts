import { readdir } from "node:fs/promises";
import { join } from "node:path";

import {
  assetDirectory,
  type GeneratedAssetName,
  type GeneratedAssets,
  generateAssets,
  generatedAssetNames,
} from "./generate-assets.ts";

function bytesEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  return left.every((value, index) => value === right[index]);
}

function assertEqualAssets(left: GeneratedAssets, right: GeneratedAssets, context: string): void {
  for (const name of generatedAssetNames) {
    const leftBytes = left.get(name);
    const rightBytes = right.get(name);
    if (leftBytes === undefined || rightBytes === undefined || !bytesEqual(leftBytes, rightBytes)) {
      throw new Error(`${context}: ${name} differs`);
    }
  }
}

const expected = await generateAssets();
const repeated = await generateAssets();
assertEqualAssets(expected, repeated, "Expo asset generation is not deterministic");

const observedNames = (await readdir(assetDirectory)).sort();
if (observedNames.join("\n") !== generatedAssetNames.join("\n")) {
  throw new Error(
    `generated asset set differs: expected ${generatedAssetNames.join(", ")}; ` +
      `observed ${observedNames.join(", ")}`,
  );
}

const observed = new Map<GeneratedAssetName, Uint8Array>();
for (const name of generatedAssetNames) {
  observed.set(name, new Uint8Array(await Bun.file(join(assetDirectory, name)).arrayBuffer()));
}
assertEqualAssets(expected, observed, "tracked embedded assets are stale");
console.log("tracked Expo web assets are deterministic and current");
