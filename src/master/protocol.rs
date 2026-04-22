use anyhow::Result;

const SOF: u8 = 0xAA;
const LEN_SIZE: usize = 2;
const CMD_SIZE: usize = 1;
const PARAM_SIZE: usize = 1;
const CRC_SIZE: usize = 2;
const HEADER_SIZE: usize = 1 + LEN_SIZE + CMD_SIZE + PARAM_SIZE;
const MIN_FRAME_SIZE: usize = HEADER_SIZE + CRC_SIZE;
const MAX_PAYLOAD_SIZE: usize = 1500;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum MessageType {
    Undefined = 0,
    Ready = 1,
    InterfaceSettings = 2,
    InterfaceState = 3,
    ServiceSettings = 4,
    ServiceState = 5,
    TcpCommand = 6,
    TcpData = 7,
    WifiScan = 8,
}

impl TryFrom<u8> for MessageType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Undefined),
            1 => Ok(Self::Ready),
            2 => Ok(Self::InterfaceSettings),
            3 => Ok(Self::InterfaceState),
            4 => Ok(Self::ServiceSettings),
            5 => Ok(Self::ServiceState),
            6 => Ok(Self::TcpCommand),
            7 => Ok(Self::TcpData),
            8 => Ok(Self::WifiScan),
            _ => anyhow::bail!("unknown MessageType: {}", value),
        }
    }
}

impl MessageType {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

#[allow(dead_code)]
pub struct Packet<'a> {
    pub cmd: u8,
    pub parameter: u8,
    pub payload: &'a [u8],
}

fn crc16_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for byte in data {
        crc ^= (*byte as u16) << 8;
        for _ in 0..8 {
            if (crc & 0x8000) != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[allow(dead_code)]
pub fn encode_packet(cmd: u8, parameter: u8, payload: &[u8]) -> Result<Vec<u8>> {
    if payload.len() > MAX_PAYLOAD_SIZE {
        anyhow::bail!("payload too large: {} > {}", payload.len(), MAX_PAYLOAD_SIZE);
    }

    let mut frame = Vec::with_capacity(MIN_FRAME_SIZE + payload.len());
    frame.push(SOF);
    frame.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    frame.push(cmd);
    frame.push(parameter);
    frame.extend_from_slice(payload);

    let crc = crc16_ccitt(&frame);
    frame.extend_from_slice(&crc.to_le_bytes());
    Ok(frame)
}

#[allow(dead_code)]
pub fn decode_packet(frame: &[u8]) -> Result<Packet<'_>> {
    if frame.len() < MIN_FRAME_SIZE {
        anyhow::bail!("frame too short: {}", frame.len());
    }
    if frame[0] != SOF {
        anyhow::bail!("invalid SOF: 0x{:02X}", frame[0]);
    }

    let payload_len = u16::from_le_bytes([frame[1], frame[2]]) as usize;
    if payload_len > MAX_PAYLOAD_SIZE {
        anyhow::bail!("payload too large in frame: {}", payload_len);
    }

    let expected_len = MIN_FRAME_SIZE + payload_len;
    if frame.len() != expected_len {
        anyhow::bail!("invalid frame length: got {}, expected {}", frame.len(), expected_len);
    }

    let crc_offset = frame.len() - CRC_SIZE;
    let received_crc = u16::from_le_bytes([frame[crc_offset], frame[crc_offset + 1]]);
    let calculated_crc = crc16_ccitt(&frame[..crc_offset]);
    if received_crc != calculated_crc {
        anyhow::bail!(
            "crc mismatch: received 0x{:04X}, calculated 0x{:04X}",
            received_crc,
            calculated_crc
        );
    }

    let payload_start = HEADER_SIZE;
    let payload_end = payload_start + payload_len;
    Ok(Packet {
        cmd: frame[3],
        parameter: frame[4],
        payload: &frame[payload_start..payload_end],
    })
}

pub fn pop_next_valid_frame(rx_buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    loop {
        let mut idx = 0_usize;
        while idx < rx_buf.len() {
            if rx_buf[idx] == SOF {
                break;
            }
            idx += 1;
        }

        if idx > 0 {
            rx_buf.drain(..idx);
        }

        if rx_buf.len() < MIN_FRAME_SIZE {
            return None;
        }

        let payload_len = u16::from_le_bytes([rx_buf[1], rx_buf[2]]) as usize;
        if payload_len > MAX_PAYLOAD_SIZE {
            rx_buf.drain(..1);
            continue;
        }

        let frame_len = MIN_FRAME_SIZE + payload_len;
        if rx_buf.len() < frame_len {
            return None;
        }

        let frame = rx_buf[..frame_len].to_vec();
        rx_buf.drain(..frame_len);

        if decode_packet(&frame).is_ok() {
            return Some(frame);
        }
    }
}
