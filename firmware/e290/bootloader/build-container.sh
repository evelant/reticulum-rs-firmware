#!/usr/bin/env bash
set -euo pipefail

script_dir="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
workspace="$(CDPATH= cd -- "$script_dir/../../.." && pwd)"
build_dir="$workspace/target/e290-bootloader"

case "$(uname -m)" in
  arm64|aarch64)
    idf_image="espressif/idf@sha256:0a952afa7b3fce016bc894a1b0cde98efb6027c4c95dd2fa6a94f6d7fee93e17"
    ;;
  x86_64|amd64)
    idf_image="espressif/idf@sha256:6e2800a69f1c6521a5651da524f811e237d13e34cad369687916d0ad0bc4ef89"
    ;;
  *)
    echo "unsupported Docker host architecture: $(uname -m)" >&2
    exit 2
    ;;
esac

mkdir -p "$build_dir"
docker run --rm \
  --user "$(id -u):$(id -g)" \
  --env HOME=/tmp \
  --volume "$workspace:/project" \
  --workdir /project/firmware/e290/bootloader \
  "$idf_image" \
  idf.py \
    -B /project/target/e290-bootloader \
    -D IDF_TARGET=esp32s3 \
    -D SDKCONFIG=/project/target/e290-bootloader/sdkconfig \
    -D SDKCONFIG_DEFAULTS=/project/firmware/e290/bootloader/sdkconfig.defaults \
    bootloader

test -s "$build_dir/bootloader/bootloader.bin"
grep -q '^CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE=y$' "$build_dir/sdkconfig"
shasum -a 256 "$build_dir/bootloader/bootloader.bin"
