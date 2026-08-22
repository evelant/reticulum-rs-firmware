# Enroll and switch appliances

The native app owns one normal Reticulum identity and one persisted PRNS node.
An appliance authorizes that identity through an identified Link during a
short physical-presence window. There is no product-specific device credential,
possession proof, or BLE-only control session.

## Controls

- `RST` restarts the application.
- `BOOT` is GPIO0 and selects the ROM download loader.
- the middle button labelled `21` is GPIO21 and opens physical enrollment.

Holding `BOOT` during reset enters the loader instead of the appliance.

## Enroll a new appliance

1. Fully provision and boot the current PRNS appliance or gateway image.
2. Wait for the display and USB diagnostics to report `READY`, then grant the
   app the platform Bluetooth permissions needed by PRNS Bluetooth Auto.
3. Open **Appliances**, choose **Add appliance**, and select **Refresh
   announces**. The app's PRNS node records bounded management candidates and
   verifies each candidate through its public management path.
4. Match the complete management destination or displayed suffix to the board
   you intend to authorize. Do not select an unverified hash received through
   another channel.
5. Release GPIO21 if it is already held, then hold it continuously for at least
   one second. The board opens one 60-second, single-use enrollment window.
6. Select the candidate and choose **Authorize selected appliance**. Keep the
   board window open while the app establishes a Link, identifies with its
   Reticulum identity, and sends the enrollment request.
7. The board commits that identity hash to its mirrored allow-list before PRNS
   admits it on privileged management and OTA paths. The app verifies the
   authorized path and saves a profile keyed by the management destination.

If the app identity is already authorized, it skips physical enrollment and
verifies the privileged path directly. Up to eight management identities can
be retained by the current product format.

## Switch appliances

Open **Appliances** and select a saved profile. One app-wide PRNS node can reach
any number of appliance destinations; switching changes the active
identity-bound SQLite profile and sync target, not the network engine.

Switching does not transfer contacts, messages, authorization, or Reticulum
identity between boards. **Nearby** lists `lxmf.delivery` destinations observed
through Reticulum; it is not a list of connected Bluetooth devices.

## Recover or revoke access

Bluetooth transport recovery and product authorization are separate. Removing
an operating-system Bluetooth record does not revoke a Reticulum management
identity, and a Bluetooth bond must never be treated as authorization.

The alpha management surface does not yet expose complete revocation UX. Until
that is implemented and powered-qualified, the deliberate development recovery
is a full board erase and app-profile reset. That rotates the appliance
identity, Bluetooth Auto identity, management allow-list, network state,
messages, and PRNS persistence.

## Troubleshooting

If no candidate appears:

- keep the app foregrounded and confirm Bluetooth permission;
- wait for the board to reach `READY`;
- use USB diagnostics to confirm the PRNS node and Bluetooth Auto interface are
  running;
- close another app or host process that may own the same local radio path;
- refresh after the next management announce; and
- verify that all relevant interfaces use compatible Reticulum configuration.

If enrollment is rejected, release GPIO21, perform the continuous one-second
hold again, and retry within 60 seconds. A window is consumed by one successful
new enrollment. An unidentified Link, malformed request, full allow-list,
unavailable product store, or expired window fails closed.

This workflow still requires real iOS and Android powered qualification. A
target build, simulator, or observed advertisement is not evidence that mobile
Bluetooth Auto enrollment works on hardware.
