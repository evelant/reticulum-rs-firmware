use std::{env, fs, path::PathBuf};

const LAB_RX_ENV: &[&str] = &[
    "RETICULUM_LAB_RX_FREQUENCY_HZ",
    "RETICULUM_LAB_RX_SPREADING_FACTOR",
    "RETICULUM_LAB_RX_BANDWIDTH_HZ",
    "RETICULUM_LAB_RX_CODING_RATE_DENOMINATOR",
    "RETICULUM_LAB_RX_PREAMBLE_SYMBOLS",
    "RETICULUM_LAB_RX_EXPLICIT_HEADER",
    "RETICULUM_LAB_RX_CRC",
    "RETICULUM_LAB_RX_IQ_INVERTED",
];
const LAB_RX_BACKPRESSURE_STALL_ENV: &str = "RETICULUM_LAB_RX_BACKPRESSURE_STALL_US";
const LAB_RX_REGULATOR_ENV: &str = "RETICULUM_LAB_RX_REGULATOR";
const LAB_RX_GAIN_ENV: &str = "RETICULUM_LAB_RX_GAIN";
const LAB_RX_RETURNED_FAULT_TRIGGER_ENV: &str = "RETICULUM_LAB_RX_RETURNED_FAULT_TRIGGER";
const LAB_RX_RETURNED_FAULT_POLICY_ENV: &str = "RETICULUM_LAB_RX_RETURNED_FAULT_POLICY";
const RESET_JOURNAL_SLOT_ENV: &str = "RETICULUM_LAB_RX_RESET_JOURNAL_SLOT";
const RESET_JOURNAL_WORD_ENV: &str = "RETICULUM_LAB_RX_RESET_JOURNAL_WORD";
const RESET_JOURNAL_WRITE_ORDINAL_ENV: &str = "RETICULUM_LAB_RX_RESET_JOURNAL_WRITE_ORDINAL";
const ESP_RTOS_MAIN_STACK_PATCH_ID: &str = "esp-rtos-0.3.0-cpu0-cpu1-main-stack-words-v2";

fn main() {
    require_esp_rtos_main_stack_patch();

    // esp-hal supplies linkall.x for the selected ESP32-S3 runtime.
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    println!("cargo:rustc-check-cfg=cfg(reticulum_lab_rx_regulator, values(\"ldo\", \"dcdc\"))");
    println!(
        "cargo:rustc-check-cfg=cfg(reticulum_lab_rx_gain, values(\"unboosted\", \"boosted\"))"
    );
    println!(
        "cargo:rustc-check-cfg=cfg(reticulum_lab_rx_returned_fault_policy, values(\"one-boot\", \"repeat-until-quarantine\"))"
    );

    let safe_idle = env::var_os("CARGO_FEATURE_SAFE_IDLE").is_some();
    let lab_rx = env::var_os("CARGO_FEATURE_LAB_RX").is_some();
    let lab_rx_backpressure = env::var_os("CARGO_FEATURE_LAB_RX_BACKPRESSURE").is_some();
    let lab_rx_electrical = env::var_os("CARGO_FEATURE_LAB_RX_ELECTRICAL_HIL").is_some();
    let lab_rx_returned_fault = env::var_os("CARGO_FEATURE_LAB_RX_RETURNED_FAULT_HIL").is_some();
    let reset_journal_corrupt =
        env::var_os("CARGO_FEATURE_LAB_RX_RESET_JOURNAL_CORRUPT_HIL").is_some();
    let reset_journal_torn = env::var_os("CARGO_FEATURE_LAB_RX_RESET_JOURNAL_TORN_HIL").is_some();
    assert!(
        !(reset_journal_corrupt && reset_journal_torn),
        "select exactly one reset-journal HIL mutation mode"
    );
    let reset_journal_hil = reset_journal_corrupt || reset_journal_torn;
    assert!(
        usize::from(lab_rx_backpressure)
            + usize::from(lab_rx_electrical)
            + usize::from(lab_rx_returned_fault)
            <= 1,
        "lab-rx-backpressure, lab-rx-electrical-hil and lab-rx-returned-fault-hil are mutually exclusive"
    );
    match (safe_idle, lab_rx, reset_journal_hil) {
        (true, false, false) => {}
        (false, true, false) => {
            require_lab_rx_environment();
            if lab_rx_backpressure {
                require_lab_rx_backpressure_environment();
            }
            if lab_rx_electrical {
                require_lab_rx_electrical_environment();
            }
            if lab_rx_returned_fault {
                require_lab_rx_returned_fault_environment();
            }
        }
        (false, false, true) => require_reset_journal_environment(reset_journal_corrupt),
        (true, true, false) => panic!(
            "safe-idle and lab-rx are mutually exclusive; build lab RX with \
             --no-default-features --features lab-rx"
        ),
        _ => panic!(
            "select exactly one firmware mode: safe-idle, lab-rx, or one reset-journal HIL mode"
        ),
    }
}

fn require_esp_rtos_main_stack_patch() {
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../vendor/esp-rtos-0.3.0/src/lib.rs");
    println!("cargo:rerun-if-changed={}", source.display());
    let contents = fs::read_to_string(&source)
        .unwrap_or_else(|error| panic!("could not read vendored esp-rtos source: {error}"));
    let compact = contents.split_whitespace().collect::<Vec<_>>().join(" ");
    let corrected_cpu0 = "stack_bottom.cast_mut(), (stack_top as usize - stack_bottom as usize) / core::mem::size_of::<MaybeUninit<u32>>(),";
    let uncorrected_cpu0 = "stack_bottom.cast_mut(), stack_top as usize - stack_bottom as usize,";
    let corrected_cpu1 = "stack.bottom().cast::<MaybeUninit<u32>>(), STACK_SIZE / core::mem::size_of::<MaybeUninit<u32>>(),";
    let uncorrected_cpu1 = "stack.bottom().cast::<MaybeUninit<u32>>(), STACK_SIZE,";
    assert!(
        compact.contains(corrected_cpu0),
        "vendored esp-rtos CPU0 main-stack word-count patch is absent or changed"
    );
    assert!(
        !compact.contains(uncorrected_cpu0),
        "vendored esp-rtos still constructs the CPU0 main-stack slice with a byte count"
    );
    assert!(
        compact.contains(corrected_cpu1),
        "vendored esp-rtos CPU1 main-stack word-count patch is absent or changed"
    );
    assert!(
        !compact.contains(uncorrected_cpu1),
        "vendored esp-rtos still constructs the CPU1 main-stack slice with a byte count"
    );
    println!("cargo:rustc-env=RETICULUM_ESP_RTOS_MAIN_STACK_PATCH={ESP_RTOS_MAIN_STACK_PATCH_ID}");
}

fn require_reset_journal_environment(corrupt: bool) {
    if corrupt {
        let slot = require_canonical_usize(RESET_JOURNAL_SLOT_ENV);
        assert!(slot < 2, "{RESET_JOURNAL_SLOT_ENV} must be 0 or 1");
        let word = require_canonical_usize(RESET_JOURNAL_WORD_ENV);
        assert!(
            word < 9,
            "{RESET_JOURNAL_WORD_ENV} must be a canonical decimal value in 0..=8"
        );
    } else {
        let ordinal = require_canonical_usize(RESET_JOURNAL_WRITE_ORDINAL_ENV);
        assert!(
            (1..=9).contains(&ordinal),
            "{RESET_JOURNAL_WRITE_ORDINAL_ENV} must be a canonical decimal value in 1..=9"
        );
    }
}

fn require_canonical_usize(name: &str) -> usize {
    println!("cargo:rerun-if-env-changed={name}");
    let value = env::var(name)
        .unwrap_or_else(|_| panic!("explicit reset-journal HIL configuration is missing {name}"));
    assert!(
        !value.is_empty()
            && value.bytes().all(|byte| byte.is_ascii_digit())
            && (value.len() == 1 || !value.starts_with('0')),
        "{name} must use canonical unsigned decimal form"
    );
    let parsed = value
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("{name} is outside usize range"));
    println!("cargo:rustc-env={name}={value}");
    parsed
}

fn require_lab_rx_returned_fault_environment() {
    println!("cargo:rerun-if-env-changed={LAB_RX_RETURNED_FAULT_TRIGGER_ENV}");
    println!("cargo:rerun-if-env-changed={LAB_RX_RETURNED_FAULT_POLICY_ENV}");

    let trigger = env::var(LAB_RX_RETURNED_FAULT_TRIGGER_ENV).unwrap_or_else(|_| {
        panic!(
            "explicit returned-fault configuration is missing {LAB_RX_RETURNED_FAULT_TRIGGER_ENV}; no trigger default exists"
        )
    });
    assert!(
        trigger == "get-irq-status-after-set-rx",
        "{LAB_RX_RETURNED_FAULT_TRIGGER_ENV} must be exactly get-irq-status-after-set-rx"
    );
    let policy = env::var(LAB_RX_RETURNED_FAULT_POLICY_ENV).unwrap_or_else(|_| {
        panic!(
            "explicit returned-fault configuration is missing {LAB_RX_RETURNED_FAULT_POLICY_ENV}; no policy default exists"
        )
    });
    match policy.as_str() {
        "one-boot" | "repeat-until-quarantine" => {}
        _ => panic!(
            "{LAB_RX_RETURNED_FAULT_POLICY_ENV} must be exactly one-boot or repeat-until-quarantine"
        ),
    }

    println!("cargo:rustc-cfg=reticulum_lab_rx_returned_fault_policy=\"{policy}\"");
    println!("cargo:rustc-env={LAB_RX_RETURNED_FAULT_TRIGGER_ENV}={trigger}");
    println!("cargo:rustc-env={LAB_RX_RETURNED_FAULT_POLICY_ENV}={policy}");
}

fn require_lab_rx_electrical_environment() {
    println!("cargo:rerun-if-env-changed={LAB_RX_REGULATOR_ENV}");
    println!("cargo:rerun-if-env-changed={LAB_RX_GAIN_ENV}");

    let regulator = env::var(LAB_RX_REGULATOR_ENV).unwrap_or_else(|_| {
        panic!(
            "explicit lab RX electrical configuration is missing {LAB_RX_REGULATOR_ENV}; no regulator default exists"
        )
    });
    match regulator.as_str() {
        "ldo" | "dcdc" => {}
        _ => panic!("{LAB_RX_REGULATOR_ENV} must be exactly ldo or dcdc"),
    }
    let gain = env::var(LAB_RX_GAIN_ENV).unwrap_or_else(|_| {
        panic!(
            "explicit lab RX electrical configuration is missing {LAB_RX_GAIN_ENV}; no receive-gain default exists"
        )
    });
    match gain.as_str() {
        "unboosted" | "boosted" => {}
        _ => panic!("{LAB_RX_GAIN_ENV} must be exactly unboosted or boosted"),
    }

    println!("cargo:rustc-cfg=reticulum_lab_rx_regulator=\"{regulator}\"");
    println!("cargo:rustc-cfg=reticulum_lab_rx_gain=\"{gain}\"");
    println!("cargo:rustc-env={LAB_RX_REGULATOR_ENV}={regulator}");
    println!("cargo:rustc-env={LAB_RX_GAIN_ENV}={gain}");
}

fn require_lab_rx_backpressure_environment() {
    println!("cargo:rerun-if-env-changed={LAB_RX_BACKPRESSURE_STALL_ENV}");
    let value = match env::var(LAB_RX_BACKPRESSURE_STALL_ENV) {
        Ok(value) if !value.trim().is_empty() => value,
        _ => panic!(
            "explicit lab RX backpressure configuration is missing \
             {LAB_RX_BACKPRESSURE_STALL_ENV}; the instrumented feature has no stall default"
        ),
    };
    match value.parse::<u64>() {
        Ok(duration_us) if duration_us != 0 => {}
        _ => panic!(
            "{LAB_RX_BACKPRESSURE_STALL_ENV} must be an explicit non-zero u64 microsecond duration"
        ),
    }
}

fn require_lab_rx_environment() {
    for name in LAB_RX_ENV {
        println!("cargo:rerun-if-env-changed={name}");
        match env::var(name) {
            Ok(value) if !value.trim().is_empty() => {}
            _ => panic!(
                "explicit lab RX configuration is missing {name}; no frequency or modulation defaults exist"
            ),
        }
    }
}
