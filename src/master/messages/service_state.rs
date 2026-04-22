use anyhow::Result;
use serde::Serialize;

use crate::master::messages::service_settings::ServiceType;
use crate::master::protocol::{MessageType, encode_packet};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpDisconnectReason {
    Undefined = 0,
    ClientClosedConnection = 1,
    ServerClosedConnection = 2,
    InactivityTimeout = 3,
    NotConnected = 4,
}

impl TcpDisconnectReason {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[derive(Debug, Clone, Serialize)]
struct TcpConnectedPayload {
    connected: bool,
    index: u8,
    #[serde(rename = "remoteIp")]
    remote_ip: String,
    #[serde(rename = "remotePort")]
    remote_port: u16,
}

#[derive(Debug, Clone, Serialize)]
struct TcpDisconnectedPayload {
    connected: bool,
    index: u8,
    reason: u8,
}

#[derive(Debug, Clone, Serialize)]
struct NtpSyncedPayload {
    stratum: u8,
    timet: i64,
    server: String,
}

#[derive(Debug, Clone, Serialize)]
struct NtpErrorPayload {
    error: i32,
    server: String,
}

pub fn encode_tcp_connected(index: u8, remote_ip: String, remote_port: u16) -> Result<Vec<u8>> {
    let payload = TcpConnectedPayload {
        connected: true,
        index,
        remote_ip,
        remote_port,
    };
    let payload = rmp_serde::to_vec_named(&payload)?;
    encode_packet(
        MessageType::ServiceState.as_u8(),
        ServiceType::TcpServer.as_u8(),
        &payload,
    )
}

pub fn encode_tcp_disconnected(index: u8, reason: TcpDisconnectReason) -> Result<Vec<u8>> {
    let payload = TcpDisconnectedPayload {
        connected: false,
        index,
        reason: reason.as_u8(),
    };
    let payload = rmp_serde::to_vec_named(&payload)?;
    encode_packet(
        MessageType::ServiceState.as_u8(),
        ServiceType::TcpServer.as_u8(),
        &payload,
    )
}

pub fn encode_ntp_synced(stratum: u8, timet: i64, server: String) -> Result<Vec<u8>> {
    let payload = NtpSyncedPayload {
        stratum,
        timet,
        server,
    };
    let payload = rmp_serde::to_vec_named(&payload)?;
    encode_packet(
        MessageType::ServiceState.as_u8(),
        ServiceType::NtpClient.as_u8(),
        &payload,
    )
}

pub fn encode_ntp_error(error: i32, server: String) -> Result<Vec<u8>> {
    let payload = NtpErrorPayload { error, server };
    let payload = rmp_serde::to_vec_named(&payload)?;
    encode_packet(
        MessageType::ServiceState.as_u8(),
        ServiceType::NtpClient.as_u8(),
        &payload,
    )
}
