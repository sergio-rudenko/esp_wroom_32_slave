#!/usr/bin/env python3
"""
Send periodic UDP discovery requests (JSON {"type":"XXX"}) to a broadcast address
and log replies. Matches firmware mock in src/services/udp_listener.rs:
  request ports: 47701, 23629
  response ports: 23569, 21913
"""

from __future__ import annotations

import argparse
import json
import logging
import select
import socket
import sys
import time

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
            logging.info("Bound local UDP 0.0.0.0:%s for replies", port)
        except OSError as exc:
            logging.error("Cannot bind 0.0.0.0:%s: %s", port, exc)
            for opened in socks:
                opened.close()
            raise SystemExit(1) from exc
    return socks


def drain_replies(socks: list[socket.socket], deadline: float) -> None:
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        readable, _, _ = select.select(socks, [], [], min(remaining, 0.25))
        if not readable:
            continue
        for sock in readable:
            try:
                data, addr = sock.recvfrom(4096)
            except OSError as exc:
                logging.warning("recvfrom failed: %s", exc)
                continue
            text = data.decode("utf-8", errors="replace")
            logging.info("RECV from %s:%s (%d bytes): %s", addr[0], addr[1], len(data), text)
            try:
                obj = json.loads(text)
                logging.info("RECV JSON: %s", json.dumps(obj, ensure_ascii=False))
            except json.JSONDecodeError:
                logging.debug("RECV body is not JSON")


def main() -> None:
    parser = argparse.ArgumentParser(description="Check ESP32 UDP listener (1 request/sec).")
    parser.add_argument("broadcast", help="Broadcast IPv4 (e.g. 192.168.1.255)")
    parser.add_argument(
        "--request-port",
        type=int,
        default=None,
        metavar="PORT",
        help=f"Fixed request port (default: round-robin {REQUEST_PORTS})",
    )
    parser.add_argument("-v", "--verbose", action="store_true", help="Debug logs")
    args = parser.parse_args()

    setup_logging(args.verbose)
    recv_socks = open_response_sockets()

    send_sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    send_sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
    send_sock.bind(("0.0.0.0", 0))
    local = send_sock.getsockname()
    logging.info("Send socket bound at %s:%s", local[0], local[1])

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
            logging.info("SEND to %s:%s (%d bytes): %s", dest[0], dest[1], len(payload), payload)

            deadline = time.monotonic() + 1.0
            drain_replies(recv_socks, deadline)
            sleep_left = deadline - time.monotonic()
            if sleep_left > 0:
                time.sleep(sleep_left)
    except KeyboardInterrupt:
        logging.info("Stopped by user")
    finally:
        send_sock.close()
        for sock in recv_socks:
            sock.close()


if __name__ == "__main__":
    main()
