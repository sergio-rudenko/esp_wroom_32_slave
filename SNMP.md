# SNMP Service Contract (v2c and v3)

This document defines the UART contract for adding SNMP service support into firmware.

Status: draft contract for implementation.

## 1. Transport envelope

SNMP service uses existing firmware packet framing and message types:

- `CMD = ServiceSettings` (`4`) from master to ESP32
- `CMD = ServiceState` (`5`) from ESP32 to master
- `PARAM = ServiceType::SnmpAgent` (recommended value: `4`, after `NtpClient=3`)
- payload encoding: MsgPack map (`rmp-serde` style), JSON examples are equivalent schemas

## 2. ServiceSettings (master -> ESP32)

### 2.1 ServiceType

- `Undefined = 0`
- `UdpListener = 1`
- `TcpServer = 2`
- `NtpClient = 3`
- `SnmpAgent = 4` (new)

### 2.2 Payload schema: `SnmpSettings`

```json
{
  "enabled": true,
  "version": "both",
  "listenPort": 161,
  "engineId": "80001f8880e963000001",
  "sysName": "esp32-slave-01",
  "sysLocation": "cabinet-a",
  "sysContact": "ops@example.local",
  "v2c": {
    "communities": [
      {
        "name": "public",
        "access": "ro",
        "sources": ["192.168.1.0/24"]
      }
    ]
  },
  "v3": {
    "users": [
      {
        "username": "monitor",
        "securityLevel": "authPriv",
        "auth": { "protocol": "sha256", "password": "auth-secret" },
        "priv": { "protocol": "aes128", "password": "priv-secret" }
      }
    ]
  },
  "traps": {
    "enabled": false,
    "targets": []
  }
}
```

### 2.3 Field contract

- `enabled` (`bool`, required): enables/disables SNMP runtime.
- `version` (`string`, required): `v2c | v3 | both`.
- `listenPort` (`u16`, optional, default `161`): UDP listen port, must be non-zero.
- `engineId` (`string`, required when `version` includes `v3`): hex string, recommended `10..64` hex chars.
- `sysName`, `sysLocation`, `sysContact` (`string`, optional): MIB-II system fields.
- `v2c` (`object`, required when `version` includes `v2c`):
  - `communities` (`array`, min 1):
    - `name` (`string`, non-empty, max 32)
    - `access` (`string`: `ro | rw`)
    - `sources` (`array<string>`, optional): CIDR/IP allowlist
- `v3` (`object`, required when `version` includes `v3`):
  - `users` (`array`, min 1):
    - `username` (`string`, non-empty, max 32)
    - `securityLevel` (`string`: `noAuthNoPriv | authNoPriv | authPriv`)
    - `auth` (`object`, required for `authNoPriv` and `authPriv`):
      - `protocol` (`md5 | sha1 | sha256`; recommended `sha256`)
      - `password` (`string`, min 8)
    - `priv` (`object`, required for `authPriv`):
      - `protocol` (`des | aes128`; recommended `aes128`)
      - `password` (`string`, min 8)
- `traps` (`object`, optional):
  - `enabled` (`bool`)
  - `targets` (`array`):
    - `host` (`string`, required)
    - `port` (`u16`, optional, default `162`)
    - `version` (`v2c | v3`)
    - `community` (`string`, required for `v2c`)
    - `user` (`string`, required for `v3`)

### 2.4 Validation rules

- If `enabled=false`, service may accept partial config and remain idle.
- If `version` includes `v2c`, at least one v2c community is required.
- If `version` includes `v3`, at least one v3 user and `engineId` are required.
- Reject `listenPort=0`.
- Reject empty usernames/community names.
- For `authPriv`, both auth and privacy sections are required.
- Never print plaintext passwords in logs.

## 3. ServiceState (ESP32 -> master)

All SNMP state events are sent as:

- `CMD = ServiceState` (`5`)
- `PARAM = ServiceType::SnmpAgent` (`4`)
- payload is MsgPack map

### 3.1 Recommended event payloads

#### `snmp_started`

```json
{
  "event": "snmp_started",
  "enabled": true,
  "version": "both",
  "listenPort": 161
}
```

#### `snmp_stopped`

```json
{
  "event": "snmp_stopped",
  "enabled": false
}
```

#### `snmp_error`

```json
{
  "event": "snmp_error",
  "error": 3,
  "reason": "socket_bind_failed",
  "detail": "bind 0.0.0.0:161 failed"
}
```

#### `snmp_auth_fail`

```json
{
  "event": "snmp_auth_fail",
  "version": "v3",
  "sourceIp": "192.168.1.10",
  "user": "monitor",
  "reason": "wrong_auth"
}
```

#### `snmp_trap_sent`

```json
{
  "event": "snmp_trap_sent",
  "target": "192.168.1.200:162",
  "oid": "1.3.6.1.6.3.1.1.5.3"
}
```

### 3.2 Suggested numeric error codes

- `1` `invalid_config`
- `2` `not_supported_profile`
- `3` `socket_bind_failed`
- `4` `socket_runtime_error`
- `5` `trap_send_failed`
- `6` `security_error`

## 4. Runtime integration checklist

- Add `SnmpAgent` to `ServiceType`.
- Add `SnmpSettings` to `ServiceSettings` decode/validation.
- Add `snmp_settings_sender/receiver` in `main`.
- Add `services::snmp_agent::spawn_task(...)`.
- Add state encoders in `service_state`.
- Keep WDT feeds and non-blocking wait loops.
- Guard startup with existing lwIP gate logic.

## 5. Security notes

- Password-like fields must be write-only in logs.
- Do not include secrets in `Debug` dumps or UART diagnostics.
- Use source ACL for v2c and v3 where possible.
- Emit explicit auth failure state events for observability.
