# App UI: Generic Interfaces, Nodes/People Surfaces, and Layout Rework

## Status (review pass, 2026-08-20)

- Phase 1 — done (brand removal, Chats|Contacts subtab, provider split, Appliances/Settings).
- Phase 2a — done (`DiagnosticInterfaceKind` widened to `LoRa | TcpClient | TcpServer | Other`
  across device-api CBOR + firmware + TS).
- Phase 2b — **deferred** (see "Config/store migration decision" below).
- Phase 3 — done (generic `buildNodeInterfaces` projection + `NodeInterfacesPanel` + Configure deep-links).
- Phase 4 — done (Net Overview reworked to combined status via `buildNodeInterfaces`).
- Phase 5 — done (Chats|Contacts moved against the bottom nav; Nomad relevance chip).
- Phase 6 — done (desktop icon rail + collapsible left People / right Nodes panels).
- Phase 7 — not started (blocked on Rust APIs).

## Config/store migration decision (Phase 2b) — DEFERRED

Do **not** restructure `NetworkConfigView`/`NetworkConfigMutation`/`network-config-store`
into a generic interface list yet. Rationale:

- The E290 has exactly two packet interfaces (LoRa always; TCP client on the gateway
  profile) plus a Wi-Fi link. The flat config models that exactly; a list buys nothing on
  this board.
- The UI is already generic via `buildNodeInterfaces` (a read-only projection). The
  migration changes the source-of-truth shape, not the user-visible UI.
- Highest regression-risk change in the plan (device-api CBOR + NOR-flash journal format +
  firmware + app + TS regen) with zero user-visible benefit today.
- Extension points already exist (tagged `NetworkConfigMutation`, widened
  `DiagnosticInterfaceKind`, transport-neutral `interface-router`).

**Trigger to revisit:** immediately before adding the first non-E290 packet interface
(Ethernet, RNode-over-USB, TCP server, UDP, Auto, or a second LoRa), with a concrete kind
+ config shape in hand.

**Active exception:** an in-progress effort adds a **TCP node directly in the app to
support OTA updates of boards**. That is a phone-local TCP interface, which will need a
phone-side interface store (separate from the board's `network-config-store`) and a
phone-side RNS stack. When that lands, model it as the first "local interface" in the
Nodes surface (`This phone` entry) and treat the board config migration independently —
do not conflate the two stores.

## Decisions (confirmed with user)

- Desktop chrome: slim icon rail + two collapsible side panels (People left, Nodes right).
- Net tab: pure diagnostics activity; all interface settings live in the Nodes surface.
- Link-layer: Wi-Fi station/Ethernet are appliance "links", NOT Reticulum interfaces
  (Python RNS has no Wi-Fi concept; rete config types are the source of truth).
- Breaking device-API + persisted-config change is acceptable (alpha reset, documented).

## Phase 1 — Immediate layout fixes (approved, implement first)

1. **Remove the "Reticulum" brand text** from the compact top bar.
   - Messages tab: left slot becomes a `Chats | Contacts` segmented control.
   - Other tabs: left slot empty; appliance chip + settings gear right-aligned.

2. **Contacts becomes a real subtab** (no modal):
   - Add `MessagePane = "chats" | "contacts"` to `lib/navigation.ts`.
   - `appliance-context.tsx`: replace `mobileSidebarVisible`/`openContacts`/`closeContacts`
     with `messagePane` + `selectMessagePane(pane)` + `messagePaneRef`.
     Update call sites: `navigate` (reset to "chats" when leaving lxmf), notification
     effect (line ~1851), `browseNomad` (line ~1865), notification suppression
     `navigationOverlayVisible: messagePaneRef.current === "contacts"` (line ~1167),
     context value (lines ~1888-1890).
   - `AppTopBar.tsx`: drop `onOpenContacts`/`showContactsButton`; add `messagePane` +
     `onSelectMessagePane`. Compact branch renders the segmented control (shared
     `workspaceSwitcher` styles) when `workspace === "lxmf"`, `<View />` spacer otherwise.
   - `AppShell.tsx`: pass `messagePane`/`selectMessagePane`; remove `openContacts` usage.
   - `app/(tabs)/index.tsx` (MessagesScreen): render `ApplianceSidebar` with new
     `inline={compact}` prop only when `!compact || messagePane === "contacts"`, and
     `ConversationPanel` only when `!compact || messagePane === "chats"`.
     Pass `onClose={() => selectMessagePane("chats")}` and
     `visible={!compact || messagePane === "contacts"}`.
   - `ApplianceSidebar.tsx`: add `inline?: boolean` prop. When `compact && inline`,
     render `sidebarContents` in a plain `View` + `ScrollView` (reuse
     `sidebarCompactScroller`/`sidebarCompactContent`, attach `drawerScroller` ref);
     keep the modal branch for `compact && !inline`; desktop branch unchanged.
   - `appliance-screen-styles.ts`: add `sidebarInline` style (flex:1, minHeight:0,
     background).

3. **Chip opens a bottom sheet instead of a modal** — deferred to Phase 3 (the Nodes
   surface). iOS `presentation: "modal"` is already a page sheet; the real cross-platform
   sheet/panel arrives with the Nodes surface.

## Phase 2 — Generic interface model (Rust, breaking)

- `InterfaceKind` union in device-api/appliance-runtime grounded in rete's existing
  config types (no re-implementation): `lora` (LoRaConfig), `tcp_client`
  (rete-tokio ReconnectingTcpClient), `tcp_server` (TcpServerConfig), `serial_kiss`
  (SerialConfig/Kiss), `auto` (AutoInterfaceConfig), `local_client`/`local_server`
  (LocalServerConfig). Future placeholders only: `udp`, `i2p` (define when rete
  implements them).
- Each interface view: `{ id, kind, name, enabled, state, bitrate, mtu, rx/tx counters,
  announce stats, kind-specific extras }` (mirror RNS `get_interface_stats`).
- Restructure `NetworkConfigView` -> interface list + per-interface mutations, keeping
  whole-config revision CAS. `DiagnosticInterfaceView.kind` widened to the union; LoRa
  diagnostics attach to the `lora` card.
- Links (Wi-Fi station, future Ethernet) remain board config under the appliance; IP
  interfaces show link dependency in status.
- `network-config-store` migration: documented alpha reset (AGENTS.md requires explicit
  migration/reset consequences).
- Regenerate TS (`bun run api:generate`).

## Phase 3 — Nodes surface

- Top-bar chip opens the Nodes surface: bottom sheet on phone, right panel on desktop.
- Lists "This phone" (future local interfaces) + appliances (existing profiles).
- Appliance detail page: combined status strip (X/Y interfaces online, counters,
  announce state), Links (Wi-Fi station), one card per interface (status + Configure ->
  per-kind settings screen). Replaces the current Appliances page + Net settings.

## Phase 4 — Net tab reorg (diagnostics)

- Sub-tabs: Status (interface strip + combined), Routes, Discovery (announces/RMAP),
  Map (future live announce map). All per-interface settings removed from Net.

## Phase 5 — People surface

- Phone: Contacts subtab (Phase 1) grows identity management (trust pinning, identity
  sharing, LXST targets).
- Desktop: collapsible left People panel; rows carry relevance chips (Chat / Nomad /
  Call / Forum), requests, nearby.

## Phase 6 — Desktop chrome

- Slim icon rail (5 destinations + People/Nodes toggles) replacing the labeled
  `navSidebar`; collapsible left People panel + right Nodes panel around content.

## Phase 7 — Future (not now)

- **Phone-local TCP node for board OTA updates** (in progress in another thread) — becomes
  the first "This phone" interface in the Nodes surface. Needs a phone-side RNS stack and
  a phone-side interface store, separate from the board's `network-config-store`.
- Phone-local rete interfaces (Rust stack in native module), generalizing the above.
- Live network map (rns-map style).
- Identity/trust management, storage management, LXSF calls, file transfer, groupchat.

## Verification (after each phase)

- `bun run typecheck`, `bun x biome check`, `bun test ./src`.
- After web-affecting changes: `bun run build:web && bun run assets:check`.
- After Rust changes: `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`,
  `cargo test --locked`, regenerate API bindings and commit generated output.
