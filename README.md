# ESP32 UART Slave Controller 2

Rust starter for `ESP32-WROOM-32UE` with focus on stable production stack.

Project continuity notes are stored in `CONTEXT.md`.

## Why this stack

For max stability with Wi-Fi + Ethernet + UART in one firmware:

- `ESP-IDF` (FreeRTOS under the hood)
- `esp-idf-hal` + `esp-idf-svc`

`esp-hal` is great for bare-metal, but Wi-Fi/Ethernet integration is currently far more mature in the ESP-IDF ecosystem.

## Implemented bootstrap

- UART0 logs via `EspLogger`
- UART1 configured for host controller (STM32) on:
  - TX: `GPIO17`
  - RX: `GPIO16`
  - baud: `115200`
- UART1 runtime architecture:
  - dedicated TX task reads outgoing frames from TX queue/channel
  - dedicated RX task reads bytes from UART, reassembles valid protocol frames, pushes them to RX queue/channel
- READY message flow:
  - READY is sent at startup
  - READY is sent every 5s until first valid frame from master is received
  - boot reason for READY is resolved locally inside `messages/ready`
- Packet framing protocol implemented in `master/protocol`:
  - `SOF(0xAA) + LEN(u16 LE payload size) + CMD + PARAM + PAYLOAD + CRC16-CCITT`
- `master/messages` modular message layout:
  - `ready` (outbound only, encode)
  - `interface_settings` (inbound only, decode)
  - `interface_state` (outbound only, encode)
  - `service_settings` (inbound only, decode; `ServiceType` includes `UdpListener`, stubs for `TcpServer` / `NtpClient`)
- RX decode wired for `InterfaceSettings` and `ServiceSettings` (UDP listener path) with logging and routing to tasks
- Wi-Fi STA task (`src/network/wifi_station.rs`):
  - receives `WiFiStationSettings` from channel
  - applies STA config and connects/reconnects
  - monitors link state and emits `InterfaceState` to STM32 (connected, connect_error, got_ip, rssi, disconnected)
  - after `EspWifi` / lwIP init, bumps a shared gate counter so UDP sockets do not race tcpip startup
- Ethernet LAN8720 RMII task (`src/network/ethernet.rs`):
  - same pattern as STA; pinout documented in code
  - bumps the same lwIP gate after `EthDriver` / `BlockingEth` init
- UDP discovery listener (`src/services/udp_listener.rs`):
  - waits for `UdpListenerSettings` (from master or dev mock)
  - waits until Wi-Fi and Ethernet driver tasks have finished lwIP-related init (no fixed delay; works without link/IP)
  - binds `0.0.0.0` on `requestPorts`, matches JSON `{"type":…}`, replies unicast to client IP on `responsePorts` (round-robin)
- Dev mocks in `main` for Wi-Fi, Ethernet, and UDP listener until the STM32 config path is complete
- Host-side check script: `check_udp_listener.py` (broadcast IP argument, logs send/recv once per second)
- `sdkconfig.defaults` prefilled for LAN8720 RMII baseline

## Build/flash (first setup)

1. Install toolchain helper:
   - `cargo install espup`
2. Install Espressif Rust toolchain:
   - `espup install`
   - Restart shell and run: `source $HOME/export-esp.sh`
3. Install flasher:
   - `cargo install espflash`
4. Build:
   - `cargo build --target xtensa-esp32-espidf`
5. Flash + monitor:
   - `cargo run`

## Project structure (UART / master / network / services)

- `src/master/transport.rs` — UART1 hardware init
- `src/master/protocol.rs` — frame encode/decode + stream frame extraction
- `src/master/messages/ready.rs` — READY (encode only)
- `src/master/messages/interface_settings.rs` — InterfaceSettings (decode only)
- `src/master/messages/interface_state.rs` — InterfaceState (encode only)
- `src/master/messages/service_settings.rs` — ServiceSettings / `UdpListener` payload (decode only)
- `src/network/wifi_station.rs` — Wi-Fi STA task
- `src/network/ethernet.rs` — Ethernet task
- `src/services/udp_listener.rs` — UDP discovery listener task

## Next steps

- **TCP Server** service (`ServiceType::TcpServer` in `ServiceSettings`): accept connections, framing with STM32 per protocol plan
- Wi-Fi AP runtime task (`WiFiAccessPoint` in `InterfaceSettings`)
- Apply static IP from `InterfaceSettings` when `dhcp=false` (STA and Ethernet)
- Extend `InterfaceState` as new events appear
