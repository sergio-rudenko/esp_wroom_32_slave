use anyhow::Result;
use serde::Serialize;

use crate::master::messages::interface_settings::InterfaceType;
use crate::master::protocol::{MessageType, encode_packet};

#[derive(Debug, Clone, Serialize)]
pub struct ConnectedPayload {
    pub connected: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectErrorPayload {
    pub connected: bool,
    #[serde(rename = "error")]
    pub connect_error: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GotIpPayload {
    pub mac: String,
    #[serde(rename = "ip")]
    pub ip_config: [String; 5],
}

#[derive(Debug, Clone, Serialize)]
pub struct RssiPayload {
    pub rssi: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisconnectedPayload {
    pub connected: bool,
    #[serde(rename = "reason")]
    pub disconnect_reason: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApStartedPayload {
    pub started: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApStartErrorPayload {
    pub started: bool,
    pub error: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApClientConnectedPayload {
    #[serde(rename = "clientConnected")]
    pub client_connected: bool,
    pub mac: String,
    pub ip: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApClientDisconnectedPayload {
    #[serde(rename = "clientConnected")]
    pub client_connected: bool,
    pub mac: String,
}

fn encode_payload<T: Serialize>(interface: InterfaceType, payload: &T) -> Result<Vec<u8>> {
    let payload = rmp_serde::to_vec_named(payload)?;
    encode_packet(
        MessageType::InterfaceState.as_u8(),
        interface.as_u8(),
        &payload,
    )
}

pub fn encode_connected(interface: InterfaceType) -> Result<Vec<u8>> {
    encode_payload(interface, &ConnectedPayload { connected: true })
}

pub fn encode_connect_error(interface: InterfaceType, connect_error: i32) -> Result<Vec<u8>> {
    encode_payload(
        interface,
        &ConnectErrorPayload {
            connected: false,
            connect_error,
        },
    )
}

pub fn encode_got_ip(interface: InterfaceType, mac: String, ip_config: [String; 5]) -> Result<Vec<u8>> {
    encode_payload(interface, &GotIpPayload { mac, ip_config })
}

pub fn encode_rssi(interface: InterfaceType, rssi: i32) -> Result<Vec<u8>> {
    encode_payload(interface, &RssiPayload { rssi })
}

pub fn encode_disconnected(interface: InterfaceType, disconnect_reason: i32) -> Result<Vec<u8>> {
    encode_payload(
        interface,
        &DisconnectedPayload {
            connected: false,
            disconnect_reason,
        },
    )
}

pub fn encode_ap_started(interface: InterfaceType) -> Result<Vec<u8>> {
    encode_payload(interface, &ApStartedPayload { started: true })
}

pub fn encode_ap_start_error(interface: InterfaceType, error: i32) -> Result<Vec<u8>> {
    encode_payload(
        interface,
        &ApStartErrorPayload {
            started: false,
            error,
        },
    )
}

pub fn encode_ap_client_connected(interface: InterfaceType, mac: String, ip: String) -> Result<Vec<u8>> {
    encode_payload(
        interface,
        &ApClientConnectedPayload {
            client_connected: true,
            mac,
            ip,
        },
    )
}

pub fn encode_ap_client_disconnected(interface: InterfaceType, mac: String) -> Result<Vec<u8>> {
    encode_payload(
        interface,
        &ApClientDisconnectedPayload {
            client_connected: false,
            mac,
        },
    )
}
