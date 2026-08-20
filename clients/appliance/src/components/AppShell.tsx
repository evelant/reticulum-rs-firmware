import { Slot, useRouter } from "expo-router";
import { useState } from "react";
import { KeyboardAvoidingView, Text, View } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";

import { useAppliance } from "../lib/appliance-context.tsx";
import { applianceProfilesPresentation } from "../lib/appliance-profiles.ts";
import { applianceStatusPresentation } from "../lib/appliance-status.ts";
import { ApplianceBanners } from "./ApplianceBanners.tsx";
import { ActionButton } from "./AppliancePrimitives.tsx";
import { ApplianceSidebar } from "./ApplianceSidebar.tsx";
import { AppliancesPanel } from "./AppliancesPanel.tsx";
import { AppNavBar } from "./AppNavBar.tsx";
import { AppTopBar } from "./AppTopBar.tsx";
import { APPLIANCE_KEYBOARD_LAYOUT } from "./appliance-screen-layout.ts";
import { styles } from "./appliance-screen-styles.ts";
import { MessageSubtabs } from "./MessageSubtabs.tsx";
import { OnboardingPanel } from "./OnboardingPanel.tsx";

const KEYBOARD_LAYOUT = APPLIANCE_KEYBOARD_LAYOUT;

export function AppShell() {
  const appliance = useAppliance();
  const router = useRouter();
  const [peoplePanelVisible, setPeoplePanelVisible] = useState(true);
  const [nodesPanelVisible, setNodesPanelVisible] = useState(false);
  const {
    addingAppliance,
    bleCandidateScanner,
    browseNomad,
    busy,
    cancelOnboarding,
    chooseContact,
    compact,
    connectivityAvailable,
    contacts,
    conversations,
    deviceName,
    foreground,
    keyboardVisible,
    navigate,
    nearbyReader,
    onboarding,
    onboardingMutation,
    profiles,
    ready,
    selected,
    showSidebar,
    snapshot,
    switchToKnownProfile,
    upsertContact,
    workspace,
  } = appliance;

  const presentation = applianceStatusPresentation(snapshot);
  const activeProfile =
    profiles === null ? null : applianceProfilesPresentation(profiles).activeProfile;
  const chipLabel = deviceName ?? activeProfile?.boardLabel ?? presentation.boardLabel;
  const onOpenAppliances = showSidebar
    ? () => setNodesPanelVisible((visible) => !visible)
    : () => router.navigate("/appliances");
  const onOpenSettings = () => router.navigate("/settings");
  const onTogglePeople = () => setPeoplePanelVisible((visible) => !visible);

  return (
    <SafeAreaView style={styles.safeArea}>
      <AppTopBar
        busy={busy}
        chipLabel={chipLabel}
        compact={compact}
        onOpenAppliances={onOpenAppliances}
        onOpenSettings={onOpenSettings}
        ready={ready}
        snapshot={snapshot}
        workspace={workspace}
      />
      <ApplianceBanners />
      {ready ? (
        <>
          <View style={styles.shell}>
            {showSidebar ? (
              <AppNavBar
                connectivityAvailable={connectivityAvailable}
                nodesPanelVisible={nodesPanelVisible}
                onNavigate={navigate}
                onOpenAppliances={onOpenAppliances}
                onOpenSettings={onOpenSettings}
                onTogglePeople={onTogglePeople}
                peoplePanelVisible={peoplePanelVisible}
                showSidebar
                workspace={workspace}
              />
            ) : null}
            {showSidebar && peoplePanelVisible ? (
              <ApplianceSidebar
                busy={busy}
                compact={false}
                contacts={contacts}
                conversations={conversations}
                foreground={foreground}
                onBrowseNomad={browseNomad}
                onClose={() => {}}
                onRefreshNearby={nearbyReader}
                onSelect={chooseContact}
                onUpsert={upsertContact}
                selected={selected}
                snapshot={snapshot}
                visible
              />
            ) : null}
            <KeyboardAvoidingView
              behavior={KEYBOARD_LAYOUT.avoidingBehavior}
              enabled={KEYBOARD_LAYOUT.avoidingEnabled}
              style={styles.keyboardAvoiding}
            >
              <Slot />
            </KeyboardAvoidingView>
            {showSidebar && nodesPanelVisible ? (
              <View style={styles.nodesPanel}>
                <View style={styles.overlayHeader}>
                  <View>
                    <Text style={styles.eyebrow}>NODES</Text>
                    <Text style={styles.title}>Appliances & interfaces</Text>
                  </View>
                  <ActionButton
                    label="Close"
                    onPress={() => setNodesPanelVisible(false)}
                    secondary
                  />
                </View>
                <AppliancesPanel />
              </View>
            ) : null}
          </View>
          {!showSidebar && !keyboardVisible ? (
            <>
              {workspace === "lxmf" ? <MessageSubtabs /> : null}
              <AppNavBar
                connectivityAvailable={connectivityAvailable}
                nodesPanelVisible={false}
                onNavigate={navigate}
                onOpenAppliances={onOpenAppliances}
                onOpenSettings={onOpenSettings}
                onTogglePeople={onTogglePeople}
                peoplePanelVisible={false}
                showSidebar={false}
                workspace={workspace}
              />
            </>
          ) : null}
        </>
      ) : (
        <OnboardingPanel
          addingAppliance={addingAppliance}
          busy={busy}
          knownProfiles={profiles}
          onboarding={onboarding}
          onCancel={cancelOnboarding}
          onMutation={onboardingMutation}
          onScanBleCandidates={bleCandidateScanner}
          onSwitchKnownProfile={switchToKnownProfile}
        />
      )}
    </SafeAreaView>
  );
}
