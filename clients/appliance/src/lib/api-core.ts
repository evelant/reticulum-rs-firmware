import type { ErrorBody } from "../generated/api.ts";

export class ApiError extends Error {
  readonly status: number;

  constructor(status: number, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
  }
}

export async function decodeSuccessResponse<T>(response: Response): Promise<T> {
  const body = await response.text();
  if (body.length === 0) return undefined as T;
  return JSON.parse(body) as T;
}

export function capabilityFromUrl(value: string, allowQuery = false): string | null {
  const url = new URL(value, "http://localhost");
  const fragment = new URLSearchParams(url.hash.startsWith("#") ? url.hash.slice(1) : url.hash);
  const capability = fragment.get("cap") ?? (allowQuery ? url.searchParams.get("cap") : null);
  return capability === null || capability === "" ? null : capability;
}

export async function apiError(response: Response): Promise<ApiError> {
  let detail = `${response.status} ${response.statusText}`;
  try {
    const error = (await response.json()) as ErrorBody;
    detail = error.error;
  } catch {
    // Preserve the HTTP status when the error body is not JSON.
  }
  return new ApiError(response.status, detail);
}
