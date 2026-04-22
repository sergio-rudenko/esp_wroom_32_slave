# Инструменты `tools/`

В этой папке лежат Python-скрипты для проверки прошивки без STM32.

## 1) `check_udp_listener.py`

Скрипт проверяет UDP discovery listener на ESP32:

- отправляет broadcast-запрос `{"type":"XXX"}` раз в секунду;
- по умолчанию чередует request-порты `47701` и `23629`;
- слушает ответы на локальных портах `23569` и `21913`;
- логирует отправки и ответы (включая попытку JSON-разбора ответа).

### Запуск

```bash
python3 tools/check_udp_listener.py 192.168.1.255
```

С фиксированным request-портом:

```bash
python3 tools/check_udp_listener.py 192.168.1.255 --request-port 47701
```

Подробные логи:

```bash
python3 tools/check_udp_listener.py 192.168.1.255 -v
```

### Параметры

- `broadcast` — IPv4 broadcast-адрес (например, `192.168.1.255` или `255.255.255.255`);
- `--request-port` — фиксированный порт запроса (если не указан, используется round-robin);
- `-v` / `--verbose` — детальные логи.

### Важно

- если порты `23569`/`21913` заняты другим процессом, скрипт завершится ошибкой bind;
- остановка: `Ctrl+C`.

---

## 2) `mock_master_uart.py`

Скрипт эмулирует мастер-контроллер STM32 по UART:

- открывает последовательный порт (`COM2`, `/dev/ttyUSB0`, `/dev/ttyACM0` и т.д.);
- отправляет в ESP32 `InterfaceSettings` (WiFiStation / Ethernet) в формате:
  - framing: `SOF + LEN + CMD + PARAM + PAYLOAD + CRC16-CCITT`;
  - payload: MsgPack;
- принимает и декодирует кадры от ESP32:
  - `READY` (в т.ч. `boot_reason`);
  - `InterfaceState` и другие сообщения с попыткой MsgPack-декодирования;
  - `WifiScan` ответы (чанки, где `PARAM` = номер чанка).

### Зависимости

```bash
pip install pyserial msgpack
```

### Базовый запуск

Linux:

```bash
python3 tools/mock_master_uart.py /dev/ttyUSB0
```

Windows:

```bash
python tools/mock_master_uart.py COM2
```

### Полезные параметры

- `--baud 115200` — скорость UART (по умолчанию `115200`);
- `--send wifi|ethernet|both|none` — что отправлять при старте (по умолчанию `both`);
- `--wifi-json '{...}'` — override WiFiStation-конфига JSON-объектом;
- `--ethernet-json '{...}'` — override Ethernet-конфига JSON-объектом;
- `--startup-delay-ms 300` — задержка перед отправкой после открытия порта;
- `--send-udp-listener` — дополнительно отправить `ServiceSettings` для `UdpListener`;
- `--udp-listener-json '{...}'` — override `UdpListener`-настроек JSON-объектом;
- `--send-tcp-server` — дополнительно отправить `ServiceSettings` для `TcpServer`;
- `--tcp-server-json '{...}'` — override `TcpServer`-настроек JSON-объектом;
- `--send-ntp-client` — дополнительно отправить `ServiceSettings` для `NtpClient`;
- `--ntp-client-json '{...}'` — override `NtpClient`-настроек JSON-объектом;
- `--send-wifi-scan` — однократно отправить запрос `WifiScan` через 5 секунд после старта;
- `--wifi-scan-limit N` — `limit` в запросе `WifiScan` (`0` = вернуть все AP, по умолчанию `0`);
- `-v` / `--verbose` — детальные логи.

Пример с явными конфигами:

```bash
python3 tools/mock_master_uart.py /dev/ttyUSB0 \
  --send both \
  --send-udp-listener \
  --send-tcp-server \
  --send-ntp-client \
  --send-wifi-scan \
  --wifi-scan-limit 20 \
  --wifi-json '{"enabled":true,"ssid":"Test123","password":"12345678","reconnectPeriod":15,"dhcp":true}' \
  --ethernet-json '{"enabled":true,"dhcp":true}' \
  --udp-listener-json '{"requestPorts":[47701,23629],"responsePorts":[23569,21913],"requestType":"XXX","serviceId":"YYY","deviceType":0,"port":8000}' \
  --tcp-server-json '{"port":8000,"clientTimeout":0}' \
  --ntp-client-json '{"enabled":true,"timezone":180,"resyncPeriod":15,"servers":["0.pool.ntp.org","1.pool.ntp.org","2.pool.ntp.org"]}' \
  -v
```

### Что делать, если нет обмена

1. Проверьте порт и скорость (`115200`).
2. Проверьте, что TX/RX/GND между USB-UART и ESP32 подключены корректно.
3. Убедитесь, что никакая другая программа не держит этот COM/tty.
4. Включите `-v` и посмотрите, появляются ли сырые RX-данные/кадры.
5. Для теста сначала запустите только `--send wifi` или только `--send ethernet`.
