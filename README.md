# ESP32 UART Slave Controller 2

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
cd esp32-uart-slave-conroller2
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

Поля:

- `SOF`: стартовый байт `0xAA`;
- `LEN`: длина `PAYLOAD` (не всего кадра);
- `CMD`: тип сообщения (`MessageType`);
- `PARAM`: дополнительный параметр (тип интерфейса/сервиса, индекс TCP-слота, индекс чанка и т.п.);
- `PAYLOAD`: полезные данные (MsgPack JSON или raw bytes);
- `CRC16`: CRC16-CCITT по заголовку+payload (без CRC).

### 3.2. Основные `MessageType`

- `Ready` — только `ESP32 -> STM32`;
- `InterfaceSettings` — только `STM32 -> ESP32`;
- `InterfaceState` — только `ESP32 -> STM32`;
- `ServiceSettings` — только `STM32 -> ESP32`;
- `ServiceState` — только `ESP32 -> STM32`;
- `TcpCommand` — только `STM32 -> ESP32`;
- `TcpData` — двунаправленно;
- `WifiScan` — двунаправленно.

### 3.3. READY

`ESP32` отправляет `Ready`:

- при старте;
- каждые 5 секунд, пока не получен первый валидный кадр от мастера.

`PARAM` содержит `BootReason`.

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

После рестарта `ESP32` отправляет `Ready`, где `PARAM` содержит причину сброса.  
Для watchdog сценариев это обычно:

- `InterruptWatchdog`
- `TaskWatchdog`
- `OtherWatchdog`
- `CpuLockup`

Полный список `BootReason`:

- `0` `Undefined`
- `1` `Power`
- `2` `External`
- `3` `Software`
- `4` `Panic`
- `5` `InterruptWatchdog`
- `6` `TaskWatchdog`
- `7` `OtherWatchdog`
- `8` `DeepSleep`
- `9` `Brownout`
- `10` `Sdio`
- `11` `Usb`
- `12` `Jtag`
- `13` `Efuse`
- `14` `PowerGlitch`
- `15` `CpuLockup`

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

Поля:

- `enabled`: включить/выключить STA;
- `ssid`, `password`: учетные данные;
- `reconnectPeriod`: период переподключения (сек);
- `dhcp`: DHCP (`true`) или static (`false`);
- `static`: `ip, mask, gateway, dns1, dns2`.

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

Поля:

- `enabled`: включить/выключить AP;
- `ssid`, `password`: параметры точки доступа;
- `channel`: Wi-Fi канал;
- `maxClients`: максимум клиентов;
- `static`: IP AP и mask (AP работает со static IP).

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

Поля:

- `enabled`: включить/выключить Ethernet;
- `dhcp`: DHCP или static;
- `static`: `ip, mask, gateway, dns1, dns2`.

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

Поля:

- `requestPorts`: порты приема broadcast-запросов (до 2);
- `responsePorts`: порты ответа (до 2, round-robin);
- `requestType`: ожидаемый `type` в запросе;
- `serviceId`: идентификатор сервиса;
- `deviceType`: тип устройства (`uint16`);
- `port`: TCP-порт, который рекламируется клиенту.

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

Поля:

- `port`: порт `0.0.0.0:port`;
- `clientTimeout`: таймаут неактивности в секундах (`0` — выключен).

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

`reason` (`TCPDisconnectReason`):

- `1` ClientClosedConnection
- `2` ServerClosedConnection
- `3` InactivityTimeout
- `4` NotConnected

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

Поля:

- `enabled`: включить/выключить NTP;
- `timezone`: смещение от UTC в минутах;
- `resyncPeriod`: период ресинка в минутах;
- `servers`: до 3 серверов по приоритету.

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

1. `cargo build --release`;
2. `espflash save-image` (ELF -> app image);
3. `esptool merge-bin` (bootloader + partition-table + app -> `firmware.bin`).

После сборки:

```bash
esptool write-flash 0x0 firmware.bin
```

---

## Дополнительно

- Инженерный контекст и журнал решений: `CONTEXT.md`.
- Документация по хост-инструментам: `tools/README.md`.
