use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::master::protocol::{self, MessageType, Packet};

const MAX_PAYLOAD_SIZE: usize = 1500;

#[derive(Debug, Clone, Deserialize)]
pub struct WifiScanRequestPayload {
    #[serde(default)]
    pub limit: u16,
}

#[derive(Debug, Clone)]
pub struct WifiScanRequestMessage {
    pub limit: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct WifiScanResultItem {
    pub ssid: String,
    pub bssid: String,
    pub channel: u8,
    pub rssi: i8,
    #[serde(rename = "authMethod")]
    pub auth_method: Vec<String>,
}

pub fn decode(packet: &Packet<'_>) -> Result<WifiScanRequestMessage> {
    if packet.cmd != MessageType::WifiScan.as_u8() {
        anyhow::bail!("not a WifiScan packet: cmd={}", packet.cmd);
    }
    if packet.parameter != 0 {
        anyhow::bail!("WifiScan request PARAM must be 0, got {}", packet.parameter);
    }
    let request: WifiScanRequestPayload = rmp_serde::from_slice(packet.payload)?;
    Ok(WifiScanRequestMessage {
        limit: request.limit,
    })
}

pub fn encode(chunk_index: u8, items: &[WifiScanResultItem]) -> Result<Vec<u8>> {
    let payload = rmp_serde::to_vec(items)?;
    if payload.len() > MAX_PAYLOAD_SIZE {
        anyhow::bail!(
            "WifiScan payload too large: {} > {}",
            payload.len(),
            MAX_PAYLOAD_SIZE
        );
    }
    protocol::encode_packet(MessageType::WifiScan.as_u8(), chunk_index, &payload)
}
