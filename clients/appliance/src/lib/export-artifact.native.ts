import { File, Paths } from "expo-file-system";
import * as Sharing from "expo-sharing";

import type { ExportArtifact } from "./export-artifact.ts";

function exportUti(mimeType: ExportArtifact["mimeType"]): string {
  return mimeType === "application/json" ? "public.json" : "public.comma-separated-values-text";
}

/** Write one temporary native file, share it, and remove it after the sheet closes. */
export async function deliverExportArtifact(artifact: ExportArtifact): Promise<void> {
  if (!(await Sharing.isAvailableAsync())) {
    throw new Error("File sharing is unavailable on this device");
  }

  const file = new File(Paths.cache, artifact.filename);
  file.create({ overwrite: true });
  try {
    file.write(artifact.contents);
    await Sharing.shareAsync(file.uri, {
      UTI: exportUti(artifact.mimeType),
      dialogTitle: "Export Reticulum RF trace",
      mimeType: artifact.mimeType,
    });
  } finally {
    if (file.exists) file.delete();
  }
}
