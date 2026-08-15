use std::env;

const ESP_RTOS_UPSTREAM_IDENTITY: &str = "esp-rtos-upstream-b50efcb-stack-words-v1";

fn main() {
    emit_esp_rtos_upstream_identity();

    if env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("xtensa") {
        // esp-hal supplies linkall.x for the selected ESP32-S3 runtime.
        println!("cargo:rustc-link-arg=-Tlinkall.x");
    }
}

fn emit_esp_rtos_upstream_identity() {
    println!("cargo:rustc-env=RETICULUM_ESP_RTOS_UPSTREAM_IDENTITY={ESP_RTOS_UPSTREAM_IDENTITY}");
}
