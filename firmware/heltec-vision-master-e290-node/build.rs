use std::{env, fs, path::PathBuf};

#[path = "src/partition_contract.rs"]
#[allow(dead_code)]
mod partition_contract;

const ESP_RTOS_MAIN_STACK_PATCH_ID: &str = "esp-rtos-0.3.0-cpu0-cpu1-main-stack-words-v2";

fn main() {
    require_esp_rtos_main_stack_patch();
    require_partition_contract();
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
        println!("cargo:rustc-link-arg=-Tlinkall.x");
    }
}

fn require_partition_contract() {
    use partition_contract::{
        ANNOUNCE_CLOCK_LABEL, ANNOUNCE_CLOCK_LEN, ANNOUNCE_CLOCK_OFFSET, DEVICE_CONFIG_LABEL,
        DEVICE_CONFIG_LEN, DEVICE_CONFIG_OFFSET, NODE_IDENTITY_LABEL, NODE_IDENTITY_LEN,
        NODE_IDENTITY_OFFSET, NODE_JOURNAL_LABEL, NODE_JOURNAL_LEN, NODE_JOURNAL_OFFSET,
    };

    let path = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../partitions/heltec-vision-master-e290-node.csv");
    println!("cargo:rerun-if-changed={}", path.display());
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("could not read E290 partition CSV: {error}"));
    let rows = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split(',').map(str::trim).collect::<Vec<_>>())
        .collect::<Vec<_>>();

    for (label, subtype, offset, len) in [
        (
            NODE_IDENTITY_LABEL,
            "undefined",
            NODE_IDENTITY_OFFSET,
            NODE_IDENTITY_LEN,
        ),
        (
            ANNOUNCE_CLOCK_LABEL,
            "undefined",
            ANNOUNCE_CLOCK_OFFSET,
            ANNOUNCE_CLOCK_LEN,
        ),
        (
            DEVICE_CONFIG_LABEL,
            "nvs",
            DEVICE_CONFIG_OFFSET,
            DEVICE_CONFIG_LEN,
        ),
        (
            NODE_JOURNAL_LABEL,
            "undefined",
            NODE_JOURNAL_OFFSET,
            NODE_JOURNAL_LEN,
        ),
    ] {
        let matches = rows
            .iter()
            .filter(|row| row.first().copied() == Some(label))
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "E290 partition CSV must contain exactly one {label} row"
        );
        let row = matches[0];
        assert!(row.len() >= 5, "E290 partition row {label} is incomplete");
        assert_eq!(row[1], "data", "{label} must be a data partition");
        assert_eq!(row[2], subtype, "{label} has the wrong subtype");
        assert_eq!(parse_hex(row[3]), offset, "{label} has the wrong offset");
        assert_eq!(parse_hex(row[4]), len, "{label} has the wrong length");
        assert!(
            row.get(5).is_none_or(|flags| flags.is_empty()),
            "{label} must be writable and unencrypted"
        );
    }
}

fn parse_hex(value: &str) -> u32 {
    let digits = value
        .strip_prefix("0x")
        .unwrap_or_else(|| panic!("partition value is not hexadecimal: {value}"));
    u32::from_str_radix(digits, 16)
        .unwrap_or_else(|error| panic!("invalid partition value {value}: {error}"))
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
