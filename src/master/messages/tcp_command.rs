use anyhow::Result;
use serde::Deserialize;

use crate::master::protocol::{MessageType, Packet};

#[derive(Debug, Clone, Deserialize)]
pub struct TcpCommandPayload {
    pub close: bool,
}

#[derive(Debug, Clone)]
pub struct TcpCommandMessage {
    pub index: u8,
    pub command: TcpCommandPayload,
}

pub fn decode(packet: &Packet<'_>) -> Result<TcpCommandMessage> {
    if packet.cmd != MessageType::TcpCommand.as_u8() {
        anyhow::bail!("not a TcpCommand packet: cmd={}", packet.cmd);
    }
    if packet.parameter > 3 {
        anyhow::bail!("TcpCommand index out of range: {}", packet.parameter);
    }

    let command: TcpCommandPayload = rmp_serde::from_slice(packet.payload)?;
    Ok(TcpCommandMessage {
        index: packet.parameter,
        command,
    })
}
