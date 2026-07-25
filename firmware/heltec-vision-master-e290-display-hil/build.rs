fn main() {
    // esp-hal supplies linkall.x for the selected ESP32-S3 runtime.
    println!("cargo:rustc-link-arg=-Tlinkall.x");
}
