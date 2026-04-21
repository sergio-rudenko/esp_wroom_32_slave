# Project Context

This file captures current decisions and status so work can resume quickly after moving disks, reopening Cursor, or starting a new chat.

## Goal

Embedded Rust firmware for `ESP32-WROOM-32UE` with:

- UART0 logs
- UART1 link to host controller (`STM32`)
- Wi-Fi (`STA` + `AP`)
- Ethernet (`LAN8720A-CP`)
- Maximum stability over novelty

## Chosen stack

Stability-first stack:

- `ESP-IDF` (FreeRTOS underneath)
- `esp-idf-sys`
- `esp-idf-hal`
- `esp-idf-svc`

Reason: on ESP32, Wi-Fi + Ethernet support is currently more mature in ESP-IDF ecosystem than pure `esp-hal` route.

## Current implementation status

Implemented:

- Bootstrapped Rust project for ESP32 target
- UART0 logs via `EspLogger`
- UART1 init for STM32 (`GPIO17` TX, `GPIO16` RX, `115200`)
- UART1 split into async/non-blocking runtime tasks:
  - TX task: sends framed packets from TX queue/channel
  - RX task: reads UART bytes, extracts valid frames, pushes to RX queue/channel
- Packet framing protocol implemented:
  - `SOF(0xAA) + LEN(u16 LE payload size) + CMD + PARAM + PAYLOAD + CRC16-CCITT`
- READY message implemented and enabled:
  - sent once at startup
  - then every 5s until first valid frame from master is received
  - boot reason resolved locally in READY message module from real ESP reset reason
- Message modules introduced under `src/master/messages`:
  - `ready` (ESP32 -> STM32, encode only)
  - `interface_settings` (STM32 -> ESP32, decode only)
  - `interface_state` (ESP32 -> STM32, encode only)
- `InterfaceSettings` decode from MsgPack payload implemented with validation and RX routing
- Wi-Fi STA runtime task implemented (`src/network/wifi_station.rs`):
  - dedicated async task consumes `WiFiStation` interface settings from channel
  - applies config and connects/reconnects with `reconnectPeriod`
  - monitors link state (`is_connected`/`is_up`)
  - emits `InterfaceState` events to STM32:
    - `connected` (with STA MAC)
    - `connect_error` (prefers Wi-Fi disconnect reason, fallback to ESP error code)
    - `got_ip` (IP/mask/gateway)
    - `rssi` (periodic, every 20s while connected)
    - `disconnected` (with human-readable reason logging + reason code in payload)
- Ethernet init function placeholder (`init_ethernet_lan8720`)
- `sdkconfig.defaults` baseline for LAN8720 RMII

Not implemented yet:

- WiFi AP runtime task
- Ethernet driver bring-up with full pin mapping and event handling
- Applying decoded `InterfaceSettings` for `WiFiAccessPoint` and `Ethernet` into live runtime tasks

## Build/toolchain notes

Configured:

- `rust-toolchain.toml`: `channel = "esp"`
- `.cargo/config.toml` target: `xtensa-esp32-espidf`
- `.cargo/config.toml` unstable: `build-std = ["std", "panic_abort"]`

Why `build-std` is enabled:

- In current environment, prebuilt `core/std` for `xtensa-esp32-espidf` was unavailable at build time.
- `build-std` avoids `E0463` (`can't find crate for core/std`) by building std from source.

## Known blockers seen in this environment

After fixing `E0463`, build progressed and then failed in `esp-idf-sys` because:

- `esp-idf` git clone from GitHub failed (`https://github.com/espressif/esp-idf.git`)
- Root cause: network/DNS/connectivity restriction, not Rust code logic

## Recovery checklist (new machine/new folder)

1. Open project folder.
2. In terminal:
   - `source $HOME/export-esp.sh`
   - `cargo clean`
   - `cargo build`
3. If network-restricted, use local ESP-IDF:
   - `export IDF_PATH=/absolute/path/to/esp-idf`
   - `cargo build`

## Verified working build recipe (Apr 2026)

Dependency set confirmed to compile in this workspace:

- `esp-idf-sys = "0.36"`
- `esp-idf-hal = "0.45"`
- `esp-idf-svc = "0.51"` with features `["alloc", "critical-section", "experimental"]`
- `serde = "1"` with `derive`
- `rmp-serde = "1"`
- `embuild = { version = "0.33", features = ["espidf"] }` (build-dependency)

Important project config:

- `.cargo/config.toml` uses `linker = "ldproxy"` for target `xtensa-esp32-espidf`
- `ldproxy` must be installed (`cargo install ldproxy`)
- `python3.12-venv` is required for ESP-IDF Python env creation

Confirmed local build command sequence:

1. `source $HOME/export-esp.sh`
2. `export IDF_PATH=/mnt/projects/esp32-uart-slave-conroller2/.embuild/espressif/esp-idf/v5.2.3`
3. `export IDF_TOOLS_PATH=/mnt/projects/esp32-uart-slave-conroller2/.embuild/espressif`
4. `cargo build`

Errors solved during recovery:

- `ensurepip is not available` during `install-python-env` -> install `python3.12-venv`
- `E0308` in `esp-idf-svc` (`*const i8` vs `*const u8`) -> use compatible crate versions above
- `cannot find espidf in embuild` -> enable `embuild` feature `espidf`
- `xtensa-esp32-elf-gcc ... --ldproxy-linker` unknown option -> install/use `ldproxy` as linker
- `undefined reference to __pender` -> remove `embassy-time-driver` feature from `esp-idf-svc`

## Next engineering steps

1. Apply `InterfaceSettings` messages to runtime network config changes (`WiFiStation`, `WiFiAccessPoint`, `Ethernet`).
2. Implement production-ready Wi-Fi mixed mode (`STA+AP`) by adding AP task and integrating with STA task.
3. Implement LAN8720 bring-up with exact board pinout (RMII clock, PHY addr, power/reset GPIO).
4. Add timeout/retry/watchdog policy around master communication and network state transitions.
