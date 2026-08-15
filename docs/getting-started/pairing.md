# Pair and switch appliances

The native app pairs directly over BLE. Rust creates and stores the device
credential inside the native owner; TypeScript never receives its secret
bytes. Importing a credential file is a development recovery path, not normal
onboarding.

## Controls

- `RST` restarts the application.
- `BOOT` is GPIO0 and selects the ROM download loader.
- the middle button labelled `21` is GPIO21 and confirms physical presence.

Use GPIO21 only after the app asks for it. Holding `BOOT` during reset enters
the loader instead of pairing mode.

## Pair a new appliance

1. Flash and boot the current appliance or gateway firmware.
2. Wait for the display to reach `READY` and grant the app Bluetooth
   permission.
3. Open **Appliances**, choose **Add appliance**, and scan for nearby boards.
4. Select the board whose displayed suffix matches the advertisement. The app
   never silently chooses the first result.
5. Start pairing. When prompted for presence, hold GPIO21 continuously for at
   least two seconds. The pairing window is deliberately forgiving.
6. Enter the six-digit passkey shown on that board in the operating-system
   Bluetooth prompt.
7. Return to the app and continue. Keep the app foregrounded while it creates
   the appliance credential and reconnects normally.

The board retains both the Bluetooth bond and the device credential. Ordinary
reconnects do not display another passkey.

Repeat the flow to add another appliance. Each profile has an isolated device
credential and SQLite database.

## Switch appliances

Open **Appliances** and select a saved profile. The app closes the current BLE,
session, and database owners before opening the selected profile.

Switching does not transfer contacts, messages, credentials, or Reticulum
identity between boards.

BLE appliance discovery and Reticulum peer discovery are different:

- **Add appliance** finds a nearby board that the phone can control.
- **Nearby** lists `lxmf.delivery` peers learned by the connected board over
  Reticulum.

Choose a peer in **Nearby** to open or save it as a contact. Manual destination
hash entry remains available when no announce is visible.

## Recover a stale or unavailable Bluetooth bond

Use board-only recovery when the retained bond belongs to another phone or an
ordinary reconnect cannot use it. The previous phone is not required.

1. Hold GPIO21 before pressing `RST`.
2. Keep GPIO21 held through reset for at least three seconds, then release it.
3. Wait for the display to show Bluetooth recovery.
4. Forget a stale operating-system Bluetooth entry on the new phone if one is
   present.
5. In the app, choose **Repair Bluetooth** for an existing profile or **Add
   appliance** for a new profile.
6. When the app later asks for physical presence, hold GPIO21 again and finish
   the displayed-passkey flow.

The reset-time hold clears only the board's Bluetooth bond. It preserves the
Reticulum identity, device credentials, network configuration, messages, and
submission journal. The later hold separately authorizes the new connection.

## Troubleshooting

If no board appears:

- keep the app foregrounded and confirm Bluetooth permission;
- confirm the display reached `READY` or the recovery screen;
- reset once without holding `BOOT`;
- close another phone or host process that may own the sole GATT connection;
  and
- confirm the selected app profile is looking for the expected board suffix.

If no passkey appears, release GPIO21 and hold it again after the app reports
that the selected BLE link is open. If credential activation completed but the
normal reconnect did not, use **Repair Bluetooth** instead of creating a
second profile.

A full flash-data erase rotates the node identity, BLE identity, bond, and
device credentials. Remove the corresponding app profile and pair the
appliance as new after such an erase. A board using any other on-device format
must be fully erased and re-paired before installing this image. If the app
also reports an unsupported local database schema, follow the reset procedure
in the [app guide](app.md#reset-incompatible-app-data).
