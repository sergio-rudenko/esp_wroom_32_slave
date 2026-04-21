# Project Context

This file captures current decisions and status so work can resume quickly after moving disks, reopening Cursor, or starting a new chat.

## Goal

Embedded Rust firmware for `ESP32-WROOM-32UE` with:

- UART0 logs
- UART1 link to host controller (`STM32`)
- Wi-Fi (`STA` + future `AP`)
- Ethernet (`LAN8720A-CP`)
- Services (UDP discovery listener done; **TCP Server** next)
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
- UART1 split into async/non-blocking runtime tasks (TX/RX threads + channels)
- Packet framing protocol:
  - `SOF(0xAA) + LEN(u16 LE payload size) + CMD + PARAM + PAYLOAD + CRC16-CCITT`
- READY message (encode only): periodic until first master frame; real ESP reset reason
- Message modules under `src/master/messages`:
  - `ready`, `interface_settings`, `interface_state`, **`service_settings`**
- `InterfaceSettings` decode (MsgPack), validation, RX routing to Wi-Fi STA and Ethernet tasks
- **`ServiceSettings`** decode for **`ServiceType::UdpListener`** (MsgPack JSON fields: `requestPorts`, `responsePorts`, `requestType`, `serviceId`, `deviceType`, `port`). `TcpServer` / `NtpClient` return “not implemented” until those features exist
- Wi-Fi STA task (`wifi_station.rs`): connect/reconnect, `InterfaceState` events, RSSI, disconnect reason text in logs; **`lwip_socket_gate`** increment after `EspWifi` / `BlockingWifi` init (success or fatal error)
- Ethernet LAN8720 RMII task (`ethernet.rs`): link monitor, `InterfaceState`; same **`lwip_socket_gate`** pattern after `EthDriver` / `BlockingEth` init
- **UDP listener** (`services/udp_listener.rs`):
  - waits for gate count **2** (both driver tasks finished lwIP-related init) before `std::net::UdpSocket::bind`, avoiding `tcpip_send_msg_wait_sem` / Invalid mbox races; does **not** require link or IP on any interface
  - binds `0.0.0.0` on each request port; JSON request/response as per spec; round-robin response ports
- **`check_udp_listener.py`**: host test tool (broadcast IP CLI arg, 1 Hz probe, logs)
- Dev **mocks** in `main` for Wi-Fi, Ethernet, UDP listener (remove when STM32 owns config)

Not implemented yet:

- **TCP Server** service (next git-flow feature): `ServiceSettings` for `ServiceType::TcpServer`, accept pool, protocol toward STM32
- Wi-Fi AP runtime task
- Applying static IP for STA/Ethernet when `dhcp=false` in `InterfaceSettings`

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
- `serde = "1"` with `derive`, `rmp-serde = "1"`, **`serde_json = "1"`**
- `embuild = { version = "0.33", features = ["espidf"] }` (build-dependency)

Important project config:

- `.cargo/config.toml` uses `linker = "ldproxy"` for target `xtensa-esp32-espidf`
- `ldproxy` must be installed (`cargo install ldproxy`)
- `python3.12-venv` is required for ESP-IDF Python env creation

Confirmed local build command sequence:

1. `source $HOME/export-esp.sh`
2. `export IDF_PATH=/mnt/projects/esp32-uart-slave-conroller2/.embuild/espressif/esp-idf/v5.2.3`
3. `export IDF_TOOLS_PATH=/mnt/projects/esp32-uart-slave-conroller2/.embuild/espressif`
4. `cargo build --target xtensa-esp32-espidf`

Errors solved during recovery:

- `ensurepip is not available` during `install-python-env` -> install `python3.12-venv`
- `E0308` in `esp-idf-svc` (`*const i8` vs `*const u8`) -> use compatible crate versions above
- `cannot find espidf in embuild` -> enable `embuild` feature `espidf`
- `xtensa-esp32-elf-gcc ... --ldproxy-linker` unknown option -> install/use `ldproxy` as linker
- `undefined reference to __pender` -> remove `embassy-time-driver` feature from `esp-idf-svc`
- **`Invalid mbox` / `tcpip_send_msg_wait_sem`** when opening UDP during Wi-Fi init -> **`lwip_socket_gate`**: STA and ETH tasks increment after their lwIP init; UDP listener waits for expected count before `UdpSocket::bind`

## Next engineering steps

1. **TCP Server** (`feature/TCP-Server`): decode `ServiceSettings` for `TcpServer`, dedicated task, `std::net::TcpListener` or IDF-friendly accept loop, bridge to UART/protocol as designed.
2. Apply static IP from `InterfaceSettings` when `dhcp=false` (Wi-Fi STA and Ethernet).
3. Wi-Fi AP task and mixed STA+AP policy.
4. Optional: timeout/retry/watchdog policy around master communication and network state transitions.
