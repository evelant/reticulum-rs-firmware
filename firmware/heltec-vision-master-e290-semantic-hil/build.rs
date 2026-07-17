use std::{env, fs, path::PathBuf};

const ESP_RTOS_MAIN_STACK_PATCH_ID: &str = "esp-rtos-0.3.0-cpu0-cpu1-main-stack-words-v2";

fn main() {
    require_esp_rtos_main_stack_patch();
    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
        println!("cargo:rustc-link-arg=-Tlinkall.x");
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
