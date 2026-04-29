# Project Context

This file captures current decisions and status so work can resume quickly after moving disks, reopening Cursor, or starting a new chat.

## Goal

Embedded Rust firmware for `ESP32-WROOM-32UE` with:

- UART0 logs
- UART1 link to host controller (`STM32`)
- Wi-Fi (`STA` + future `AP`)
- Ethernet (`LAN8720A-CP`)
- Services (UDP discovery listener, TCP server, and NTP client)
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
- UART1 init for STM32 (`GPIO14` TX, `GPIO4` RX, `115200`)
- UART1 split into async/non-blocking runtime tasks (TX/RX threads + channels)
- Packet framing protocol:
  - `SOF(0xAA) + LEN(u16 LE payload size) + CMD + PARAM + PAYLOAD + CRC16-CCITT`
- READY message (encode only): periodic until first master frame; real ESP reset reason
- Message modules under `src/master/messages`:
  - `ready`, `interface_settings`, `interface_state`, **`service_settings`**, **`service_state`**, **`tcp_command`**, **`tcp_data`**, **`wifi_scan`**
- `InterfaceSettings` decode (MsgPack), validation, RX routing to Wi-Fi STA and Ethernet tasks
- **`ServiceSettings`** decode for:
  - `ServiceType::UdpListener` (`requestPorts`, `responsePorts`, `requestType`, `serviceId`, `deviceType`, `port`)
  - `ServiceType::TcpServer` (`port`, `clientTimeout`)
  - `ServiceType::NtpClient` (`enabled`, `timezone`, `resyncPeriod`, `servers`)
- Wi-Fi runtime task (`network/wifi/mod.rs`): connect/reconnect, `InterfaceState` events, RSSI, disconnect reason text in logs; **`lwip_socket_gate`** increment after `EspWifi` / `BlockingWifi` init (success or fatal error)
- Wi-Fi invalid-credentials path hardened:
  - `wait_netif_up` replaced with bounded polling (`wait_for_netif_up_or_disconnect`, 15s timeout)
  - WDT feed is guaranteed during netif-up wait and during pre-retry idle waits
  - `connect_error` + `disconnected` are emitted on early disconnect (e.g. `4WAY_HANDSHAKE_TIMEOUT`)
  - Wi-Fi disconnect reasons are centralized in typed enum `WifiDisconnectReason` (`network/wifi/state.rs`)
- Wi-Fi AP (`WiFiAccessPoint`) runtime is implemented inside `network/wifi/mod.rs` using one shared Wi-Fi driver:
  - accepts AP settings (`enabled`, `ssid`, `password`, `channel`, `maxClients`, `static[ip,mask]`)
  - applies AP-only or AP+STA (`Configuration::AccessPoint` / `Configuration::Mixed`) depending on station state
  - emits AP state messages via `InterfaceState` (`started=true`, `started=false+error`, `clientConnected=true/false`)
  - reports AP client `mac` and assigned DHCP `ip` on connect events
- Wi-Fi STA and Ethernet now apply `dhcp`/`static` from `InterfaceSettings`:
  - `dhcp=true`: DHCP client mode
  - `dhcp=false`: static `ip/netmask/gateway/dns/secondary_dns` is parsed and applied to netif
- Ethernet LAN8720 RMII task (`ethernet.rs`): link monitor, `InterfaceState`; same **`lwip_socket_gate`** pattern after `EthDriver` / `BlockingEth` init
- Ethernet runtime hardening and board alignment:
  - RMII clock is configured as internal output on `GPIO16` (`OutputGpio16`) to match validated ESP-AT wiring
  - PHY power is explicitly enabled on `GPIO5` before EMAC/PHY init (with startup delay)
  - PHY address uses autodetect (`ESP_ETH_PHY_ADDR_AUTO`)
  - startup fallback profile is available in firmware via `master::config::ETHERNET_MOCK_DHCP_ON_BOOT` (`enabled=true`, `dhcp=true`) for link tests without master config
  - blocking `wait_connected`/`wait_netif_up` paths were replaced with WDT-safe polling waits
  - after cable unplug, task waits for link recovery without periodic `connect_error` spam, while still emitting `ethernet_disconnected` on link loss
- `WifiScan` message flow implemented:
  - inbound (`STM32 -> ESP32`): MsgPack `{ "limit": N }`, `PARAM=0`
  - handled by `network/wifi` task, which performs Wi-Fi scan
  - outbound (`ESP32 -> STM32`): MsgPack array of AP objects `{ssid,bssid,channel,rssi,authMethod}`
  - responses are chunked by `MAX_PAYLOAD_SIZE`, `PARAM` carries chunk index (`0..`)
  - empty result is sent as MsgPack-encoded empty array `[]` in chunk `PARAM=0`
- **UDP listener** (`services/udp_listener.rs`):
  - waits for gate count **2** (both driver tasks finished lwIP-related init) before `std::net::UdpSocket::bind`, avoiding `tcpip_send_msg_wait_sem` / Invalid mbox races; does **not** require link or IP on any interface
  - binds `0.0.0.0` on each request port; JSON request/response as per spec; round-robin response ports
- **`tools/check_udp_listener.py`**: host test tool (broadcast IP CLI arg, 1 Hz probe, logs)
- **`tools/mock_master_uart.py`**: host-side STM32 emulator via UART (`InterfaceSettings`/`ServiceSettings`/`WifiScan` TX + ESP frame decode; includes `--send-wifi-ap --wifi-ap-json`; `--send-wifi-scan --wifi-scan-limit` sends one scan request after 5 seconds)
- Tooling documentation rule: when scripts in `tools/` are changed, update `tools/README.md` in the same task/commit so CLI options and examples stay in sync.
- **TCP server** (`services/tcp_server.rs`) implemented:
  - listens on `0.0.0.0:port` from `ServiceSettings(TcpServer)`
  - max 4 simultaneous clients (`index` 0..3)
  - emits `ServiceState` on connect/disconnect (`ClientClosedConnection`, `ServerClosedConnection`, `InactivityTimeout`, `NotConnected`)
  - bridges client bytes to master as `TcpData`; accepts inbound `TcpData` from master and writes to socket
  - supports `TcpCommand { close: true }` from master
- **NTP client** (`services/ntp_client.rs`) implemented:
  - settings: `enabled`, `timezone` (minutes), `resyncPeriod` (minutes, default 15), up to 3 servers by priority
  - emits `ServiceState` success payload `{stratum, timet, server}` and error payload `{error, server}`
  - resync uses configured `resyncPeriod`
  - guarded by real link state: sync is skipped while active Wi-Fi/Ethernet link count is zero (prevents repeated error spam)
- **Task WDT** integrated (`system/wdt.rs`):
  - TWDT timeout configured to `10s`
  - if TWDT is already initialized by runtime, firmware reconfigures it via `esp_task_wdt_reconfigure`
  - subscribed tasks: `main-loop`, `uart-tx-task`, `uart-rx-task`, `wifi-task`, `eth-task`, `udp-listener`, `tcp-server`, `ntp-client`
  - feeds are placed in all long-running loops and wait loops (`recv_timeout`, lwIP init waits, reconnect waits)
  - early task exits now explicitly unsubscribe from TWDT to avoid false watchdog triggers on dead pthread handles
- `READY` boot reason now maps full ESP32 reset reasons (power/external/software/panic/watchdog variants/deepsleep/brownout/sdio/usb/jtag/efuse/power glitch/cpu lockup)
- Large stream note:
  - `TcpData` is forwarded over UART `115200`; TX queue is unbounded
  - sustained large bursts (100KB+) can accumulate backlog in RAM and increase latency/instability risk
  - practical safe burst target without flow control: ~`32..64KB`

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
2. `export IDF_PATH=/mnt/projects/esp_wroom_32_slave/.embuild/espressif/esp-idf/v5.2.3`
3. `export IDF_TOOLS_PATH=/mnt/projects/esp_wroom_32_slave/.embuild/espressif`
4. `cargo build --target xtensa-esp32-espidf`

`tools/build_firmware.sh` now additionally enforces environment/build safety:

- auto-sources `~/export-esp.sh` when present
- auto-detects project-local ESP-IDF at `.embuild/espressif/esp-idf/v5.2.3` when `IDF_PATH` is missing
- defaults to `ESP_IDF_TOOLS_INSTALL_DIR=global` (`IDF_TOOLS_PATH=~/.espressif`) to avoid project-filesystem symlink issues in Python venv creation
- defaults `CARGO_TARGET_DIR` to `~/.cache/esp_wroom_32_slave/target` to avoid `/mnt/...` symlink restrictions during ESP-IDF CMake stage
- regenerates `partition-table.bin` from project `partitions.csv` before image merge to guarantee custom partition layout in final `firmware.bin`

Errors solved during recovery:

- `ensurepip is not available` during `install-python-env` -> install `python3.12-venv`
- `E0308` in `esp-idf-svc` (`*const i8` vs `*const u8`) -> use compatible crate versions above
- `cannot find espidf in embuild` -> enable `embuild` feature `espidf`
- `xtensa-esp32-elf-gcc ... --ldproxy-linker` unknown option -> install/use `ldproxy` as linker
- `undefined reference to __pender` -> remove `embassy-time-driver` feature from `esp-idf-svc`
- **`Invalid mbox` / `tcpip_send_msg_wait_sem`** when opening UDP during Wi-Fi init -> **`lwip_socket_gate`**: STA and ETH tasks increment after their lwIP init; UDP listener waits for expected count before `UdpSocket::bind`
- `Image length ... doesn't fit in partition length 1048576` at boot -> enforce custom partition table (`sdkconfig.defaults` + partition table regeneration in `build_firmware.sh`)
- `task_wdt` trigger while testing bad Wi-Fi credentials -> remove blocking netif-up wait and emit deterministic STA error/disconnect states

## Next engineering steps

1. Add explicit status/error signaling for `WifiScan` execution failures/timeouts.
2. Optional: extend watchdog diagnostics with task-local heartbeat counters and periodic health snapshots in logs.
3. Completed: `README.md` converted to contract-style protocol reference (field tables, ranges, required/optional markers for interfaces/services/boot reasons/frame fields).

## Documentation sync notes

This file keeps engineering context and historical rationale. Public onboarding and protocol docs live in `README.md`.

Key items migrated from old root `README.md` and preserved here:

- Stability rationale for stack choice (`ESP-IDF` + `esp-idf-*` crates over bare-metal route for Wi-Fi/Ethernet-heavy firmware).
- Runtime split by responsibility:
  - `master` (UART transport + frame protocol + message codecs)
  - `network` (Wi-Fi/Ethernet drivers and interface state)
  - `services` (UDP listener, TCP server, NTP client)
- Release artifact flow:
  - firmware image is produced with `tools/build_firmware.sh`
  - script builds release ELF, creates app image, merges bootloader + partition table + app into root `firmware.bin`
- Partition sizing constraint and fix:
  - app image exceeded 1MB factory slot in default table
  - project now uses custom `partitions.csv` (larger factory partition) during firmware generation flow
