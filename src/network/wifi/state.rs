use anyhow::Result;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use std::sync::atomic::AtomicI32;
use std::sync::mpsc;

use crate::master::messages::interface_settings::{InterfaceType, WiFiStationSettings};
use crate::master::messages::interface_state;

pub fn recv_next_or_stop(
    rx: &mpsc::Receiver<WiFiStationSettings>,
) -> Result<WiFiStationSettings, ()> {
    match rx.recv() {
        Ok(next_settings) => Ok(next_settings),
        Err(_) => {
            log::info!("WiFiStation task stopped: event channel closed");
            Err(())
        }
    }
}

pub fn send_interface_state(
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    frame_result: Result<Vec<u8>>,
    event_name: &str,
) {
    match frame_result {
        Ok(frame) => {
            if let Err(err) = uart_tx_queue_sender.send(frame) {
                log::warn!("InterfaceState {event_name} enqueue failed: {err}");
            } else {
                log::info!("Enqueued InterfaceState event: {event_name}");
            }
        }
        Err(err) => log::warn!("InterfaceState {event_name} encode failed: {err:#}"),
    }
}

pub fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub fn resolve_connect_error_code(esp_error_code: i32, disconnect_reason: &AtomicI32) -> i32 {
    let reason = disconnect_reason.swap(0, core::sync::atomic::Ordering::Relaxed);
    if reason != 0 {
        log::info!(
            "WiFiStation connect_error mapped from disconnect reason={} ({})",
            reason,
            disconnect_reason_text(reason)
        );
        reason
    } else {
        esp_error_code
    }
}

pub fn report_rssi_once(
    wifi: &BlockingWifi<EspWifi<'_>>,
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
) {
    match wifi.wifi().get_rssi() {
        Ok(rssi) => {
            send_interface_state(
                uart_tx_queue_sender,
                interface_state::encode_rssi(InterfaceType::WiFiStation, rssi),
                "rssi",
            );
        }
        Err(err) => log::warn!("WiFiStation get_rssi failed: {err:#}"),
    }
}

pub fn disconnect_reason_text(reason: i32) -> &'static str {
    match reason {
        0 => "UNKNOWN_OR_NOT_SET",
        1 => "UNSPECIFIED",
        2 => "AUTH_EXPIRE",
        3 => "AUTH_LEAVE",
        4 => "ASSOC_EXPIRE",
        5 => "ASSOC_TOOMANY",
        6 => "NOT_AUTHED",
        7 => "NOT_ASSOCED",
        8 => "ASSOC_LEAVE",
        14 => "MIC_FAILURE",
        15 => "4WAY_HANDSHAKE_TIMEOUT",
        16 => "GROUP_KEY_UPDATE_TIMEOUT",
        23 => "802_1X_AUTH_FAILED",
        39 => "TIMEOUT",
        46 => "PEER_INITIATED",
        47 => "AP_INITIATED",
        200 => "BEACON_TIMEOUT",
        201 => "NO_AP_FOUND",
        202 => "AUTH_FAIL",
        203 => "ASSOC_FAIL",
        204 => "HANDSHAKE_TIMEOUT",
        205 => "CONNECTION_FAIL",
        206 => "AP_TSF_RESET",
        207 => "ROAMING",
        210 => "NO_AP_FOUND_COMPATIBLE_SECURITY",
        211 => "NO_AP_FOUND_AUTHMODE_THRESHOLD",
        212 => "NO_AP_FOUND_RSSI_THRESHOLD",
        _ => "OTHER",
    }
}
