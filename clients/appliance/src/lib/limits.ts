const encoder = new TextEncoder();

export function utf8ByteLength(value: string): number {
  return encoder.encode(value).byteLength;
}

export function byteLimitError(value: string, maximum: number, label: string): string | null {
  const bytes = utf8ByteLength(value);
  return bytes > maximum ? `${label} is ${bytes} bytes; the maximum is ${maximum}` : null;
}
