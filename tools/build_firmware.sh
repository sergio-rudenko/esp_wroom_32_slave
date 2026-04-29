#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
FIRMWARE_PATH="$PROJECT_ROOT/firmware.bin"
PARTITIONS_CSV="$PROJECT_ROOT/partitions.csv"
LOCAL_ESPRESSIF_DIR="$PROJECT_ROOT/.embuild/espressif"
LOCAL_IDF_PATH="$LOCAL_ESPRESSIF_DIR/esp-idf/v5.2.3"

setup_esp_environment() {
    if [[ -f "$HOME/export-esp.sh" ]]; then
        # shellcheck disable=SC1090
        source "$HOME/export-esp.sh"
    fi

    if [[ -z "${IDF_PATH:-}" && -d "$LOCAL_IDF_PATH" ]]; then
        export IDF_PATH="$LOCAL_IDF_PATH"
        echo "Using local ESP-IDF: $IDF_PATH"
    fi

    if [[ -z "${IDF_TOOLS_PATH:-}" && -d "$LOCAL_ESPRESSIF_DIR" ]]; then
        export IDF_TOOLS_PATH="$LOCAL_ESPRESSIF_DIR"
    fi

    # Some filesystems disallow creating the lib64 -> lib symlink required by python venv
    # inside project-local .embuild. Use global Espressif tools dir in HOME by default.
    if [[ -z "${ESP_IDF_TOOLS_INSTALL_DIR:-}" ]]; then
        export ESP_IDF_TOOLS_INSTALL_DIR=global
    fi
    if [[ "${ESP_IDF_TOOLS_INSTALL_DIR}" == "global" ]]; then
        export IDF_TOOLS_PATH="$HOME/.espressif"
    fi

    if [[ -z "${IDF_PATH:-}" ]]; then
        cat >&2 <<EOF
Error: IDF_PATH is not set and local ESP-IDF was not found.
Run once:
  source "\$HOME/export-esp.sh"
or install ESP toolchain via:
  espup install
EOF
        exit 1
    fi
}

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

regenerate_partition_table_bin() {
    local gen_part_py="$IDF_PATH/components/partition_table/gen_esp32part.py"
    if [[ ! -f "$gen_part_py" ]]; then
        echo "Error: partition table generator not found: $gen_part_py" >&2
        exit 1
    fi

    echo "Rebuilding partition-table.bin from project partitions.csv..."
    python3 "$gen_part_py" "$PARTITIONS_CSV" "$PARTITION_TABLE_PATH"
}

cd "$PROJECT_ROOT"
setup_esp_environment

# Some mounted filesystems (for example /mnt/data) do not support symlinks.
# ESP-IDF CMake build requires symlink support, so keep cargo artifacts in HOME.
if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    export CARGO_TARGET_DIR="$HOME/.cache/esp_wroom_32_slave/target"
fi
TARGET_DIR="$CARGO_TARGET_DIR/xtensa-esp32-espidf/release"
ELF_PATH="$TARGET_DIR/esp_wroom_32_slave"
APP_IMAGE_PATH="$TARGET_DIR/app-image.bin"
BOOTLOADER_PATH="$TARGET_DIR/bootloader.bin"
PARTITION_TABLE_PATH="$TARGET_DIR/partition-table.bin"
mkdir -p "$TARGET_DIR"

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

regenerate_partition_table_bin

echo "[2/3] Generating app image..."
espflash save-image --chip esp32 "$ELF_PATH" "$APP_IMAGE_PATH"

echo "[3/3] Merging flash image to firmware.bin..."
esptool --chip esp32 merge-bin -o "$FIRMWARE_PATH" \
    0x1000 "$BOOTLOADER_PATH" \
    0x8000 "$PARTITION_TABLE_PATH" \
    0x10000 "$APP_IMAGE_PATH"

echo "Done: $FIRMWARE_PATH"
