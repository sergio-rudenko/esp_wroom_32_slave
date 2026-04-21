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
- RX decode wired for `InterfaceSettings` with logging and routing
- Wi-Fi STA task implemented in `src/network/wifi_station.rs`:
  - receives `WiFiStation` settings events
  - applies STA config and connects/reconnects
  - monitors link state and emits interface state events to STM32
  - reports RSSI periodically (every 20s while connected)
- Ethernet LAN8720 init stub
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
   - `cargo build`
5. Flash + monitor:
   - `cargo run`

## Project structure (UART/master)

- `src/master/transport.rs` - UART1 hardware init
- `src/master/protocol.rs` - frame encode/decode + stream frame extraction
- `src/master/messages/ready.rs` - READY message (encode only)
- `src/master/messages/interface_settings.rs` - InterfaceSettings (decode only)
- `src/master/messages/interface_state.rs` - InterfaceState (encode only)
- `src/network/wifi_station.rs` - WiFiStation task and reconnect loop

## Next steps

- Add WiFi AP and Ethernet runtime tasks (similar to WiFiStation)
- Replace Ethernet placeholder with full `esp-idf-svc::eth` setup for LAN8720 board wiring
- Extend `InterfaceState` coverage for AP/Ethernet runtime events
