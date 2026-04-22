use anyhow::Result;

use crate::master::protocol::{MessageType, Packet, encode_packet};

#[derive(Debug, Clone)]
pub struct TcpDataMessage {
    pub index: u8,
    pub payload: Vec<u8>,
}

pub fn encode(index: u8, payload: &[u8]) -> Result<Vec<u8>> {
    if index > 3 {
        anyhow::bail!("TcpData index out of range: {}", index);
    }
    encode_packet(MessageType::TcpData.as_u8(), index, payload)
}

pub fn decode(packet: &Packet<'_>) -> Result<TcpDataMessage> {
    if packet.cmd != MessageType::TcpData.as_u8() {
        anyhow::bail!("not a TcpData packet: cmd={}", packet.cmd);
    }
    if packet.parameter > 3 {
        anyhow::bail!("TcpData index out of range: {}", packet.parameter);
    }
    Ok(TcpDataMessage {
        index: packet.parameter,
        payload: packet.payload.to_vec(),
    })
}
