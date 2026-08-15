import { describe, expect, test } from "bun:test";

import {
  BRIDGE_PROTOCOL_VERSION,
  FRAME_INDICATION,
  MAXIMUM_PROFILE_FRAGMENT_BYTES,
  MAX_PRE_READY_INDICATION_FRAMES,
  PreReadyIndicationBuffer,
  decodeWriteFrame,
  encodeIndicationFrame,
  parseBridgeProfile,
  relayWrite,
  sendIndication,
  validateCapabilities,
} from "./bridge-core";

const profile = {
  bridgeProtocol: BRIDGE_PROTOCOL_VERSION,
  gattProfileMajor: 2,
  gattProfileMinor: 3,
  serviceUuid: "f3c8a0b0-5e7a-4c51-a3b9-7d2160d20a02",
  rxUuid: "f3c8a0b1-5e7a-4c51-a3b9-7d2160d20a02",
  txUuid: "f3c8a0b2-5e7a-4c51-a3b9-7d2160d20a02",
  maximumFragmentBytes: 20,
  operationTimeoutMs: 5_000,
  writeType: "with_response",
  txDelivery: "indication",
};

function writeFrame(id: number, value: number[]): ArrayBuffer {
  const bytes = new Uint8Array(6 + value.length);
  bytes[0] = BRIDGE_PROTOCOL_VERSION;
  bytes[1] = 1;
  new DataView(bytes.buffer).setUint32(2, id, false);
  bytes.set(value, 6);
  return bytes.buffer;
}

describe("browser bridge protocol", () => {
  test("accepts a safe write bound up to the generated profile ceiling", () => {
    expect(parseBridgeProfile(profile).maximumFragmentBytes).toBe(20);
    expect(
      parseBridgeProfile({
        ...profile,
        maximumFragmentBytes: MAXIMUM_PROFILE_FRAGMENT_BYTES,
      }).maximumFragmentBytes,
    ).toBe(248);
    expect(() =>
      parseBridgeProfile({ ...profile, maximumFragmentBytes: 249 }),
    ).toThrow();
    expect(() =>
      parseBridgeProfile({ ...profile, writeType: "without_response" }),
    ).toThrow();
  });

  test("validates exact GATT properties", () => {
    expect(() =>
      validateCapabilities({ writeWithResponse: true, indicate: true }),
    ).not.toThrow();
    expect(() =>
      validateCapabilities({ writeWithResponse: false, indicate: true }),
    ).toThrow();
    expect(() =>
      validateCapabilities({ writeWithResponse: true, indicate: false }),
    ).toThrow();
  });

  test("decodes one bounded opaque Rust write", () => {
    const decoded = decodeWriteFrame(writeFrame(7, [1, 2, 3]), 20);
    expect(decoded.id).toBe(7);
    expect([...decoded.value]).toEqual([1, 2, 3]);
    expect(() => decodeWriteFrame(writeFrame(1, []), 20)).toThrow();
    expect(() =>
      decodeWriteFrame(writeFrame(1, new Array(21).fill(0)), 20),
    ).toThrow();
  });

  test("relays with-response writes and acks only after the fake platform resolves", async () => {
    let resolveWrite: (() => void) | undefined;
    const writes: Uint8Array[] = [];
    const controls: string[] = [];
    const pending = relayWrite(
      {
        writeValueWithResponse(value) {
          writes.push(value);
          return new Promise<void>((resolve) => {
            resolveWrite = resolve;
          });
        },
      },
      (control) => controls.push(control),
      writeFrame(9, [0xaa, 0xbb]),
      20,
    );
    await Promise.resolve();
    expect([...writes[0]]).toEqual([0xaa, 0xbb]);
    expect(controls).toEqual([]);
    resolveWrite?.();
    await pending;
    expect(JSON.parse(controls[0])).toEqual({ type: "write_ack", id: 9 });
  });

  test("reports fake platform rejection without retrying bytes", async () => {
    let calls = 0;
    const controls: string[] = [];
    await relayWrite(
      {
        async writeValueWithResponse() {
          calls += 1;
          throw new Error("denied");
        },
      },
      (control) => controls.push(control),
      writeFrame(11, [4, 5]),
      20,
    );
    expect(calls).toBe(1);
    expect(JSON.parse(controls[0])).toEqual({
      type: "write_error",
      id: 11,
      error: "denied",
    });
  });

  test("copies indications and enforces the bounded socket queue", () => {
    const sent: Uint8Array[] = [];
    sendIndication(
      {
        bufferedAmount: 0,
        send(value) {
          sent.push(value);
        },
      },
      new DataView(new Uint8Array([7, 8, 9]).buffer),
      20,
    );
    expect([...sent[0]]).toEqual([
      BRIDGE_PROTOCOL_VERSION,
      FRAME_INDICATION,
      7,
      8,
      9,
    ]);
    expect(() =>
      sendIndication(
        { bufferedAmount: 65_535, send() {} },
        new DataView(new Uint8Array([1]).buffer),
        20,
      ),
    ).toThrow();
    expect(() =>
      encodeIndicationFrame(
        new DataView(new Uint8Array(21).buffer),
        20,
      ),
    ).toThrow();
  });

  test("buffers subscription-race indications and flushes them in order after ready", () => {
    const sent: Uint8Array[] = [];
    const sender = {
      bufferedAmount: 0,
      send(value: Uint8Array) {
        sent.push(value);
      },
    };
    const pending = new PreReadyIndicationBuffer();
    pending.push(
      sender,
      new DataView(new Uint8Array([1, 2]).buffer),
      20,
    );
    pending.push(sender, new DataView(new Uint8Array([3]).buffer), 20);
    expect(sent).toEqual([]);
    pending.markReady(sender);
    expect(sent.map((value) => [...value])).toEqual([
      [BRIDGE_PROTOCOL_VERSION, FRAME_INDICATION, 1, 2],
      [BRIDGE_PROTOCOL_VERSION, FRAME_INDICATION, 3],
    ]);
    pending.push(sender, new DataView(new Uint8Array([4]).buffer), 20);
    expect([...sent[2]]).toEqual([
      BRIDGE_PROTOCOL_VERSION,
      FRAME_INDICATION,
      4,
    ]);
  });

  test("terminates rather than growing the pre-ready indication queue", () => {
    const pending = new PreReadyIndicationBuffer();
    const sender = { bufferedAmount: 0, send() {} };
    for (let index = 0; index < MAX_PRE_READY_INDICATION_FRAMES; index += 1) {
      pending.push(
        sender,
        new DataView(new Uint8Array([index]).buffer),
        20,
      );
    }
    expect(() =>
      pending.push(sender, new DataView(new Uint8Array([0]).buffer), 20),
    ).toThrow();
  });
});
