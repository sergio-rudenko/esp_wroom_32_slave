# Changelog

## 2026-04-22

### Added
- `services/tcp_server` with up to 4 client slots, `TcpCommand` close handling, inactivity timeout, and bidirectional `TcpData` bridge.
- `services/ntp_client` with prioritized server sync, `ServiceState` success/error payloads, configurable `resyncPeriod`, and periodic resync.
- New message modules:
  - `master/messages/service_state`
  - `master/messages/tcp_command`
  - `master/messages/tcp_data`
- Host tools under `tools/`:
  - `check_udp_listener.py`
  - `mock_master_uart.py`
  - `tools/README.md`

### Changed
- Renamed protocol message `TcpEvent` to `TcpData` (`MessageType` value `7` remains unchanged).
- `ServiceSettings::NtpClient` now supports:
  - `enabled`
  - `timezone`
  - `resyncPeriod` (default 15)
  - `servers` (priority list, max 3)
- NTP sync gating now uses **real active link state** from Wi-Fi/Ethernet runtime tasks instead of interface presence heuristics.
- `tools/mock_master_uart.py` updated:
  - fixed UDP settings CLI mapping (`--udp-listener-json`)
  - supports sending `ServiceSettings` for `NtpClient` (`--send-ntp-client`, `--ntp-client-json`)
  - improved `TcpData`/`TcpCommand` param logging as slot index.

### Removed
- Legacy root-level `check_udp_listener.py` (moved to `tools/`).
