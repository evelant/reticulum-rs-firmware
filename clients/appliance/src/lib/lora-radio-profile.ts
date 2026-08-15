import type { LoraRadioProfileView } from "../generated/api.ts";

/**
 * Current HT-RA62-HF fitted-path limits. The firmware remains authoritative;
 * these app-side checks exist to make common input mistakes actionable before
 * a compare-and-swap configuration write.
 */
export const LORA_FITTED_PATH_MIN_HZ = 863_000_000;
export const LORA_FITTED_PATH_MAX_HZ = 928_000_000;

export const LORA_BANDWIDTH_OPTIONS_HZ = [
  10_420, 15_630, 20_830, 31_250, 41_670, 62_500, 125_000, 250_000, 500_000,
] as const;
export const LORA_SPREADING_FACTOR_OPTIONS = [7, 8, 9, 10, 11, 12] as const;
export const LORA_CODING_RATE_DENOMINATOR_OPTIONS = [5, 6, 7, 8] as const;
export const LORA_TX_POWER_OPTIONS_DBM = [14, 17, 20, 22] as const;

export interface LoraRadioProfileDraft {
  bandwidthHz: number;
  codingRateDenominator: number;
  frequencyMhz: string;
  spreadingFactor: number;
  txPowerDbm: number;
}

export interface LoraRadioParameters {
  readonly bandwidth_hz: number;
  readonly coding_rate_denominator: number;
  readonly frequency_hz: number;
  readonly spreading_factor: number;
}

export interface LoraProfilePreset {
  readonly description: string;
  readonly id: string;
  readonly label: string;
  readonly parameters: LoraRadioParameters;
}

/**
 * This is a project-qualified compatibility tuple, not a Reticulum standard.
 * Transmit power intentionally remains separate so selecting a preset cannot
 * silently increase it.
 */
export const LORA_PROFILE_PRESETS = [
  {
    description: "Matches the current E290 NA915 appliance default",
    id: "e290-na915-default",
    label: "NA915 default",
    parameters: {
      bandwidth_hz: 125_000,
      coding_rate_denominator: 5,
      frequency_hz: 915_000_000,
      spreading_factor: 7,
    },
  },
] as const satisfies readonly LoraProfilePreset[];

type ProfileValidationResult =
  | { readonly error: string; readonly ok: false }
  | { readonly ok: true; readonly profile: LoraRadioProfileView };

export type RmapConfigImportResult =
  | { readonly error: string; readonly ok: false }
  | {
      readonly ok: true;
      readonly profile: LoraRadioProfileView;
      readonly sectionName: string;
    };

const BANDWIDTH_EDGE_WIDTH_HZ = new Map<number, number>([
  [7_810, 7_813],
  [10_420, 10_417],
  [15_630, 15_625],
  [20_830, 20_834],
  [31_250, 31_250],
  [41_670, 41_667],
  [62_500, 62_500],
  [125_000, 125_000],
  [250_000, 250_000],
  [500_000, 500_000],
]);

const RMAP_BANDWIDTH_ALIASES = new Map<number, number>([
  [7_800, 7_810],
  [10_400, 10_420],
  [15_600, 15_630],
  [20_800, 20_830],
  [41_700, 41_670],
]);

function includesNumber<const Values extends readonly number[]>(
  values: Values,
  candidate: number,
): candidate is Values[number] {
  return values.some((value) => value === candidate);
}

function canonicalBandwidthHz(value: number): number {
  return RMAP_BANDWIDTH_ALIASES.get(value) ?? value;
}

function isUnverifiedRnodeLdroTuple(bandwidthHz: number, spreadingFactor: number): boolean {
  switch (bandwidthHz) {
    case 7_810:
      return spreadingFactor >= 7;
    case 10_420:
    case 15_630:
      return spreadingFactor >= 8;
    case 20_830:
      return spreadingFactor >= 9;
    case 31_250:
    case 41_670:
      return spreadingFactor >= 10;
    case 62_500:
      return spreadingFactor >= 11;
    case 125_000:
      return spreadingFactor === 11;
    case 250_000:
      return spreadingFactor === 12;
    default:
      return false;
  }
}

export function parseFrequencyMhz(
  input: string,
):
  | { readonly error: string; readonly ok: false }
  | { readonly frequencyHz: number; readonly ok: true } {
  const trimmed = input.trim();
  if (!/^\d+(?:\.\d{1,6})?$/.test(trimmed)) {
    return {
      error: "Frequency must be decimal MHz with no sign or exponent and at most 6 decimals",
      ok: false,
    };
  }

  const [whole = "", fraction = ""] = trimmed.split(".");
  const frequencyHz = Number.parseInt(whole, 10) * 1_000_000 + Number(fraction.padEnd(6, "0"));
  if (!Number.isSafeInteger(frequencyHz)) {
    return { error: "Frequency is outside the exact numeric range", ok: false };
  }
  return { frequencyHz, ok: true };
}

export function formatFrequencyMhzInput(frequencyHz: number): string {
  const whole = Math.floor(frequencyHz / 1_000_000);
  const fraction = String(frequencyHz % 1_000_000)
    .padStart(6, "0")
    .replace(/0+$/, "");
  return fraction.length === 0 ? String(whole) : `${whole}.${fraction}`;
}

export function formatLoraBandwidth(bandwidthHz: number): string {
  const kilohertz = bandwidthHz / 1_000;
  return `${Number.isInteger(kilohertz) ? kilohertz : kilohertz.toFixed(2).replace(/0+$/, "").replace(/\.$/, "")} kHz`;
}

export function formatLoraRadioParameters(parameters: LoraRadioParameters): string {
  return `${(parameters.frequency_hz / 1_000_000).toFixed(3)} MHz · BW ${formatLoraBandwidth(parameters.bandwidth_hz)} · SF${parameters.spreading_factor} · CR 4/${parameters.coding_rate_denominator}`;
}

export function draftFromLoraProfile(profile: LoraRadioProfileView): LoraRadioProfileDraft {
  return {
    bandwidthHz: profile.bandwidth_hz,
    codingRateDenominator: profile.coding_rate_denominator,
    frequencyMhz: formatFrequencyMhzInput(profile.frequency_hz),
    spreadingFactor: profile.spreading_factor,
    txPowerDbm: profile.tx_power_dbm,
  };
}

export function loraProfilesEqual(
  left: LoraRadioProfileView,
  right: LoraRadioProfileView,
): boolean {
  return (
    left.frequency_hz === right.frequency_hz &&
    left.bandwidth_hz === right.bandwidth_hz &&
    left.spreading_factor === right.spreading_factor &&
    left.coding_rate_denominator === right.coding_rate_denominator &&
    left.tx_power_dbm === right.tx_power_dbm
  );
}

export function matchingLoraPresetId(parameters: LoraRadioParameters): string | null {
  const preset = LORA_PROFILE_PRESETS.find(
    (candidate) =>
      candidate.parameters.frequency_hz === parameters.frequency_hz &&
      candidate.parameters.bandwidth_hz === parameters.bandwidth_hz &&
      candidate.parameters.spreading_factor === parameters.spreading_factor &&
      candidate.parameters.coding_rate_denominator === parameters.coding_rate_denominator,
  );
  return preset?.id ?? null;
}

export function applyLoraPreset(
  draft: LoraRadioProfileDraft,
  preset: LoraProfilePreset,
): LoraRadioProfileDraft {
  return {
    bandwidthHz: preset.parameters.bandwidth_hz,
    codingRateDenominator: preset.parameters.coding_rate_denominator,
    frequencyMhz: formatFrequencyMhzInput(preset.parameters.frequency_hz),
    spreadingFactor: preset.parameters.spreading_factor,
    txPowerDbm: draft.txPowerDbm,
  };
}

export function validateLoraRadioProfileDraft(
  draft: LoraRadioProfileDraft,
): ProfileValidationResult {
  const frequency = parseFrequencyMhz(draft.frequencyMhz);
  if (!frequency.ok) return frequency;

  const bandwidthHz = canonicalBandwidthHz(draft.bandwidthHz);
  if (!includesNumber(LORA_BANDWIDTH_OPTIONS_HZ, bandwidthHz)) {
    return { error: "Select a supported LoRa bandwidth", ok: false };
  }
  if (!includesNumber(LORA_SPREADING_FACTOR_OPTIONS, draft.spreadingFactor)) {
    return { error: "Spreading factor must be from 7 through 12", ok: false };
  }
  if (!includesNumber(LORA_CODING_RATE_DENOMINATOR_OPTIONS, draft.codingRateDenominator)) {
    return { error: "Coding rate must be from 4/5 through 4/8", ok: false };
  }
  if (!includesNumber(LORA_TX_POWER_OPTIONS_DBM, draft.txPowerDbm)) {
    return { error: "Select a transmit power supported by this appliance", ok: false };
  }

  const edgeWidthHz = BANDWIDTH_EDGE_WIDTH_HZ.get(bandwidthHz);
  if (edgeWidthHz === undefined) {
    return { error: "Select a supported LoRa bandwidth", ok: false };
  }
  const lowerHalfHz = Math.floor(edgeWidthHz / 2);
  const upperHalfHz = edgeWidthHz - lowerHalfHz;
  if (
    frequency.frequencyHz - lowerHalfHz < LORA_FITTED_PATH_MIN_HZ ||
    frequency.frequencyHz + upperHalfHz > LORA_FITTED_PATH_MAX_HZ
  ) {
    return {
      error: `The complete channel must fit inside this radio's ${(LORA_FITTED_PATH_MIN_HZ / 1_000_000).toFixed(0)}–${(LORA_FITTED_PATH_MAX_HZ / 1_000_000).toFixed(0)} MHz fitted path`,
      ok: false,
    };
  }
  if (isUnverifiedRnodeLdroTuple(bandwidthHz, draft.spreadingFactor)) {
    return {
      error: `BW ${formatLoraBandwidth(bandwidthHz)} with SF${draft.spreadingFactor} is not yet qualified for RNode interoperability`,
      ok: false,
    };
  }

  return {
    ok: true,
    profile: {
      bandwidth_hz: bandwidthHz,
      coding_rate_denominator: draft.codingRateDenominator,
      frequency_hz: frequency.frequencyHz,
      spreading_factor: draft.spreadingFactor,
      tx_power_dbm: draft.txPowerDbm,
    },
  };
}

function stripInlineComment(line: string): string {
  let quote: "'" | '"' | null = null;
  for (let index = 0; index < line.length; index += 1) {
    const character = line[index];
    if ((character === '"' || character === "'") && line[index - 1] !== "\\") {
      quote = quote === null ? character : quote === character ? null : quote;
      continue;
    }
    if (quote === null && (character === "#" || character === ";")) {
      return line.slice(0, index);
    }
  }
  return line;
}

function unquoteConfigValue(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed.length < 2) return trimmed;
  const first = trimmed[0];
  const last = trimmed.at(-1);
  if (first === '"' || first === "'") return last === first ? trimmed.slice(1, -1) : null;
  return last === '"' || last === "'" ? null : trimmed;
}

function parseUnsignedConfigInteger(
  key: string,
  value: string,
): { readonly error: string; readonly ok: false } | { readonly ok: true; readonly value: number } {
  const unquoted = unquoteConfigValue(value);
  if (unquoted === null || !/^\d+$/.test(unquoted)) {
    return { error: `${key} must be a non-negative whole number`, ok: false };
  }
  const parsed = Number(unquoted);
  if (!Number.isSafeInteger(parsed)) {
    return { error: `${key} is outside the exact numeric range`, ok: false };
  }
  return { ok: true, value: parsed };
}

export function parseRmapReticulumConfig(
  input: string,
  currentTxPowerDbm: number,
): RmapConfigImportResult {
  let sectionName: string | null = null;
  const values = new Map<string, string>();

  for (const [zeroBasedIndex, sourceLine] of input.replaceAll("\r\n", "\n").split("\n").entries()) {
    const lineNumber = zeroBasedIndex + 1;
    const line = stripInlineComment(sourceLine).trim();
    if (line.length === 0) continue;

    const section = /^\[\[([^[\]]+)\]\]$/.exec(line);
    if (section !== null) {
      const name = section[1]?.trim() ?? "";
      if (name.length === 0) {
        return { error: `RMAP line ${lineNumber} has an empty interface name`, ok: false };
      }
      if (sectionName !== null) {
        return { error: "Paste exactly one RNodeInterface block", ok: false };
      }
      sectionName = name;
      continue;
    }

    const assignment = /^([A-Za-z][A-Za-z0-9_-]*)\s*=\s*(.+)$/.exec(line);
    if (assignment === null) {
      return { error: `RMAP line ${lineNumber} is not a valid key = value assignment`, ok: false };
    }
    if (sectionName === null) {
      return { error: "RMAP config must begin with a [[Name]] interface block", ok: false };
    }
    const key = assignment[1]?.toLowerCase() ?? "";
    const value = assignment[2]?.trim() ?? "";
    if (values.has(key)) {
      return { error: `RMAP key ${key} appears more than once`, ok: false };
    }
    values.set(key, value);
  }

  if (sectionName === null) {
    return { error: "RMAP config must begin with a [[Name]] interface block", ok: false };
  }
  const rawType = values.get("type");
  const interfaceType = rawType === undefined ? null : unquoteConfigValue(rawType);
  if (interfaceType?.toLowerCase() !== "rnodeinterface") {
    return { error: "RMAP config type must be RNodeInterface", ok: false };
  }

  const requiredKeys = ["frequency", "bandwidth", "spreadingfactor", "codingrate"] as const;
  for (const key of requiredKeys) {
    if (!values.has(key)) {
      return { error: `RMAP config is missing required key ${key}`, ok: false };
    }
  }

  const parsed = new Map<string, number>();
  for (const key of [...requiredKeys, "txpower"] as const) {
    const rawValue = values.get(key);
    if (rawValue === undefined) continue;
    const result = parseUnsignedConfigInteger(key, rawValue);
    if (!result.ok) return result;
    parsed.set(key, result.value);
  }

  const frequencyHz = parsed.get("frequency");
  const bandwidthHz = parsed.get("bandwidth");
  const spreadingFactor = parsed.get("spreadingfactor");
  const codingRateDenominator = parsed.get("codingrate");
  if (
    frequencyHz === undefined ||
    bandwidthHz === undefined ||
    spreadingFactor === undefined ||
    codingRateDenominator === undefined
  ) {
    return { error: "RMAP config did not produce a complete LoRa profile", ok: false };
  }

  const validation = validateLoraRadioProfileDraft({
    bandwidthHz: canonicalBandwidthHz(bandwidthHz),
    codingRateDenominator,
    frequencyMhz: formatFrequencyMhzInput(frequencyHz),
    spreadingFactor,
    txPowerDbm: parsed.get("txpower") ?? currentTxPowerDbm,
  });
  if (!validation.ok) return validation;
  return { ok: true, profile: validation.profile, sectionName };
}
