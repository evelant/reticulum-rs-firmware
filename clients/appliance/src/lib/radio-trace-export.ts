import type { ExportArtifact } from "./export-artifact.ts";

export const RADIO_TRACE_EXPORT_SCHEMA = "org.reticulum.appliance.rf-trace" as const;
export const RADIO_TRACE_EXPORT_SCHEMA_VERSION = 1 as const;
export const RADIO_TRACE_EXPORT_PAGE_SIZE = 100 as const;

export interface RadioTraceExportSource {
  readonly board_label: string;
  readonly device_id: string | null;
  readonly lxmf_delivery_destination: string | null;
  readonly primary_destination: string | null;
  readonly profile_key: string | null;
}

/** Structural page boundary shared by generated RF-trace DTOs. */
export interface RadioTraceExportPage<Event> {
  readonly events: readonly Event[];
  readonly history_incomplete: boolean;
  readonly next_before_event_id: number | null;
}

/** Structural request boundary shared by generated RF-trace DTOs. */
export interface RadioTraceExportPageRequest {
  readonly before_event_id: number | null;
  readonly limit: number;
  readonly timeline_sequence: number | null;
}

export type RadioTracePageReader<Event> = (
  request: RadioTraceExportPageRequest,
) => Promise<RadioTraceExportPage<Event>>;

export interface RadioTraceExportDocument<Event> {
  readonly schema: typeof RADIO_TRACE_EXPORT_SCHEMA;
  readonly schema_version: typeof RADIO_TRACE_EXPORT_SCHEMA_VERSION;
  readonly exported_at_unix_ms: number;
  readonly source: RadioTraceExportSource;
  readonly scope: { readonly timeline_sequence: number | null };
  readonly history_incomplete: boolean;
  /** Oldest-first so sequential RF behavior reads naturally outside the app. */
  readonly events: readonly Event[];
}

export interface RadioTraceCollection<Event> {
  readonly events: readonly Event[];
  readonly historyIncomplete: boolean;
}

/**
 * Read a complete newest-first cursor stream and return it in chronological
 * order. A repeated cursor is a protocol error; exporting partial evidence as
 * if it were complete would be misleading.
 */
export async function collectCompleteRadioTrace<Event>(
  read: RadioTracePageReader<Event>,
  timelineSequence: number | null,
): Promise<RadioTraceCollection<Event>> {
  let beforeEventId: number | null = null;
  let historyIncomplete = false;
  const newestFirst: Event[] = [];
  const observedCursors = new Set<number>();

  for (;;) {
    const page = await read({
      before_event_id: beforeEventId,
      limit: RADIO_TRACE_EXPORT_PAGE_SIZE,
      timeline_sequence: timelineSequence,
    });
    newestFirst.push(...page.events);
    historyIncomplete ||= page.history_incomplete;

    const next = page.next_before_event_id;
    if (next === null) break;
    if (!Number.isSafeInteger(next) || next <= 0) {
      throw new Error("RF trace export received an invalid pagination cursor");
    }
    if (observedCursors.has(next)) {
      throw new Error("RF trace export pagination repeated a cursor");
    }
    observedCursors.add(next);
    beforeEventId = next;
  }

  return { events: newestFirst.reverse(), historyIncomplete };
}

export function createRadioTraceExportDocument<Event>(input: {
  readonly collection: RadioTraceCollection<Event>;
  readonly exportedAtUnixMs: number;
  readonly source: RadioTraceExportSource;
  readonly timelineSequence: number | null;
}): RadioTraceExportDocument<Event> {
  if (!Number.isSafeInteger(input.exportedAtUnixMs) || input.exportedAtUnixMs < 0) {
    throw new RangeError("RF trace export time must be a non-negative safe integer");
  }
  return {
    schema: RADIO_TRACE_EXPORT_SCHEMA,
    schema_version: RADIO_TRACE_EXPORT_SCHEMA_VERSION,
    exported_at_unix_ms: input.exportedAtUnixMs,
    source: input.source,
    scope: { timeline_sequence: input.timelineSequence },
    history_incomplete: input.collection.historyIncomplete,
    events: input.collection.events,
  };
}

function safeFilenamePart(value: string): string {
  const safe = value
    .trim()
    .toLocaleLowerCase()
    .replaceAll(/[^a-z0-9._-]+/g, "-")
    .replaceAll(/^-+|-+$/g, "");
  return safe === "" ? "appliance" : safe.slice(0, 64);
}

function compactUtcTimestamp(unixMs: number): string {
  return new Date(unixMs).toISOString().replaceAll(/[-:]/g, "").replace(".000Z", "Z");
}

function artifactFilename(
  document: RadioTraceExportDocument<unknown>,
  extension: "csv" | "json",
): string {
  const scope =
    document.scope.timeline_sequence === null
      ? "all"
      : `message-${document.scope.timeline_sequence}`;
  return `${[
    "reticulum-rf-trace",
    safeFilenamePart(document.source.board_label),
    scope,
    compactUtcTimestamp(document.exported_at_unix_ms),
  ].join("-")}.${extension}`;
}

export function radioTraceJsonArtifact(
  document: RadioTraceExportDocument<unknown>,
): ExportArtifact {
  return {
    contents: `${JSON.stringify(document, null, 2)}\n`,
    filename: artifactFilename(document, "json"),
    mimeType: "application/json",
  };
}

type CsvScalar = boolean | number | string | null;

function flattenCsvValue(value: unknown, prefix: string, output: Map<string, CsvScalar>): void {
  if (value === null || typeof value === "string" || typeof value === "boolean") {
    output.set(prefix, value);
    return;
  }
  if (typeof value === "number") {
    output.set(prefix, Number.isFinite(value) ? value : String(value));
    return;
  }
  if (Array.isArray(value)) {
    output.set(prefix, JSON.stringify(value));
    return;
  }
  if (typeof value === "object") {
    const entries = Object.entries(value as Readonly<Record<string, unknown>>).sort(
      ([left], [right]) => left.localeCompare(right),
    );
    if (entries.length === 0) output.set(prefix, "{}");
    for (const [key, nested] of entries) {
      flattenCsvValue(nested, prefix === "" ? key : `${prefix}.${key}`, output);
    }
    return;
  }
  output.set(prefix, String(value));
}

function csvCell(value: CsvScalar | undefined): string {
  if (value === null || value === undefined) return "";
  const text = String(value);
  return /[",\r\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

/**
 * Flatten every generated event field without maintaining a second handwritten
 * RF schema in TypeScript. Arrays remain compact JSON values in one CSV cell.
 */
export function radioTraceCsvArtifact(document: RadioTraceExportDocument<unknown>): ExportArtifact {
  const rows = document.events.map((event) => {
    const row = new Map<string, CsvScalar>();
    row.set("export.schema", document.schema);
    row.set("export.schema_version", document.schema_version);
    row.set("export.exported_at_unix_ms", document.exported_at_unix_ms);
    row.set("export.history_incomplete", document.history_incomplete);
    row.set("export.scope.timeline_sequence", document.scope.timeline_sequence);
    flattenCsvValue(document.source, "source", row);
    flattenCsvValue(event, "event", row);
    return row;
  });
  const fixedHeaders = [
    "export.schema",
    "export.schema_version",
    "export.exported_at_unix_ms",
    "export.history_incomplete",
    "export.scope.timeline_sequence",
    "source.board_label",
    "source.device_id",
    "source.primary_destination",
    "source.lxmf_delivery_destination",
    "source.profile_key",
  ];
  const dynamicHeaders = [...new Set(rows.flatMap((row) => [...row.keys()]))]
    .filter((header) => !fixedHeaders.includes(header))
    .sort();
  const headers = [...fixedHeaders, ...dynamicHeaders];
  const lines = [
    headers.map(csvCell).join(","),
    ...rows.map((row) => headers.map((header) => csvCell(row.get(header))).join(",")),
  ];
  return {
    contents: `${lines.join("\r\n")}\r\n`,
    filename: artifactFilename(document, "csv"),
    mimeType: "text/csv",
  };
}
