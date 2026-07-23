import * as Linking from "expo-linking";
import { Platform } from "react-native";

import type {
  ApplianceSnapshot,
  ContactRequest,
  ContactView,
  HttpApplianceSnapshot,
  MutationResponse,
  NoContent,
  OnboardingView,
  RecoveryRequest,
  SendRequest,
  SendResponse,
  SessionRequest,
  TimelineView,
} from "../generated/api.ts";
import { apiError, capabilityFromUrl, decodeSuccessResponse } from "./api-core.ts";
import type { ApplianceClient } from "./appliance-client.ts";
import { applianceSnapshotFromHttp } from "./http-projection.ts";

const CLIENT_HEADER = "X-Reticulum-Client";
const CLIENT_HEADER_VALUE = "web-alpha";

function normalizedOrigin(value: string | undefined): string {
  return value?.trim().replace(/\/$/, "") ?? "";
}

export function configuredApiOrigin(): string {
  if (Platform.OS === "web") return "";
  return normalizedOrigin(process.env.EXPO_PUBLIC_APPLIANCE_URL);
}

export class HttpApplianceClient implements ApplianceClient {
  readonly #origin: string;

  constructor(origin = configuredApiOrigin()) {
    this.#origin = normalizedOrigin(origin);
  }

  async bootstrapSession(): Promise<void> {
    const initialUrl =
      Platform.OS === "web" && typeof window !== "undefined"
        ? window.location.href
        : await Linking.getInitialURL();
    if (initialUrl === null) return;
    const capability = capabilityFromUrl(initialUrl, Platform.OS !== "web");
    if (capability === null) return;
    await this.#request<NoContent, SessionRequest>("/api/v1/session", {
      method: "POST",
      body: { capability },
    });
    if (Platform.OS === "web" && typeof window !== "undefined") {
      window.history.replaceState(null, "", `${window.location.pathname}${window.location.search}`);
    }
  }

  async snapshot(): Promise<ApplianceSnapshot> {
    return applianceSnapshotFromHttp(
      await this.#request<HttpApplianceSnapshot>("/api/v1/snapshot"),
    );
  }

  onboarding(): Promise<OnboardingView> {
    return this.#request("/api/v1/onboarding");
  }

  contacts(): Promise<ContactView[]> {
    return this.#request("/api/v1/contacts");
  }

  timeline(destination: string): Promise<TimelineView[]> {
    return this.#request(`/api/v1/conversations/${destination}`);
  }

  upsertContact(destination: string, request: ContactRequest): Promise<MutationResponse> {
    return this.#request(`/api/v1/contacts/${destination}`, {
      method: "PUT",
      body: request,
    });
  }

  send(request: SendRequest): Promise<SendResponse> {
    return this.#request("/api/v1/messages", { method: "POST", body: request });
  }

  startOnboarding(): Promise<NoContent> {
    return this.#request("/api/v1/onboarding/start", { method: "POST", body: {} });
  }

  refreshOnboarding(): Promise<NoContent> {
    return this.#request("/api/v1/onboarding/refresh", { method: "POST", body: {} });
  }

  recoverOnboarding(request: RecoveryRequest): Promise<NoContent> {
    return this.#request("/api/v1/onboarding/recover", { method: "POST", body: request });
  }

  sync(): Promise<NoContent> {
    return this.#request("/api/v1/sync", { method: "POST", body: {} });
  }

  reconnect(): Promise<NoContent> {
    return this.#request("/api/v1/reconnect", { method: "POST", body: {} });
  }

  subscribeInvalidations(onInvalidate: () => void, onError: () => void): (() => void) | null {
    if (Platform.OS !== "web" || typeof EventSource === "undefined") return null;
    const events = new EventSource(this.#url("/api/v1/events"));
    events.addEventListener("invalidate", onInvalidate);
    events.onerror = onError;
    return () => events.close();
  }

  dispose(): void {}

  #url(path: string): string {
    if (this.#origin.length === 0) return path;
    return `${this.#origin}${path}`;
  }

  async #request<ResponseBody, RequestBody = never>(
    path: string,
    options: { method?: "GET"; body?: never } | { method: "POST" | "PUT"; body: RequestBody } = {},
  ): Promise<ResponseBody> {
    if (Platform.OS !== "web" && this.#origin.length === 0) {
      throw new Error(
        "No appliance endpoint is configured. Set EXPO_PUBLIC_APPLIANCE_URL for the current HTTP prototype.",
      );
    }
    const headers = new Headers();
    let body: string | undefined;
    if ("body" in options && options.body !== undefined) {
      headers.set("Content-Type", "application/json");
      headers.set(CLIENT_HEADER, CLIENT_HEADER_VALUE);
      body = JSON.stringify(options.body);
    }
    const response = await fetch(this.#url(path), {
      method: options.method ?? "GET",
      headers,
      body,
      credentials: "include",
    });
    if (!response.ok) {
      throw await apiError(response);
    }
    return decodeSuccessResponse<ResponseBody>(response);
  }
}
