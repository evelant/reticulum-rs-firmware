import type { NativeProfileStoreSnapshot } from "@reticulum/appliance-native";
import { useEffect, useState } from "react";
import {
  ActivityIndicator,
  Modal,
  Platform,
  Pressable,
  ScrollView,
  Text,
  View,
} from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import type { ApplianceSnapshot } from "../generated/api.ts";
import {
  type ApplianceProfilePresentation,
  applianceProfilesPresentation,
} from "../lib/appliance-profiles.ts";
import { applianceStatusPresentation, connectionStateLabel } from "../lib/appliance-status.ts";
import type { NativeCoreStatus } from "../lib/native-core-types.ts";
import { ActionButton, MetaRow } from "./AppliancePrimitives.tsx";
import { styles } from "./appliance-screen-styles.ts";

export type ProfileOperation =
  | { readonly state: "idle" }
  | { readonly message: string; readonly state: "switching" }
  | { readonly message: string; readonly state: "success" }
  | { readonly message: string; readonly state: "error" };

type ProfileConfirmation =
  | {
      readonly action: "switch";
      readonly profile: ApplianceProfilePresentation;
    }
  | {
      readonly action: "forget";
      readonly profile: ApplianceProfilePresentation;
    };

interface ApplianceProfileManagerProps {
  readonly busy: boolean;
  readonly canAdd: boolean;
  readonly catalog: NativeProfileStoreSnapshot;
  readonly onActivate: (profileKey: string) => Promise<boolean>;
  readonly onAdd: () => void;
  readonly onClearOperation: () => void;
  readonly onClose: () => void;
  readonly onForget: ((profileKey: string) => Promise<boolean>) | null;
  readonly onReconnect: () => Promise<boolean>;
  readonly operation: ProfileOperation;
  readonly visible: boolean;
}

function ApplianceProfileManager({
  busy,
  canAdd,
  catalog,
  onActivate,
  onAdd,
  onClearOperation,
  onClose,
  onForget,
  onReconnect,
  operation,
  visible,
}: ApplianceProfileManagerProps) {
  const [confirmation, setConfirmation] = useState<ProfileConfirmation | null>(null);
  const presentation = applianceProfilesPresentation(catalog);
  const operating = operation.state === "switching";

  useEffect(() => {
    if (!visible) setConfirmation(null);
  }, [visible]);

  const confirmOperation = async () => {
    if (confirmation === null) return;
    const completed =
      confirmation.action === "switch"
        ? await onActivate(confirmation.profile.profileKey)
        : await onForget?.(confirmation.profile.profileKey);
    if (completed) setConfirmation(null);
  };

  return (
    <Modal
      animationType="slide"
      onRequestClose={() => {
        if (!operating) onClose();
      }}
      presentationStyle="pageSheet"
      transparent={false}
      visible={visible}
    >
      <SafeAreaView style={styles.profileManagerSafeArea}>
        <View style={styles.profileManagerHeading}>
          <View style={styles.profileManagerHeadingCopy}>
            <Text style={styles.eyebrow}>APPLIANCES</Text>
            <Text style={styles.profileManagerTitle}>Choose a Reticulum node</Text>
          </View>
          <ActionButton disabled={operating} label="Done" onPress={onClose} secondary />
        </View>
        <ScrollView
          contentContainerStyle={styles.profileManagerContent}
          style={styles.profileManagerScroller}
        >
          <Text style={styles.secondaryText}>
            Each appliance keeps isolated local contacts, conversations, and durable outbox state.
          </Text>
          {operation.state === "idle" ? null : (
            <View
              accessibilityLiveRegion={operation.state === "error" ? "assertive" : "polite"}
              style={[
                styles.profileOperation,
                operation.state === "error" && styles.profileOperationError,
                operation.state === "success" && styles.profileOperationSuccess,
              ]}
            >
              {operating ? <ActivityIndicator color="#91e6a7" /> : null}
              <Text
                style={[
                  styles.profileOperationText,
                  operation.state === "error" && styles.profileOperationErrorText,
                ]}
              >
                {operation.message}
              </Text>
            </View>
          )}
          <View style={styles.profileList}>
            {presentation.profiles.map((profile) => {
              return (
                <View
                  accessibilityLabel={`${profile.active ? "Active" : "Saved"} appliance ${profile.boardLabel}`}
                  key={profile.profileKey}
                  style={[styles.profileRow, profile.active && styles.profileRowActive]}
                >
                  <View style={styles.profileRowHeading}>
                    <Text selectable style={styles.profileBoardLabel}>
                      {profile.boardLabel}
                    </Text>
                    <Text
                      style={[styles.profileBadge, profile.active && styles.profileBadgeActive]}
                    >
                      {profile.active ? "ACTIVE" : "SAVED"}
                    </Text>
                  </View>
                  <Text selectable style={styles.monospace}>
                    {profile.managementDestination}
                  </Text>
                  <Text style={styles.profileGeneration}>LXMF {profile.lxmfDestination}</Text>
                  {profile.active ? (
                    <>
                      <View style={styles.profileRowActions}>
                        <ActionButton
                          disabled={busy || operating}
                          label="Reconnect"
                          onPress={() => {
                            onClearOperation();
                            void onReconnect();
                          }}
                          secondary
                        />
                      </View>
                      <Text style={styles.profileGeneration}>
                        Switch to another appliance before forgetting this active profile.
                      </Text>
                    </>
                  ) : (
                    <View style={styles.profileRowActions}>
                      <ActionButton
                        disabled={busy || operating}
                        label="Switch"
                        onPress={() => {
                          onClearOperation();
                          setConfirmation({ action: "switch", profile });
                        }}
                      />
                      {onForget === null ? null : (
                        <ActionButton
                          disabled={busy || operating}
                          label="Forget"
                          onPress={() => {
                            onClearOperation();
                            setConfirmation({ action: "forget", profile });
                          }}
                          secondary
                        />
                      )}
                    </View>
                  )}
                </View>
              );
            })}
          </View>
          {confirmation === null ? null : (
            <View accessibilityLiveRegion="polite" style={styles.profileConfirmation}>
              <Text style={styles.profileConfirmationTitle}>
                {confirmation.action === "switch"
                  ? `Switch to ${confirmation.profile.boardLabel}?`
                  : `Forget ${confirmation.profile.boardLabel} from this phone?`}
              </Text>
              <Text style={styles.secondaryText}>
                {confirmation.action === "switch"
                  ? "The current connection will close. Profile-local messages and contacts stay isolated, and any unsent composer text will be discarded."
                  : "This permanently deletes this phone's local messages, contacts, and outbox for this appliance. It does not revoke this app's Reticulum identity from the board allow-list."}
              </Text>
              <View style={styles.actionRow}>
                <ActionButton
                  disabled={busy || operating}
                  label={
                    operating
                      ? confirmation.action === "switch"
                        ? "Switching…"
                        : "Forgetting…"
                      : confirmation.action === "switch"
                        ? "Switch appliance"
                        : "Delete local data"
                  }
                  onPress={() => void confirmOperation()}
                />
                <ActionButton
                  disabled={operating}
                  label={confirmation.action === "switch" ? "Keep current" : "Keep appliance"}
                  onPress={() => setConfirmation(null)}
                  secondary
                />
              </View>
            </View>
          )}
          <View style={styles.profileAddSection}>
            <Text style={styles.profileConfirmationTitle}>Another physical node</Text>
            <Text style={styles.secondaryText}>
              Select a verified management announce and authorize this app-owned Reticulum identity.
            </Text>
            <View style={styles.actionRow}>
              <ActionButton
                disabled={busy || operating || !canAdd}
                label="Add appliance"
                onPress={() => {
                  onClearOperation();
                  onClose();
                  onAdd();
                }}
              />
            </View>
            {canAdd ? null : (
              <Text style={styles.profileGeneration}>
                Adding is unavailable on this transport; saved profiles can still be switched.
              </Text>
            )}
          </View>
        </ScrollView>
      </SafeAreaView>
    </Modal>
  );
}

interface ApplianceStatusCardProps {
  readonly busy: boolean;
  readonly canAddAppliance: boolean;
  readonly compact: boolean;
  readonly applianceLabel: string | null;
  readonly nativeCore: NativeCoreStatus | null;
  readonly onActivateProfile: (profileKey: string) => Promise<boolean>;
  readonly onAddAppliance: () => void;
  readonly onClearProfileOperation: () => void;
  readonly onForgetProfile: ((profileKey: string) => Promise<boolean>) | null;
  readonly onReconnect: () => Promise<boolean>;
  readonly onSync: () => void;
  readonly profileOperation: ProfileOperation;
  readonly profiles: NativeProfileStoreSnapshot | null;
  readonly snapshot: ApplianceSnapshot | null;
}

export function ApplianceStatusCard({
  busy,
  canAddAppliance,
  compact,
  applianceLabel,
  nativeCore,
  onActivateProfile,
  onAddAppliance,
  onClearProfileOperation,
  onForgetProfile,
  onReconnect,
  onSync,
  profileOperation,
  profiles,
  snapshot,
}: ApplianceStatusCardProps) {
  const [showDetails, setShowDetails] = useState(false);
  const [showProfiles, setShowProfiles] = useState(false);
  const presentation = applianceStatusPresentation(snapshot);
  const activeProfile =
    profiles === null ? null : applianceProfilesPresentation(profiles).activeProfile;
  const connectionReady = snapshot?.connection.state === "ready";
  const nativeApiLabel =
    nativeCore?.label ?? (Platform.OS === "web" ? "Web client" : "Checking native bridge");

  return (
    <View style={[styles.applianceStatusCard, compact && styles.applianceStatusCardCompact]}>
      <View
        style={[styles.applianceStatusHeading, compact && styles.applianceStatusHeadingCompact]}
      >
        <View
          style={[styles.applianceStatusIdentity, compact && styles.applianceStatusIdentityCompact]}
        >
          {compact ? null : <Text style={styles.eyebrow}>APPLIANCE STATUS</Text>}
          <Text
            numberOfLines={compact ? 1 : undefined}
            selectable
            style={[styles.applianceStatusBoard, compact && styles.applianceStatusBoardCompact]}
          >
            {applianceLabel ?? activeProfile?.boardLabel ?? presentation.boardLabel}
          </Text>
          <Text
            accessibilityLiveRegion="polite"
            style={[
              styles.applianceStatusConnection,
              compact && styles.applianceStatusConnectionCompact,
              presentation.tone === "ready" && styles.applianceStatusConnectionReady,
              presentation.tone === "faulted" && styles.applianceStatusConnectionFaulted,
            ]}
          >
            {compact ? connectionStateLabel(snapshot?.connection) : presentation.connectionLabel}
          </Text>
        </View>
        <View
          style={[styles.applianceStatusActions, compact && styles.applianceStatusActionsCompact]}
        >
          {compact && !connectionReady ? (
            <Pressable
              accessibilityLabel="Reconnect to active appliance"
              accessibilityRole="button"
              accessibilityState={{ disabled: busy }}
              disabled={busy}
              hitSlop={7}
              onPress={() => void onReconnect()}
              style={({ pressed }) => [
                styles.statusDetailsButton,
                styles.statusDetailsButtonCompact,
                busy && styles.buttonDisabled,
                pressed && !busy && styles.buttonPressed,
              ]}
            >
              <Text style={styles.statusDetailsButtonText}>Reconnect</Text>
            </Pressable>
          ) : null}
          {profiles === null ? null : (
            <Pressable
              accessibilityLabel="Manage saved appliances"
              accessibilityRole="button"
              accessibilityState={{ disabled: busy }}
              disabled={busy}
              hitSlop={7}
              onPress={() => {
                onClearProfileOperation();
                setShowProfiles(true);
              }}
              style={({ pressed }) => [
                styles.statusDetailsButton,
                compact && styles.statusDetailsButtonCompact,
                busy && styles.buttonDisabled,
                pressed && !busy && styles.buttonPressed,
              ]}
            >
              <Text style={styles.statusDetailsButtonText}>{compact ? "Nodes" : "Appliances"}</Text>
            </Pressable>
          )}
          <Pressable
            accessibilityLabel={`${showDetails ? "Hide" : "Show"} appliance diagnostics`}
            accessibilityRole="button"
            accessibilityState={{ expanded: showDetails }}
            hitSlop={7}
            onPress={() => setShowDetails((visible) => !visible)}
            style={({ pressed }) => [
              styles.statusDetailsButton,
              compact && styles.statusDetailsButtonCompact,
              pressed && styles.buttonPressed,
            ]}
          >
            <Text style={styles.statusDetailsButtonText}>
              {showDetails ? (compact ? "Less" : "Hide details") : "Details"}
            </Text>
          </Pressable>
        </View>
      </View>
      {!compact || showDetails ? (
        <>
          <View style={styles.applianceActivity}>
            <Text style={styles.applianceActivityItem}>{presentation.pendingOutboxLabel}</Text>
            <Text style={styles.applianceActivitySeparator}>·</Text>
            <Text style={styles.applianceActivityItem}>{presentation.importedThisRunLabel}</Text>
            <Text style={styles.applianceActivitySeparator}>·</Text>
            <Text style={styles.applianceActivityItem}>{presentation.contactCountLabel}</Text>
          </View>
          <View style={styles.applianceDestination}>
            <Text style={styles.applianceDestinationLabel}>LOCAL LXMF</Text>
            <Text selectable style={styles.applianceDestinationValue}>
              {presentation.lxmfDestination ?? "Not available"}
            </Text>
          </View>
        </>
      ) : null}
      {showDetails ? (
        <View style={styles.applianceStatusDetails}>
          <MetaRow label="Endpoint" value={presentation.endpoint ?? "Not connected"} />
          <MetaRow label="Primary" value={presentation.primaryDestination ?? "Not available"} />
          <MetaRow label="Device ID" value={presentation.deviceId ?? "Not available"} />
          <MetaRow label="API" value={nativeApiLabel} />
          {compact ? (
            <View style={styles.statusUtilityActions}>
              <ActionButton disabled={busy} label="Sync" onPress={onSync} secondary />
              <ActionButton
                disabled={busy}
                label="Reconnect"
                onPress={() => void onReconnect()}
                secondary
              />
            </View>
          ) : null}
        </View>
      ) : null}
      {profiles === null ? null : (
        <ApplianceProfileManager
          busy={busy}
          canAdd={canAddAppliance}
          catalog={profiles}
          onActivate={onActivateProfile}
          onAdd={onAddAppliance}
          onClearOperation={onClearProfileOperation}
          onClose={() => {
            setShowProfiles(false);
            onClearProfileOperation();
          }}
          onForget={onForgetProfile}
          onReconnect={onReconnect}
          operation={profileOperation}
          visible={showProfiles}
        />
      )}
    </View>
  );
}
