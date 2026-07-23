const FILE_URI_PREFIX = "file://";

/**
 * Convert an app-private Expo `file:///…` URI into the absolute native path
 * expected by Rust's filesystem APIs.
 *
 * Expo owns selection of the platform-specific document directory. This
 * adapter deliberately rejects remote authorities and non-file schemes rather
 * than handing an ambiguous string to SQLite.
 */
export function nativePathFromFileUri(uri: string): string {
  if (!uri.startsWith(FILE_URI_PREFIX)) {
    throw new Error("native database URI must use the file scheme");
  }
  const encodedPath = uri.slice(FILE_URI_PREFIX.length);
  if (!encodedPath.startsWith("/")) {
    throw new Error("native database URI must not contain a remote authority");
  }
  const path = decodeURIComponent(encodedPath);
  if (path.includes("\0")) {
    throw new Error("native database path must not contain a null byte");
  }
  return path;
}
