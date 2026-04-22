use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::master::protocol::{MessageType, Packet};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceType {
    Undefined = 0,
    UdpListener = 1,
    TcpServer = 2,
    NtpClient = 3,
}

impl ServiceType {
    #[allow(dead_code)]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for ServiceType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Undefined),
            1 => Ok(Self::UdpListener),
            2 => Ok(Self::TcpServer),
            3 => Ok(Self::NtpClient),
            _ => anyhow::bail!("unknown ServiceType: {}", value),
        }
    }
}

/// MsgPack/JSON payload for `ServiceType::UdpListener` (from host).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpListenerSettings {
    #[serde(rename = "requestPorts")]
    pub request_ports: Vec<u16>,
    #[serde(rename = "responsePorts")]
    pub response_ports: Vec<u16>,
    #[serde(rename = "requestType")]
    pub request_type: String,
    #[serde(rename = "serviceId")]
    pub service_id: String,
    #[serde(rename = "deviceType")]
    pub device_type: u16,
    /// TCP port advertised to clients in the UDP response.
    pub port: u16,
}

/// MsgPack/JSON payload for `ServiceType::TcpServer` (from host).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpServerSettings {
    pub port: u16,
    #[serde(rename = "clientTimeout")]
    pub client_timeout: u16,
}

/// MsgPack/JSON payload for `ServiceType::NtpClient` (from host).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NtpClientSettings {
    pub enabled: bool,
    pub timezone: i32,
    #[serde(default = "default_ntp_resync_period", rename = "resyncPeriod")]
    pub resync_period: u16,
    pub servers: Vec<String>,
}

fn default_ntp_resync_period() -> u16 {
    15
}

#[derive(Debug, Clone)]
pub enum ServiceSettings {
    UdpListener(UdpListenerSettings),
    TcpServer(TcpServerSettings),
    NtpClient(NtpClientSettings),
}

#[derive(Debug, Clone)]
pub struct ServiceSettingsMessage {
    pub service: ServiceType,
    pub settings: ServiceSettings,
}

pub fn decode(packet: &Packet<'_>) -> Result<ServiceSettingsMessage> {
    if packet.cmd != MessageType::ServiceSettings.as_u8() {
        anyhow::bail!("not a ServiceSettings packet: cmd={}", packet.cmd);
    }

    let service = ServiceType::try_from(packet.parameter)?;
    match service {
        ServiceType::UdpListener => {
            let s: UdpListenerSettings = rmp_serde::from_slice(packet.payload)?;
            validate_udp_listener(&s)?;
            Ok(ServiceSettingsMessage {
                service,
                settings: ServiceSettings::UdpListener(s),
            })
        }
        ServiceType::TcpServer => {
            let s: TcpServerSettings = rmp_serde::from_slice(packet.payload)?;
            validate_tcp_server(&s)?;
            Ok(ServiceSettingsMessage {
                service,
                settings: ServiceSettings::TcpServer(s),
            })
        }
        ServiceType::NtpClient => {
            let s: NtpClientSettings = rmp_serde::from_slice(packet.payload)?;
            validate_ntp_client(&s)?;
            Ok(ServiceSettingsMessage {
                service,
                settings: ServiceSettings::NtpClient(s),
            })
        }
        ServiceType::Undefined => anyhow::bail!("ServiceSettings service type is Undefined"),
    }
}

fn validate_udp_listener(s: &UdpListenerSettings) -> Result<()> {
    if s.request_ports.is_empty() || s.request_ports.len() > 2 {
        anyhow::bail!("UDP listener requestPorts must have 1..=2 elements, got {}", s.request_ports.len());
    }
    if s.response_ports.is_empty() || s.response_ports.len() > 2 {
        anyhow::bail!("UDP listener responsePorts must have 1..=2 elements, got {}", s.response_ports.len());
    }
    for &p in &s.request_ports {
        if p == 0 {
            anyhow::bail!("UDP listener request port must be non-zero");
        }
    }
    for &p in &s.response_ports {
        if p == 0 {
            anyhow::bail!("UDP listener response port must be non-zero");
        }
    }
    if s.service_id.as_bytes().len() > 32 {
        anyhow::bail!("UDP listener serviceId too long: {} bytes", s.service_id.as_bytes().len());
    }
    Ok(())
}

fn validate_tcp_server(s: &TcpServerSettings) -> Result<()> {
    if s.port == 0 {
        anyhow::bail!("TCP server port must be in 1..=65535");
    }
    if s.client_timeout > 600 {
        anyhow::bail!("TCP server clientTimeout out of range: {}", s.client_timeout);
    }
    Ok(())
}

fn validate_ntp_client(s: &NtpClientSettings) -> Result<()> {
    if !(-720..=840).contains(&s.timezone) {
        anyhow::bail!("NTP client timezone out of range: {}", s.timezone);
    }
    if s.servers.len() > 3 {
        anyhow::bail!("NTP client servers must have at most 3 items, got {}", s.servers.len());
    }
    if s.enabled && s.servers.is_empty() {
        anyhow::bail!("NTP client servers required when enabled=true");
    }
    if !(1..=1440).contains(&s.resync_period) {
        anyhow::bail!("NTP client resyncPeriod out of range: {}", s.resync_period);
    }
    for server in &s.servers {
        if server.trim().is_empty() {
            anyhow::bail!("NTP client server entry must be non-empty");
        }
    }
    Ok(())
}
