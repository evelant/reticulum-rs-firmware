import type { MessageActivityEventView, MessageActivityPageView } from "../generated/api.ts";

export const MESSAGE_NOTIFICATION_LEDGER_VERSION = 1;
export const MESSAGE_NOTIFICATION_PAGE_SIZE = 100;

export interface MessageNotificationProfileLedger {
  readonly lastEventId: number;
}

export interface MessageNotificationLedger {
  readonly profiles: Readonly<Record<string, MessageNotificationProfileLedger>>;
  readonly version: typeof MESSAGE_NOTIFICATION_LEDGER_VERSION;
}

export interface MessageNotificationLedgerStore {
  load(): Promise<MessageNotificationLedger>;
  save(ledger: MessageNotificationLedger): Promise<void>;
}

export interface InboundMessageNotification {
  readonly eventId: number;
  readonly messageId: string;
  readonly peer: string;
  readonly timelineSequence: number;
}

export interface MessageNotificationReconcileInput {
  readonly isCurrent?: () => boolean;
  readonly loadPage: (beforeEventId: number | null) => Promise<MessageActivityPageView>;
  readonly notify: (notification: InboundMessageNotification) => Promise<void>;
  readonly profileKey: string;
}

export interface MessageNotificationReconcileResult {
  readonly baselineEstablished: boolean;
  readonly lastEventId: number;
  readonly notificationsPresented: number;
}

export interface MessageNotificationTarget {
  readonly destination: string;
  readonly kind: "lxmf_message";
  readonly messageId: string;
  readonly profileKey: string;
}

export interface MessageNotificationPresentationContext {
  readonly foreground: boolean;
  readonly navigationOverlayVisible: boolean;
  readonly selectedDestination: string | null;
  readonly workspace: string;
}

export class SupersededMessageNotificationReconciliation extends Error {
  constructor() {
    super("message notification reconciliation was superseded");
    this.name = "SupersededMessageNotificationReconciliation";
  }
}

const EMPTY_LEDGER: MessageNotificationLedger = {
  profiles: {},
  version: MESSAGE_NOTIFICATION_LEDGER_VERSION,
};

function isSafeEventId(value: unknown): value is number {
  return Number.isSafeInteger(value) && Number(value) >= 0;
}

function normalizeProfileKey(profileKey: string): string {
  return profileKey.trim().toLowerCase();
}

/**
 * Corrupt or incompatible UI-owned state must fail closed. Returning an empty
 * ledger establishes a new baseline instead of surprising the user with every
 * historical message as a notification.
 */
export function parseMessageNotificationLedger(raw: string): MessageNotificationLedger {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return EMPTY_LEDGER;
  }
  if (
    typeof parsed !== "object" ||
    parsed === null ||
    !("version" in parsed) ||
    parsed.version !== MESSAGE_NOTIFICATION_LEDGER_VERSION ||
    !("profiles" in parsed) ||
    typeof parsed.profiles !== "object" ||
    parsed.profiles === null ||
    Array.isArray(parsed.profiles)
  ) {
    return EMPTY_LEDGER;
  }

  const profiles: Record<string, MessageNotificationProfileLedger> = {};
  for (const [profileKey, candidate] of Object.entries(parsed.profiles)) {
    if (
      typeof candidate !== "object" ||
      candidate === null ||
      !("lastEventId" in candidate) ||
      !isSafeEventId(candidate.lastEventId)
    ) {
      continue;
    }
    const normalizedProfileKey = normalizeProfileKey(profileKey);
    if (normalizedProfileKey !== "") {
      profiles[normalizedProfileKey] = { lastEventId: candidate.lastEventId };
    }
  }
  return { profiles, version: MESSAGE_NOTIFICATION_LEDGER_VERSION };
}

export function messageNotificationIdentifier(profileKey: string, messageId: string): string {
  return `lxmf:${normalizeProfileKey(profileKey)}:${messageId.trim().toLowerCase()}`;
}

export function parseMessageNotificationTarget(
  data: Record<string, unknown>,
): MessageNotificationTarget | null {
  if (data.kind !== "lxmf_message") return null;
  const profileKey = typeof data.profileKey === "string" ? data.profileKey.trim() : "";
  const destination = typeof data.destination === "string" ? data.destination.trim() : "";
  const messageId = typeof data.messageId === "string" ? data.messageId.trim() : "";
  if (
    profileKey === "" ||
    profileKey.length > 256 ||
    destination === "" ||
    destination.length > 256 ||
    messageId === "" ||
    messageId.length > 256
  ) {
    return null;
  }
  return {
    destination,
    kind: "lxmf_message",
    messageId,
    profileKey: normalizeProfileKey(profileKey),
  };
}

export function enqueueMessageNotificationTarget(
  queue: readonly MessageNotificationTarget[],
  target: MessageNotificationTarget,
): readonly MessageNotificationTarget[] {
  const duplicate = queue.some(
    (candidate) =>
      candidate.profileKey === target.profileKey &&
      candidate.destination === target.destination &&
      candidate.messageId === target.messageId,
  );
  return duplicate ? queue : [...queue, target];
}

export function shouldPresentInboundMessageNotification(
  peer: string,
  context: MessageNotificationPresentationContext,
): boolean {
  const exactConversationVisible =
    context.foreground &&
    context.workspace === "lxmf" &&
    !context.navigationOverlayVisible &&
    context.selectedDestination === peer;
  return !exactConversationVisible;
}

function newestEventId(events: readonly MessageActivityEventView[]): number {
  let newest = 0;
  for (const event of events) {
    if (isSafeEventId(event.event_id)) newest = Math.max(newest, event.event_id);
  }
  return newest;
}

function notificationFor(event: MessageActivityEventView): InboundMessageNotification | null {
  if (
    event.direction !== "inbound" ||
    event.activity.kind !== "inbound_imported" ||
    !isSafeEventId(event.event_id)
  ) {
    return null;
  }
  return {
    eventId: event.event_id,
    messageId: event.activity.message_id,
    peer: event.peer,
    timelineSequence: event.timeline_sequence,
  };
}

/**
 * Reconciles the Rust-owned durable activity journal into phone-local alerts.
 *
 * The first observation establishes a baseline so enabling notifications does
 * not replay the whole inbox. Subsequent reads page back to the durable
 * watermark, and the watermark is persisted after every successfully
 * presented inbound event. A deterministic native notification identifier
 * supplies a second dedupe layer if the process stops between presentation and
 * persistence.
 *
 * This actor runs only while JavaScript is alive. A later native background
 * PRNS worker should feed the same durable activity journal and ledger contract;
 * this class deliberately makes no locked-phone delivery claim.
 */
export class MessageNotificationReconciler {
  readonly #store: MessageNotificationLedgerStore;
  #tail: Promise<void> = Promise.resolve();

  constructor(store: MessageNotificationLedgerStore) {
    this.#store = store;
  }

  reconcile(input: MessageNotificationReconcileInput): Promise<MessageNotificationReconcileResult> {
    const operation = this.#tail.then(() => this.#reconcile(input));
    this.#tail = operation.then(
      () => undefined,
      () => undefined,
    );
    return operation;
  }

  forgetProfile(profileKey: string): Promise<void> {
    const operation = this.#tail.then(async () => {
      const ledger = await this.#store.load();
      const normalizedProfileKey = normalizeProfileKey(profileKey);
      if (!(normalizedProfileKey in ledger.profiles)) return;
      const profiles = { ...ledger.profiles };
      delete profiles[normalizedProfileKey];
      await this.#store.save({ profiles, version: MESSAGE_NOTIFICATION_LEDGER_VERSION });
    });
    this.#tail = operation.then(
      () => undefined,
      () => undefined,
    );
    return operation;
  }

  async #reconcile(
    input: MessageNotificationReconcileInput,
  ): Promise<MessageNotificationReconcileResult> {
    this.#assertCurrent(input);
    const profileKey = normalizeProfileKey(input.profileKey);
    if (profileKey === "") throw new Error("notification profile key must not be empty");

    let ledger = await this.#store.load();
    this.#assertCurrent(input);
    const firstPage = await input.loadPage(null);
    this.#assertCurrent(input);
    const firstPageNewest = newestEventId(firstPage.events);
    const retained = ledger.profiles[profileKey];

    if (retained === undefined || firstPageNewest < retained.lastEventId) {
      ledger = await this.#saveProfile(ledger, profileKey, firstPageNewest);
      return {
        baselineEstablished: true,
        lastEventId: ledger.profiles[profileKey]?.lastEventId ?? firstPageNewest,
        notificationsPresented: 0,
      };
    }
    if (firstPageNewest === retained.lastEventId) {
      return {
        baselineEstablished: false,
        lastEventId: retained.lastEventId,
        notificationsPresented: 0,
      };
    }

    const events = [...firstPage.events];
    let nextBeforeEventId = firstPage.next_before_event_id;
    let reachedWatermark = events.some((event) => event.event_id <= retained.lastEventId);
    const requestedPages = new Set<number>();
    while (!reachedWatermark && nextBeforeEventId !== null) {
      if (requestedPages.has(nextBeforeEventId)) {
        throw new Error("message activity pagination repeated a cursor");
      }
      requestedPages.add(nextBeforeEventId);
      const page = await input.loadPage(nextBeforeEventId);
      this.#assertCurrent(input);
      events.push(...page.events);
      reachedWatermark = page.events.some((event) => event.event_id <= retained.lastEventId);
      nextBeforeEventId = page.next_before_event_id;
    }

    const unseen = [...new Map(events.map((event) => [event.event_id, event])).values()]
      .filter((event) => isSafeEventId(event.event_id) && event.event_id > retained.lastEventId)
      .sort((left, right) => left.event_id - right.event_id);
    let lastEventId = retained.lastEventId;
    let notificationsPresented = 0;
    for (const event of unseen) {
      this.#assertCurrent(input);
      const notification = notificationFor(event);
      if (notification !== null) {
        await input.notify(notification);
        notificationsPresented += 1;
        ledger = await this.#saveProfile(ledger, profileKey, event.event_id);
      }
      lastEventId = event.event_id;
    }
    if (lastEventId !== (ledger.profiles[profileKey]?.lastEventId ?? retained.lastEventId)) {
      ledger = await this.#saveProfile(ledger, profileKey, lastEventId);
    }

    return {
      baselineEstablished: false,
      lastEventId: ledger.profiles[profileKey]?.lastEventId ?? lastEventId,
      notificationsPresented,
    };
  }

  #assertCurrent(input: MessageNotificationReconcileInput): void {
    if (input.isCurrent?.() === false) {
      throw new SupersededMessageNotificationReconciliation();
    }
  }

  async #saveProfile(
    ledger: MessageNotificationLedger,
    profileKey: string,
    lastEventId: number,
  ): Promise<MessageNotificationLedger> {
    const next: MessageNotificationLedger = {
      profiles: {
        ...ledger.profiles,
        [profileKey]: { lastEventId },
      },
      version: MESSAGE_NOTIFICATION_LEDGER_VERSION,
    };
    await this.#store.save(next);
    return next;
  }
}
