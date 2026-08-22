use std::{env, fs, path::PathBuf};

#[path = "src/partition_contract.rs"]
#[allow(dead_code)]
mod partition_contract;

const ESP_RTOS_UPSTREAM_IDENTITY: &str = "esp-rtos-upstream-b50efcb-stack-words-v1";
fn main() {
    emit_esp_rtos_upstream_identity();
    require_partition_contract();
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
        println!("cargo:rustc-link-arg=-Tlinkall.x");
    }
}

fn require_partition_contract() {
    use partition_contract::{
        OTA_0_LABEL, OTA_0_LEN, OTA_0_OFFSET, OTA_1_LABEL, OTA_1_LEN, OTA_1_OFFSET, OTA_DATA_LABEL,
        OTA_DATA_LEN, OTA_DATA_OFFSET, PRNS_STATE_LABEL, PRNS_STATE_LEN, PRNS_STATE_OFFSET,
        PRODUCT_STATE_LABEL, PRODUCT_STATE_LEN, PRODUCT_STATE_OFFSET,
    };

    let path =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("../../partitions/e290.csv");
    println!("cargo:rerun-if-changed={}", path.display());
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("could not read E290 partition CSV: {error}"));
    let rows = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split(',').map(str::trim).collect::<Vec<_>>())
        .collect::<Vec<_>>();

    let required = [
        (OTA_0_LABEL, "app", "ota_0", OTA_0_OFFSET, OTA_0_LEN),
        (OTA_1_LABEL, "app", "ota_1", OTA_1_OFFSET, OTA_1_LEN),
        (OTA_DATA_LABEL, "data", "ota", OTA_DATA_OFFSET, OTA_DATA_LEN),
        (
            PRODUCT_STATE_LABEL,
            "data",
            "undefined",
            PRODUCT_STATE_OFFSET,
            PRODUCT_STATE_LEN,
        ),
        (
            PRNS_STATE_LABEL,
            "data",
            "undefined",
            PRNS_STATE_OFFSET,
            PRNS_STATE_LEN,
        ),
    ];

    for &(label, kind, subtype, offset, len) in &required {
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
        assert_eq!(row[1], kind, "{label} has the wrong partition type");
        assert_eq!(row[2], subtype, "{label} has the wrong subtype");
        assert_eq!(parse_hex(row[3]), offset, "{label} has the wrong offset");
        assert_eq!(parse_hex(row[4]), len, "{label} has the wrong length");
        assert!(
            row.get(5).is_none_or(|flags| flags.is_empty()),
            "{label} must be writable and unencrypted"
        );
    }

    for row in &rows {
        assert!(row.len() >= 5, "E290 partition row is incomplete: {row:?}");
        let label = row[0];
        let offset = parse_hex(row[3]);
        let len = parse_hex(row[4]);
        let end = offset
            .checked_add(len)
            .unwrap_or_else(|| panic!("E290 partition range overflows: {label}"));
        for &(required_label, _, _, required_offset, required_len) in &required {
            let required_end = required_offset
                .checked_add(required_len)
                .expect("fixed E290 product partition range must not overflow");
            assert!(
                label == required_label || offset >= required_end || required_offset >= end,
                "E290 partition {label} overlaps protected range {required_label}"
            );
        }
    }
}

fn parse_hex(value: &str) -> u32 {
    let digits = value
        .strip_prefix("0x")
        .unwrap_or_else(|| panic!("partition value is not hexadecimal: {value}"));
    u32::from_str_radix(digits, 16)
        .unwrap_or_else(|error| panic!("invalid partition value {value}: {error}"))
}

fn emit_esp_rtos_upstream_identity() {
    println!("cargo:rustc-env=RETICULUM_ESP_RTOS_UPSTREAM_IDENTITY={ESP_RTOS_UPSTREAM_IDENTITY}");
}
