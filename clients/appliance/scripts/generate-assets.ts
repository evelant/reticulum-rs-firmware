import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { extname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { assertExpectedBun, EXPECTED_BUN_REVISION, EXPECTED_BUN_VERSION } from "./toolchain.ts";

const clientDirectory = fileURLToPath(new URL("../", import.meta.url));
export const assetDirectory = resolve(clientDirectory, "../../crates/lxmf-chat-service/assets");

export const generatedAssetNames = ["app.js", "index.html", "manifest.json", "style.css"] as const;
export type GeneratedAssetName = (typeof generatedAssetNames)[number];
export type GeneratedAssets = ReadonlyMap<GeneratedAssetName, Uint8Array>;

const encoder = new TextEncoder();

interface ManifestAsset {
  readonly bytes: number;
  readonly path: Exclude<GeneratedAssetName, "manifest.json">;
  readonly sha256: string;
}

async function listFiles(root: string, directory = root): Promise<string[]> {
  const files: string[] = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await listFiles(root, path)));
    } else if (entry.isFile()) {
      files.push(relative(root, path).split(sep).join("/"));
    } else {
      throw new Error(`Expo export contains a non-file entry: ${path}`);
    }
  }
  return files.sort();
}

async function runExpoExport(outputDirectory: string): Promise<void> {
  const environment: Record<string, string> = {};
  for (const [name, value] of Object.entries(process.env)) {
    if (name !== "NO_COLOR" && value !== undefined) environment[name] = value;
  }
  environment.CI = "1";
  environment.FORCE_COLOR = "0";
  const processHandle = Bun.spawn({
    cmd: [
      process.execPath,
      "--bun",
      "expo",
      "export",
      "--platform",
      "web",
      "--output-dir",
      outputDirectory,
      "--clear",
    ],
    cwd: clientDirectory,
    env: environment,
    stderr: "inherit",
    stdout: "inherit",
  });
  const exitCode = await processHandle.exited;
  if (exitCode !== 0) throw new Error(`Expo web export failed with status ${exitCode}`);
}

function requireSingleMatch(value: string, pattern: RegExp, description: string): RegExpMatchArray {
  const matches = [...value.matchAll(pattern)];
  if (matches.length !== 1 || matches[0] === undefined) {
    throw new Error(`expected one ${description}, observed ${matches.length}`);
  }
  return matches[0];
}

interface InlinedBundle {
  readonly assetPaths: string[];
  readonly source: string;
}

async function inlineMetroAssets(bundle: string, exportDirectory: string): Promise<InlinedBundle> {
  const assetUrls = [
    ...new Set(bundle.match(/\/assets\/[A-Za-z0-9_./@-]+\.[A-Za-z0-9]+/g) ?? []),
  ].sort();
  let result = bundle;
  for (const url of assetUrls) {
    const extension = extname(url).toLowerCase();
    if (extension !== ".png") {
      throw new Error(`unsupported Metro runtime asset in web bundle: ${url}`);
    }
    const path = resolve(exportDirectory, `.${url}`);
    if (!path.startsWith(`${resolve(exportDirectory)}${sep}`)) {
      throw new Error(`Metro runtime asset escaped the export root: ${url}`);
    }
    const bytes = await readFile(path);
    const dataUrl = `data:image/png;base64,${bytes.toString("base64")}`;
    result = result.replaceAll(url, dataUrl);
  }
  if (/\/assets\//.test(result)) {
    throw new Error("Expo bundle contains an unhandled external asset URL");
  }
  return { assetPaths: assetUrls.map((url) => url.slice(1)), source: result };
}

function dedent(value: string): string {
  const lines = value.split("\n");
  while (lines[0]?.trim() === "") lines.shift();
  while (lines.at(-1)?.trim() === "") lines.pop();
  const indentation = Math.min(
    ...lines
      .filter((line) => line.trim() !== "")
      .map((line) => line.match(/^\s*/)?.[0].length ?? 0),
  );
  return `${lines.map((line) => line.slice(indentation)).join("\n")}\n`;
}

async function normalizeExport(
  exportDirectory: string,
): Promise<Map<Exclude<GeneratedAssetName, "manifest.json">, Uint8Array>> {
  const files = await listFiles(exportDirectory);
  const bundlePaths = files.filter((path) =>
    /^_expo\/static\/js\/web\/entry-[a-f0-9]+\.js$/.test(path),
  );
  if (bundlePaths.length !== 1 || bundlePaths[0] === undefined) {
    throw new Error(`expected one Metro web entry bundle, observed ${bundlePaths.length}`);
  }
  if (!files.includes("index.html") || !files.includes("metadata.json")) {
    throw new Error("Expo single-page export is missing index.html or metadata.json");
  }

  const sourceHtml = await Bun.file(join(exportDirectory, "index.html")).text();
  const styleMatch = requireSingleMatch(
    sourceHtml,
    /<style id="expo-reset">([\s\S]*?)<\/style>/g,
    "Expo reset stylesheet",
  );
  const scriptMatch = requireSingleMatch(
    sourceHtml,
    /<script src="([^"]+)" defer><\/script>/g,
    "Metro entry script",
  );
  if (scriptMatch[1] !== `/${bundlePaths[0]}`) {
    throw new Error(`index.html references unexpected Metro entry ${scriptMatch[1]}`);
  }
  if (
    (sourceHtml.match(/<script\b/g) ?? []).length !== 1 ||
    (sourceHtml.match(/<style\b/g) ?? []).length !== 1
  ) {
    throw new Error("Expo single-page shell contains an unexpected script or stylesheet");
  }
  const style = dedent(styleMatch[1] ?? "");
  const html = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1, shrink-to-fit=no">
    <meta name="theme-color" content="#101411">
    <title>Reticulum LXMF</title>
    <link rel="stylesheet" href="/style.css">
  </head>
  <body>
    <noscript>You need to enable JavaScript to run this app.</noscript>
    <div id="root"></div>
    <script src="/app.js" defer></script>
  </body>
</html>
`;
  if (/<style\b|<script(?! src="\/app\.js" defer><\/script>)/.test(html)) {
    throw new Error("normalized Expo shell contains inline executable or style content");
  }
  if (/\/(?:_expo|assets)\//.test(html)) {
    throw new Error("normalized Expo shell references an unserved export path");
  }

  const rawBundle = await Bun.file(join(exportDirectory, bundlePaths[0])).text();
  const bundle = await inlineMetroAssets(rawBundle, exportDirectory);
  const expectedFiles = new Set([
    "index.html",
    "metadata.json",
    bundlePaths[0],
    ...bundle.assetPaths,
  ]);
  const unexpectedFiles = files.filter(
    (path) =>
      !expectedFiles.has(path) &&
      !(/@[2-4]x\.png$/.test(path) && expectedFiles.has(path.replace(/@[2-4]x(?=\.png$)/, ""))),
  );
  const missingFiles = [...expectedFiles].filter((path) => !files.includes(path));
  if (unexpectedFiles.length > 0 || missingFiles.length > 0) {
    throw new Error(
      `Expo export file graph changed: unexpected [${unexpectedFiles.join(", ")}], ` +
        `missing [${missingFiles.join(", ")}]`,
    );
  }
  return new Map([
    ["app.js", encoder.encode(bundle.source)],
    ["index.html", encoder.encode(html)],
    ["style.css", encoder.encode(style)],
  ]);
}

function sha256(bytes: Uint8Array): string {
  return new Bun.CryptoHasher("sha256").update(bytes).digest("hex");
}

function manifestFor(
  assets: ReadonlyMap<Exclude<GeneratedAssetName, "manifest.json">, Uint8Array>,
): Uint8Array {
  const manifestAssets: ManifestAsset[] = [];
  for (const path of ["app.js", "index.html", "style.css"] as const) {
    const bytes = assets.get(path);
    if (bytes === undefined) throw new Error(`missing generated asset ${path}`);
    manifestAssets.push({ bytes: bytes.byteLength, path, sha256: sha256(bytes) });
  }
  return encoder.encode(
    `${JSON.stringify(
      {
        schema: 2,
        source: "clients/appliance",
        generator: {
          bun: { revision: EXPECTED_BUN_REVISION, version: EXPECTED_BUN_VERSION },
          expo: "57.0.8",
          expo_router: "57.0.8",
          mode: "single",
        },
        assets: manifestAssets,
      },
      null,
      2,
    )}\n`,
  );
}

function requireRuntimeAsset(
  assets: ReadonlyMap<Exclude<GeneratedAssetName, "manifest.json">, Uint8Array>,
  path: Exclude<GeneratedAssetName, "manifest.json">,
): Uint8Array {
  const bytes = assets.get(path);
  if (bytes === undefined) throw new Error(`missing generated asset ${path}`);
  return bytes;
}

export async function generateAssets(): Promise<GeneratedAssets> {
  assertExpectedBun();
  const temporaryDirectory = await mkdtemp(join(tmpdir(), "reticulum-appliance-expo-"));
  try {
    await runExpoExport(temporaryDirectory);
    const runtimeAssets = await normalizeExport(temporaryDirectory);
    return new Map<GeneratedAssetName, Uint8Array>([
      ["app.js", requireRuntimeAsset(runtimeAssets, "app.js")],
      ["index.html", requireRuntimeAsset(runtimeAssets, "index.html")],
      ["manifest.json", manifestFor(runtimeAssets)],
      ["style.css", requireRuntimeAsset(runtimeAssets, "style.css")],
    ]);
  } finally {
    await rm(temporaryDirectory, { force: true, recursive: true });
  }
}
