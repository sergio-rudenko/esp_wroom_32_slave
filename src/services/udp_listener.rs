use anyhow::Result;
use esp_idf_hal::delay::FreeRtos;
use log::*;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

use crate::master::messages::service_settings::UdpListenerSettings;

const UDP_LISTENER_TASK_STACK_BYTES: usize = 16 * 1024;
const POLL_DELAY_MS: u32 = 2;
const RECV_BUF_LEN: usize = 1536;

#[derive(Deserialize)]
struct RequestJson {
    #[serde(rename = "type")]
    request_type: String,
}

#[derive(Serialize)]
struct AdvertiseResponse<'a> {
    #[serde(rename = "serviceId")]
    service_id: &'a str,
    #[serde(rename = "deviceType")]
    device_type: u16,
    port: u16,
}

/// Waits for `UdpListenerSettings` from the master, then binds UDP on `0.0.0.0` for each
/// `requestPorts` entry, receives broadcast/unicast JSON `{"type": "<requestType>"}`, and
/// replies to `client_ip:responsePorts[rr]` (round-robin) with the advertise JSON.
pub fn spawn_task(
    udp_settings_receiver: mpsc::Receiver<UdpListenerSettings>,
    lwip_socket_gate: Arc<AtomicU8>,
    lwip_socket_gate_expected: u8,
) -> Result<()> {
    thread::Builder::new()
        .name("udp-listener".into())
        .stack_size(UDP_LISTENER_TASK_STACK_BYTES)
        .spawn(move || run(udp_settings_receiver, lwip_socket_gate, lwip_socket_gate_expected))
        .map_err(|e| anyhow::anyhow!("failed to spawn UDP listener task: {e}"))?;
    Ok(())
}

fn run(
    udp_settings_receiver: mpsc::Receiver<UdpListenerSettings>,
    lwip_socket_gate: Arc<AtomicU8>,
    lwip_socket_gate_expected: u8,
) {
    info!("UDP listener task started; waiting for ServiceSettings (UdpListener)…");

    let mut settings: UdpListenerSettings = loop {
        match udp_settings_receiver.recv() {
            Ok(settings) => break settings,
            Err(_) => {
                info!("UDP listener task: settings channel closed before first config");
                return;
            }
        }
    };

    let mut sockets: Vec<UdpSocket> = Vec::new();
    let mut buf = [0_u8; RECV_BUF_LEN];
    let mut response_rr: usize = 0;

    loop {
        if sockets.is_empty() {
            wait_for_lwip_driver_init(&lwip_socket_gate, lwip_socket_gate_expected);
            match open_sockets(&settings) {
                Ok(s) => {
                    sockets = s;
                    set_nonblocking(&sockets);
                }
                Err(e) => {
                    error!("UDP listener: bind failed: {e:#}; waiting for new settings");
                    match udp_settings_receiver.recv() {
                        Ok(next_settings) => settings = next_settings,
                        Err(_) => {
                            info!("UDP listener task: settings channel closed");
                            return;
                        }
                    }
                    continue;
                }
            }
        }

        while let Ok(next_settings) = udp_settings_receiver.try_recv() {
            info!("UDP listener: applying new settings");
            drop(sockets);
            sockets = Vec::new();
            settings = next_settings;
        }

        if sockets.is_empty() {
            FreeRtos::delay_ms(10);
            continue;
        }

        for sock in &sockets {
            match sock.recv_from(&mut buf) {
                Ok((n, src)) => {
                    if n == 0 {
                        continue;
                    }
                    if let Err(e) =
                        handle_datagram(&settings, &buf[..n], src, sock, &mut response_rr)
                    {
                        warn!("UDP listener: handle datagram: {e:#}");
                    }
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => warn!("UDP listener: recv_from: {e:#}"),
            }
        }

        FreeRtos::delay_ms(POLL_DELAY_MS);
    }
}

/// Avoids racing `std::net::UdpSocket::bind` with `EspWifi` / `EthDriver` lwIP setup (tcpip mbox).
/// Does not require any interface to have link or IP — only that driver init has finished.
fn wait_for_lwip_driver_init(counter: &Arc<AtomicU8>, expected: u8) {
    const SLICE_MS: u32 = 50;
    const MAX_WAIT_MS: u32 = 120_000;
    let mut waited = 0_u32;
    while counter.load(Ordering::SeqCst) < expected && waited < MAX_WAIT_MS {
        FreeRtos::delay_ms(SLICE_MS);
        waited += SLICE_MS;
    }
    let n = counter.load(Ordering::SeqCst);
    if n < expected {
        warn!(
            "UDP listener: lwIP driver init signals {}/{} after {} ms — binding anyway",
            n, expected, MAX_WAIT_MS
        );
    } else {
        info!(
            "UDP listener: lwIP driver init signals {}/{} (safe to open UDP sockets)",
            n, expected
        );
    }
}

fn open_sockets(settings: &UdpListenerSettings) -> Result<Vec<UdpSocket>> {
    let mut v = Vec::with_capacity(settings.request_ports.len());
    for &port in &settings.request_ports {
        let addr: SocketAddr = format!("0.0.0.0:{}", port).parse()?;
        let sock = UdpSocket::bind(addr)?;
        info!(
            "UDP listener: bound 0.0.0.0:{} (all interfaces) for requests",
            port
        );
        v.push(sock);
    }
    Ok(v)
}

fn set_nonblocking(sockets: &[UdpSocket]) {
    for s in sockets {
        if let Err(e) = s.set_nonblocking(true) {
            warn!("UDP listener: set_nonblocking: {e:#}");
        }
    }
}

fn handle_datagram(
    settings: &UdpListenerSettings,
    data: &[u8],
    src: SocketAddr,
    reply_socket: &UdpSocket,
    response_rr: &mut usize,
) -> Result<()> {
    let text = core::str::from_utf8(data).map_err(|e| anyhow::anyhow!("request not UTF-8: {e}"))?;
    let req: RequestJson = serde_json::from_str(text.trim())
        .map_err(|e| anyhow::anyhow!("request JSON: {e}"))?;

    if req.request_type != settings.request_type {
        debug!(
            "UDP listener: ignore request type='{}' (expected '{}')",
            req.request_type, settings.request_type
        );
        return Ok(());
    }

    let rports = &settings.response_ports;
    let rport = rports[*response_rr % rports.len()];
    *response_rr = response_rr.wrapping_add(1);

    let dest = SocketAddr::new(src.ip(), rport);
    let body = AdvertiseResponse {
        service_id: &settings.service_id,
        device_type: settings.device_type,
        port: settings.port,
    };
    let payload = serde_json::to_vec(&body)?;
    reply_socket.send_to(&payload, dest)?;
    info!(
        "UDP listener: sent advertise to {} (type matched), {} bytes",
        dest,
        payload.len()
    );
    Ok(())
}
