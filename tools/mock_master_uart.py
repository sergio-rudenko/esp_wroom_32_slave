#!/usr/bin/env python3
"""
UART master emulator for ESP32 firmware protocol.

- Opens serial port (COMx on Windows, /dev/ttyUSBx on Linux)
- Sends InterfaceSettings (MsgPack payload in framed packet)
- Receives and decodes frames from ESP32 (READY, InterfaceState, etc.)

Requires:
  pip install pyserial msgpack
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
import time
from dataclasses import dataclass
from typing import Any

import msgpack  # type: ignore
import serial  # type: ignore

SOF = 0xAA
CRC_SIZE = 2
HEADER_SIZE = 5  # SOF + LEN(2) + CMD + PARAM
MIN_FRAME_SIZE = HEADER_SIZE + CRC_SIZE
MAX_PAYLOAD_SIZE = 1500


class MessageType:
    UNDEFINED = 0
    READY = 1
    INTERFACE_SETTINGS = 2
    INTERFACE_STATE = 3
    SERVICE_SETTINGS = 4
    SERVICE_STATE = 5
    TCP_COMMAND = 6
    TCP_EVENT = 7


class InterfaceType:
    UNDEFINED = 0
    WIFI_STATION = 1
    WIFI_AP = 2
    ETHERNET = 3


class ServiceType:
    UNDEFINED = 0
    UDP_LISTENER = 1
    TCP_SERVER = 2
    NTP_CLIENT = 3


MSG_NAMES = {
    0: "Undefined",
    1: "Ready",
    2: "InterfaceSettings",
    3: "InterfaceState",
    4: "ServiceSettings",
    5: "ServiceState",
    6: "TcpCommand",
    7: "TcpEvent",
}

IFACE_NAMES = {
    0: "Undefined",
    1: "WiFiStation",
    2: "WiFiAccessPoint",
    3: "Ethernet",
}

SERVICE_NAMES = {
    0: "Undefined",
    1: "UdpListener",
    2: "TcpServer",
    3: "NtpClient",
}


@dataclass
class Packet:
    cmd: int
    parameter: int
    payload: bytes


def setup_logging(verbose: bool) -> None:
    level = logging.DEBUG if verbose else logging.INFO
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(levelname)s %(message)s",
        datefmt="%H:%M:%S",
        stream=sys.stdout,
    )


def crc16_ccitt(data: bytes) -> int:
    crc = 0xFFFF
    for b in data:
        crc ^= b << 8
        for _ in range(8):
            if crc & 0x8000:
                crc = ((crc << 1) ^ 0x1021) & 0xFFFF
            else:
                crc = (crc << 1) & 0xFFFF
    return crc


def encode_packet(cmd: int, parameter: int, payload: bytes) -> bytes:
    if len(payload) > MAX_PAYLOAD_SIZE:
        raise ValueError(f"payload too large: {len(payload)}")
    frame = bytearray()
    frame.append(SOF)
    frame.extend(len(payload).to_bytes(2, "little"))
    frame.append(cmd & 0xFF)
    frame.append(parameter & 0xFF)
    frame.extend(payload)
    frame.extend(crc16_ccitt(bytes(frame)).to_bytes(2, "little"))
    return bytes(frame)


def decode_packet(frame: bytes) -> Packet:
    if len(frame) < MIN_FRAME_SIZE:
        raise ValueError("frame too short")
    if frame[0] != SOF:
        raise ValueError(f"bad SOF: 0x{frame[0]:02X}")
    payload_len = int.from_bytes(frame[1:3], "little")
    if payload_len > MAX_PAYLOAD_SIZE:
        raise ValueError("payload too large")
    expected = MIN_FRAME_SIZE + payload_len
    if len(frame) != expected:
        raise ValueError(f"bad frame length {len(frame)} != {expected}")
    recv_crc = int.from_bytes(frame[-2:], "little")
    calc_crc = crc16_ccitt(frame[:-2])
    if recv_crc != calc_crc:
        raise ValueError(f"crc mismatch {recv_crc:04X} != {calc_crc:04X}")
    return Packet(cmd=frame[3], parameter=frame[4], payload=frame[5:-2])


def pop_next_valid_frame(rx_buf: bytearray) -> bytes | None:
    while True:
        idx = 0
        while idx < len(rx_buf) and rx_buf[idx] != SOF:
            idx += 1
        if idx > 0:
            del rx_buf[:idx]
        if len(rx_buf) < MIN_FRAME_SIZE:
            return None
        payload_len = int.from_bytes(rx_buf[1:3], "little")
        if payload_len > MAX_PAYLOAD_SIZE:
            del rx_buf[:1]
            continue
        frame_len = MIN_FRAME_SIZE + payload_len
        if len(rx_buf) < frame_len:
            return None
        candidate = bytes(rx_buf[:frame_len])
        del rx_buf[:frame_len]
        try:
            decode_packet(candidate)
            return candidate
        except ValueError:
            continue


def default_wifi_station_settings() -> dict[str, Any]:
    return {
        "enabled": True,
        "ssid": "Test123",
        "password": "12345678",
        "reconnectPeriod": 15,
        "dhcp": True,
    }


def default_ethernet_settings() -> dict[str, Any]:
    return {
        "enabled": True,
        "dhcp": True,
    }


def default_udp_listener_settings() -> dict[str, Any]:
    return {
        "requestPorts": [47701, 23629],
        "responsePorts": [23569, 21913],
        "requestType": "XXX",
        "serviceId": "YYY",
        "deviceType": 0,
        "port": 8000,
    }


def send_interface_settings(ser: serial.Serial, interface: int, settings: dict[str, Any]) -> None:
    payload = msgpack.packb(settings, use_bin_type=True)
    frame = encode_packet(MessageType.INTERFACE_SETTINGS, interface, payload)
    ser.write(frame)
    ser.flush()
    logging.info(
        "TX InterfaceSettings iface=%s bytes=%d payload=%s",
        IFACE_NAMES.get(interface, str(interface)),
        len(frame),
        json.dumps(settings, ensure_ascii=False),
    )


def send_service_settings(ser: serial.Serial, service: int, settings: dict[str, Any]) -> None:
    payload = msgpack.packb(settings, use_bin_type=True)
    frame = encode_packet(MessageType.SERVICE_SETTINGS, service, payload)
    ser.write(frame)
    ser.flush()
    logging.info(
        "TX ServiceSettings service=%s bytes=%d payload=%s",
        SERVICE_NAMES.get(service, str(service)),
        len(frame),
        json.dumps(settings, ensure_ascii=False),
    )


def decode_payload(payload: bytes) -> Any:
    if not payload:
        return None
    return msgpack.unpackb(payload, raw=False)


def log_rx_packet(pkt: Packet, raw_frame: bytes) -> None:
    cmd_name = MSG_NAMES.get(pkt.cmd, f"Unknown({pkt.cmd})")
    iface_name = IFACE_NAMES.get(pkt.parameter, str(pkt.parameter))
    logging.info("RX %s param=%s payload_len=%d", cmd_name, iface_name, len(pkt.payload))

    if pkt.cmd == MessageType.READY:
        boot_reason = pkt.parameter
        boot_name = {0: "Undefined", 1: "Power", 2: "Reset", 3: "Watchdog"}.get(boot_reason, str(boot_reason))
        logging.info("  READY boot_reason=%s(%s)", boot_reason, boot_name)
        return

    if pkt.payload:
        try:
            obj = decode_payload(pkt.payload)
            logging.info("  payload=%s", json.dumps(obj, ensure_ascii=False))
        except Exception as exc:
            logging.warning("  MsgPack decode failed: %s", exc)
            logging.debug("  raw payload hex: %s", pkt.payload.hex())
    else:
        logging.debug("  empty payload")

    logging.debug("  frame hex: %s", raw_frame.hex())


def parse_json_settings(value: str) -> dict[str, Any]:
    try:
        obj = json.loads(value)
    except json.JSONDecodeError as exc:
        raise argparse.ArgumentTypeError(f"invalid JSON: {exc}") from exc
    if not isinstance(obj, dict):
        raise argparse.ArgumentTypeError("JSON settings must be an object")
    return obj


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Mock STM32 master over UART: send InterfaceSettings, decode ESP32 replies."
    )
    parser.add_argument("port", help="Serial port, e.g. COM2 or /dev/ttyUSB0")
    parser.add_argument("--baud", type=int, default=115200, help="UART baudrate (default: 115200)")
    parser.add_argument(
        "--send",
        choices=("wifi", "ethernet", "both", "none"),
        default="both",
        help="What InterfaceSettings to send at startup",
    )
    parser.add_argument(
        "--wifi-json",
        type=parse_json_settings,
        default=None,
        help="Override WiFiStation settings JSON object",
    )
    parser.add_argument(
        "--ethernet-json",
        type=parse_json_settings,
        default=None,
        help="Override Ethernet settings JSON object",
    )
    parser.add_argument(
        "--startup-delay-ms",
        type=int,
        default=300,
        help="Delay before sending settings after opening UART",
    )
    parser.add_argument(
        "--send-udp-listener",
        action="store_true",
        help="Send ServiceSettings/UdpListener after InterfaceSettings",
    )
    parser.add_argument(
        "--udp-json",
        type=parse_json_settings,
        default=None,
        help="Override UdpListener settings JSON object",
    )
    parser.add_argument("-v", "--verbose", action="store_true", help="Debug logs")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    setup_logging(args.verbose)

    wifi_settings = args.wifi_json if args.wifi_json is not None else default_wifi_station_settings()
    ethernet_settings = args.ethernet_json if args.ethernet_json is not None else default_ethernet_settings()
    udp_settings = args.udp_json if args.udp_json is not None else default_udp_listener_settings()

    try:
        ser = serial.Serial(args.port, args.baud, timeout=0.05)
    except Exception as exc:
        logging.error("Failed to open serial port %s: %s", args.port, exc)
        raise SystemExit(1) from exc

    logging.info("Opened %s @ %d", args.port, args.baud)
    if args.startup_delay_ms > 0:
        time.sleep(args.startup_delay_ms / 1000.0)

    try:
        if args.send in ("wifi", "both"):
            send_interface_settings(ser, InterfaceType.WIFI_STATION, wifi_settings)
        if args.send in ("ethernet", "both"):
            send_interface_settings(ser, InterfaceType.ETHERNET, ethernet_settings)
        if args.send_udp_listener:
            send_service_settings(ser, ServiceType.UDP_LISTENER, udp_settings)

        rx_buf = bytearray()
        while True:
            chunk = ser.read(256)
            if chunk:
                rx_buf.extend(chunk)
                while True:
                    frame = pop_next_valid_frame(rx_buf)
                    if frame is None:
                        break
                    try:
                        pkt = decode_packet(frame)
                        log_rx_packet(pkt, frame)
                    except ValueError as exc:
                        logging.warning("RX decode failed: %s", exc)
    except KeyboardInterrupt:
        logging.info("Stopped by user")
    finally:
        ser.close()
        logging.info("Serial closed")


if __name__ == "__main__":
    main()
