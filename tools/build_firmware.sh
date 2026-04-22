#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TARGET_DIR="$PROJECT_ROOT/target/xtensa-esp32-espidf/release"
ELF_PATH="$TARGET_DIR/esp_wroom_32_slave"
APP_IMAGE_PATH="$TARGET_DIR/app-image.bin"
BOOTLOADER_PATH="$TARGET_DIR/bootloader.bin"
PARTITION_TABLE_PATH="$TARGET_DIR/partition-table.bin"
FIRMWARE_PATH="$PROJECT_ROOT/firmware.bin"
PARTITIONS_CSV="$PROJECT_ROOT/partitions.csv"

copy_partitions_csv_into_out_dirs() {
    shopt -s nullglob
    local copied=false
    for out_dir in "$TARGET_DIR"/build/esp-idf-sys-*/out; do
        cp "$PARTITIONS_CSV" "$out_dir/partitions.csv"
        copied=true
    done
    shopt -u nullglob

    if [[ "$copied" == true ]]; then
        echo "Prepared partitions.csv in esp-idf-sys out directories."
    fi
}

cd "$PROJECT_ROOT"

echo "[1/3] Building release firmware..."
if [[ ! -f "$PARTITIONS_CSV" ]]; then
    echo "Error: missing partition table file: $PARTITIONS_CSV" >&2
    exit 1
fi

copy_partitions_csv_into_out_dirs
if ! cargo build --release; then
    echo "Retrying build after preparing partition table in OUT_DIR..."
    copy_partitions_csv_into_out_dirs
    cargo build --release
fi

if [[ ! -f "$ELF_PATH" ]]; then
    echo "Error: ELF not found: $ELF_PATH" >&2
    exit 1
fi

if [[ ! -f "$BOOTLOADER_PATH" || ! -f "$PARTITION_TABLE_PATH" ]]; then
    echo "Error: Missing bootloader or partition table in $TARGET_DIR" >&2
    exit 1
fi

echo "[2/3] Generating app image..."
espflash save-image --chip esp32 "$ELF_PATH" "$APP_IMAGE_PATH"

echo "[3/3] Merging flash image to firmware.bin..."
esptool --chip esp32 merge-bin -o "$FIRMWARE_PATH" \
    0x1000 "$BOOTLOADER_PATH" \
    0x8000 "$PARTITION_TABLE_PATH" \
    0x10000 "$APP_IMAGE_PATH"

echo "Done: $FIRMWARE_PATH"
