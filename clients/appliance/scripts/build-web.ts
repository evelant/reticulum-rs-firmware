import { mkdir, rm } from "node:fs/promises";
import { join } from "node:path";

import { assetDirectory, generateAssets } from "./generate-assets.ts";

const assets = await generateAssets();
await rm(assetDirectory, { force: true, recursive: true });
await mkdir(assetDirectory, { recursive: true });
for (const [name, bytes] of assets) {
  const path = join(assetDirectory, name);
  await Bun.write(path, bytes);
  console.log(`wrote ${path} (${bytes.byteLength} bytes)`);
}
