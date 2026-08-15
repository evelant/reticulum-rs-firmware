import { describe, expect, test } from "bun:test";

import type { MessageActivityEventView, MessageActivityPageView } from "../generated/api.ts";
import {
  enqueueMessageNotificationTarget,
  type InboundMessageNotification,
  MESSAGE_NOTIFICATION_LEDGER_VERSION,
  type MessageNotificationLedger,
  type MessageNotificationLedgerStore,
  MessageNotificationReconciler,
  messageNotificationIdentifier,
  parseMessageNotificationLedger,
  parseMessageNotificationTarget,
  SupersededMessageNotificationReconciliation,
  shouldPresentInboundMessageNotification,
} from "./message-notifications.ts";

const PROFILE = "653239302d6170692d31aca704e13f88";
const PEER = "00112233445566778899aabbccddeeff";

function inbound(eventId: number, messageId = `${eventId}`.padStart(64, "0")) {
  return {
    event_id: eventId,
    observed_at_unix_ms: 1_700_000_000_000 + eventId,
    timeline_sequence: eventId,
    peer: PEER,
    direction: "inbound",
    outbox_id: null,
    attempt_number: null,
    attempt_location: null,
    ingress_observation: null,
    message_location: null,
    receiver_location: null,
    activity: { kind: "inbound_imported", message_id: messageId },
  } satisfies MessageActivityEventView;
}

function outbound(eventId: number): MessageActivityEventView {
  return {
    event_id: eventId,
    observed_at_unix_ms: 1_700_000_000_000 + eventId,
    timeline_sequence: eventId,
    peer: PEER,
    direction: "outbound",
    outbox_id: eventId,
    attempt_number: null,
    attempt_location: null,
    ingress_observation: null,
    message_location: null,
    receiver_location: null,
    activity: { kind: "outbound_queued" },
  };
}

function page(
  events: MessageActivityEventView[],
  nextBeforeEventId: number | null = null,
): MessageActivityPageView {
  return {
    events,
    history_incomplete: false,
    next_before_event_id: nextBeforeEventId,
  };
}

class MemoryStore implements MessageNotificationLedgerStore {
  ledger: MessageNotificationLedger;
  saves = 0;

  constructor(lastEventId?: number) {
    this.ledger = {
      profiles: lastEventId === undefined ? {} : { [PROFILE]: { lastEventId } },
      version: MESSAGE_NOTIFICATION_LEDGER_VERSION,
    };
  }

  async load(): Promise<MessageNotificationLedger> {
    return this.ledger;
  }

  async save(ledger: MessageNotificationLedger): Promise<void> {
    this.ledger = ledger;
    this.saves += 1;
  }
}

describe("message notification ledger", () => {
  test("fails closed on corrupt state and retains valid profile watermarks", () => {
    expect(parseMessageNotificationLedger("not json")).toEqual({
      profiles: {},
      version: 1,
    });
    expect(
      parseMessageNotificationLedger(
        JSON.stringify({
          profiles: {
            " PROFILE-A ": { lastEventId: 17 },
            invalid: { lastEventId: -1 },
          },
          version: 1,
        }),
      ),
    ).toEqual({
      profiles: { "profile-a": { lastEventId: 17 } },
      version: 1,
    });
  });

  test("validates notification tap data and makes deterministic identifiers", () => {
    expect(
      parseMessageNotificationTarget({
        destination: PEER,
        kind: "lxmf_message",
        messageId: "ab".repeat(32),
        profileKey: ` ${PROFILE.toUpperCase()} `,
      }),
    ).toEqual({
      destination: PEER,
      kind: "lxmf_message",
      messageId: "ab".repeat(32),
      profileKey: PROFILE,
    });
    expect(parseMessageNotificationTarget({ kind: "lxmf_message" })).toBeNull();
    expect(messageNotificationIdentifier(` ${PROFILE.toUpperCase()} `, "AB")).toBe(
      `lxmf:${PROFILE}:ab`,
    );
  });

  test("deduplicates one notification response while preserving later taps", () => {
    const first = {
      destination: PEER,
      kind: "lxmf_message",
      messageId: "aa".repeat(32),
      profileKey: PROFILE,
    } as const;
    const second = { ...first, messageId: "bb".repeat(32) };

    const one = enqueueMessageNotificationTarget([], first);
    expect(enqueueMessageNotificationTarget(one, { ...first })).toBe(one);
    expect(enqueueMessageNotificationTarget(one, second)).toEqual([first, second]);
  });

  test("suppresses only the exact conversation that is actually visible", () => {
    const visible = {
      foreground: true,
      navigationOverlayVisible: false,
      selectedDestination: PEER,
      workspace: "lxmf",
    };
    expect(shouldPresentInboundMessageNotification(PEER, visible)).toBeFalse();
    expect(
      shouldPresentInboundMessageNotification(PEER, {
        ...visible,
        navigationOverlayVisible: true,
      }),
    ).toBeTrue();
    expect(
      shouldPresentInboundMessageNotification(PEER, { ...visible, foreground: false }),
    ).toBeTrue();
    expect(
      shouldPresentInboundMessageNotification(PEER, { ...visible, workspace: "activity" }),
    ).toBeTrue();
    expect(shouldPresentInboundMessageNotification("ff".repeat(16), visible)).toBeTrue();
  });
});

describe("message notification reconciliation", () => {
  test("establishes a first-run baseline without replaying historical messages", async () => {
    const store = new MemoryStore();
    const notified: InboundMessageNotification[] = [];
    const result = await new MessageNotificationReconciler(store).reconcile({
      loadPage: async () => page([inbound(8), outbound(7)]),
      notify: async (notification) => {
        notified.push(notification);
      },
      profileKey: PROFILE,
    });

    expect(result).toEqual({
      baselineEstablished: true,
      lastEventId: 8,
      notificationsPresented: 0,
    });
    expect(notified).toEqual([]);
    expect(store.ledger.profiles[PROFILE]).toEqual({ lastEventId: 8 });
  });

  test("pages to the watermark and presents new inbound events in durable order", async () => {
    const store = new MemoryStore(2);
    const notified: number[] = [];
    const requested: Array<number | null> = [];
    const pages = new Map<number | null, MessageActivityPageView>([
      [null, page([inbound(6), outbound(5)], 5)],
      [5, page([inbound(4), inbound(3)], 3)],
      [3, page([outbound(2)], null)],
    ]);

    const result = await new MessageNotificationReconciler(store).reconcile({
      loadPage: async (cursor) => {
        requested.push(cursor);
        const retained = pages.get(cursor);
        if (retained === undefined) throw new Error(`unexpected cursor ${cursor}`);
        return retained;
      },
      notify: async ({ eventId }) => {
        notified.push(eventId);
      },
      profileKey: PROFILE,
    });

    expect(requested).toEqual([null, 5, 3]);
    expect(notified).toEqual([3, 4, 6]);
    expect(result.lastEventId).toBe(6);
    expect(store.ledger.profiles[PROFILE]).toEqual({ lastEventId: 6 });
  });

  test("retries only the failed presentation after persisting earlier notifications", async () => {
    const store = new MemoryStore(10);
    const reconciler = new MessageNotificationReconciler(store);
    const firstAttempt: number[] = [];
    await expect(
      reconciler.reconcile({
        loadPage: async () => page([inbound(12), inbound(11), outbound(10)]),
        notify: async ({ eventId }) => {
          firstAttempt.push(eventId);
          if (eventId === 12) throw new Error("native scheduler unavailable");
        },
        profileKey: PROFILE,
      }),
    ).rejects.toThrow("native scheduler unavailable");

    expect(firstAttempt).toEqual([11, 12]);
    expect(store.ledger.profiles[PROFILE]).toEqual({ lastEventId: 11 });

    const retry: number[] = [];
    await reconciler.reconcile({
      loadPage: async () => page([inbound(12), inbound(11), outbound(10)]),
      notify: async ({ eventId }) => {
        retry.push(eventId);
      },
      profileKey: PROFILE,
    });
    expect(retry).toEqual([12]);
    expect(store.ledger.profiles[PROFILE]).toEqual({ lastEventId: 12 });
  });

  test("re-baselines instead of replaying when a profile database is recreated", async () => {
    const store = new MemoryStore(40);
    const notified: number[] = [];
    const result = await new MessageNotificationReconciler(store).reconcile({
      loadPage: async () => page([inbound(2), inbound(1)]),
      notify: async ({ eventId }) => {
        notified.push(eventId);
      },
      profileKey: PROFILE,
    });

    expect(result.baselineEstablished).toBeTrue();
    expect(result.lastEventId).toBe(2);
    expect(notified).toEqual([]);
  });

  test("discards a page if its active profile was superseded while loading", async () => {
    const store = new MemoryStore(4);
    const notified: number[] = [];
    let current = true;
    const reconciliation = new MessageNotificationReconciler(store).reconcile({
      isCurrent: () => current,
      loadPage: async () => {
        current = false;
        return page([inbound(5), outbound(4)]);
      },
      notify: async ({ eventId }) => {
        notified.push(eventId);
      },
      profileKey: PROFILE,
    });

    await expect(reconciliation).rejects.toBeInstanceOf(
      SupersededMessageNotificationReconciliation,
    );
    expect(notified).toEqual([]);
    expect(store.saves).toBe(0);
    expect(store.ledger.profiles[PROFILE]).toEqual({ lastEventId: 4 });
  });

  test("forgets only the selected appliance watermark", async () => {
    const store = new MemoryStore(7);
    store.ledger = {
      profiles: {
        [PROFILE]: { lastEventId: 7 },
        other: { lastEventId: 9 },
      },
      version: 1,
    };
    const reconciler = new MessageNotificationReconciler(store);

    await reconciler.forgetProfile(PROFILE.toUpperCase());

    expect(store.ledger.profiles).toEqual({ other: { lastEventId: 9 } });
  });
});
