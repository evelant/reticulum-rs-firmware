# Pair and switch appliances

The normal native setup path is fileless BLE onboarding. The board creates the
device credential inside the native Rust owner; TypeScript never handles its
secret bytes. Importing an `.rdpkey` remains a development/recovery fallback,
not the expected first-run flow.

These instructions describe the physically qualified iOS path. The same UI is
available on Android, but Android BLE hardware remains unqualified.

## Before starting

- Flash the current [`ble-api-proof` E290 image](firmware-e290.md).
- Keep a 915 MHz antenna attached and wait for the display to leave
  `STARTING`.
- Install a native app build; Expo Go and the web target cannot perform this
  BLE flow.
- Grant the app Bluetooth permission and keep it in the foreground.

The E290 has three relevant controls:

- `RST` restarts the application;
- `BOOT` is GPIO0 and is used only for ROM download mode; and
- the middle button labelled `21` is GPIO21 and confirms pairing presence.

## Pair the first board

1. Open **Add appliance**.
2. Choose **Find nearby boards**.
3. Select the intended `reticulum-pair-…` advertisement. A board without a
   durable Bluetooth bond reserves this recovery-only name; its six-character
   suffix is the one shown on the display. The app never auto-selects the first
   board.
4. Release GPIO21 before choosing **Start pairing**.
5. When the selected BLE link is open and the app asks for presence, hold the
   middle GPIO21 button continuously for at least two seconds. A three-second
   hold is fine; there is no narrow start-time requirement.
6. For a new Bluetooth bond, enter the six digits shown on that board in the
   operating-system Bluetooth prompt.
7. Return to the app and choose **Continue after holding GPIO21**. Do not wait
   for the board to say `READY - OPEN APP` first; that display state appears
   only after the following application credential exchange completes.
8. Keep the app foregrounded and the board nearby while it activates the
   credential and reconnects as a normal authenticated appliance.

The application pairing window remains open for five minutes after the
recognized hold. After the first authenticated app session succeeds, the board
returns to its normal `reticulum-e290-…` name. Reboots later reuse both the
durable Bluetooth bond and the device credential, so normal reconnect does not
show another code.

## Add a second board

Open **Appliances**, choose **Add appliance**, and repeat the same flow for the
other advertisement. Each board receives an isolated app-private credential
profile and SQLite database.

If a discovery name exactly matches a saved profile, the app offers to switch
to it instead of starting a second pairing ceremony for the same board.

## Switch boards

Open **Appliances** and select the saved profile. The app closes the current
BLE/database owner before opening the selected one, then scans for that
profile's exact E290 advertisement and authenticates it.

Switching does not transfer contacts, message databases, device credentials,
or Reticulum private identity between boards.

## Find Reticulum contacts

Appliance discovery and Reticulum peer discovery are separate:

- **Find nearby boards** uses phone BLE only to select and authorize an
  appliance.
- **Nearby** in the messaging UI asks the connected appliance for authenticated
  `lxmf.delivery` announces it learned over Reticulum.

Choose a Reticulum peer from **Nearby** to add or open the contact. Manual
destination-hash entry remains available as an advanced fallback.

## Recovery

### No board appears

- Confirm the board display reached `READY`.
- Confirm Bluetooth permission and keep the app foregrounded.
- Reboot the board once with `RST`; do not hold `BOOT`.
- Ensure another phone or host process is not holding the GATT connection.

If the board remains attached to a phone you cannot access, use the
[board-only Bluetooth recovery](#replace-an-unavailable-phones-bluetooth-bond)
below. You do not need the previous phone to release or forget the board.

### Replace an unavailable phone's Bluetooth bond

Use this when the board's one retained Bluetooth bond belongs to another phone,
or when that bond is stale and ordinary reconnect cannot recover it:

1. Hold the middle GPIO21 button before pressing `RST`.
2. Keep GPIO21 held continuously while the board resets, for at least three
   seconds total.
3. Release GPIO21. The firmware clears only its retained Bluetooth bond and
   continues booting.
4. Wait for the board to finish booting and show
   `BLE RECOVERY - OPEN APP`. Keep GPIO21 released while the app finds the
   `reticulum-pair-…` advertisement and opens the board.
5. If this phone already lists the board in iOS or Android Bluetooth settings,
   forget or unpair that stale operating-system entry.
6. In the app, use **Repair Bluetooth** for an existing appliance profile, or
   **Add appliance** if this phone has no profile for the board.
7. When the app asks for physical presence, hold GPIO21 again and complete the
   displayed-code flow.

The reset-time hold and the later pairing hold are deliberately separate. The
first authorizes deletion of the board's old Bluetooth bond; it does not leave
pairing authorized for whichever phone connects next. Ordinary saved profiles
target only `reticulum-e290-…`, so they cannot consume the board's sole BLE
slot while it advertises `reticulum-pair-…` for explicit recovery.

This recovery does **not** factory-reset the appliance. It preserves the
Reticulum identity and destinations, application credentials, Wi-Fi and TCP
configuration, messages, journals, and app data stored on the board. The
previous phone is not required, although its cached Bluetooth bond will no
longer connect after the board accepts a replacement.

### The code never appears

- Release GPIO21, then hold it continuously for at least two seconds only after
  the app has opened the selected device.
- Confirm you held the middle `21` button, not `BOOT`.
- Keep the app on the pairing screen; the five-minute application window is
  intentionally forgiving.

### Pairing completed but reconnect is slow

Leave the app foregrounded. The board deliberately keeps the
`reticulum-pair-…` name through the replacement phone's first authenticated app
session, then returns to `reticulum-e290-…`. Use **Repair Bluetooth** again if
that complete sequence did not finish; do not begin a separate credential
pairing ceremony while activation is being committed.

### The board was factory-reset

A factory reset rotates the node identity and BLE address. Remove the obsolete
peripheral from the operating-system Bluetooth settings, remove or replace the
old app profile, and pair the freshly provisioned board as a new appliance.

For protocol ownership, security boundaries, and the remaining negative-test
matrix, see [ADR 0019](../adr/0019-secure-ble-appliance-onboarding.md).
