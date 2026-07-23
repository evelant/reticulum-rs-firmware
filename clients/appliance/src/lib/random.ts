import { getRandomBytes } from "expo-crypto";

export function randomHex(bytes: number): string {
  const data = getRandomBytes(bytes);
  return Array.from(data, (value) => value.toString(16).padStart(2, "0")).join("");
}
