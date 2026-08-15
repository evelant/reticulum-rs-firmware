/** Complete user-visible diagnostic file produced before platform delivery. */
export interface ExportArtifact {
  readonly contents: string;
  readonly filename: string;
  readonly mimeType: "application/json" | "text/csv";
}

/** Deliver one generated diagnostic file through the current platform. */
export async function deliverExportArtifact(artifact: ExportArtifact): Promise<void> {
  if (typeof document === "undefined") {
    throw new Error("Browser file downloads are unavailable on this platform");
  }

  const blob = new Blob([artifact.contents], { type: `${artifact.mimeType};charset=utf-8` });
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.download = artifact.filename;
  anchor.href = url;
  anchor.style.display = "none";
  document.body.append(anchor);
  try {
    anchor.click();
  } finally {
    anchor.remove();
    // Let the browser claim the object URL before releasing our reference.
    setTimeout(() => URL.revokeObjectURL(url), 0);
  }
}
