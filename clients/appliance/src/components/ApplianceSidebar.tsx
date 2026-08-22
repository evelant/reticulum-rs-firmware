import { useCallback, useEffect, useRef, useState } from "react";
import {
  KeyboardAvoidingView,
  Modal,
  Platform,
  Pressable,
  ScrollView,
  Text,
  TextInput,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import type { ApplianceSnapshot, ContactView, ConversationPeerView } from "../generated/api.ts";
import { MAX_CONTACT_NAME_BYTES } from "../generated/api.ts";
import { errorText } from "../lib/app-error.ts";
import { connectionTransportLabel } from "../lib/appliance-status.ts";
import { bytesText } from "../lib/bytes-text.ts";
import { contactSaveIntent } from "../lib/contact-editor.ts";
import {
  conversationPeerLabel,
  messageRequestPeers,
  outboundOnlyUnsavedPeers,
  suggestedContactName,
} from "../lib/conversation-peers.ts";
import { ForegroundNearbyPoll } from "../lib/foreground-nearby-poll.ts";
import { LatestRequest } from "../lib/latest-request.ts";
import { byteLimitError } from "../lib/limits.ts";
import {
  associatedNomadDestinationForLxmf,
  NEARBY_FOREGROUND_POLL_INTERVAL_MS,
  type NearbyPeerView,
  nearbyContacts,
  nearbyNetworkSummary,
  nearbyObserverLabel,
  nearbyObserverSummaryHint,
  nearbyPeerFingerprint,
  nearbyPeerObservationHint,
  nearbyPeerSuggestedName,
  nearbySnapshotElapsedMs,
} from "../lib/nearby-peers.ts";
import { ActionButton } from "./AppliancePrimitives.tsx";
import { APPLIANCE_KEYBOARD_LAYOUT } from "./appliance-screen-layout.ts";
import { styles } from "./appliance-screen-styles.ts";

const KEYBOARD_LAYOUT = APPLIANCE_KEYBOARD_LAYOUT;

interface NearbyPanelProps {
  readonly active: boolean;
  readonly applianceLabel: string | null;
  readonly busy: boolean;
  readonly compact: boolean;
  readonly connected: boolean;
  readonly contacts: ContactView[];
  readonly loadError: string | null;
  readonly loaded: boolean;
  readonly loading: boolean;
  readonly onBrowseNomad: (destination: string) => void;
  readonly onRefresh: (() => Promise<void>) | null;
  readonly onSelect: (destination: string) => void;
  readonly onUpsert: (destination: string, name: string) => Promise<boolean>;
  readonly peers: readonly NearbyPeerView[];
  readonly snapshotFetchedAtMs: number | null;
}

function NearbyPanel({
  active,
  applianceLabel,
  busy,
  compact,
  connected,
  contacts,
  loadError,
  loaded,
  loading,
  onBrowseNomad,
  onRefresh,
  onSelect,
  onUpsert,
  peers,
  snapshotFetchedAtMs,
}: NearbyPanelProps) {
  const [addingDestination, setAddingDestination] = useState<string | null>(null);
  const [ageClockMs, setAgeClockMs] = useState(() => Date.now());
  useEffect(() => {
    setAgeClockMs(Date.now());
    if (!active) return;
    const timer = setInterval(() => setAgeClockMs(Date.now()), 1_000);
    return () => clearInterval(timer);
  }, [active]);
  const elapsedSinceFetchMs = nearbySnapshotElapsedMs(snapshotFetchedAtMs, ageClockMs);
  const networkSummary = nearbyNetworkSummary(peers, contacts, elapsedSinceFetchMs);
  const nearbyContactSummaries = nearbyContacts(peers);

  const choosePeer = async (peer: NearbyPeerView, alreadyAdded: boolean) => {
    if (alreadyAdded) {
      onSelect(peer.destination);
      return;
    }
    setAddingDestination(peer.destination);
    try {
      await onUpsert(peer.destination, nearbyPeerSuggestedName(peer));
    } finally {
      setAddingDestination(null);
    }
  };

  const peerRows = nearbyContactSummaries.map((nearbyContact) => {
    const peer = nearbyContact.representative;
    const existing = contacts.some((contact) => contact.destination === nearbyContact.destination);
    const adding = addingDestination === nearbyContact.destination;
    return (
      <View
        key={nearbyContact.destination}
        style={[
          styles.nearbyPeer,
          existing && styles.nearbyPeerAdded,
          busy && styles.buttonDisabled,
        ]}
      >
        <View style={styles.nearbyPeerHeading}>
          <Text numberOfLines={1} style={styles.nearbyPeerName}>
            {nearbyPeerSuggestedName(peer)}
          </Text>
          <View style={styles.nearbyPeerButtons}>
            <Pressable
              accessibilityHint="Opens this peer's associated Nomad node in the browser"
              accessibilityLabel={`Browse ${nearbyPeerSuggestedName(peer)} on NomadNet`}
              accessibilityRole="button"
              disabled={busy}
              onPress={() => onBrowseNomad(peer.associated_nomad_destination)}
              style={({ pressed }) => [
                styles.nearbyPeerButton,
                pressed && !busy && styles.contactPressed,
              ]}
            >
              <Text style={styles.nearbyPeerAction}>Browse</Text>
            </Pressable>
            <Pressable
              accessibilityHint={
                existing
                  ? "Opens the existing conversation"
                  : "Adds this authenticated peer as a contact"
              }
              accessibilityLabel={`${existing ? "Open" : "Add"} ${nearbyPeerSuggestedName(peer)}`}
              accessibilityRole="button"
              disabled={busy}
              onPress={() => void choosePeer(peer, existing)}
              style={({ pressed }) => [
                styles.nearbyPeerButton,
                pressed && !busy && styles.contactPressed,
              ]}
            >
              <Text style={styles.nearbyPeerAction}>
                {adding ? "Adding…" : existing ? "Open" : "Add"}
              </Text>
            </Pressable>
          </View>
        </View>
        {nearbyContact.observations.map((observation) => (
          <Text
            key={`${observation.observer_kind}:${observation.observer_management_destination ?? "phone"}:${observation.interface_id.join(":")}`}
            style={styles.nearbyStatus}
          >
            {nearbyPeerObservationHint(observation, applianceLabel, elapsedSinceFetchMs)}
          </Text>
        ))}
        <Text selectable style={styles.monospace}>
          ID {nearbyPeerFingerprint(peer)}
        </Text>
      </View>
    );
  });

  return (
    <View style={styles.nearbyPanel}>
      <View style={styles.nearbyHeading}>
        <View style={styles.nearbyTitle}>
          <Text style={styles.contactName}>Nearby</Text>
          <Text style={styles.nearbyCaption}>
            {loaded
              ? `${networkSummary.peerCount} authenticated contacts · ${networkSummary.observationCount} observations · ${networkSummary.unaddedPeerCount} not saved`
              : "Authenticated LXMF announces"}
          </Text>
        </View>
        <Pressable
          accessibilityLabel="Refresh nearby peers"
          accessibilityRole="button"
          disabled={busy || loading || !connected || onRefresh === null}
          onPress={() => void onRefresh?.()}
          style={({ pressed }) => [
            styles.smallButton,
            (busy || loading || !connected || onRefresh === null) && styles.buttonDisabled,
            pressed && styles.buttonPressed,
          ]}
        >
          <Text style={styles.smallButtonText}>{loading ? "Scanning…" : "Refresh"}</Text>
        </Pressable>
      </View>
      {onRefresh === null ? (
        <Text accessibilityLiveRegion="polite" style={styles.nearbyStatus}>
          Nearby discovery is not available from this firmware yet.
        </Text>
      ) : !connected ? (
        <Text accessibilityLiveRegion="polite" style={styles.nearbyStatus}>
          Connect to the appliance to read peers it has heard.
        </Text>
      ) : loading && !loaded ? (
        <Text accessibilityLiveRegion="polite" style={styles.nearbyStatus}>
          Reading authenticated announces from the appliance…
        </Text>
      ) : loadError !== null ? (
        <View accessibilityLiveRegion="assertive" style={styles.nearbyError}>
          <Text style={styles.inlineError}>{loadError}</Text>
          <Text style={styles.nearbyStatus}>Tap Refresh to try again.</Text>
        </View>
      ) : loaded && networkSummary.peerCount === 0 ? (
        <Text accessibilityLiveRegion="polite" style={styles.nearbyStatus}>
          No authenticated LXMF announces received yet. Leave both nodes powered, then refresh.
        </Text>
      ) : (
        <>
          <View style={styles.nearbyInterfaces}>
            <Text style={styles.applianceDestinationLabel}>
              OBSERVING NODES ({networkSummary.observerCount})
            </Text>
            {networkSummary.observers.map((observer) => (
              <View
                key={`${observer.observerKind}:${observer.observerManagementDestination ?? "phone"}`}
                style={styles.nearbyInterfaceRow}
              >
                <Text style={styles.nearbyInterfaceName}>
                  {nearbyObserverLabel(observer, applianceLabel)}
                </Text>
                <Text style={styles.nearbyStatus}>{nearbyObserverSummaryHint(observer)}</Text>
              </View>
            ))}
          </View>
          {compact ? (
            <View style={styles.nearbyList}>{peerRows}</View>
          ) : (
            <ScrollView
              contentContainerStyle={styles.nearbyList}
              keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
              keyboardShouldPersistTaps="handled"
              nestedScrollEnabled
              style={styles.nearbyScroller}
            >
              {peerRows}
            </ScrollView>
          )}
        </>
      )}
    </View>
  );
}

interface ApplianceSidebarProps {
  readonly applianceLabel: string | null;
  readonly busy: boolean;
  readonly compact: boolean;
  readonly contacts: ContactView[];
  readonly conversations: ConversationPeerView[];
  readonly foreground: boolean;
  readonly inline?: boolean;
  readonly onBrowseNomad: (destination: string) => void;
  readonly onClose: () => void;
  readonly onRefreshNearby: (() => Promise<NearbyPeerView[]>) | null;
  readonly onSelect: (destination: string) => void;
  readonly onUpsert: (
    destination: string,
    name: string,
    selectAfterSave?: boolean,
  ) => Promise<boolean>;
  readonly selected: string | null;
  readonly snapshot: ApplianceSnapshot | null;
  readonly visible: boolean;
}

export function ApplianceSidebar({
  applianceLabel,
  busy,
  compact,
  contacts,
  conversations,
  foreground,
  inline = false,
  onBrowseNomad,
  onClose,
  onRefreshNearby,
  onSelect,
  onUpsert,
  selected,
  snapshot,
  visible,
}: ApplianceSidebarProps) {
  const [showForm, setShowForm] = useState(false);
  const [showNearby, setShowNearby] = useState(false);
  const [name, setName] = useState("");
  const [destination, setDestination] = useState("");
  const [editingDestination, setEditingDestination] = useState<string | null>(null);
  const [requestDestination, setRequestDestination] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [nearbyPeers, setNearbyPeers] = useState<NearbyPeerView[]>([]);
  const [nearbyLoadError, setNearbyLoadError] = useState<string | null>(null);
  const [nearbyLoaded, setNearbyLoaded] = useState(false);
  const [nearbyLoading, setNearbyLoading] = useState(false);
  const [nearbySnapshotFetchedAtMs, setNearbySnapshotFetchedAtMs] = useState<number | null>(null);
  const nearbyRequests = useRef(new LatestRequest());
  const nearbyRefreshInFlight = useRef(false);
  const drawerScroller = useRef<ScrollView | null>(null);
  const readyConnection = snapshot?.connection.state === "ready" ? snapshot.connection : undefined;
  const nearbyConnectionKey =
    readyConnection === undefined
      ? null
      : [
          snapshot?.device?.device_id ?? "",
          connectionTransportLabel(readyConnection.transport),
          readyConnection.endpoint,
          readyConnection.device_label,
        ].join("\u0000");
  const nearbyConnectionKeyRef = useRef(nearbyConnectionKey);

  const resetContactForm = () => {
    setShowForm(false);
    setName("");
    setDestination("");
    setEditingDestination(null);
    setRequestDestination(null);
    setFormError(null);
  };

  const revealContactForm = () => {
    requestAnimationFrame(() => drawerScroller.current?.scrollTo({ animated: true, y: 0 }));
  };

  const beginAddingContact = () => {
    setShowNearby(false);
    setName("");
    setDestination("");
    setEditingDestination(null);
    setRequestDestination(null);
    setFormError(null);
    setShowForm(true);
    revealContactForm();
  };

  const beginEditingContact = (contact: ContactView) => {
    setShowNearby(false);
    setName(contact.name);
    setDestination(contact.destination);
    setEditingDestination(contact.destination);
    setRequestDestination(null);
    setFormError(null);
    setShowForm(true);
    revealContactForm();
  };

  const beginSavingUnsavedPeer = (peer: ConversationPeerView) => {
    setShowNearby(false);
    setName(suggestedContactName(peer.destination));
    setDestination(peer.destination);
    setEditingDestination(null);
    setRequestDestination(peer.destination);
    setFormError(null);
    setShowForm(true);
    revealContactForm();
  };

  const selectContact = (selectedDestination: string) => {
    resetContactForm();
    onSelect(selectedDestination);
    if (compact) onClose();
  };

  const upsertContact = async (
    selectedDestination: string,
    selectedName: string,
    selectAfterSave = true,
  ) => {
    const saved = await onUpsert(selectedDestination, selectedName, selectAfterSave);
    if (saved && compact) onClose();
    return saved;
  };

  const refreshNearby = useCallback(async () => {
    const source = nearbyConnectionKey;
    if (onRefreshNearby === null || source === null || nearbyRefreshInFlight.current) return;

    nearbyRefreshInFlight.current = true;
    const request = nearbyRequests.current.begin();
    setNearbyLoading(true);
    setNearbyLoadError(null);
    try {
      const discovered = await onRefreshNearby();
      if (!nearbyRequests.current.accepts(request) || nearbyConnectionKeyRef.current !== source) {
        return;
      }
      setNearbyPeers(discovered);
      setNearbySnapshotFetchedAtMs(Date.now());
      setNearbyLoaded(true);
    } catch (nextError) {
      if (!nearbyRequests.current.accepts(request) || nearbyConnectionKeyRef.current !== source) {
        return;
      }
      setNearbyLoadError(errorText(nextError));
      setNearbyLoaded(true);
    } finally {
      nearbyRefreshInFlight.current = false;
      if (nearbyRequests.current.accepts(request) && nearbyConnectionKeyRef.current === source) {
        setNearbyLoading(false);
      }
    }
  }, [nearbyConnectionKey, onRefreshNearby]);

  useEffect(() => {
    nearbyConnectionKeyRef.current = nearbyConnectionKey;
    nearbyRequests.current.invalidate();
    setNearbyPeers([]);
    setNearbyLoadError(null);
    setNearbyLoaded(false);
    setNearbyLoading(false);
    setNearbySnapshotFetchedAtMs(null);

    return () => nearbyRequests.current.invalidate();
  }, [nearbyConnectionKey]);

  useEffect(() => {
    if (visible) return;
    setShowForm(false);
    setName("");
    setDestination("");
    setEditingDestination(null);
    setRequestDestination(null);
    setFormError(null);
  }, [visible]);

  useEffect(() => {
    if (
      busy ||
      !foreground ||
      !showNearby ||
      !visible ||
      nearbyConnectionKey === null ||
      onRefreshNearby === null
    ) {
      return;
    }

    const poll = new ForegroundNearbyPoll(refreshNearby, NEARBY_FOREGROUND_POLL_INTERVAL_MS);
    poll.start();
    return () => poll.stop();
  }, [busy, foreground, nearbyConnectionKey, onRefreshNearby, refreshNearby, showNearby, visible]);

  const save = async () => {
    const intent =
      requestDestination === null
        ? contactSaveIntent(name, destination, editingDestination)
        : {
            destination: requestDestination,
            name,
            selectAfterSave: false,
          };
    const nameError = byteLimitError(name, MAX_CONTACT_NAME_BYTES, "Name");
    if (nameError !== null) {
      setFormError(nameError);
      return;
    }
    if (name.trim().length === 0) {
      setFormError("Name is required");
      return;
    }
    if (!/^[0-9a-f]{32}$/.test(intent.destination)) {
      setFormError("LXMF destination must be exactly 32 hexadecimal characters");
      return;
    }
    setFormError(null);
    if (!(await upsertContact(intent.destination, intent.name, intent.selectAfterSave))) return;
    resetContactForm();
  };

  const peerPreview = (peer: ConversationPeerView): string => {
    const lastMessage = peer.last_message;
    return lastMessage === null
      ? `${peer.message_count} stored message${peer.message_count === 1 ? "" : "s"}`
      : `${lastMessage.direction === "inbound" ? "Received" : "Sent"} · ${
          bytesText(lastMessage.content) || "Empty message"
        }`;
  };

  const contactRows = contacts.map((contact) => {
    const nomadDestination = associatedNomadDestinationForLxmf(nearbyPeers, contact.destination);
    const displayName = contact.name || "Unnamed contact";
    const peer = conversations.find((candidate) => candidate.destination === contact.destination);
    return (
      <View
        key={contact.destination}
        style={[styles.contact, selected === contact.destination && styles.contactActive]}
      >
        <Pressable
          accessibilityLabel={`Open ${displayName}`}
          accessibilityRole="button"
          onPress={() => selectContact(contact.destination)}
          style={({ pressed }) => [styles.contactSelection, pressed && styles.contactPressed]}
        >
          <View style={styles.contactNameRow}>
            <Text numberOfLines={1} style={styles.contactName}>
              {displayName}
            </Text>
            {nomadDestination === null ? null : (
              <View style={styles.relevanceChip}>
                <Text style={styles.relevanceChipText}>Nomad</Text>
              </View>
            )}
          </View>
          {peer === undefined || peer.last_message === null ? null : (
            <Text numberOfLines={1} style={styles.messageRequestPreview}>
              {peerPreview(peer)}
            </Text>
          )}
          <Text selectable style={styles.monospace}>
            {contact.destination}
          </Text>
        </Pressable>
        <View style={styles.contactActions}>
          <Pressable
            accessibilityHint="Changes this phone's local name for the contact"
            accessibilityLabel={`Rename ${displayName}`}
            accessibilityRole="button"
            disabled={busy}
            onPress={() => beginEditingContact(contact)}
            style={({ pressed }) => [
              styles.nearbyPeerButton,
              busy && styles.buttonDisabled,
              pressed && !busy && styles.contactPressed,
            ]}
          >
            <Text style={styles.nearbyPeerAction}>Edit</Text>
          </Pressable>
          {nomadDestination === null ? null : (
            <Pressable
              accessibilityHint="Uses the distinct Nomad destination authenticated in this peer's nearby announce"
              accessibilityLabel={`Browse ${displayName} on NomadNet`}
              accessibilityRole="button"
              disabled={busy}
              onPress={() => onBrowseNomad(nomadDestination)}
              style={({ pressed }) => [
                styles.nearbyPeerButton,
                busy && styles.buttonDisabled,
                pressed && !busy && styles.contactPressed,
              ]}
            >
              <Text style={styles.nearbyPeerAction}>Browse</Text>
            </Pressable>
          )}
        </View>
      </View>
    );
  });

  const unsavedPeerRow = (peer: ConversationPeerView, inboundRequest: boolean) => {
    const displayName = conversationPeerLabel(peer);
    const preview = peerPreview(peer);
    return (
      <View
        key={peer.destination}
        style={[styles.contact, selected === peer.destination && styles.contactActive]}
      >
        <Pressable
          accessibilityLabel={
            inboundRequest
              ? `Open message request from ${displayName}, ${peer.inbound_message_count} message${peer.inbound_message_count === 1 ? "" : "s"}`
              : `Open unsaved conversation with ${displayName}, ${peer.message_count} message${peer.message_count === 1 ? "" : "s"}`
          }
          accessibilityRole="button"
          onPress={() => selectContact(peer.destination)}
          style={({ pressed }) => [styles.contactSelection, pressed && styles.contactPressed]}
        >
          <Text numberOfLines={1} style={styles.contactName}>
            {displayName}
          </Text>
          <Text numberOfLines={1} style={styles.messageRequestPreview}>
            {preview}
          </Text>
          <Text selectable style={styles.monospace}>
            {peer.destination}
          </Text>
        </Pressable>
        <View style={styles.contactActions}>
          <Pressable
            accessibilityHint="Adds a local name without changing the authenticated destination"
            accessibilityLabel={`Save ${displayName} as a contact`}
            accessibilityRole="button"
            disabled={busy}
            onPress={() => beginSavingUnsavedPeer(peer)}
            style={({ pressed }) => [
              styles.nearbyPeerButton,
              busy && styles.buttonDisabled,
              pressed && !busy && styles.contactPressed,
            ]}
          >
            <Text style={styles.nearbyPeerAction}>Save</Text>
          </Pressable>
        </View>
      </View>
    );
  };
  const requestRows = messageRequestPeers(conversations).map((peer) => unsavedPeerRow(peer, true));
  const outboundUnsavedRows = outboundOnlyUnsavedPeers(conversations).map((peer) =>
    unsavedPeerRow(peer, false),
  );

  const conversationRows = (
    <>
      {requestRows.length === 0 ? null : (
        <View style={styles.messageRequestsSection}>
          <Text style={styles.applianceDestinationLabel}>
            MESSAGE REQUESTS ({requestRows.length})
          </Text>
          <Text style={styles.nearbyStatus}>
            Authenticated inbound senders that are not saved as contacts.
          </Text>
          <View style={styles.contacts}>{requestRows}</View>
        </View>
      )}
      {outboundUnsavedRows.length === 0 ? null : (
        <View style={styles.messageRequestsSection}>
          <Text style={styles.applianceDestinationLabel}>
            UNSAVED CONVERSATIONS ({outboundUnsavedRows.length})
          </Text>
          <Text style={styles.nearbyStatus}>
            Outbound history for destinations that are not saved as contacts.
          </Text>
          <View style={styles.contacts}>{outboundUnsavedRows}</View>
        </View>
      )}
      <View style={styles.contacts}>{contactRows}</View>
      {showNearby ? (
        <NearbyPanel
          active={foreground && visible}
          applianceLabel={applianceLabel}
          busy={busy}
          compact={compact}
          connected={readyConnection !== undefined}
          contacts={contacts}
          loadError={nearbyLoadError}
          loaded={nearbyLoaded}
          loading={nearbyLoading}
          onBrowseNomad={onBrowseNomad}
          onRefresh={onRefreshNearby === null ? null : refreshNearby}
          onSelect={selectContact}
          onUpsert={upsertContact}
          peers={nearbyPeers}
          snapshotFetchedAtMs={nearbySnapshotFetchedAtMs}
        />
      ) : null}
    </>
  );

  const sidebarContents = (
    <>
      <View style={styles.sectionHeading}>
        <Text style={styles.heading}>Contacts</Text>
        <View style={styles.sectionActions}>
          <Pressable
            accessibilityLabel={showNearby ? "Hide nearby peers" : "Find nearby peers"}
            accessibilityRole="button"
            onPress={() => {
              resetContactForm();
              setShowNearby((visible) => !visible);
            }}
            style={[styles.smallButton, showNearby && styles.smallButtonActive]}
          >
            <Text style={styles.smallButtonText}>Nearby</Text>
          </Pressable>
          <Pressable
            accessibilityLabel="Add contact manually"
            accessibilityRole="button"
            onPress={beginAddingContact}
            style={styles.addButton}
          >
            <Text style={styles.addButtonText}>+</Text>
          </Pressable>
        </View>
      </View>
      {showForm ? (
        <View style={styles.contactForm}>
          <Text style={styles.contactName}>
            {editingDestination !== null
              ? "Rename contact"
              : requestDestination !== null
                ? "Save conversation peer"
                : "Add contact"}
          </Text>
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
            accessibilityLabel={
              editingDestination === null && requestDestination === null
                ? "LXMF destination"
                : "LXMF destination, fixed for this contact"
            }
            accessibilityState={{
              disabled: busy || editingDestination !== null || requestDestination !== null,
            }}
            autoCapitalize="none"
            autoCorrect={false}
            editable={!busy && editingDestination === null && requestDestination === null}
            maxLength={32}
            onChangeText={setDestination}
            selectTextOnFocus={editingDestination !== null || requestDestination !== null}
            style={[
              styles.input,
              styles.monospaceInput,
              (editingDestination !== null || requestDestination !== null) && styles.inputReadOnly,
            ]}
            value={destination}
          />
          {formError === null ? null : <Text style={styles.inlineError}>{formError}</Text>}
          <View style={styles.actionRow}>
            <ActionButton
              disabled={busy}
              label={
                editingDestination !== null
                  ? "Save name"
                  : requestDestination !== null
                    ? "Add contact"
                    : "Save"
              }
              onPress={() => void save()}
            />
            <ActionButton label="Cancel" onPress={resetContactForm} secondary />
          </View>
        </View>
      ) : null}
      {compact ? (
        conversationRows
      ) : (
        <ScrollView
          contentContainerStyle={styles.contacts}
          keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
          keyboardShouldPersistTaps="handled"
        >
          {conversationRows}
        </ScrollView>
      )}
    </>
  );

  if (compact && !inline) {
    return (
      <Modal animationType="slide" onRequestClose={onClose} transparent visible={visible}>
        <KeyboardAvoidingView
          behavior={KEYBOARD_LAYOUT.avoidingBehavior}
          enabled={KEYBOARD_LAYOUT.avoidingEnabled}
          style={styles.sidebarDrawerBackdrop}
        >
          <Pressable
            accessibilityLabel="Close contacts"
            accessibilityRole="button"
            onPress={onClose}
            style={styles.sidebarDrawerDismiss}
          />
          <SafeAreaView style={styles.sidebarDrawer}>
            <View style={styles.sidebarDrawerHeading}>
              <View>
                <Text style={styles.eyebrow}>MESSAGES</Text>
                <Text style={styles.profileManagerTitle}>Contacts</Text>
              </View>
              <ActionButton label="Done" onPress={onClose} secondary />
            </View>
            <ScrollView
              automaticallyAdjustKeyboardInsets={Platform.OS === "ios"}
              contentContainerStyle={styles.sidebarCompactContent}
              keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
              keyboardShouldPersistTaps="handled"
              ref={drawerScroller}
              style={styles.sidebarCompactScroller}
            >
              {sidebarContents}
            </ScrollView>
          </SafeAreaView>
        </KeyboardAvoidingView>
      </Modal>
    );
  }

  if (compact) {
    return (
      <View style={styles.sidebarInline}>
        <ScrollView
          automaticallyAdjustKeyboardInsets={Platform.OS === "ios"}
          contentContainerStyle={styles.sidebarCompactContent}
          keyboardDismissMode={KEYBOARD_LAYOUT.dismissMode}
          keyboardShouldPersistTaps="handled"
          ref={drawerScroller}
          style={styles.sidebarCompactScroller}
        >
          {sidebarContents}
        </ScrollView>
      </View>
    );
  }

  return <View style={styles.sidebar}>{sidebarContents}</View>;
}
