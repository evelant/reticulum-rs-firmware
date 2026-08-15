import { useState } from "react";
import {
  ActivityIndicator,
  Platform,
  Pressable,
  ScrollView,
  StyleSheet,
  Switch,
  Text,
  TextInput,
  View,
} from "react-native";

import {
  DEFAULT_RETICULUM_TCP_PORT,
  MAX_WIFI_NETWORK_PROFILES,
  type NetworkConfigMutation,
  type NetworkConfigMutationOutcome,
  type ReticulumTcpPeerView,
  type RmapPhoneLocation,
  type WifiNetworkProfileView,
} from "../generated/api.ts";
import type {
  NetworkConfigController,
  NetworkConfigControllerState,
} from "../lib/network-config.ts";
import {
  networkBytesText,
  networkSsidInput,
  tcpPeerHostnameInputError,
  tcpPeerInputError,
  wifiPassphraseError,
} from "../lib/network-config-input.ts";
import { captureForegroundPhoneLocation } from "../lib/phone-location.ts";
import {
  isPublicReticulumEndpointSelected,
  PUBLIC_RETICULUM_TCP_ENDPOINTS,
} from "../lib/public-reticulum-endpoints.ts";
import type { RadioRoutesControllerState } from "../lib/radio-routes.ts";
import { randomHex } from "../lib/random.ts";
import { rmapRuntimePresentation } from "../lib/rmap-runtime-diagnostics.ts";
import {
  reticulumDnsDiagnosticDetails,
  reticulumTcpDiagnostic,
  reticulumTcpStateLabel,
} from "../lib/tcp-runtime-diagnostics.ts";
import { LoraProfileEditor } from "./LoraProfileEditor.tsx";
import { RadioRoutesPanel } from "./RadioRoutesPanel.tsx";

interface ConnectivityPanelProps {
  readonly announceNow?: () => Promise<"already_pending" | "queued">;
  readonly controller: NetworkConfigController;
  readonly onRefreshRadioRoutes?: () => void;
  readonly radioRoutesState?: RadioRoutesControllerState | null;
  readonly state: NetworkConfigControllerState;
}

interface WifiEditor {
  readonly credentialConfigured: boolean;
  enabled: boolean;
  passphrase: string;
  readonly profileId: string;
  priority: string;
  ssid: string;
}

interface TcpEditor {
  address: string;
  enabled: boolean;
  kind: "hostname" | "ipv4";
  port: string;
}

type AnnounceState =
  | { readonly state: "idle" }
  | { readonly state: "running" }
  | { readonly message: string; readonly state: "success" }
  | { readonly message: string; readonly state: "error" };

interface SmallButtonProps {
  readonly destructive?: boolean;
  readonly disabled?: boolean;
  readonly label: string;
  readonly onPress: () => void;
  readonly primary?: boolean;
}

function SmallButton({
  destructive = false,
  disabled = false,
  label,
  onPress,
  primary = false,
}: SmallButtonProps) {
  return (
    <Pressable
      accessibilityRole="button"
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.button,
        primary && styles.buttonPrimary,
        destructive && styles.buttonDestructive,
        disabled && styles.buttonDisabled,
        pressed && !disabled && styles.buttonPressed,
      ]}
    >
      <Text
        style={[
          styles.buttonText,
          primary && styles.buttonPrimaryText,
          destructive && styles.buttonDestructiveText,
        ]}
      >
        {label}
      </Text>
    </Pressable>
  );
}

function wifiStateLabel(state: NetworkConfigControllerState["runtime"]): string {
  if (state === null) return "Unknown";
  switch (state.wifi_state) {
    case "disabled":
      return "Disabled";
    case "disconnected":
      return "Disconnected";
    case "connecting":
      return "Connecting";
    case "connected":
      return "Connected";
  }
}

function profileEditor(profile: WifiNetworkProfileView): WifiEditor {
  return {
    credentialConfigured: profile.credential_configured,
    enabled: profile.enabled,
    passphrase: "",
    priority: String(profile.priority),
    profileId: profile.profile_id,
    ssid: networkBytesText(profile.ssid),
  };
}

function newProfileEditor(): WifiEditor {
  return {
    credentialConfigured: false,
    enabled: true,
    passphrase: "",
    priority: "128",
    profileId: randomHex(16),
    ssid: "",
  };
}

function tcpPeerAddress(peer: ReticulumTcpPeerView): string {
  return "hostname" in peer ? peer.hostname : peer.ipv4_address;
}

function tcpEditor(peer: ReticulumTcpPeerView | null): TcpEditor {
  return {
    address: peer === null ? "" : tcpPeerAddress(peer),
    enabled: peer?.enabled ?? true,
    kind: peer !== null && "ipv4_address" in peer ? "ipv4" : "hostname",
    port: String(peer?.port ?? DEFAULT_RETICULUM_TCP_PORT),
  };
}

function rmapLocationLabel(location: RmapPhoneLocation): string {
  return `${(location.latitude_e6 / 1_000_000).toFixed(4)}, ${(location.longitude_e6 / 1_000_000).toFixed(4)}`;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function mutationNotice(
  controller: NetworkConfigController,
  state: NetworkConfigControllerState,
  disabled: boolean,
) {
  switch (state.mutation.state) {
    case "idle":
      return null;
    case "running":
      return (
        <View accessibilityLiveRegion="polite" style={styles.notice}>
          <ActivityIndicator color={colors.green} />
          <Text style={styles.noticeText}>Saving configuration…</Text>
        </View>
      );
    case "retryable_error":
      return (
        <View accessibilityLiveRegion="assertive" style={[styles.notice, styles.noticeError]}>
          <Text style={styles.errorText}>
            The result is uncertain: {state.mutation.error}. Retry replays the exact same change.
          </Text>
          <View style={styles.actionRow}>
            <SmallButton
              disabled={disabled}
              label="Retry exact change"
              onPress={() => void controller.retryMutation()}
              primary
            />
            <SmallButton
              disabled={disabled}
              label="Discard retry"
              onPress={() => controller.abandonMutationRetry()}
            />
          </View>
        </View>
      );
    case "revision_conflict":
      return (
        <View accessibilityLiveRegion="polite" style={styles.notice}>
          <Text style={styles.noticeText}>
            The board changed at revision {state.mutation.currentRevision}. Its current
            configuration has been reloaded; review and save again.
          </Text>
          <SmallButton label="Dismiss" onPress={() => controller.clearMutationNotice()} />
        </View>
      );
    case "applied":
      return (
        <View accessibilityLiveRegion="polite" style={[styles.notice, styles.noticeSuccess]}>
          <Text style={styles.successText}>
            Saved as revision {state.mutation.revision}
            {state.mutation.rebootRequired ? "; restart required to apply" : ""}.
          </Text>
          <SmallButton label="Dismiss" onPress={() => controller.clearMutationNotice()} />
        </View>
      );
    case "error":
      return (
        <View accessibilityLiveRegion="assertive" style={[styles.notice, styles.noticeError]}>
          <Text style={styles.errorText}>{state.mutation.error}</Text>
          <SmallButton label="Dismiss" onPress={() => controller.clearMutationNotice()} />
        </View>
      );
  }
}

export function ConnectivityPanel({
  announceNow,
  controller,
  onRefreshRadioRoutes,
  radioRoutesState = null,
  state,
}: ConnectivityPanelProps) {
  const [announceState, setAnnounceState] = useState<AnnounceState>({ state: "idle" });
  const [formError, setFormError] = useState<string | null>(null);
  const [locationCaptureRunning, setLocationCaptureRunning] = useState(false);
  const [tcpForm, setTcpForm] = useState<TcpEditor | null>(null);
  const [wifiForm, setWifiForm] = useState<WifiEditor | null>(null);
  const mutating = state.mutation.state === "running";
  const configuration = state.configuration;
  const tcpDiagnostic = reticulumTcpDiagnostic(state.runtime);
  const dnsDetails = reticulumDnsDiagnosticDetails(state.runtime);
  const rmapRuntime = rmapRuntimePresentation(
    state.runtime,
    configuration?.rmap_discovery_enabled ?? false,
  );

  const saveWifi = async () => {
    if (wifiForm === null) return;
    const priority = Number(wifiForm.priority);
    if (!Number.isInteger(priority) || priority < 0 || priority > 255) {
      setFormError("Wi-Fi priority must be a whole number from 0 to 255");
      return;
    }
    const ssid = networkSsidInput(wifiForm.ssid);
    if ("error" in ssid) {
      setFormError(ssid.error);
      return;
    }
    if (wifiForm.passphrase.length === 0 && !wifiForm.credentialConfigured) {
      setFormError("A password is required for a new WPA2 network");
      return;
    }
    if (wifiForm.passphrase.length > 0) {
      const passwordError = wifiPassphraseError(wifiForm.passphrase);
      if (passwordError !== null) {
        setFormError(passwordError);
        return;
      }
    }

    setFormError(null);
    const mutation: NetworkConfigMutation = {
      credential:
        wifiForm.passphrase.length === 0
          ? { kind: "keep" }
          : { kind: "replace", passphrase: wifiForm.passphrase },
      enabled: wifiForm.enabled,
      kind: "upsert_wifi",
      priority,
      profile_id: wifiForm.profileId,
      ssid: ssid.value,
    };
    let outcome: NetworkConfigMutationOutcome | null = null;
    try {
      outcome = await controller.mutate(mutation);
    } finally {
      setWifiForm((form) => (form === null ? null : { ...form, passphrase: "" }));
    }
    if (outcome?.outcome === "applied") setWifiForm(null);
  };

  const removeWifi = async (profileId: string) => {
    setFormError(null);
    const outcome = await controller.mutate({ kind: "remove_wifi", profile_id: profileId });
    if (outcome?.outcome === "applied" && wifiForm?.profileId === profileId) setWifiForm(null);
  };

  const saveTcpPeer = async () => {
    if (tcpForm === null) return;
    const error =
      tcpForm.kind === "hostname"
        ? tcpPeerHostnameInputError(tcpForm.address, tcpForm.port)
        : tcpPeerInputError(tcpForm.address, tcpForm.port);
    if (error !== null) {
      setFormError(error);
      return;
    }
    setFormError(null);
    const outcome =
      tcpForm.kind === "hostname"
        ? await controller.mutate({
            kind: "replace_tcp_host_peer",
            peer: {
              enabled: tcpForm.enabled,
              hostname: tcpForm.address,
              port: Number(tcpForm.port),
            },
          })
        : await controller.mutate({
            kind: "replace_tcp_peer",
            peer: {
              enabled: tcpForm.enabled,
              ipv4_address: tcpForm.address,
              port: Number(tcpForm.port),
            },
          });
    if (outcome?.outcome === "applied") setTcpForm(null);
  };

  const clearTcpPeer = async () => {
    setFormError(null);
    const outcome = await controller.mutate({
      kind:
        configuration?.tcp_peer !== null &&
        configuration?.tcp_peer !== undefined &&
        "hostname" in configuration.tcp_peer
          ? "replace_tcp_host_peer"
          : "replace_tcp_peer",
      peer: null,
    });
    if (outcome?.outcome === "applied") setTcpForm(null);
  };

  const applyGatewayPolicy = async (
    wifiTransportEnabled: boolean,
    automaticAnnouncesEnabled: boolean,
  ) => {
    setFormError(null);
    await controller.mutate({
      automatic_announces_enabled: automaticAnnouncesEnabled,
      kind: "set_gateway_policy",
      wifi_transport_enabled: wifiTransportEnabled,
    });
  };

  const applyRmapConfig = async (
    discoveryEnabled: boolean,
    shareLocation: boolean,
    phoneLocation: RmapPhoneLocation | null,
  ) => {
    setFormError(null);
    await controller.mutate({
      discovery_enabled: discoveryEnabled,
      kind: "set_rmap_config",
      phone_location: phoneLocation,
      share_location: shareLocation,
    });
  };

  const selectPublicEndpoint = async (
    endpoint: (typeof PUBLIC_RETICULUM_TCP_ENDPOINTS)[number],
  ) => {
    setFormError(null);
    await controller.mutate({
      kind: "replace_tcp_host_peer",
      peer: {
        enabled: true,
        hostname: endpoint.hostname,
        port: endpoint.port,
      },
    });
  };

  const captureLocation = async () => {
    if (configuration === null || locationCaptureRunning || mutating) return;
    setFormError(null);
    setLocationCaptureRunning(true);
    try {
      const location = await captureForegroundPhoneLocation({
        accuracy: "balanced",
        precision: "approximately_100m",
      });
      await applyRmapConfig(configuration.rmap_discovery_enabled, true, {
        latitude_e6: location.latitude_e6,
        longitude_e6: location.longitude_e6,
      });
    } catch (error) {
      setFormError(`Phone location was not saved: ${errorText(error)}`);
    } finally {
      setLocationCaptureRunning(false);
    }
  };

  const runAnnounce = async () => {
    if (announceNow === undefined || announceState.state === "running") return;
    setAnnounceState({ state: "running" });
    try {
      const disposition = await announceNow();
      setAnnounceState({
        message:
          disposition === "queued"
            ? "Service announce queued."
            : "An announce cycle is already pending.",
        state: "success",
      });
    } catch (error) {
      setAnnounceState({ message: `Announce failed: ${errorText(error)}`, state: "error" });
    }
  };

  return (
    <ScrollView
      contentContainerStyle={styles.content}
      keyboardDismissMode={Platform.OS === "ios" ? "interactive" : "on-drag"}
      keyboardShouldPersistTaps="handled"
      style={styles.scroller}
    >
      <View style={styles.heading}>
        <View style={styles.headingCopy}>
          <Text style={styles.eyebrow}>CONNECTIVITY</Text>
          <Text style={styles.title}>Gateway and discovery</Text>
          <Text style={styles.secondary}>
            Bridge LoRa over one optional Wi-Fi/IP peer, or remain an ad-hoc radio-only node.
          </Text>
        </View>
        <SmallButton
          disabled={mutating || state.loadState === "loading"}
          label="Refresh"
          onPress={() => void controller.refresh()}
        />
      </View>

      {state.rebootRequired ? (
        <View accessibilityLiveRegion="polite" style={styles.rebootBanner}>
          <Text style={styles.rebootTitle}>Restart appliance to apply changes</Text>
          <Text style={styles.rebootText}>
            Configured revision{" "}
            {state.runtime?.configured_revision ?? configuration?.revision ?? "—"} · applied
            revision {state.runtime?.applied_revision ?? "—"}
          </Text>
        </View>
      ) : null}

      {mutationNotice(controller, state, mutating)}
      {formError === null ? null : (
        <View accessibilityLiveRegion="assertive" style={[styles.notice, styles.noticeError]}>
          <Text style={styles.errorText}>{formError}</Text>
        </View>
      )}
      {state.loadError === null ? null : (
        <View accessibilityLiveRegion="assertive" style={[styles.notice, styles.noticeError]}>
          <Text style={styles.errorText}>Configuration could not be read: {state.loadError}</Text>
        </View>
      )}
      {state.statusError === null ? null : (
        <Text accessibilityLiveRegion="polite" style={styles.inlineError}>
          Live status unavailable: {state.statusError}
        </Text>
      )}

      {state.loadState === "loading" && configuration === null ? (
        <View style={styles.loading}>
          <ActivityIndicator color={colors.green} />
          <Text style={styles.secondary}>Reading board-owned configuration…</Text>
        </View>
      ) : null}

      <View style={styles.statusGrid}>
        <View style={styles.statusCard}>
          <Text style={styles.cardEyebrow}>WI-FI STATION</Text>
          <Text style={styles.statusValue}>{wifiStateLabel(state.runtime)}</Text>
          <Text style={styles.meta}>
            {state.runtime?.connected_ssid === null || state.runtime?.connected_ssid === undefined
              ? "No associated SSID"
              : networkBytesText(state.runtime.connected_ssid)}
          </Text>
          <Text style={styles.meta}>
            {state.runtime?.ipv4_address ?? "No DHCP address"}
            {state.runtime?.rssi_dbm === null || state.runtime?.rssi_dbm === undefined
              ? ""
              : ` · ${state.runtime.rssi_dbm} dBm`}
          </Text>
          {configuration === null ? null : (
            <View style={styles.statusCardControl}>
              <View style={styles.switchCopy}>
                <Text style={styles.label}>Wi-Fi radio after restart</Text>
                <Text style={styles.help}>
                  Turn off Wi-Fi and TCP without deleting saved networks or the peer.
                </Text>
              </View>
              <Switch
                accessibilityLabel="Wi-Fi radio after restart"
                disabled={mutating}
                onValueChange={(enabled) =>
                  void applyGatewayPolicy(enabled, configuration.automatic_announces_enabled)
                }
                trackColor={{ false: colors.line, true: colors.greenDark }}
                value={configuration.wifi_transport_enabled}
              />
            </View>
          )}
        </View>
        <View style={styles.statusCard}>
          <Text style={styles.cardEyebrow}>RETICULUM TCP</Text>
          <Text style={styles.statusValue}>
            {reticulumTcpStateLabel(state.runtime?.tcp_peer_state ?? null)}
          </Text>
          <Text style={styles.meta}>
            {configuration?.tcp_peer === null || configuration?.tcp_peer === undefined
              ? "No outbound peer"
              : `${tcpPeerAddress(configuration.tcp_peer)}:${configuration.tcp_peer.port}`}
          </Text>
          <Text style={styles.meta}>
            Desired revision {configuration?.revision ?? "—"} · active{" "}
            {state.runtime?.applied_revision ?? "—"}
          </Text>
          {tcpDiagnostic === null ? null : (
            <Text
              accessibilityLiveRegion={
                state.runtime?.tcp_peer_state === "backoff" ? "polite" : "none"
              }
              style={styles.inlineError}
            >
              {tcpDiagnostic}
            </Text>
          )}
          {dnsDetails === null ? null : (
            <View style={styles.dnsDetails}>
              <Text style={styles.dnsHeading}>DNS RESOLUTION</Text>
              {dnsDetails.rows.map((row) => (
                <View key={row.key} style={styles.dnsRow}>
                  <Text selectable style={styles.dnsResolver}>
                    {row.label}
                  </Text>
                  <Text
                    style={[
                      styles.dnsOutcome,
                      row.tone === "failure" && styles.dnsOutcomeFailure,
                      row.tone === "success" && styles.dnsOutcomeSuccess,
                    ]}
                  >
                    {row.outcome}
                  </Text>
                </View>
              ))}
              <Text selectable style={styles.dnsContext}>
                {dnsDetails.context}
              </Text>
              {dnsDetails.resolution === null ? null : (
                <Text selectable style={styles.dnsResolution}>
                  {dnsDetails.resolution}
                </Text>
              )}
            </View>
          )}
        </View>
      </View>

      {configuration === null ? null : (
        <LoraProfileEditor
          desiredProfile={configuration.lora_profile}
          disabled={mutating}
          key={state.deviceKey ?? "inactive"}
          onSave={(profile) => controller.mutate({ kind: "set_lora_profile", profile })}
          rebootRequired={state.rebootRequired}
          runningProfile={radioRoutesState?.snapshot?.lora ?? null}
        />
      )}

      {radioRoutesState === null || onRefreshRadioRoutes === undefined ? null : (
        <RadioRoutesPanel
          disabled={mutating}
          onRefresh={onRefreshRadioRoutes}
          state={radioRoutesState}
        />
      )}

      {configuration === null ? null : (
        <>
          <View style={styles.section}>
            <View style={styles.sectionHeading}>
              <View style={styles.sectionHeadingCopy}>
                <Text style={styles.sectionTitle}>Service announces</Text>
                <Text style={styles.secondary}>
                  Manual announces always remain available. Changes apply after an appliance
                  restart.
                </Text>
              </View>
              <SmallButton
                disabled={
                  announceNow === undefined ||
                  announceState.state === "running" ||
                  state.loadState !== "ready"
                }
                label={announceState.state === "running" ? "Queuing…" : "Announce now"}
                onPress={() => void runAnnounce()}
                primary
              />
            </View>
            {announceState.state === "success" || announceState.state === "error" ? (
              <Text
                accessibilityLiveRegion={announceState.state === "error" ? "assertive" : "polite"}
                style={announceState.state === "error" ? styles.inlineError : styles.inlineSuccess}
              >
                {announceState.message}
              </Text>
            ) : null}
            {announceNow === undefined ? (
              <Text style={styles.help}>
                This client build does not expose the appliance&apos;s manual announce operation.
              </Text>
            ) : null}
            <View style={styles.compactSwitchList}>
              <View style={styles.switchRow}>
                <View style={styles.switchCopy}>
                  <Text style={styles.label}>Automatic service announces</Text>
                  <Text style={styles.help}>
                    Periodically announce this appliance&apos;s primary, LXMF, and NomadNet
                    destinations. RMAP publication is controlled separately.
                  </Text>
                </View>
                <Switch
                  accessibilityLabel="Automatic service announces enabled"
                  disabled={mutating}
                  onValueChange={(enabled) =>
                    void applyGatewayPolicy(configuration.wifi_transport_enabled, enabled)
                  }
                  trackColor={{ false: colors.line, true: colors.greenDark }}
                  value={configuration.automatic_announces_enabled}
                />
              </View>
            </View>
          </View>

          <View style={styles.section}>
            <View style={styles.sectionHeadingCopy}>
              <Text style={styles.sectionTitle}>Public Reticulum IP peer</Text>
              <Text style={styles.secondary}>
                Choose one community TCP endpoint. This replaces the current peer; hostnames are
                resolved again on reconnect.
              </Text>
            </View>
            <View style={styles.endpointList}>
              {PUBLIC_RETICULUM_TCP_ENDPOINTS.map((endpoint) => {
                const active = isPublicReticulumEndpointSelected(configuration.tcp_peer, endpoint);
                return (
                  <View key={endpoint.id} style={styles.endpointRow}>
                    <View style={styles.endpointCopy}>
                      <Text style={styles.endpointTitle}>{endpoint.label}</Text>
                      <Text selectable style={styles.meta}>
                        {endpoint.hostname}:{endpoint.port}
                      </Text>
                      <Text selectable style={styles.endpointIdentity}>
                        ID seen {endpoint.expectedTransportId} · verified {endpoint.verifiedOn}
                      </Text>
                    </View>
                    <SmallButton
                      disabled={mutating || active}
                      label={active ? "Selected" : "Use"}
                      onPress={() => void selectPublicEndpoint(endpoint)}
                      primary={!active}
                    />
                  </View>
                );
              })}
            </View>
            <Text style={styles.privacyText}>
              Public operators are untrusted carriers: Reticulum protects message contents, but an
              operator can observe this appliance&apos;s IP address, timing and traffic volume, or
              drop traffic. Advertised transport IDs above are diagnostics, not security pins.
            </Text>
          </View>

          <View style={styles.section}>
            <View style={styles.sectionHeadingCopy}>
              <Text style={styles.sectionTitle}>RMAP World discovery</Text>
              <Text style={styles.secondary}>
                Opt in to a signed public marker for this appliance&apos;s LoRa interface. RMAP
                targets the configured public TCP interface once it is ready, or available Reticulum
                interfaces on a radio-only node. Its six-hour cadence begins only after a
                publication is accepted.
              </Text>
            </View>
            <View accessibilityLiveRegion="polite" style={styles.rmapStatus}>
              <Text style={styles.rmapStatusEyebrow}>CURRENT STATUS</Text>
              <Text
                style={[
                  styles.rmapStatusHeadline,
                  rmapRuntime.tone === "error" && styles.rmapStatusError,
                  rmapRuntime.tone === "success" && styles.rmapStatusSuccess,
                  rmapRuntime.tone === "warning" && styles.rmapStatusWarning,
                ]}
              >
                {rmapRuntime.headline}
              </Text>
              {rmapRuntime.rows.map((row) => (
                <Text key={row} selectable style={styles.rmapStatusRow}>
                  {row}
                </Text>
              ))}
            </View>
            <View style={styles.compactSwitchList}>
              <View style={styles.switchRow}>
                <View style={styles.switchCopy}>
                  <Text style={styles.label}>Publish on RMAP World</Text>
                  <Text style={styles.help}>Name and LoRa parameters become public.</Text>
                </View>
                <Switch
                  accessibilityLabel="RMAP World discovery enabled"
                  disabled={mutating || locationCaptureRunning}
                  onValueChange={(enabled) =>
                    void applyRmapConfig(
                      enabled,
                      configuration.rmap_share_location,
                      configuration.rmap_phone_location,
                    )
                  }
                  trackColor={{ false: colors.line, true: colors.greenDark }}
                  value={configuration.rmap_discovery_enabled}
                />
              </View>
              {configuration.rmap_phone_location === null ? null : (
                <View style={styles.switchRow}>
                  <View style={styles.switchCopy}>
                    <Text style={styles.label}>Include saved phone location</Text>
                    <Text selectable style={styles.help}>
                      Approximate position {rmapLocationLabel(configuration.rmap_phone_location)}
                    </Text>
                  </View>
                  <Switch
                    accessibilityLabel="Share saved phone location on RMAP World"
                    disabled={mutating || locationCaptureRunning}
                    onValueChange={(enabled) =>
                      void applyRmapConfig(
                        configuration.rmap_discovery_enabled,
                        enabled,
                        configuration.rmap_phone_location,
                      )
                    }
                    trackColor={{ false: colors.line, true: colors.greenDark }}
                    value={configuration.rmap_share_location}
                  />
                </View>
              )}
            </View>
            <View style={styles.actionRow}>
              <SmallButton
                disabled={mutating || locationCaptureRunning}
                label={
                  locationCaptureRunning
                    ? "Getting location…"
                    : configuration.rmap_phone_location === null
                      ? "Use phone location (~100 m)"
                      : "Update phone location"
                }
                onPress={() => void captureLocation()}
                primary={configuration.rmap_phone_location === null}
              />
              {configuration.rmap_phone_location === null ? null : (
                <SmallButton
                  disabled={mutating || locationCaptureRunning}
                  label="Remove location"
                  onPress={() =>
                    void applyRmapConfig(configuration.rmap_discovery_enabled, false, null)
                  }
                />
              )}
            </View>
            <Text style={styles.privacyText}>
              Location is requested once in the foreground, rounded to roughly 100 metres, and
              stored on the appliance; there is no background tracking. Published name, radio
              details and location can remain visible on RMAP World for up to seven days after
              sharing is disabled.
            </Text>
          </View>

          <View style={styles.section}>
            <View style={styles.sectionHeading}>
              <View>
                <Text style={styles.sectionTitle}>Known Wi-Fi networks</Text>
                <Text style={styles.secondary}>
                  {configuration.wifi_profiles.length}/{MAX_WIFI_NETWORK_PROFILES} saved · larger
                  priority values are preferred
                </Text>
              </View>
              <SmallButton
                disabled={
                  mutating || configuration.wifi_profiles.length >= MAX_WIFI_NETWORK_PROFILES
                }
                label="Add network"
                onPress={() => {
                  setFormError(null);
                  setWifiForm(newProfileEditor());
                }}
                primary
              />
            </View>

            {configuration.wifi_profiles.length === 0 ? (
              <Text style={styles.emptyText}>No Wi-Fi networks are saved on this appliance.</Text>
            ) : (
              <View style={styles.list}>
                {configuration.wifi_profiles.map((profile) => {
                  const active = state.runtime?.active_wifi_profile === profile.profile_id;
                  return (
                    <View
                      key={profile.profile_id}
                      style={[styles.profileCard, active && styles.profileCardActive]}
                    >
                      <View style={styles.profileHeading}>
                        <View style={styles.profileCopy}>
                          <Text selectable style={styles.profileSsid}>
                            {networkBytesText(profile.ssid)}
                          </Text>
                          <Text style={styles.meta}>
                            Priority {profile.priority} · {profile.enabled ? "enabled" : "disabled"}{" "}
                            · {profile.credential_configured ? "password saved" : "no password"}
                          </Text>
                          <Text selectable style={styles.profileId}>
                            {profile.profile_id}
                          </Text>
                        </View>
                        {active ? <Text style={styles.activeBadge}>ACTIVE</Text> : null}
                      </View>
                      <View style={styles.actionRow}>
                        <SmallButton
                          disabled={mutating}
                          label="Edit"
                          onPress={() => {
                            setFormError(null);
                            setWifiForm(profileEditor(profile));
                          }}
                        />
                        <SmallButton
                          destructive
                          disabled={mutating}
                          label="Remove"
                          onPress={() => void removeWifi(profile.profile_id)}
                        />
                      </View>
                    </View>
                  );
                })}
              </View>
            )}

            {wifiForm === null ? null : (
              <View style={styles.editor}>
                <Text style={styles.editorTitle}>
                  {wifiForm.credentialConfigured ? "Edit Wi-Fi network" : "Add Wi-Fi network"}
                </Text>
                <Text style={styles.label}>SSID</Text>
                <TextInput
                  accessibilityLabel="Wi-Fi SSID"
                  autoCapitalize="none"
                  autoCorrect={false}
                  editable={!mutating}
                  onChangeText={(ssid) => setWifiForm({ ...wifiForm, ssid })}
                  placeholder="Network name or hex:…"
                  placeholderTextColor={colors.muted}
                  style={styles.input}
                  value={wifiForm.ssid}
                />
                <Text style={styles.help}>
                  UTF-8 text is used as entered. Existing binary SSIDs remain exact as hex:…
                </Text>
                <Text style={styles.label}>Priority (larger wins)</Text>
                <TextInput
                  accessibilityLabel="Wi-Fi priority"
                  editable={!mutating}
                  keyboardType="number-pad"
                  onChangeText={(priority) => setWifiForm({ ...wifiForm, priority })}
                  style={styles.input}
                  value={wifiForm.priority}
                />
                <View style={styles.switchRow}>
                  <View style={styles.switchCopy}>
                    <Text style={styles.label}>Enabled</Text>
                    <Text style={styles.help}>Allow station selection after restart.</Text>
                  </View>
                  <Switch
                    accessibilityLabel="Wi-Fi network enabled"
                    disabled={mutating}
                    onValueChange={(enabled) => setWifiForm({ ...wifiForm, enabled })}
                    trackColor={{ false: colors.line, true: colors.greenDark }}
                    value={wifiForm.enabled}
                  />
                </View>
                <Text style={styles.label}>WPA2 password</Text>
                <TextInput
                  accessibilityLabel="Wi-Fi password"
                  autoCapitalize="none"
                  autoCorrect={false}
                  editable={!mutating}
                  onChangeText={(passphrase) => setWifiForm({ ...wifiForm, passphrase })}
                  placeholder={
                    wifiForm.credentialConfigured
                      ? "Leave blank to keep saved password"
                      : "Required"
                  }
                  placeholderTextColor={colors.muted}
                  secureTextEntry
                  style={styles.input}
                  value={wifiForm.passphrase}
                />
                <Text style={styles.help}>
                  The password is sent once to this board and is never stored by the app.
                </Text>
                <View style={styles.actionRow}>
                  <SmallButton
                    disabled={mutating}
                    label="Save network"
                    onPress={() => void saveWifi()}
                    primary
                  />
                  <SmallButton
                    disabled={mutating}
                    label="Cancel"
                    onPress={() => {
                      setFormError(null);
                      setWifiForm(null);
                    }}
                  />
                </View>
              </View>
            )}
          </View>

          <View style={styles.section}>
            <View style={styles.sectionHeading}>
              <View style={styles.sectionHeadingCopy}>
                <Text style={styles.sectionTitle}>Custom outbound TCP peer</Text>
                <Text style={styles.secondary}>
                  Exactly one hostname or IPv4 peer can be active. Saving replaces any preset.
                </Text>
              </View>
              <SmallButton
                disabled={mutating}
                label={configuration.tcp_peer === null ? "Configure" : "Edit"}
                onPress={() => {
                  setFormError(null);
                  setTcpForm(tcpEditor(configuration.tcp_peer));
                }}
                primary
              />
            </View>

            {configuration.tcp_peer === null ? (
              <Text style={styles.emptyText}>No TCP peer is configured.</Text>
            ) : (
              <View style={styles.profileCard}>
                <Text selectable style={styles.profileSsid}>
                  {tcpPeerAddress(configuration.tcp_peer)}:{configuration.tcp_peer.port}
                </Text>
                <Text style={styles.meta}>
                  {"hostname" in configuration.tcp_peer ? "Hostname" : "IPv4"} ·{" "}
                  {configuration.tcp_peer.enabled ? "enabled" : "disabled"}
                </Text>
                <SmallButton
                  destructive
                  disabled={mutating}
                  label="Clear peer"
                  onPress={() => void clearTcpPeer()}
                />
              </View>
            )}

            {tcpForm === null ? null : (
              <View style={styles.editor}>
                <Text style={styles.editorTitle}>Configure outbound peer</Text>
                <View style={styles.segmentedRow}>
                  <SmallButton
                    disabled={mutating}
                    label="Hostname"
                    onPress={() => setTcpForm({ ...tcpForm, address: "", kind: "hostname" })}
                    primary={tcpForm.kind === "hostname"}
                  />
                  <SmallButton
                    disabled={mutating}
                    label="IPv4"
                    onPress={() => setTcpForm({ ...tcpForm, address: "", kind: "ipv4" })}
                    primary={tcpForm.kind === "ipv4"}
                  />
                </View>
                <Text style={styles.label}>
                  {tcpForm.kind === "hostname" ? "DNS hostname" : "IPv4 address"}
                </Text>
                <TextInput
                  accessibilityLabel={
                    tcpForm.kind === "hostname" ? "TCP peer DNS hostname" : "TCP peer IPv4 address"
                  }
                  autoCapitalize="none"
                  autoCorrect={false}
                  editable={!mutating}
                  keyboardType={tcpForm.kind === "hostname" ? "url" : "numbers-and-punctuation"}
                  onChangeText={(address) => setTcpForm({ ...tcpForm, address })}
                  placeholder={tcpForm.kind === "hostname" ? "node.example.org" : "192.0.2.10"}
                  placeholderTextColor={colors.muted}
                  style={styles.input}
                  value={tcpForm.address}
                />
                <Text style={styles.label}>Port</Text>
                <TextInput
                  accessibilityLabel="TCP peer port"
                  editable={!mutating}
                  keyboardType="number-pad"
                  onChangeText={(port) => setTcpForm({ ...tcpForm, port })}
                  style={styles.input}
                  value={tcpForm.port}
                />
                <View style={styles.switchRow}>
                  <View style={styles.switchCopy}>
                    <Text style={styles.label}>Enabled</Text>
                    <Text style={styles.help}>Connect after Wi-Fi has an address.</Text>
                  </View>
                  <Switch
                    accessibilityLabel="TCP peer enabled"
                    disabled={mutating}
                    onValueChange={(enabled) => setTcpForm({ ...tcpForm, enabled })}
                    trackColor={{ false: colors.line, true: colors.greenDark }}
                    value={tcpForm.enabled}
                  />
                </View>
                <View style={styles.actionRow}>
                  <SmallButton
                    disabled={mutating}
                    label="Save peer"
                    onPress={() => void saveTcpPeer()}
                    primary
                  />
                  <SmallButton
                    disabled={mutating}
                    label="Cancel"
                    onPress={() => {
                      setFormError(null);
                      setTcpForm(null);
                    }}
                  />
                </View>
              </View>
            )}
          </View>
        </>
      )}
    </ScrollView>
  );
}

const colors = {
  background: "#101411",
  green: "#91e6a7",
  greenDark: "#173f24",
  line: "#303b33",
  muted: "#93a096",
  panel: "#171d19",
  panel2: "#1d2520",
  red: "#ff9b91",
  text: "#ecf2ea",
} as const;

const styles = StyleSheet.create({
  scroller: { flex: 1, minHeight: 0, backgroundColor: colors.background },
  content: {
    width: "100%",
    maxWidth: 820,
    alignSelf: "center",
    padding: 12,
    paddingBottom: 48,
    gap: 10,
  },
  heading: {
    flexDirection: "row",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 12,
  },
  headingCopy: { flex: 1, minWidth: 0, gap: 4 },
  eyebrow: { color: colors.green, fontSize: 10, fontWeight: "800", letterSpacing: 1.5 },
  title: { color: colors.text, fontSize: 20, fontWeight: "800" },
  secondary: { color: colors.muted, fontSize: 12, lineHeight: 18 },
  statusGrid: { flexDirection: "row", flexWrap: "wrap", gap: 10 },
  statusCard: {
    flexGrow: 1,
    flexBasis: 260,
    minWidth: 0,
    padding: 13,
    gap: 4,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 10,
    backgroundColor: colors.panel,
  },
  statusCardControl: {
    marginTop: 5,
    paddingTop: 8,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
  cardEyebrow: { color: colors.muted, fontSize: 9, fontWeight: "800", letterSpacing: 1.1 },
  statusValue: { color: colors.text, fontSize: 17, fontWeight: "800" },
  meta: { color: colors.muted, fontSize: 11, lineHeight: 16 },
  dnsDetails: {
    marginTop: 2,
    paddingTop: 5,
    gap: 2,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
  dnsHeading: { color: colors.muted, fontSize: 8, fontWeight: "800", letterSpacing: 1 },
  dnsRow: {
    minWidth: 0,
    flexDirection: "row",
    alignItems: "baseline",
    justifyContent: "space-between",
    gap: 8,
  },
  dnsResolver: {
    flexShrink: 1,
    color: colors.text,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 9,
  },
  dnsOutcome: { flexShrink: 0, color: colors.muted, fontSize: 9, textAlign: "right" },
  dnsOutcomeFailure: { color: colors.red },
  dnsOutcomeSuccess: { color: colors.green },
  dnsContext: { color: colors.muted, fontSize: 8, lineHeight: 12 },
  dnsResolution: { color: colors.green, fontSize: 9, lineHeight: 13 },
  section: {
    padding: 12,
    gap: 10,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 11,
    backgroundColor: colors.panel,
  },
  sectionHeading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    flexWrap: "wrap",
    gap: 10,
  },
  sectionHeadingCopy: { flex: 1, minWidth: 220, gap: 2 },
  sectionTitle: { color: colors.text, fontSize: 16, fontWeight: "800" },
  rmapStatus: {
    padding: 10,
    gap: 2,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.panel2,
  },
  rmapStatusEyebrow: {
    color: colors.muted,
    fontSize: 8,
    fontWeight: "800",
    letterSpacing: 1,
  },
  rmapStatusHeadline: { color: colors.text, fontSize: 13, fontWeight: "800" },
  rmapStatusError: { color: colors.red },
  rmapStatusSuccess: { color: colors.green },
  rmapStatusWarning: { color: "#f1c56c" },
  rmapStatusRow: { color: colors.muted, fontSize: 9, lineHeight: 13 },
  compactSwitchList: { gap: 2 },
  endpointList: { gap: 6 },
  endpointRow: {
    minHeight: 54,
    flexDirection: "row",
    alignItems: "center",
    gap: 8,
    paddingHorizontal: 10,
    paddingVertical: 7,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.panel2,
  },
  endpointCopy: { flex: 1, minWidth: 0, gap: 1 },
  endpointTitle: { color: colors.text, fontSize: 13, fontWeight: "800" },
  endpointIdentity: {
    color: colors.muted,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 8,
    lineHeight: 12,
  },
  privacyText: {
    color: "#cabd98",
    fontSize: 10,
    lineHeight: 15,
  },
  list: { gap: 8 },
  profileCard: {
    padding: 12,
    gap: 8,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel2,
  },
  profileCardActive: { borderColor: "#4c8d5b", backgroundColor: colors.greenDark },
  profileHeading: {
    flexDirection: "row",
    alignItems: "flex-start",
    justifyContent: "space-between",
    gap: 10,
  },
  profileCopy: { flex: 1, minWidth: 0, gap: 2 },
  profileSsid: { color: colors.text, fontSize: 15, fontWeight: "700" },
  profileId: {
    color: colors.muted,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 9,
  },
  activeBadge: {
    paddingHorizontal: 7,
    paddingVertical: 3,
    color: colors.green,
    fontSize: 9,
    fontWeight: "800",
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 999,
  },
  emptyText: {
    paddingVertical: 10,
    color: colors.muted,
    fontSize: 12,
    textAlign: "center",
  },
  editor: {
    padding: 13,
    gap: 7,
    borderColor: "#4c8d5b",
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel2,
  },
  editorTitle: { color: colors.text, fontSize: 15, fontWeight: "800" },
  label: { color: colors.text, fontSize: 12, fontWeight: "700" },
  help: { color: colors.muted, fontSize: 10, lineHeight: 15 },
  input: {
    minHeight: 42,
    paddingHorizontal: 11,
    paddingVertical: 8,
    color: colors.text,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: colors.background,
  },
  switchRow: {
    minHeight: 46,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
  },
  switchCopy: { flex: 1, minWidth: 0 },
  actionRow: { flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 8 },
  segmentedRow: { flexDirection: "row", alignItems: "center", gap: 6 },
  button: {
    minHeight: 34,
    justifyContent: "center",
    paddingHorizontal: 11,
    paddingVertical: 6,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
  },
  buttonPrimary: { borderColor: "#5b9c69", backgroundColor: colors.green },
  buttonDestructive: { borderColor: "#70413d" },
  buttonDisabled: { opacity: 0.4 },
  buttonPressed: { opacity: 0.78 },
  buttonText: { color: colors.text, fontSize: 11, fontWeight: "700", textAlign: "center" },
  buttonPrimaryText: { color: "#0d1b11" },
  buttonDestructiveText: { color: colors.red },
  notice: {
    padding: 11,
    flexDirection: "row",
    alignItems: "center",
    flexWrap: "wrap",
    gap: 9,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: colors.panel2,
  },
  noticeError: { borderColor: "#70413d", backgroundColor: "#321d1b" },
  noticeSuccess: { borderColor: "#356344", backgroundColor: colors.greenDark },
  noticeText: { flexGrow: 1, flexBasis: 260, color: colors.muted, fontSize: 12, lineHeight: 18 },
  errorText: { flexGrow: 1, flexBasis: 260, color: colors.red, fontSize: 12, lineHeight: 18 },
  successText: { flexGrow: 1, flexBasis: 260, color: colors.green, fontSize: 12, lineHeight: 18 },
  inlineError: { color: colors.red, fontSize: 11 },
  inlineSuccess: { color: colors.green, fontSize: 11 },
  rebootBanner: {
    padding: 12,
    gap: 3,
    borderColor: "#9a7937",
    borderWidth: 1,
    borderRadius: 9,
    backgroundColor: "#332a18",
  },
  rebootTitle: { color: "#f2d58c", fontSize: 13, fontWeight: "800" },
  rebootText: { color: "#cabd98", fontSize: 11 },
  loading: {
    minHeight: 80,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "center",
    gap: 10,
  },
});
