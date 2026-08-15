import { describe, expect, test } from "bun:test";

import {
  applyLoraPreset,
  draftFromLoraProfile,
  formatFrequencyMhzInput,
  formatLoraBandwidth,
  formatLoraRadioParameters,
  LORA_PROFILE_PRESETS,
  loraProfilesEqual,
  matchingLoraPresetId,
  parseFrequencyMhz,
  parseRmapReticulumConfig,
  validateLoraRadioProfileDraft,
} from "./lora-radio-profile.ts";

const DEFAULT_PROFILE = {
  bandwidth_hz: 125_000,
  coding_rate_denominator: 5,
  frequency_hz: 915_000_000,
  spreading_factor: 7,
  tx_power_dbm: 20,
} as const;

function errorOf(
  result: { readonly error: string; readonly ok: false } | { readonly ok: true },
): string {
  if (result.ok) throw new Error("expected validation to fail");
  return result.error;
}

describe("LoRa radio profile inputs", () => {
  test("converts decimal MHz to exact integer Hz without accepting float syntax", () => {
    expect(parseFrequencyMhz("915")).toEqual({ frequencyHz: 915_000_000, ok: true });
    expect(parseFrequencyMhz(" 915.123456 ")).toEqual({
      frequencyHz: 915_123_456,
      ok: true,
    });
    expect(formatFrequencyMhzInput(915_123_400)).toBe("915.1234");

    expect(parseFrequencyMhz("9.15e2")).toEqual({
      error: "Frequency must be decimal MHz with no sign or exponent and at most 6 decimals",
      ok: false,
    });
    expect(parseFrequencyMhz("+915").ok).toBe(false);
    expect(parseFrequencyMhz("915.1234567").ok).toBe(false);
  });

  test("validates the complete fitted-path channel and discrete board controls", () => {
    expect(validateLoraRadioProfileDraft(draftFromLoraProfile(DEFAULT_PROFILE))).toEqual({
      ok: true,
      profile: DEFAULT_PROFILE,
    });

    expect(
      validateLoraRadioProfileDraft({
        ...draftFromLoraProfile(DEFAULT_PROFILE),
        bandwidthHz: 500_000,
        frequencyMhz: "863.25",
      }).ok,
    ).toBe(true);
    expect(
      validateLoraRadioProfileDraft({
        ...draftFromLoraProfile(DEFAULT_PROFILE),
        bandwidthHz: 500_000,
        frequencyMhz: "863.249999",
      }),
    ).toEqual({
      error: "The complete channel must fit inside this radio's 863–928 MHz fitted path",
      ok: false,
    });

    expect(
      errorOf(
        validateLoraRadioProfileDraft({
          ...draftFromLoraProfile(DEFAULT_PROFILE),
          bandwidthHz: 100_000,
        }),
      ),
    ).toContain("supported LoRa bandwidth");
    expect(
      errorOf(
        validateLoraRadioProfileDraft({
          ...draftFromLoraProfile(DEFAULT_PROFILE),
          spreadingFactor: 6,
        }),
      ),
    ).toContain("7 through 12");
    expect(
      errorOf(
        validateLoraRadioProfileDraft({
          ...draftFromLoraProfile(DEFAULT_PROFILE),
          codingRateDenominator: 9,
        }),
      ),
    ).toContain("4/5 through 4/8");
    expect(
      errorOf(
        validateLoraRadioProfileDraft({
          ...draftFromLoraProfile(DEFAULT_PROFILE),
          txPowerDbm: 21,
        }),
      ),
    ).toContain("supported by this appliance");
  });

  test("rejects tuples whose low-data-rate optimization differs from RNode", () => {
    expect(
      validateLoraRadioProfileDraft({
        ...draftFromLoraProfile(DEFAULT_PROFILE),
        spreadingFactor: 11,
      }),
    ).toEqual({
      error: "BW 125 kHz with SF11 is not yet qualified for RNode interoperability",
      ok: false,
    });
    expect(
      validateLoraRadioProfileDraft({
        ...draftFromLoraProfile(DEFAULT_PROFILE),
        spreadingFactor: 12,
      }).ok,
    ).toBe(true);
  });

  test("formats profiles and applies a preset without silently changing power", () => {
    const draft = applyLoraPreset(
      { ...draftFromLoraProfile(DEFAULT_PROFILE), frequencyMhz: "920", txPowerDbm: 17 },
      LORA_PROFILE_PRESETS[0],
    );
    expect(draft).toEqual({
      bandwidthHz: 125_000,
      codingRateDenominator: 5,
      frequencyMhz: "915",
      spreadingFactor: 7,
      txPowerDbm: 17,
    });
    expect(matchingLoraPresetId(DEFAULT_PROFILE)).toBe("e290-na915-default");
    expect(matchingLoraPresetId({ ...DEFAULT_PROFILE, spreading_factor: 8 })).toBeNull();
    expect(loraProfilesEqual(DEFAULT_PROFILE, { ...DEFAULT_PROFILE })).toBe(true);
    expect(loraProfilesEqual(DEFAULT_PROFILE, { ...DEFAULT_PROFILE, tx_power_dbm: 22 })).toBe(
      false,
    );
    expect(formatLoraBandwidth(41_670)).toBe("41.67 kHz");
    expect(formatLoraRadioParameters(DEFAULT_PROFILE)).toBe(
      "915.000 MHz · BW 125 kHz · SF7 · CR 4/5",
    );
  });
});

describe("RMAP.world Reticulum config import", () => {
  test("parses one whitespace, comment, case, and order-tolerant RNode block", () => {
    const result = parseRmapReticulumConfig(
      `
        # Copied from RMAP.world
        [[Field Test]] ; an interface name
        SpreadingFactor = 8
        TYPE = "rNoDeInTeRfAcE"
        bandwidth=125000
        interface_enabled = Yes
        codingrate = 5 # CR 4/5
        frequency = 915000000
      `,
      20,
    );
    expect(result).toEqual({
      ok: true,
      profile: { ...DEFAULT_PROFILE, spreading_factor: 8 },
      sectionName: "Field Test",
    });
  });

  test("imports optional power and normalizes RNode's rounded bandwidth labels", () => {
    expect(
      parseRmapReticulumConfig(
        `
          [[Narrow test]]
          type=RNodeInterface
          frequency=915000000
          bandwidth=41700
          spreadingfactor=9
          codingrate=8
          txpower=22
        `,
        14,
      ),
    ).toEqual({
      ok: true,
      profile: {
        bandwidth_hz: 41_670,
        coding_rate_denominator: 8,
        frequency_hz: 915_000_000,
        spreading_factor: 9,
        tx_power_dbm: 22,
      },
      sectionName: "Narrow test",
    });
  });

  test("rejects duplicate, incomplete, malformed, and non-RNode input", () => {
    const common = `
      [[Mesh]]
      type=RNodeInterface
      frequency=915000000
      bandwidth=125000
      spreadingfactor=8
      codingrate=5
    `;
    expect(parseRmapReticulumConfig(`${common}\nfrequency=916000000`, 14)).toEqual({
      error: "RMAP key frequency appears more than once",
      ok: false,
    });
    expect(parseRmapReticulumConfig(common.replace("codingrate=5", ""), 14)).toEqual({
      error: "RMAP config is missing required key codingrate",
      ok: false,
    });
    expect(errorOf(parseRmapReticulumConfig(`${common}\nthis is not config`, 14))).toContain(
      "not a valid key = value assignment",
    );
    expect(
      parseRmapReticulumConfig(common.replace("RNodeInterface", "TCPClientInterface"), 14),
    ).toEqual({
      error: "RMAP config type must be RNodeInterface",
      ok: false,
    });
    expect(parseRmapReticulumConfig(`${common}\n[[Second]]`, 14)).toEqual({
      error: "Paste exactly one RNodeInterface block",
      ok: false,
    });
  });

  test("rejects malformed values and unsupported imported tuples before preview", () => {
    const common = `
      [[Mesh]]
      type=RNodeInterface
      frequency=915000000
      bandwidth=125000
      spreadingfactor=8
      codingrate=5
    `;
    expect(
      parseRmapReticulumConfig(common.replace("frequency=915000000", "frequency=915e6"), 14),
    ).toEqual({ error: "frequency must be a non-negative whole number", ok: false });
    expect(errorOf(parseRmapReticulumConfig(`${common}\ntxpower=21`, 14))).toContain(
      "supported by this appliance",
    );
    expect(
      errorOf(
        parseRmapReticulumConfig(common.replace("spreadingfactor=8", "spreadingfactor=11"), 14),
      ),
    ).toContain("not yet qualified for RNode interoperability");
  });
});
