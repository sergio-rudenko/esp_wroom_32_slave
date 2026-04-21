use anyhow::Result;
use serde::Serialize;

use crate::master::messages::interface_settings::InterfaceType;
use crate::master::protocol::{MessageType, encode_packet};

#[derive(Debug, Clone, Serialize)]
pub struct ConnectedPayload {
    pub connected: bool,
    pub mac: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConnectErrorPayload {
    pub connected: bool,
    #[serde(rename = "connectError")]
    pub connect_error: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct GotIpPayload {
    #[serde(rename = "ipConfig")]
    pub ip_config: [String; 3],
}

#[derive(Debug, Clone, Serialize)]
pub struct RssiPayload {
    pub rssi: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisconnectedPayload {
    pub connected: bool,
    #[serde(rename = "disconnectReason")]
    pub disconnect_reason: i32,
}

fn encode_payload<T: Serialize>(interface: InterfaceType, payload: &T) -> Result<Vec<u8>> {
    let payload = rmp_serde::to_vec_named(payload)?;
    encode_packet(
        MessageType::InterfaceState.as_u8(),
        interface.as_u8(),
        &payload,
    )
}

pub fn encode_connected(interface: InterfaceType, mac: String) -> Result<Vec<u8>> {
    encode_payload(
        interface,
        &ConnectedPayload {
            connected: true,
            mac,
        },
    )
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

pub fn encode_got_ip(interface: InterfaceType, ip_config: [String; 3]) -> Result<Vec<u8>> {
    encode_payload(interface, &GotIpPayload { ip_config })
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
