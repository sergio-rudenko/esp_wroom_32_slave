use anyhow::Result;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use std::sync::atomic::AtomicI32;
use std::sync::mpsc;

use crate::master::messages::interface_settings::{InterfaceType, WiFiStationSettings};
use crate::master::messages::interface_state;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum WifiDisconnectReason {
    UnknownOrNotSet = 0,
    Unspecified = 1,
    AuthExpire = 2,
    AuthLeave = 3,
    AssocExpire = 4,
    AssocTooMany = 5,
    NotAuthed = 6,
    NotAssoced = 7,
    AssocLeave = 8,
    MicFailure = 14,
    FourWayHandshakeTimeout = 15,
    GroupKeyUpdateTimeout = 16,
    Auth8021xFailed = 23,
    Timeout = 39,
    PeerInitiated = 46,
    ApInitiated = 47,
    BeaconTimeout = 200,
    NoApFound = 201,
    AuthFail = 202,
    AssocFail = 203,
    HandshakeTimeout = 204,
    ConnectionFail = 205,
    ApTsfReset = 206,
    Roaming = 207,
    NoApFoundCompatibleSecurity = 210,
    NoApFoundAuthmodeThreshold = 211,
    NoApFoundRssiThreshold = 212,
}

impl WifiDisconnectReason {
    pub fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::UnknownOrNotSet),
            1 => Some(Self::Unspecified),
            2 => Some(Self::AuthExpire),
            3 => Some(Self::AuthLeave),
            4 => Some(Self::AssocExpire),
            5 => Some(Self::AssocTooMany),
            6 => Some(Self::NotAuthed),
            7 => Some(Self::NotAssoced),
            8 => Some(Self::AssocLeave),
            14 => Some(Self::MicFailure),
            15 => Some(Self::FourWayHandshakeTimeout),
            16 => Some(Self::GroupKeyUpdateTimeout),
            23 => Some(Self::Auth8021xFailed),
            39 => Some(Self::Timeout),
            46 => Some(Self::PeerInitiated),
            47 => Some(Self::ApInitiated),
            200 => Some(Self::BeaconTimeout),
            201 => Some(Self::NoApFound),
            202 => Some(Self::AuthFail),
            203 => Some(Self::AssocFail),
            204 => Some(Self::HandshakeTimeout),
            205 => Some(Self::ConnectionFail),
            206 => Some(Self::ApTsfReset),
            207 => Some(Self::Roaming),
            210 => Some(Self::NoApFoundCompatibleSecurity),
            211 => Some(Self::NoApFoundAuthmodeThreshold),
            212 => Some(Self::NoApFoundRssiThreshold),
            _ => None,
        }
    }

    pub fn as_text(self) -> &'static str {
        match self {
            Self::UnknownOrNotSet => "UNKNOWN_OR_NOT_SET",
            Self::Unspecified => "UNSPECIFIED",
            Self::AuthExpire => "AUTH_EXPIRE",
            Self::AuthLeave => "AUTH_LEAVE",
            Self::AssocExpire => "ASSOC_EXPIRE",
            Self::AssocTooMany => "ASSOC_TOOMANY",
            Self::NotAuthed => "NOT_AUTHED",
            Self::NotAssoced => "NOT_ASSOCED",
            Self::AssocLeave => "ASSOC_LEAVE",
            Self::MicFailure => "MIC_FAILURE",
            Self::FourWayHandshakeTimeout => "4WAY_HANDSHAKE_TIMEOUT",
            Self::GroupKeyUpdateTimeout => "GROUP_KEY_UPDATE_TIMEOUT",
            Self::Auth8021xFailed => "802_1X_AUTH_FAILED",
            Self::Timeout => "TIMEOUT",
            Self::PeerInitiated => "PEER_INITIATED",
            Self::ApInitiated => "AP_INITIATED",
            Self::BeaconTimeout => "BEACON_TIMEOUT",
            Self::NoApFound => "NO_AP_FOUND",
            Self::AuthFail => "AUTH_FAIL",
            Self::AssocFail => "ASSOC_FAIL",
            Self::HandshakeTimeout => "HANDSHAKE_TIMEOUT",
            Self::ConnectionFail => "CONNECTION_FAIL",
            Self::ApTsfReset => "AP_TSF_RESET",
            Self::Roaming => "ROAMING",
            Self::NoApFoundCompatibleSecurity => "NO_AP_FOUND_COMPATIBLE_SECURITY",
            Self::NoApFoundAuthmodeThreshold => "NO_AP_FOUND_AUTHMODE_THRESHOLD",
            Self::NoApFoundRssiThreshold => "NO_AP_FOUND_RSSI_THRESHOLD",
        }
    }
}

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
    WifiDisconnectReason::from_code(reason)
        .map(WifiDisconnectReason::as_text)
        .unwrap_or("OTHER")
}
