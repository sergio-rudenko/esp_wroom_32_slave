#!/usr/bin/env python3
"""
Send periodic UDP discovery requests (JSON {"type": "XXX"}) to a broadcast address
and log replies. Matches default firmware mock in services/udp_listener::mock_settings:
  request ports: 47701, 23629
  response ports (ESP sends TO your host on these): 23569, 21913

Поведение
Аргумент: broadcast — IPv4 адрес broadcast (например 192.168.1.255 или 255.255.255.255).

Раз в секунду: отправка JSON {"type":"XXX"} на выбранный порт запроса (по умолчанию по 
очереди 47701 и 23629, как в моке прошивки).

Приём: два UDP-сокета на 0.0.0.0:23569 и 0.0.0.0:21913, потому что ESP шлёт ответ на ваш 
IP, но на порт из responsePorts, а не на эфемерный порт отправки.

Лог: каждая отправка (SEND …) и каждый ответ (RECV … + разбор JSON при успехе).

Запуск

python3 check_udp_listener.py 192.168.1.255

Фиксированный порт запроса (без чередования):

python3 check_udp_listener.py 192.168.1.255 --request-port 47701

Подробнее в логах:

python3 check_udp_listener.py 192.168.1.255 -v

Остановка: Ctrl+C. Если порты 23569/21913 уже заняты на ПК, скрипт выйдет 
с ошибкой bind — освободите порты или измените мок на стороне ESP.
"""

from __future__ import annotations

import argparse
import json
import logging
import select
import socket
import sys
import time

# Default mock from services/udp_listener.rs
REQUEST_PORTS = (47701, 23629)
RESPONSE_PORTS = (23569, 21913)
REQUEST_PAYLOAD = {"type": "XXX"}


def setup_logging(verbose: bool) -> None:
    level = logging.DEBUG if verbose else logging.INFO
    logging.basicConfig(
        level=level,
        format="%(asctime)s %(levelname)s %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
        stream=sys.stdout,
    )


def open_response_sockets() -> list[socket.socket]:
    socks: list[socket.socket] = []
    for port in RESPONSE_PORTS:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(("0.0.0.0", port))
            socks.append(s)
            logging.info("Bound local UDP 0.0.0.0:%s for advertise replies", port)
        except OSError as e:
            logging.error("Cannot bind 0.0.0.0:%s: %s (close other apps using this port)", port, e)
            for x in socks:
                x.close()
            raise SystemExit(1) from e
    return socks


def drain_replies(socks: list[socket.socket], deadline: float) -> None:
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        r, _, _ = select.select(socks, [], [], min(remaining, 0.25))
        if not r:
            continue
        for s in r:
            try:
                data, addr = s.recvfrom(4096)
            except OSError as e:
                logging.warning("recvfrom failed: %s", e)
                continue
            text = data.decode("utf-8", errors="replace")
            logging.info("RECV from %s:%s (%d bytes): %s", addr[0], addr[1], len(data), text)
            try:
                obj = json.loads(text)
                logging.info("RECV JSON: %s", json.dumps(obj, ensure_ascii=False))
            except json.JSONDecodeError:
                logging.debug("RECV body is not JSON")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Check esp32 udp_listener (mock): send discovery once per second."
    )
    parser.add_argument(
        "broadcast",
        help="Broadcast IPv4 to send requests to (e.g. 192.168.1.255 or 255.255.255.255)",
    )
    parser.add_argument(
        "--request-port",
        type=int,
        default=None,
        metavar="PORT",
        help=f"Fixed request UDP port (default: alternate each second {REQUEST_PORTS})",
    )
    parser.add_argument("-v", "--verbose", action="store_true", help="Debug logging")
    args = parser.parse_args()

    setup_logging(args.verbose)

    recv_socks = open_response_sockets()

    send_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    send_sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    send_sock.bind(("0.0.0.0", 0))
    local = send_sock.getsockname()
    logging.info("Send socket bound at %s:%s (SO_BROADCAST=1)", local[0], local[1])

    payload = json.dumps(REQUEST_PAYLOAD, separators=(",", ":")).encode("utf-8")
    rr = 0

    try:
        while True:
            if args.request_port is not None:
                req_port = args.request_port
            else:
                req_port = REQUEST_PORTS[rr % len(REQUEST_PORTS)]
                rr += 1

            dest = (args.broadcast, req_port)
            send_sock.sendto(payload, dest)
            logging.info(
                "SEND to %s:%s (%d bytes): %s",
                dest[0],
                dest[1],
                len(payload),
                payload.decode("utf-8"),
            )

            # Collect replies until next second
            deadline = time.monotonic() + 1.0
            drain_replies(recv_socks, deadline)

            sleep_left = deadline - time.monotonic()
            if sleep_left > 0:
                time.sleep(sleep_left)
    except KeyboardInterrupt:
        logging.info("Stopped by user")
    finally:
        send_sock.close()
        for s in recv_socks:
            s.close()


if __name__ == "__main__":
    main()
