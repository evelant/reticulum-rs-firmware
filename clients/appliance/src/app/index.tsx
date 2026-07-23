import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ActivityIndicator,
  Platform,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  useWindowDimensions,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import type {
  ApplianceSnapshot,
  BytesView,
  ConnectionState,
  ConnectionTransport,
  ContactView,
  OnboardingView,
  RecoveryRequest,
  SendRequest,
  TimelineView,
} from "../generated/api.ts";
import {
  MAX_CONTACT_NAME_BYTES,
  MAX_LXMF_BASIC_CONTENT_BYTES,
  MAX_LXMF_BASIC_TITLE_BYTES,
} from "../generated/api.ts";
import { ApplianceApi } from "../lib/api";
import { type DraftIdentity, ensureDraftIdentity } from "../lib/draft.ts";
import { LatestRequest } from "../lib/latest-request.ts";
import { byteLimitError, utf8ByteLength } from "../lib/limits.ts";
import { readNativeCoreStatus } from "../lib/native-core";
import type { NativeCoreStatus } from "../lib/native-core-types.ts";
import { onboardingPresentation } from "../lib/onboarding.ts";
import { randomHex } from "../lib/random.ts";

const EMPTY_ONBOARDING: OnboardingView = { available: false, method: null, snapshot: null };

function connectionLabel(connection: ConnectionState | undefined): string {
  return connection?.state.replaceAll("_", " ") ?? "starting";
}

function transportLabel(transport: ConnectionTransport): string {
  return typeof transport === "string"
    ? transport.replaceAll("_", " ")
    : transport.other.replaceAll("_", " ");
}

function bytesText(field: BytesView): string {
  return field.encoding === "utf8" ? field.value : `hex:${field.value}`;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

interface ActionButtonProps {
  readonly disabled?: boolean;
  readonly label: string;
  readonly onPress: () => void;
  readonly secondary?: boolean;
}

function ActionButton({ disabled = false, label, onPress, secondary = false }: ActionButtonProps) {
  return (
    <Pressable
      accessibilityRole="button"
      disabled={disabled}
      onPress={onPress}
      style={({ pressed }) => [
        styles.button,
        secondary && styles.buttonSecondary,
        disabled && styles.buttonDisabled,
        pressed && !disabled && styles.buttonPressed,
      ]}
    >
      <Text style={[styles.buttonText, secondary && styles.buttonSecondaryText]}>{label}</Text>
    </Pressable>
  );
}

interface OnboardingPanelProps {
  readonly busy: boolean;
  readonly onboarding: OnboardingView;
  readonly onMutation: (path: "start" | "refresh" | RecoveryRequest["action"]) => void;
}

function OnboardingPanel({ busy, onboarding, onMutation }: OnboardingPanelProps) {
  const presentation = onboardingPresentation(onboarding);
  if (presentation.ready) return null;
  return (
    <View accessibilityLiveRegion="polite" style={styles.onboarding}>
      <Text style={styles.eyebrow}>FIRST-RUN SETUP</Text>
      <Text style={styles.onboardingTitle}>{presentation.title}</Text>
      <Text style={styles.secondaryText}>{presentation.instruction}</Text>
      {presentation.identifierLabel === null ? null : (
        <View style={styles.serialRow}>
          <Text style={styles.metaLabel}>{presentation.identifierLabel}</Text>
          <Text selectable style={styles.monospace}>
            {onboarding.snapshot?.usb_serial ?? "—"}
          </Text>
        </View>
      )}
      <View style={styles.actionRow}>
        {presentation.canStart ? (
          <ActionButton
            disabled={busy}
            label={presentation.startLabel}
            onPress={() => onMutation("start")}
          />
        ) : null}
        {presentation.canResume ? (
          <ActionButton
            disabled={busy}
            label="Resume pairing"
            onPress={() => onMutation("resume_known_pending")}
          />
        ) : null}
        {presentation.canAbort ? (
          <ActionButton
            disabled={busy}
            label="Abort pending state"
            onPress={() => onMutation("abort_orphan")}
            secondary
          />
        ) : null}
        {presentation.canRefresh ? (
          <ActionButton
            disabled={busy}
            label="Recheck local state"
            onPress={() => onMutation("refresh")}
            secondary
          />
        ) : null}
      </View>
    </View>
  );
}

interface SidebarProps {
  readonly busy: boolean;
  readonly compact: boolean;
  readonly contacts: ContactView[];
  readonly onSelect: (destination: string) => void;
  readonly onUpsert: (destination: string, name: string) => Promise<boolean>;
  readonly selected: string | null;
  readonly snapshot: ApplianceSnapshot | null;
}

function Sidebar({
  busy,
  compact,
  contacts,
  onSelect,
  onUpsert,
  selected,
  snapshot,
}: SidebarProps) {
  const [showForm, setShowForm] = useState(false);
  const [name, setName] = useState("");
  const [destination, setDestination] = useState("");
  const [formError, setFormError] = useState<string | null>(null);
  const readyConnection = snapshot?.connection.state === "ready" ? snapshot.connection : undefined;

  const save = async () => {
    const normalizedDestination = destination.trim().toLowerCase();
    const nameError = byteLimitError(name, MAX_CONTACT_NAME_BYTES, "Name");
    if (nameError !== null) {
      setFormError(nameError);
      return;
    }
    if (name.trim().length === 0) {
      setFormError("Name is required");
      return;
    }
    if (!/^[0-9a-f]{32}$/.test(normalizedDestination)) {
      setFormError("LXMF destination must be exactly 32 hexadecimal characters");
      return;
    }
    setFormError(null);
    if (!(await onUpsert(normalizedDestination, name))) return;
    setName("");
    setDestination("");
    setShowForm(false);
  };

  return (
    <View style={[styles.sidebar, compact && styles.sidebarCompact]}>
      <View style={styles.sectionHeading}>
        <Text style={styles.heading}>Contacts</Text>
        <Pressable
          accessibilityLabel="Add contact"
          accessibilityRole="button"
          onPress={() => setShowForm(true)}
          style={styles.addButton}
        >
          <Text style={styles.addButtonText}>+</Text>
        </Pressable>
      </View>
      {showForm ? (
        <View style={styles.contactForm}>
          <Text style={styles.label}>Name</Text>
          <TextInput
            accessibilityLabel="Contact name"
            autoCapitalize="none"
            editable={!busy}
            onChangeText={setName}
            style={styles.input}
            value={name}
          />
          <Text style={styles.label}>LXMF destination</Text>
          <TextInput
            accessibilityLabel="LXMF destination"
            autoCapitalize="none"
            autoCorrect={false}
            editable={!busy}
            maxLength={32}
            onChangeText={setDestination}
            style={[styles.input, styles.monospaceInput]}
            value={destination}
          />
          {formError === null ? null : <Text style={styles.inlineError}>{formError}</Text>}
          <View style={styles.actionRow}>
            <ActionButton disabled={busy} label="Save" onPress={() => void save()} />
            <ActionButton label="Cancel" onPress={() => setShowForm(false)} secondary />
          </View>
        </View>
      ) : null}
      <ScrollView contentContainerStyle={styles.contacts}>
        {contacts.map((contact) => (
          <Pressable
            accessibilityRole="button"
            key={contact.destination}
            onPress={() => onSelect(contact.destination)}
            style={({ pressed }) => [
              styles.contact,
              selected === contact.destination && styles.contactActive,
              pressed && styles.contactPressed,
            ]}
          >
            <Text style={styles.contactName}>{contact.name || "Unnamed contact"}</Text>
            <Text selectable style={styles.monospace}>
              {contact.destination}
            </Text>
          </Pressable>
        ))}
      </ScrollView>
      <View style={styles.deviceMeta}>
        <MetaRow label="Connection" value={connectionLabel(snapshot?.connection)} />
        <MetaRow
          label="Transport"
          value={readyConnection === undefined ? "—" : transportLabel(readyConnection.transport)}
        />
        <MetaRow label="Endpoint" value={readyConnection?.endpoint ?? "—"} />
        <MetaRow label="Device" value={readyConnection?.device_label ?? "—"} />
        <MetaRow label="Pending" value={String(snapshot?.pending_outbox ?? 0)} />
        <MetaRow label="Imported" value={String(snapshot?.imported_this_run ?? 0)} />
        <MetaRow label="Local LXMF" value={snapshot?.device?.lxmf_delivery_destination ?? "—"} />
      </View>
    </View>
  );
}

function MetaRow({ label, value }: { readonly label: string; readonly value: string }) {
  return (
    <View style={styles.metaRow}>
      <Text style={styles.metaLabel}>{label}</Text>
      <Text selectable style={styles.metaValue}>
        {value}
      </Text>
    </View>
  );
}

interface ConversationProps {
  readonly busy: boolean;
  readonly contact: ContactView | undefined;
  readonly onDraftChanged: () => void;
  readonly onSend: (title: string, content: string) => Promise<boolean>;
  readonly timeline: TimelineView[];
}

function Conversation({ busy, contact, onDraftChanged, onSend, timeline }: ConversationProps) {
  const [title, setTitle] = useState("");
  const [content, setContent] = useState("");
  const [validationError, setValidationError] = useState<string | null>(null);
  const draftVersion = useRef(0);

  if (contact === undefined) {
    return (
      <View style={styles.emptyState}>
        <Text style={styles.emptyTitle}>Select or add a contact to begin.</Text>
        <Text style={styles.secondaryText}>
          The node continues receiving and routing while this app is closed.
        </Text>
      </View>
    );
  }

  const titleBytes = utf8ByteLength(title);
  const contentBytes = utf8ByteLength(content);
  const send = async () => {
    const error =
      byteLimitError(title, MAX_LXMF_BASIC_TITLE_BYTES, "Title") ??
      byteLimitError(content, MAX_LXMF_BASIC_CONTENT_BYTES, "Message") ??
      (content.length === 0 ? "Message is required" : null);
    setValidationError(error);
    if (error !== null) return;
    const submittedVersion = draftVersion.current;
    if (!(await onSend(title, content))) return;
    if (draftVersion.current === submittedVersion) {
      setTitle("");
      setContent("");
    }
  };

  return (
    <View style={styles.conversation}>
      <View style={styles.conversationHeading}>
        <Text style={styles.heading}>{contact.name || "Unnamed contact"}</Text>
        <Text selectable style={styles.monospace}>
          {contact.destination}
        </Text>
      </View>
      <ScrollView contentContainerStyle={styles.timeline} style={styles.timelineScroller}>
        {timeline.map((entry) => (
          <View
            key={`${entry.sequence}:${entry.direction}`}
            style={[
              styles.message,
              entry.direction === "outbound" ? styles.messageOutbound : styles.messageInbound,
            ]}
          >
            <Text style={styles.messageTitle}>{bytesText(entry.title) || "Untitled"}</Text>
            <Text selectable style={styles.messageContent}>
              {bytesText(entry.content)}
            </Text>
            <Text style={styles.messageFooter}>
              {new Date(entry.timestamp_ms).toLocaleString()}
              {entry.status === null ? "" : ` · ${entry.status.replaceAll("_", " ")}`}
            </Text>
          </View>
        ))}
      </ScrollView>
      <View style={styles.compose}>
        <TextInput
          accessibilityLabel="Message title"
          editable={!busy}
          onChangeText={(value) => {
            draftVersion.current += 1;
            setTitle(value);
            onDraftChanged();
          }}
          placeholder="Title (optional)"
          placeholderTextColor="#748078"
          style={styles.input}
          value={title}
        />
        <TextInput
          accessibilityLabel="Message"
          editable={!busy}
          multiline
          onChangeText={(value) => {
            draftVersion.current += 1;
            setContent(value);
            onDraftChanged();
          }}
          placeholder="Message"
          placeholderTextColor="#748078"
          style={[styles.input, styles.messageInput]}
          value={content}
        />
        {validationError === null ? null : (
          <Text style={styles.inlineError}>{validationError}</Text>
        )}
        <View style={styles.composeFooter}>
          <Text style={styles.counter}>
            Title {titleBytes} / {MAX_LXMF_BASIC_TITLE_BYTES} · Message {contentBytes} /{" "}
            {MAX_LXMF_BASIC_CONTENT_BYTES}
          </Text>
          <ActionButton disabled={busy} label="Queue message" onPress={() => void send()} />
        </View>
      </View>
    </View>
  );
}

export default function ApplianceScreen() {
  const api = useMemo(() => new ApplianceApi(), []);
  const { width } = useWindowDimensions();
  const compact = width < 760;
  const [bootstrapped, setBootstrapped] = useState(false);
  const [nativeCore, setNativeCore] = useState<NativeCoreStatus | null>(null);
  const [snapshot, setSnapshot] = useState<ApplianceSnapshot | null>(null);
  const [onboarding, setOnboarding] = useState<OnboardingView>(EMPTY_ONBOARDING);
  const [contacts, setContacts] = useState<ContactView[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [timeline, setTimeline] = useState<TimelineView[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const draft = useRef<DraftIdentity | null>(null);
  const mutationInFlight = useRef(false);
  const reconnectRequested = useRef(false);
  const refreshRequests = useRef(new LatestRequest());
  const selectedRef = useRef<string | null>(null);
  const timelineRequests = useRef(new LatestRequest());

  const ready = onboardingPresentation(onboarding).ready;
  // Missing credentials can make the dormant connector report an expected
  // local error. The onboarding panel owns that state until setup is ready.
  const displayedError = error ?? (ready ? snapshot?.last_error : null);
  const selectedContact = contacts.find((contact) => contact.destination === selected);

  useEffect(() => {
    let active = true;
    void readNativeCoreStatus().then((status) => {
      if (active) setNativeCore(status);
    });
    return () => {
      active = false;
    };
  }, []);

  useEffect(() => () => api.dispose(), [api]);

  const refresh = useCallback(async () => {
    const refreshRequest = refreshRequests.current.begin();
    try {
      const [nextSnapshot, nextOnboarding] = await Promise.all([api.snapshot(), api.onboarding()]);
      if (!refreshRequests.current.accepts(refreshRequest)) return;
      setSnapshot(nextSnapshot);
      setOnboarding(nextOnboarding);
      const nextReady = onboardingPresentation(nextOnboarding).ready;
      if (!nextReady) {
        timelineRequests.current.invalidate();
        setError(null);
        return;
      }
      const nextContacts = await api.contacts();
      if (!refreshRequests.current.accepts(refreshRequest)) return;
      setContacts(nextContacts);
      const selectedDestination = selectedRef.current;
      if (selectedDestination !== null) {
        if (nextContacts.some((contact) => contact.destination === selectedDestination)) {
          const timelineRequest = timelineRequests.current.begin();
          let nextTimeline: TimelineView[];
          try {
            nextTimeline = await api.timeline(selectedDestination);
          } catch (nextError) {
            if (
              refreshRequests.current.accepts(refreshRequest) &&
              timelineRequests.current.accepts(timelineRequest) &&
              selectedRef.current === selectedDestination
            ) {
              throw nextError;
            }
            return;
          }
          if (
            !refreshRequests.current.accepts(refreshRequest) ||
            !timelineRequests.current.accepts(timelineRequest) ||
            selectedRef.current !== selectedDestination
          ) {
            return;
          }
          setTimeline(nextTimeline);
        } else {
          timelineRequests.current.invalidate();
          selectedRef.current = null;
          setSelected(null);
          setTimeline([]);
        }
      }
      setError(null);
    } catch (nextError) {
      if (refreshRequests.current.accepts(refreshRequest)) throw nextError;
    }
  }, [api]);

  useEffect(() => {
    let active = true;
    void (async () => {
      try {
        await api.bootstrapSession();
        if (!active) return;
        setBootstrapped(true);
        await refresh();
      } catch (nextError) {
        if (active) setError(errorText(nextError));
      }
    })();
    return () => {
      active = false;
    };
  }, [api, refresh]);

  useEffect(() => {
    if (!bootstrapped) return;
    const unsubscribe = api.subscribeInvalidations(
      () => void refresh().catch((nextError) => setError(errorText(nextError))),
      () => setError("Event stream reconnecting"),
    );
    const interval =
      !ready || unsubscribe === null
        ? setInterval(
            () => void refresh().catch((nextError) => setError(errorText(nextError))),
            ready ? 2_000 : 500,
          )
        : null;
    return () => {
      unsubscribe?.();
      if (interval !== null) clearInterval(interval);
    };
  }, [api, bootstrapped, ready, refresh]);

  useEffect(() => {
    if (
      onboarding.available &&
      ready &&
      snapshot?.connection.state !== "ready" &&
      !reconnectRequested.current
    ) {
      reconnectRequested.current = true;
      void api
        .reconnect()
        .then(refresh)
        .catch((nextError) => setError(errorText(nextError)));
    }
  }, [api, onboarding.available, ready, refresh, snapshot?.connection.state]);

  const run = async (operation: () => Promise<unknown>): Promise<boolean> => {
    if (mutationInFlight.current) return false;
    mutationInFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      try {
        await operation();
      } catch (nextError) {
        setError(errorText(nextError));
        return false;
      }
      try {
        await refresh();
      } catch (nextError) {
        setError(`Action completed, but refreshing the display failed: ${errorText(nextError)}`);
      }
      return true;
    } finally {
      mutationInFlight.current = false;
      setBusy(false);
    }
  };

  const onboardingMutation = (action: "start" | "refresh" | RecoveryRequest["action"]) => {
    void run(() => {
      if (action === "start") return api.startOnboarding();
      if (action === "refresh") return api.refreshOnboarding();
      return api.recoverOnboarding({ action });
    });
  };

  const upsertContact = (destination: string, name: string): Promise<boolean> =>
    run(async () => {
      await api.upsertContact(destination, { name });
      if (selectedRef.current !== destination) draft.current = null;
      selectedRef.current = destination;
      timelineRequests.current.invalidate();
      setSelected(destination);
      setTimeline([]);
    });

  const send = async (title: string, content: string): Promise<boolean> => {
    if (selected === null) return false;
    return run(async () => {
      const identity = ensureDraftIdentity(draft.current, () => randomHex(16), Date.now);
      draft.current = identity;
      const request: SendRequest = {
        destination: selected,
        timestamp_ms: identity.timestampMs,
        idempotency_key: identity.idempotencyKey,
        title,
        content,
      };
      await api.send(request);
      if (draft.current === identity) draft.current = null;
    });
  };

  const chooseContact = (destination: string) => {
    if (selectedRef.current !== destination) draft.current = null;
    selectedRef.current = destination;
    const timelineRequest = timelineRequests.current.begin();
    setSelected(destination);
    setTimeline([]);
    void api
      .timeline(destination)
      .then((nextTimeline) => {
        if (
          timelineRequests.current.accepts(timelineRequest) &&
          selectedRef.current === destination
        ) {
          setTimeline(nextTimeline);
          setError(null);
        }
      })
      .catch((nextError) => {
        if (
          timelineRequests.current.accepts(timelineRequest) &&
          selectedRef.current === destination
        ) {
          setError(errorText(nextError));
        }
      });
  };

  return (
    <SafeAreaView style={styles.safeArea}>
      <View style={styles.topbar}>
        <View>
          <Text style={styles.eyebrow}>RETICULUM APPLIANCE</Text>
          <Text style={styles.title}>LXMF</Text>
        </View>
        <View style={styles.statusCluster}>
          {nativeCore === null ? null : (
            <View
              style={[
                styles.pill,
                nativeCore.state === "ready" ? styles.pillReady : styles.pillFaulted,
              ]}
            >
              <Text style={styles.pillText}>{nativeCore.label}</Text>
            </View>
          )}
          <View
            style={[
              styles.pill,
              snapshot?.connection.state === "ready" && styles.pillReady,
              snapshot?.connection.state === "faulted" && styles.pillFaulted,
            ]}
          >
            <Text style={styles.pillText}>
              {ready ? connectionLabel(snapshot?.connection) : "setup required"}
            </Text>
          </View>
          {compact ? null : (
            <>
              <ActionButton
                disabled={!ready || busy}
                label="Sync"
                onPress={() => void run(() => api.sync())}
                secondary
              />
              <ActionButton
                disabled={!ready || busy}
                label="Reconnect"
                onPress={() => void run(() => api.reconnect())}
                secondary
              />
            </>
          )}
        </View>
      </View>
      {displayedError === null || displayedError === undefined ? null : (
        <View accessibilityLiveRegion="assertive" style={styles.errorBanner}>
          <Text style={styles.errorText}>{displayedError}</Text>
        </View>
      )}
      {busy ? <ActivityIndicator color="#91e6a7" style={styles.activity} /> : null}
      <OnboardingPanel busy={busy} onboarding={onboarding} onMutation={onboardingMutation} />
      {ready ? (
        <View style={[styles.shell, compact && styles.shellCompact]}>
          <Sidebar
            busy={busy}
            compact={compact}
            contacts={contacts}
            onSelect={chooseContact}
            onUpsert={upsertContact}
            selected={selected}
            snapshot={snapshot}
          />
          <Conversation
            busy={busy}
            contact={selectedContact}
            key={selectedContact?.destination ?? "empty"}
            onDraftChanged={() => {
              draft.current = null;
            }}
            onSend={send}
            timeline={timeline}
          />
        </View>
      ) : null}
      {compact && ready ? (
        <View style={styles.mobileActions}>
          <ActionButton
            disabled={busy}
            label="Sync"
            onPress={() => void run(() => api.sync())}
            secondary
          />
          <ActionButton
            disabled={busy}
            label="Reconnect"
            onPress={() => void run(() => api.reconnect())}
            secondary
          />
        </View>
      ) : null}
    </SafeAreaView>
  );
}

const colors = {
  background: "#101411",
  panel: "#171d19",
  panel2: "#1d2520",
  line: "#303b33",
  text: "#ecf2ea",
  muted: "#93a096",
  green: "#91e6a7",
  greenDark: "#173f24",
  red: "#ff9b91",
} as const;

const styles = StyleSheet.create({
  safeArea: { flex: 1, minHeight: "100%", backgroundColor: colors.background },
  topbar: {
    minHeight: 84,
    paddingHorizontal: 28,
    paddingVertical: 18,
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
    backgroundColor: "#101411f2",
  },
  eyebrow: {
    marginBottom: 3,
    color: colors.green,
    fontSize: 10,
    fontWeight: "700",
    letterSpacing: 2,
  },
  title: { color: colors.text, fontSize: 24, fontWeight: "800" },
  heading: { color: colors.text, fontSize: 17, fontWeight: "700" },
  statusCluster: { flexDirection: "row", alignItems: "center", gap: 10 },
  pill: {
    paddingHorizontal: 11,
    paddingVertical: 7,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 999,
  },
  pillReady: { borderColor: "#356344", backgroundColor: colors.greenDark },
  pillFaulted: { borderColor: "#70413d", backgroundColor: "#321d1b" },
  pillText: { color: colors.muted, fontSize: 12 },
  button: {
    minHeight: 36,
    justifyContent: "center",
    paddingHorizontal: 13,
    paddingVertical: 8,
    borderRadius: 8,
    borderColor: "#5b9c69",
    borderWidth: 1,
    backgroundColor: colors.green,
  },
  buttonPressed: { opacity: 0.8 },
  buttonDisabled: { opacity: 0.4 },
  buttonSecondary: { borderColor: colors.line, backgroundColor: "transparent" },
  buttonText: { color: "#0d1b11", fontWeight: "700", textAlign: "center" },
  buttonSecondaryText: { color: "#dfe8df" },
  errorBanner: {
    marginHorizontal: 28,
    marginTop: 14,
    padding: 12,
    borderColor: "#70413d",
    borderWidth: 1,
    borderRadius: 8,
    backgroundColor: "#321d1b",
  },
  errorText: { color: colors.red },
  inlineError: { color: colors.red, fontSize: 12 },
  activity: { position: "absolute", top: 92, right: 16, zIndex: 2 },
  onboarding: {
    width: "92%",
    maxWidth: 620,
    alignSelf: "center",
    marginTop: 48,
    padding: 24,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 14,
    backgroundColor: colors.panel,
  },
  onboardingTitle: { marginBottom: 12, color: colors.text, fontSize: 22, fontWeight: "700" },
  secondaryText: { color: colors.muted, lineHeight: 22 },
  serialRow: {
    marginVertical: 20,
    paddingVertical: 13,
    gap: 8,
    borderTopColor: colors.line,
    borderTopWidth: 1,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  actionRow: { flexDirection: "row", flexWrap: "wrap", alignItems: "center", gap: 10 },
  shell: { flex: 1, flexDirection: "row", minHeight: 0 },
  shellCompact: { flexDirection: "column" },
  sidebar: {
    width: 320,
    maxWidth: "100%",
    padding: 22,
    borderRightColor: colors.line,
    borderRightWidth: 1,
    backgroundColor: colors.panel,
  },
  sidebarCompact: {
    width: "100%",
    maxHeight: 320,
    borderRightWidth: 0,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  sectionHeading: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    marginBottom: 14,
  },
  addButton: {
    width: 34,
    height: 34,
    alignItems: "center",
    justifyContent: "center",
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 17,
  },
  addButtonText: { color: colors.text, fontSize: 22 },
  contactForm: {
    gap: 9,
    marginBottom: 18,
    padding: 14,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 10,
    backgroundColor: colors.panel2,
  },
  label: { color: colors.muted, fontSize: 12 },
  input: {
    width: "100%",
    minHeight: 42,
    paddingHorizontal: 11,
    paddingVertical: 9,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 8,
    color: "#f3f7f2",
    backgroundColor: "#0d110e",
  },
  monospaceInput: {
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
  },
  contacts: { gap: 6 },
  contact: { gap: 3, padding: 11, borderColor: "transparent", borderWidth: 1, borderRadius: 8 },
  contactActive: { borderColor: colors.line, backgroundColor: colors.panel2 },
  contactPressed: { opacity: 0.8 },
  contactName: { color: "#dfe8df", fontWeight: "700" },
  monospace: {
    color: colors.muted,
    fontFamily: Platform.select({ ios: "Menlo", android: "monospace", default: "monospace" }),
    fontSize: 11,
    lineHeight: 16,
  },
  deviceMeta: {
    marginTop: 26,
    paddingTop: 16,
    gap: 8,
    borderTopColor: colors.line,
    borderTopWidth: 1,
  },
  metaRow: { flexDirection: "row", gap: 10 },
  metaLabel: {
    width: 78,
    color: colors.muted,
    fontSize: 11,
    fontWeight: "600",
    letterSpacing: 0.8,
    textTransform: "uppercase",
  },
  metaValue: { flex: 1, color: colors.text, fontSize: 12 },
  emptyState: { flex: 1, alignItems: "center", justifyContent: "center", padding: 32 },
  emptyTitle: { marginBottom: 8, color: colors.text, fontSize: 17, fontWeight: "600" },
  conversation: { flex: 1, minWidth: 0 },
  conversationHeading: {
    minHeight: 72,
    justifyContent: "center",
    gap: 4,
    paddingHorizontal: 22,
    borderBottomColor: colors.line,
    borderBottomWidth: 1,
  },
  timelineScroller: { flex: 1 },
  timeline: { flexGrow: 1, gap: 12, padding: 22, justifyContent: "flex-end" },
  message: {
    maxWidth: "78%",
    padding: 13,
    borderColor: colors.line,
    borderWidth: 1,
    borderRadius: 12,
  },
  messageInbound: { alignSelf: "flex-start", backgroundColor: colors.panel2 },
  messageOutbound: {
    alignSelf: "flex-end",
    borderColor: "#356344",
    backgroundColor: colors.greenDark,
  },
  messageTitle: { marginBottom: 5, color: colors.text, fontWeight: "700" },
  messageContent: { color: colors.text, lineHeight: 21 },
  messageFooter: { marginTop: 8, color: colors.muted, fontSize: 10 },
  compose: {
    gap: 10,
    padding: 16,
    borderTopColor: colors.line,
    borderTopWidth: 1,
    backgroundColor: colors.panel,
  },
  messageInput: { minHeight: 78, textAlignVertical: "top" },
  composeFooter: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    gap: 10,
  },
  counter: { flex: 1, color: colors.muted, fontSize: 10 },
  mobileActions: {
    flexDirection: "row",
    justifyContent: "flex-end",
    gap: 8,
    padding: 10,
    borderTopColor: colors.line,
    borderTopWidth: 1,
    backgroundColor: colors.panel,
  },
});
