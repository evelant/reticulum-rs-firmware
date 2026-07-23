export async function uniffiInitAsync(): Promise<void> {}

export function nativeBridgeContract(): never {
  throw new Error("The Rust native bridge is unavailable in a web build");
}
