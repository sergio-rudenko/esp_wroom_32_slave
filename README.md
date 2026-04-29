# esp_wroom_32_slave

Прошивка для `ESP32-WROOM-32UE`, где `ESP32` работает как сетевой сопроцессор под управлением `STM32` по UART.

---

## 1. Общее описание работы устройства

`STM32` отправляет в `ESP32` настройки интерфейсов и сервисов по UART1.  
`ESP32` применяет конфигурацию, поднимает Wi-Fi/Ethernet, запускает сервисы и отправляет обратно состояния/события/данные.

Основные роли:

- `STM32` — мастер (управление, команды, конфигурация).
- `ESP32` — slave (сеть и сетевые сервисы).

Что делает прошивка:

- поднимает `WiFi STA`, `WiFi AP`, `Ethernet`;
- запускает `UDP Listener`, `TCP Server`, `NTP Client`;
- передает сетевые события и данные сокетов в `STM32`;
- принимает управляющие команды и бинарные данные для TCP-слотов.

---

## 2. Настройка разработки и сборки (от clone до `firmware.bin`)

### 2.1. Клонирование

```bash
git clone <URL_репозитория>
cd esp_wroom_32_slave
```

### 2.2. Установка toolchain

```bash
cargo install espup
espup install
```

Открыть новый shell или выполнить:

```bash
source "$HOME/export-esp.sh"
```

### 2.3. Установка утилит прошивки

```bash
cargo install espflash
```

`esptool` обычно ставится вместе с ESP-IDF Python env. Если не установлен:

```bash
pip install esptool
```

### 2.4. Сборка merged-образа

```bash
./tools/build_firmware.sh
```

Результат:

- `firmware.bin` в корне проекта.
- используется проектная `partitions.csv` (увеличенный `factory` раздел для текущего размера приложения);
- скрипт автоматически подготавливает `partitions.csv` для `esp-idf-sys` build out-директорий.

### 2.5. Прошивка

```bash
esptool write-flash 0x0 firmware.bin
```

### 2.6. Монитор без заливки

```bash
espflash monitor --baud 115200 --port /dev/ttyUSB0
```

---

## 3. Протокол общения `STM32 <-> ESP32`

### 3.1. Формат кадра

```
SOF(1) + LEN(2, LE) + CMD(1) + PARAM(1) + PAYLOAD(LEN) + CRC16(2, LE)
```

Контракт полей фрейма:

| Поле | Размер | Тип | Обяз. | Диапазон/формат | Описание |
|---|---:|---|---|---|---|
| `SOF` | 1 байт | `u8` | Да | `0xAA` | Маркер начала кадра |
| `LEN` | 2 байта | `u16 LE` | Да | `0..1500` | Длина `PAYLOAD` в байтах |
| `CMD` | 1 байт | `u8` | Да | `MessageType` | Тип сообщения |
| `PARAM` | 1 байт | `u8` | Да | зависит от `CMD` | Подтип/индекс/код причины |
| `PAYLOAD` | `LEN` | `bytes` | Да | MsgPack JSON или raw | Тело сообщения |
| `CRC16` | 2 байта | `u16 LE` | Да | CRC16-CCITT | Контроль целостности по `SOF..PAYLOAD` |

### 3.2. Основные `MessageType`

- `Ready` — только `ESP32 -> STM32`;
- `InterfaceSettings` — только `STM32 -> ESP32`;
- `InterfaceState` — только `ESP32 -> STM32`;
- `ServiceSettings` — только `STM32 -> ESP32`;
- `ServiceState` — только `ESP32 -> STM32`;
- `TcpCommand` — только `STM32 -> ESP32`;
- `TcpData` — двунаправленно;
- `WifiScan` — двунаправленно.

Контракт `CMD`:

| `CMD` | `MessageType` | Направление | `PARAM` | `PAYLOAD` |
|---:|---|---|---|---|
| `1` | `Ready` | `ESP32 -> STM32` | `BootReason` | пусто |
| `2` | `InterfaceSettings` | `STM32 -> ESP32` | `InterfaceType` | MsgPack JSON |
| `3` | `InterfaceState` | `ESP32 -> STM32` | `InterfaceType` | MsgPack JSON |
| `4` | `ServiceSettings` | `STM32 -> ESP32` | `ServiceType` | MsgPack JSON |
| `5` | `ServiceState` | `ESP32 -> STM32` | `ServiceType` | MsgPack JSON |
| `6` | `TcpCommand` | `STM32 -> ESP32` | `index (0..3)` | MsgPack JSON |
| `7` | `TcpData` | `<->` | `index (0..3)` | raw bytes |
| `8` | `WifiScan` | `<->` | request:`0`, response:`chunk_index` | MsgPack JSON |

### 3.3. READY

`ESP32` отправляет `Ready`:

- при старте;
- каждые 5 секунд, пока не получен первый валидный кадр от мастера.

`PARAM` содержит `BootReason` (контракт в таблице 3.4).

### 3.4. Политика WDT и `BootReason`

В проекте включен `Task WDT`:

- таймаут: `10` секунд;
- режим: panic/reset при срабатывании;
- feed выполняется в рабочих циклах задач.

Под надзором WDT:

- `main-loop`
- `uart-tx-task`
- `uart-rx-task`
- `wifi-task`
- `eth-task`
- `udp-listener`
- `tcp-server`
- `ntp-client`

Особенности реализации:

- если TWDT уже инициализирован рантаймом ESP-IDF, прошивка переиспользует его и делает reconfigure до `10s`;
- при раннем завершении задачи (например, `eth-task` при отсутствии PHY на макетной плате) задача отписывается от WDT, чтобы не вызывать ложный reset из-за "мертвого" pthread.

После рестарта `ESP32` отправляет `Ready`, где `PARAM` содержит причину сброса.  
Для watchdog сценариев это обычно:

- `InterruptWatchdog`
- `TaskWatchdog`
- `OtherWatchdog`
- `CpuLockup`

Полный список `BootReason`:

| `PARAM` | `BootReason` | Описание |
|---:|---|---|
| `0` | `Undefined` | Причина не определена |
| `1` | `Power` | Подача питания |
| `2` | `External` | Внешний reset |
| `3` | `Software` | Программный reset |
| `4` | `Panic` | Panic/abort |
| `5` | `InterruptWatchdog` | Interrupt WDT |
| `6` | `TaskWatchdog` | Task WDT |
| `7` | `OtherWatchdog` | Иной WDT reset |
| `8` | `DeepSleep` | Выход из deep sleep |
| `9` | `Brownout` | Brownout reset |
| `10` | `Sdio` | SDIO reset |
| `11` | `Usb` | USB reset |
| `12` | `Jtag` | JTAG reset |
| `13` | `Efuse` | eFuse reset |
| `14` | `PowerGlitch` | Сбой питания |
| `15` | `CpuLockup` | CPU lockup |

---

## 4. Общее описание передачи настроек/приема данных

1. `STM32` отправляет `InterfaceSettings` и `ServiceSettings`.
2. `ESP32` валидирует и применяет настройки.
3. `ESP32` отправляет `InterfaceState`/`ServiceState` о результате.
4. В runtime:
   - сетевые изменения отправляются в `State` сообщения;
   - TCP-трафик передается в `TcpData`.

---

## 4.1 Настройки и состояния сетевых интерфейсов

### 4.1.1 WiFi STA

`InterfaceSettings`, `PARAM = WiFiStation`, payload (MsgPack JSON):

```json
{
  "enabled": true,
  "ssid": "MyWiFi",
  "password": "12345678",
  "reconnectPeriod": 15,
  "dhcp": false,
  "static": [
    "192.168.1.100",
    "255.255.255.0",
    "192.168.1.1",
    "8.8.8.8",
    "8.8.4.4"
  ]
}
```

Контракт payload `InterfaceSettings/WiFiStation`:

| Поле | Тип | Обяз. | Диапазон/формат | Описание |
|---|---|---|---|---|
| `enabled` | `bool` | Да | `true/false` | Включить/выключить STA |
| `ssid` | `string` | Да | `1..32` байта | SSID |
| `password` | `string` | Да | `0..64` байта | Пароль (`""` для open) |
| `reconnectPeriod` | `u16` | Нет | `1..600`, default `15` | Период reconnect, сек |
| `dhcp` | `bool` | Да | `true/false` | Режим IP |
| `static` | `string[5]` | Усл. | `dhcp=false` | `[ip, mask, gw, dns1, dns2]` |

`InterfaceState` (примерные события):

- connected:
```json
{ "connected": true, "mac": "94:B9:7E:C2:EC:C4" }
```
- got_ip:
```json
{ "ip": ["192.168.1.100", "255.255.255.0", "192.168.1.1"] }
```
- connect_error:
```json
{ "error": 12345 }
```
- disconnected:
```json
{ "connected": false, "reason": 8 }
```

Примечание по неверным credentials:

- при ошибке аутентификации (например, `4WAY_HANDSHAKE_TIMEOUT`) прошивка отправляет `connect_error`, затем `disconnected`;
- ожидание поднятия netif ограничено по времени и не блокирует WDT.

### 4.1.2 WiFi AP

`InterfaceSettings`, `PARAM = WiFiAccessPoint`, payload:

```json
{
  "enabled": true,
  "ssid": "ESP32-AP",
  "password": "12345678",
  "channel": 7,
  "maxClients": 4,
  "static": ["192.168.4.1", "255.255.255.0"]
}
```

Контракт payload `InterfaceSettings/WiFiAccessPoint`:

| Поле | Тип | Обяз. | Диапазон/формат | Описание |
|---|---|---|---|---|
| `enabled` | `bool` | Да | `true/false` | Включить/выключить AP |
| `ssid` | `string` | Да | `1..32` байта | SSID AP |
| `password` | `string` | Да | `8..64` байта или `""` | Пароль AP |
| `channel` | `u8` | Да | `1..14` | Wi-Fi канал |
| `maxClients` | `u8` | Да | `1..10` | Макс. число клиентов |
| `static` | `string[2]` | Да | `[ip, mask]` | Статический IP AP |

`InterfaceState`:

- AP поднят:
```json
{ "started": true }
```
- ошибка старта:
```json
{ "started": false, "error": 12345 }
```
- клиент подключился:
```json
{ "clientConnected": true, "mac": "11:22:33:44:55:66", "ip": "192.168.4.100" }
```
- клиент отключился:
```json
{ "clientConnected": false, "mac": "11:22:33:44:55:66" }
```

### 4.1.3 Ethernet

`InterfaceSettings`, `PARAM = Ethernet`, payload:

```json
{
  "enabled": true,
  "dhcp": false,
  "static": [
    "192.168.0.123",
    "255.255.255.0",
    "192.168.0.1",
    "8.8.8.8",
    "8.8.4.4"
  ]
}
```

Контракт payload `InterfaceSettings/Ethernet`:

| Поле | Тип | Обяз. | Диапазон/формат | Описание |
|---|---|---|---|---|
| `enabled` | `bool` | Да | `true/false` | Включить/выключить Ethernet |
| `dhcp` | `bool` | Да | `true/false` | Режим IP |
| `static` | `string[5]` | Усл. | `dhcp=false` | `[ip, mask, gw, dns1, dns2]` |

Примечания по текущей реализации Ethernet:

- для LAN8720 используется RMII clock output с ESP32 на `GPIO16`;
- питание PHY включается на `GPIO5` перед инициализацией;
- при потере линка отправляется `ethernet_disconnected`, после чего прошивка ожидает восстановление линка без периодического спама `ethernet_connect_error`;
- для стендовой проверки без мастера можно включить встроенный fallback-профиль в прошивке: `ETHERNET_MOCK_DHCP_ON_BOOT=true` (DHCP).

Состояния отправляются через `InterfaceState` аналогично STA (подключение, IP, ошибки, disconnect).

---

## 5. Общее описание работы сервисов, настроек/событий/данных

Сервисы управляются через `ServiceSettings`, `PARAM = ServiceType`.

Текущие сервисы:

- `UDPListener`;
- `TCPServer`;
- `NTPClient`.

События/результаты идут через `ServiceState`.

---

## 5.1 Детальное описание UDP Listener

Настройки (`ServiceSettings`, `PARAM=UDPListener`):

```json
{
  "requestPorts": [47701, 23629],
  "responsePorts": [23569, 21913],
  "requestType": "XXX",
  "serviceId": "YYY",
  "deviceType": 0,
  "port": 8000
}
```

Контракт payload `ServiceSettings/UDPListener`:

| Поле | Тип | Обяз. | Диапазон/формат | Описание |
|---|---|---|---|---|
| `requestPorts` | `u16[]` | Да | `1..2` элемента, `1..65535` | Порты приема запросов |
| `responsePorts` | `u16[]` | Да | `1..2` элемента, `1..65535` | Порты ответов (round-robin) |
| `requestType` | `string` | Да | непустая строка | Ожидаемый `type` в запросе |
| `serviceId` | `string` | Да | `1..32` байта | Идентификатор сервиса |
| `deviceType` | `u16` | Да | `0..65535` | Тип устройства |
| `port` | `u16` | Да | `1..65535` | Рекламируемый TCP-порт |

Формат запроса:

```json
{ "type": "XXX" }
```

Формат ответа:

```json
{ "serviceId": "YYY", "deviceType": 0, "port": 8000 }
```

---

## 5.2 Детальное описание TCP Server

Настройки (`ServiceSettings`, `PARAM=TCPServer`):

```json
{
  "port": 8000,
  "clientTimeout": 0
}
```

Контракт payload `ServiceSettings/TCPServer`:

| Поле | Тип | Обяз. | Диапазон/формат | Описание |
|---|---|---|---|---|
| `port` | `u16` | Да | `1..65535` | Порт слушателя |
| `clientTimeout` | `u16` | Да | `0..600` | Таймаут неактивности, сек (`0`=off) |

Ограничения:

- до 4 клиентов одновременно;
- слот клиента: `index` в диапазоне `0..3`.

`ServiceState` при подключении:

```json
{
  "connected": true,
  "index": 0,
  "remoteIp": "192.168.1.123",
  "remotePort": 12345
}
```

`ServiceState` при отключении:

```json
{
  "connected": false,
  "index": 0,
  "reason": 1
}
```

Контракт `ServiceState/TCPServer.reason`:

| Код | `TCPDisconnectReason` | Описание |
|---:|---|---|
| `1` | `ClientClosedConnection` | Клиент закрыл соединение |
| `2` | `ServerClosedConnection` | Сервер закрыл соединение |
| `3` | `InactivityTimeout` | Таймаут неактивности |
| `4` | `NotConnected` | Попытка операции с неактивным слотом |

Передача данных:

- `TcpData` (`ESP32 -> STM32`): сырые байты из сокета клиента, `PARAM=index`;
- `TcpData` (`STM32 -> ESP32`): сырые байты в сокет, `PARAM=index`;
- `MAX_PAYLOAD_SIZE=1500`, большой пакет режется на несколько `TcpData`.

Команда управления:

```json
{ "close": true }
```

Передается как `TcpCommand` с `PARAM=index` для принудительного разрыва.

---

## 5.3 Детальное описание NTP Client

Настройки (`ServiceSettings`, `PARAM=NTPClient`):

```json
{
  "enabled": true,
  "timezone": 180,
  "resyncPeriod": 15,
  "servers": ["0.pool.ntp.org", "1.pool.ntp.org", "2.pool.ntp.org"]
}
```

Контракт payload `ServiceSettings/NTPClient`:

| Поле | Тип | Обяз. | Диапазон/формат | Описание |
|---|---|---|---|---|
| `enabled` | `bool` | Да | `true/false` | Включить/выключить NTP |
| `timezone` | `i32` | Да | минуты UTC offset | Смещение относительно UTC |
| `resyncPeriod` | `u16` | Нет | `1..1440`, default `15` | Интервал ресинка, минуты |
| `servers` | `string[]` | Да | `1..3` элемента | Список NTP-серверов по приоритету |

`ServiceState` при успехе:

```json
{
  "stratum": 2,
  "timet": 1713700000,
  "server": "1.pool.ntp.org"
}
```

`ServiceState` при ошибке:

```json
{
  "error": 1,
  "server": "1.pool.ntp.org"
}
```

Примечание:

- если нет активных сетевых линков (Wi-Fi/Ethernet), синхронизация не запускается.

---

## 6. Общее описание скриптов из `tools`

- `check_udp_listener.py` — проверка UDP discovery;
- `mock_master_uart.py` — эмулятор STM32-мастера по UART;
- `build_firmware.sh` — сборка итогового `firmware.bin`.

### 6.1 Инструкция по `check_udp_listener.py`

Простой запуск:

```bash
python3 tools/check_udp_listener.py 192.168.1.255
```

Опции:

- `--request-port` — фиксированный request порт;
- `-v` / `--verbose` — подробные логи.

Скрипт шлет запрос раз в секунду и логирует ответы.

### 6.2 Инструкция по `mock_master_uart.py`

Запуск:

```bash
python3 tools/mock_master_uart.py /dev/ttyUSB0 --send all -v
```

Полезные опции:

- `--send wifi|ethernet|ap|all|none`
- `--wifi-json '{...}'`
- `--wifi-ap-json '{...}'`
- `--ethernet-json '{...}'`
- `--send-udp-listener --udp-listener-json '{...}'`
- `--send-tcp-server --tcp-server-json '{...}'`
- `--send-ntp-client --ntp-client-json '{...}'`
- `--send-wifi-scan --wifi-scan-limit N`

Что делает:

- отправляет `InterfaceSettings`/`ServiceSettings`/`WifiScan`;
- принимает и декодирует кадры от ESP32;
- показывает AP/TCP/NTP/WifiScan события.

### 6.3 Инструкция по `build_firmware.sh`

Запуск:

```bash
./tools/build_firmware.sh
```

Шаги скрипта:

1. подготавливает ESP-IDF окружение (`export-esp.sh`, `IDF_PATH`, `IDF_TOOLS_PATH`, `ESP_IDF_TOOLS_INSTALL_DIR=global`);
2. при необходимости выносит `CARGO_TARGET_DIR` в `$HOME/.cache/...` (для ФС без поддержки symlink);
3. `cargo build --release`;
4. принудительно пересобирает `partition-table.bin` из проектного `partitions.csv`;
5. `espflash save-image` (ELF -> app image);
6. `esptool merge-bin` (bootloader + partition-table + app -> `firmware.bin`).

Это гарантирует, что в итоговый `firmware.bin` попадает актуальная кастомная partition table (увеличенный `factory`), даже если промежуточная сборка ESP-IDF использовала дефолтный профиль.

После сборки:

```bash
esptool write-flash 0x0 firmware.bin
```

---

## Дополнительно

- Инженерный контекст и журнал решений: `CONTEXT.md`.
- Документация по хост-инструментам: `tools/README.md`.
