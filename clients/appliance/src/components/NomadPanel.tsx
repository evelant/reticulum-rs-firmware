import { useEffect, useState } from "react";
import { ActivityIndicator, Platform, ScrollView, Text, TextInput, View } from "react-native";

import {
  DEFAULT_NOMAD_PAGE_PATH,
  NOMAD_PRESENTATION_TIMEOUT_MS,
  type NomadBrowserController,
  type NomadBrowserState,
  nomadDestinationHintApplication,
  nomadFetchInputError,
  nomadRequestProvenance,
} from "../lib/nomad-browser.ts";
import { ActionButton } from "./AppliancePrimitives.tsx";
import { APPLIANCE_KEYBOARD_LAYOUT } from "./appliance-screen-layout.ts";
import { styles } from "./appliance-screen-styles.ts";

const KEYBOARD_LAYOUT = APPLIANCE_KEYBOARD_LAYOUT;

function nomadPhaseLabel(phase: string | null): string {
  switch (phase) {
    case null:
      return "Accepted; waiting for the first device update";
    case "path_lookup":
      return "Looking up a Reticulum path";
    case "link_establishment":
      return "Establishing a Reticulum Link";
    case "request_preparation":
      return "Preparing the anonymous page request";
    case "awaiting_dispatch_confirmation":
      return "Waiting for radio dispatch confirmation";
    case "awaiting_response":
      return "Waiting for the remote page";
    default:
      return phase.replaceAll("_", " ");
  }
}

function nomadFailureLabel(failure: string): string {
  switch (failure) {
    case "no_path":
      return "No usable Reticulum path was found.";
    case "link":
      return "The Reticulum Link could not be established or retained.";
    case "request":
      return "The page request could not be prepared, sent, or processed.";
    case "timeout":
      return "The remote node did not answer before its request timeout.";
    case "page_too_large":
      return "The page exceeds this appliance profile's bounded response size.";
    case "invalid_utf8":
      return "The remote response is not valid UTF-8.";
    case "internal":
      return "The appliance stopped this fetch after an internal invariant or backend failure.";
    default:
      return failure.replaceAll("_", " ");
  }
}

function nomadInitialInput(state: NomadBrowserState): {
  readonly destination: string;
  readonly path: string;
} {
  if ("request" in state) {
    return { destination: state.request.destination, path: state.request.path };
  }
  if (state.status === "input_error") {
    return { destination: state.destination, path: state.path };
  }
  return { destination: "", path: DEFAULT_NOMAD_PAGE_PATH };
}

interface NomadPanelProps {
  readonly available: boolean;
  readonly connected: boolean;
  readonly controller: NomadBrowserController;
  readonly destinationHint: string | null;
  readonly onDestinationHintConsumed: () => void;
  readonly state: NomadBrowserState;
}

export function NomadPanel({
  available,
  connected,
  controller,
  destinationHint,
  onDestinationHintConsumed,
  state,
}: NomadPanelProps) {
  const slotOwned =
    state.status === "starting" ||
    state.status === "pending" ||
    state.status === "poll_error" ||
    state.status === "timed_out";
  const initial = nomadInitialInput(state);
  const [destination, setDestination] = useState(
    slotOwned ? initial.destination : (destinationHint ?? initial.destination),
  );
  const [path, setPath] = useState(initial.path);
  const [formError, setFormError] = useState<string | null>(null);
  const fetchLabel =
    state.status === "starting" || state.status === "pending"
      ? "Fetching…"
      : state.status === "poll_error" || state.status === "timed_out"
        ? "Resume current fetch"
        : "Fetch page";
  const fetchId = "id" in state ? state.id : null;
  const requestProvenance = nomadRequestProvenance(state);

  useEffect(() => {
    const application = nomadDestinationHintApplication(destination, destinationHint, slotOwned);
    if (!application.consumed) return;
    setDestination(application.destination);
    setFormError(null);
    onDestinationHintConsumed();
  }, [destination, destinationHint, onDestinationHintConsumed, slotOwned]);

  const fetchPage = () => {
    const normalizedDestination = destination.trim().toLowerCase();
    const validationError = nomadFetchInputError(normalizedDestination, path);
    setFormError(validationError);
    if (validationError === null) void controller.start(normalizedDestination, path);
  };

  const result = (() => {
    switch (state.status) {
      case "idle":
      case "input_error":
        return (
          <Text style={styles.nomadHint}>
            Enter the peer&apos;s distinct Nomad node destination, or choose Browse beside an
            authenticated nearby peer. Its LXMF/contact destination is a different address.
          </Text>
        );
      case "starting":
        return (
          <View accessibilityLiveRegion="polite" style={styles.nomadStatus}>
            <ActivityIndicator color="#91e6a7" />
            <Text style={styles.secondaryText}>Submitting the bounded page request…</Text>
          </View>
        );
      case "start_error":
        return (
          <View accessibilityLiveRegion="assertive" style={styles.nomadResult}>
            <Text style={styles.inlineError}>{state.error}</Text>
            <Text style={styles.nomadHint}>
              The appliance may have accepted the request. Retry preserves its exact timestamp and
              idempotency key.
            </Text>
            <View style={styles.actionRow}>
              <ActionButton
                disabled={!connected}
                label="Retry same start"
                onPress={() => void controller.retryStart()}
                secondary
              />
            </View>
          </View>
        );
      case "pending":
        return (
          <View accessibilityLiveRegion="polite" style={styles.nomadStatus}>
            <ActivityIndicator color="#91e6a7" />
            <View style={styles.nomadStatusCopy}>
              <Text style={styles.nomadResultTitle}>Fetching page</Text>
              <Text style={styles.secondaryText}>{nomadPhaseLabel(state.phase)}</Text>
            </View>
          </View>
        );
      case "poll_error":
        return (
          <View accessibilityLiveRegion="assertive" style={styles.nomadResult}>
            <Text style={styles.inlineError}>{state.error}</Text>
            <Text style={styles.nomadHint}>
              The fetch ID is retained. Resume after reconnecting. If the board rebooted and made
              this boot-scoped ID stale, explicitly abandon it before starting another fetch.
            </Text>
            <View style={styles.actionRow}>
              <ActionButton
                disabled={!connected}
                label="Resume polling"
                onPress={() => void controller.resumePolling()}
                secondary
              />
              <ActionButton
                label="Abandon fetch ID"
                onPress={() => controller.abandonRetainedFetch()}
                secondary
              />
            </View>
          </View>
        );
      case "timed_out":
        return (
          <View accessibilityLiveRegion="polite" style={styles.nomadResult}>
            <Text style={styles.nomadResultTitle}>Still pending</Text>
            <Text style={styles.secondaryText}>
              Local polling paused after {NOMAD_PRESENTATION_TIMEOUT_MS / 1_000} seconds. The
              boot-scoped fetch ID is retained. Resume it, or explicitly abandon it after a board
              reset to permit a fresh request.
            </Text>
            <View style={styles.actionRow}>
              <ActionButton
                disabled={!connected}
                label="Resume polling"
                onPress={() => void controller.resumePolling()}
                secondary
              />
              <ActionButton
                label="Abandon fetch ID"
                onPress={() => controller.abandonRetainedFetch()}
                secondary
              />
            </View>
          </View>
        );
      case "failed":
        return (
          <View accessibilityLiveRegion="assertive" style={styles.nomadResult}>
            <Text style={styles.nomadResultTitle}>Fetch failed</Text>
            <Text style={styles.inlineError}>{nomadFailureLabel(state.failure)}</Text>
          </View>
        );
      case "ready":
        return (
          <View accessibilityLiveRegion="polite" style={styles.nomadResult}>
            <Text style={styles.nomadResultTitle}>Raw Micron page</Text>
            <Text style={styles.nomadHint}>
              Rendering is deliberately deferred; this first browser surface shows the exact bounded
              UTF-8 response.
            </Text>
            <View style={styles.nomadRawPage}>
              <Text selectable style={styles.nomadRawText}>
                {state.page}
              </Text>
            </View>
          </View>
        );
    }
  })();

  const contents = (
    <View style={styles.nomadCard}>
      <View style={styles.nomadHeading}>
        <View style={styles.nomadHeadingCopy}>
          <Text style={styles.eyebrow}>NOMADNET</Text>
          <Text style={styles.heading}>Browse a bounded page</Text>
        </View>
        <View style={[styles.pill, connected && styles.pillReady]}>
          <Text style={styles.pillText}>
            {!available ? "not available" : connected ? "appliance ready" : "disconnected"}
          </Text>
        </View>
      </View>
      {available ? null : (
        <Text accessibilityLiveRegion="polite" style={styles.nomadHint}>
          Nomad browsing is not available from this firmware yet.
        </Text>
      )}
      <Text style={styles.label}>Nomad node destination</Text>
      <TextInput
        accessibilityLabel="Nomad node destination"
        autoCapitalize="none"
        autoCorrect={false}
        editable={!slotOwned}
        maxLength={32}
        onChangeText={setDestination}
        placeholder="32 hexadecimal characters"
        placeholderTextColor="#748078"
        style={[styles.input, styles.monospaceInput]}
        value={destination}
      />
      <Text style={styles.nomadHint}>
        Use the distinct Nomad node destination. An LXMF/contact destination will not resolve here.
      </Text>
      <Text style={styles.label}>Page path</Text>
      <TextInput
        accessibilityLabel="Nomad page path"
        autoCapitalize="none"
        autoCorrect={false}
        editable={!slotOwned}
        onChangeText={setPath}
        placeholder={DEFAULT_NOMAD_PAGE_PATH}
        placeholderTextColor="#748078"
        style={[styles.input, styles.monospaceInput]}
        value={path}
      />
      {formError === null ? null : <Text style={styles.inlineError}>{formError}</Text>}
      <View style={styles.nomadFetchRow}>
        <ActionButton
          disabled={!available || !connected || slotOwned}
          label={fetchLabel}
          onPress={fetchPage}
        />
        <Text style={styles.nomadHint}>
          Anonymous request · one complete page · no Resource yet
        </Text>
      </View>
      {fetchId === null && requestProvenance === null ? null : (
        <View style={styles.nomadFetchMeta}>
          {fetchId === null ? null : (
            <>
              <Text style={styles.metaLabel}>Fetch ID</Text>
              <Text selectable style={styles.monospace}>
                {fetchId}
              </Text>
            </>
          )}
          {"outcome" in state ? (
            <Text style={styles.nomadHint}>Start {state.outcome.replaceAll("_", " ")}</Text>
          ) : null}
          {requestProvenance === null ? null : (
            <>
              <Text style={styles.metaLabel}>Request destination</Text>
              <Text selectable style={styles.monospace}>
                {requestProvenance.destination}
              </Text>
              <Text style={styles.metaLabel}>Request path</Text>
              <Text selectable style={styles.monospace}>
                {requestProvenance.path}
              </Text>
            </>
          )}
        </View>
      )}
      {result}
    </View>
  );

  return (
    <ScrollView
      automaticallyAdjustKeyboardInsets={Platform.OS === "ios"}
      contentContainerStyle={styles.nomadContent}
      keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
      keyboardShouldPersistTaps="handled"
      style={styles.nomadScroller}
    >
      {contents}
    </ScrollView>
  );
}
