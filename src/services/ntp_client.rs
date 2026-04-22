use anyhow::Result;
use esp_idf_hal::delay::FreeRtos;
use log::*;
use std::net::ToSocketAddrs;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::master::messages::service_settings::NtpClientSettings;
use crate::master::messages::service_state;

const NTP_CLIENT_TASK_STACK_BYTES: usize = 12 * 1024;
const RETRY_PERIOD_SECS: u64 = 15;
const NTP_PORT: u16 = 123;
const NTP_PACKET_LEN: usize = 48;
const NTP_UNIX_EPOCH_DIFF_SECS: i64 = 2_208_988_800;

// NTP client-local error codes for ServiceState payload.
const NTP_ERROR_RESOLVE: i32 = 1;
const NTP_ERROR_SOCKET: i32 = 2;
const NTP_ERROR_TIMEOUT: i32 = 3;
const NTP_ERROR_INVALID_RESPONSE: i32 = 4;

pub fn mock_settings() -> NtpClientSettings {
    NtpClientSettings {
        enabled: true,
        timezone: 180,
        resync_period: 15,
        servers: vec![
            "0.pool.ntp.org".into(),
            "1.pool.ntp.org".into(),
            "2.pool.ntp.org".into(),
        ],
    }
}

pub fn spawn_task(
    ntp_settings_receiver: mpsc::Receiver<NtpClientSettings>,
    uart_tx_queue_sender: mpsc::Sender<Vec<u8>>,
    lwip_socket_gate: Arc<AtomicU8>,
    lwip_socket_gate_expected: u8,
    connected_links: Arc<AtomicU8>,
) -> Result<()> {
    thread::Builder::new()
        .name("ntp-client".into())
        .stack_size(NTP_CLIENT_TASK_STACK_BYTES)
        .spawn(move || {
            run(
                ntp_settings_receiver,
                uart_tx_queue_sender,
                lwip_socket_gate,
                lwip_socket_gate_expected,
                connected_links,
            )
        })
        .map_err(|e| anyhow::anyhow!("failed to spawn NTP client task: {e}"))?;
    Ok(())
}

fn run(
    ntp_settings_receiver: mpsc::Receiver<NtpClientSettings>,
    uart_tx_queue_sender: mpsc::Sender<Vec<u8>>,
    lwip_socket_gate: Arc<AtomicU8>,
    lwip_socket_gate_expected: u8,
    connected_links: Arc<AtomicU8>,
) {
    info!("NTP client task started; waiting for ServiceSettings (NtpClient)...");

    let mut cfg = match ntp_settings_receiver.recv() {
        Ok(cfg) => cfg,
        Err(_) => {
            info!("NTP client task stopped: settings channel closed before first config");
            return;
        }
    };

    wait_for_lwip_driver_init(&lwip_socket_gate, lwip_socket_gate_expected);
    let mut no_link_reported = false;

    loop {
        if !cfg.enabled {
            info!("NTP client disabled");
            no_link_reported = false;
            cfg = match ntp_settings_receiver.recv() {
                Ok(next) => next,
                Err(_) => {
                    info!("NTP client task stopped: settings channel closed");
                    return;
                }
            };
            continue;
        }
        if connected_links.load(Ordering::SeqCst) == 0 {
            if !no_link_reported {
                info!("NTP client: no active Wi-Fi/Ethernet link, waiting without sync attempts");
                no_link_reported = true;
            }
            match ntp_settings_receiver.recv_timeout(Duration::from_secs(RETRY_PERIOD_SECS)) {
                Ok(next) => cfg = next,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    info!("NTP client task stopped: settings channel closed");
                    return;
                }
            }
            continue;
        }
        no_link_reported = false;

        let mut synced = false;
        for server in &cfg.servers {
            match sync_once(server, cfg.timezone) {
                Ok((stratum, timet)) => {
                    info!("NTP sync success: server={server}, stratum={stratum}, timet={timet}");
                    send_service_state(
                        &uart_tx_queue_sender,
                        service_state::encode_ntp_synced(stratum, timet, server.clone()),
                        "ntp_synced",
                    );
                    synced = true;
                    break;
                }
                Err(err_code) => {
                    warn!("NTP sync failed: server={server}, error={err_code}");
                    send_service_state(
                        &uart_tx_queue_sender,
                        service_state::encode_ntp_error(err_code, server.clone()),
                        "ntp_error",
                    );
                }
            }
        }

        let wait_secs = if synced {
            (cfg.resync_period as u64).saturating_mul(60)
        } else {
            RETRY_PERIOD_SECS
        };
        let deadline = Instant::now() + Duration::from_secs(wait_secs);

        loop {
            match ntp_settings_receiver.recv_timeout(Duration::from_millis(500)) {
                Ok(next) => {
                    cfg = next;
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    info!("NTP client task stopped: settings channel closed");
                    return;
                }
            }
        }
    }
}

fn sync_once(server: &str, timezone_minutes: i32) -> std::result::Result<(u8, i64), i32> {
    let mut resolved = match (server, NTP_PORT).to_socket_addrs() {
        Ok(it) => it,
        Err(_) => return Err(NTP_ERROR_RESOLVE),
    };
    let addr = match resolved.next() {
        Some(addr) => addr,
        None => return Err(NTP_ERROR_RESOLVE),
    };

    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(_) => return Err(NTP_ERROR_SOCKET),
    };
    if socket.set_read_timeout(Some(Duration::from_secs(3))).is_err() {
        return Err(NTP_ERROR_SOCKET);
    }
    if socket.set_write_timeout(Some(Duration::from_secs(3))).is_err() {
        return Err(NTP_ERROR_SOCKET);
    }

    let mut req = [0_u8; NTP_PACKET_LEN];
    req[0] = 0x1B; // LI=0, VN=3, Mode=3 (client)
    if socket.send_to(&req, addr).is_err() {
        return Err(NTP_ERROR_SOCKET);
    }

    let mut resp = [0_u8; NTP_PACKET_LEN];
    let read = match socket.recv(&mut resp) {
        Ok(n) => n,
        Err(err) => {
            if matches!(err.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)
            {
                return Err(NTP_ERROR_TIMEOUT);
            }
            return Err(NTP_ERROR_SOCKET);
        }
    };
    if read < NTP_PACKET_LEN {
        return Err(NTP_ERROR_INVALID_RESPONSE);
    }

    let stratum = resp[1];
    let ntp_secs = u32::from_be_bytes([resp[40], resp[41], resp[42], resp[43]]) as i64;
    if ntp_secs < NTP_UNIX_EPOCH_DIFF_SECS {
        return Err(NTP_ERROR_INVALID_RESPONSE);
    }

    let unix_utc = ntp_secs - NTP_UNIX_EPOCH_DIFF_SECS;
    let timet = unix_utc + (timezone_minutes as i64 * 60);

    // Optional consistency check against local clock monotonicity-ish sanity.
    if SystemTime::now().duration_since(UNIX_EPOCH).is_err() {
        return Err(NTP_ERROR_INVALID_RESPONSE);
    }

    Ok((stratum, timet))
}

fn send_service_state(
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    frame_result: Result<Vec<u8>>,
    event_name: &str,
) {
    match frame_result {
        Ok(frame) => {
            if let Err(err) = uart_tx_queue_sender.send(frame) {
                warn!("ServiceState {event_name} enqueue failed: {err}");
            }
        }
        Err(err) => warn!("ServiceState {event_name} encode failed: {err:#}"),
    }
}

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
            "NTP client: lwIP driver init signals {}/{} after {} ms — starting anyway",
            n, expected, MAX_WAIT_MS
        );
    }
}
