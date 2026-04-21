use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::master::protocol::{MessageType, Packet};

#[allow(dead_code)]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceType {
    Undefined = 0,
    WiFiStation = 1,
    WiFiAccessPoint = 2,
    Ethernet = 3,
}

impl InterfaceType {
    #[allow(dead_code)]
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

impl TryFrom<u8> for InterfaceType {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        match value {
            0 => Ok(Self::Undefined),
            1 => Ok(Self::WiFiStation),
            2 => Ok(Self::WiFiAccessPoint),
            3 => Ok(Self::Ethernet),
            _ => anyhow::bail!("unknown InterfaceType: {}", value),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WiFiStationSettings {
    pub enabled: bool,
    pub ssid: String,
    pub password: String,
    #[serde(default = "default_reconnect_period", rename = "reconnectPeriod")]
    pub reconnect_period: u16,
    pub dhcp: bool,
    #[serde(default, rename = "static")]
    pub static_config: Option<Vec<String>>,
}

#[allow(dead_code)]
fn default_reconnect_period() -> u16 {
    15
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WiFiAccessPointSettings {
    pub enabled: bool,
    pub ssid: String,
    pub password: String,
    pub channel: u8,
    #[serde(rename = "maxClients")]
    pub max_clients: u8,
    #[serde(rename = "static")]
    pub static_config: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EthernetSettings {
    pub enabled: bool,
    pub dhcp: bool,
    #[serde(default, rename = "static")]
    pub static_config: Option<Vec<String>>,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum InterfaceSettings {
    WiFiStation(WiFiStationSettings),
    WiFiAccessPoint(WiFiAccessPointSettings),
    Ethernet(EthernetSettings),
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct InterfaceSettingsMessage {
    pub interface: InterfaceType,
    pub settings: InterfaceSettings,
}

#[allow(dead_code)]
pub fn decode(packet: &Packet<'_>) -> Result<InterfaceSettingsMessage> {
    if packet.cmd != MessageType::InterfaceSettings.as_u8() {
        anyhow::bail!("not an InterfaceSettings packet: cmd={}", packet.cmd);
    }

    let interface = InterfaceType::try_from(packet.parameter)?;
    let settings = match interface {
        InterfaceType::WiFiStation => {
            let s: WiFiStationSettings = rmp_serde::from_slice(packet.payload)?;
            validate_wifi_station(&s)?;
            InterfaceSettings::WiFiStation(s)
        }
        InterfaceType::WiFiAccessPoint => {
            let s: WiFiAccessPointSettings = rmp_serde::from_slice(packet.payload)?;
            validate_wifi_ap(&s)?;
            InterfaceSettings::WiFiAccessPoint(s)
        }
        InterfaceType::Ethernet => {
            let s: EthernetSettings = rmp_serde::from_slice(packet.payload)?;
            validate_ethernet(&s)?;
            InterfaceSettings::Ethernet(s)
        }
        InterfaceType::Undefined => anyhow::bail!("InterfaceSettings interface type is Undefined"),
    };

    Ok(InterfaceSettingsMessage { interface, settings })
}

#[allow(dead_code)]
fn validate_wifi_station(s: &WiFiStationSettings) -> Result<()> {
    if s.ssid.as_bytes().len() > 32 {
        anyhow::bail!("WiFiStation ssid too long: {}", s.ssid.as_bytes().len());
    }
    if s.password.as_bytes().len() > 64 {
        anyhow::bail!(
            "WiFiStation password too long: {}",
            s.password.as_bytes().len()
        );
    }
    if !(1..=600).contains(&s.reconnect_period) {
        anyhow::bail!("WiFiStation reconnectPeriod out of range: {}", s.reconnect_period);
    }
    if s.dhcp {
        // ignore static_config if present
        return Ok(());
    }
    let Some(st) = &s.static_config else {
        anyhow::bail!("WiFiStation static config required when dhcp=false");
    };
    if st.len() != 5 {
        anyhow::bail!("WiFiStation static must have 5 strings, got {}", st.len());
    }
    Ok(())
}

#[allow(dead_code)]
fn validate_wifi_ap(s: &WiFiAccessPointSettings) -> Result<()> {
    if s.ssid.as_bytes().len() > 32 {
        anyhow::bail!("WiFiAccessPoint ssid too long: {}", s.ssid.as_bytes().len());
    }
    if s.password.as_bytes().len() > 64 {
        anyhow::bail!(
            "WiFiAccessPoint password too long: {}",
            s.password.as_bytes().len()
        );
    }
    if s.enabled && s.password.len() < 8 {
        anyhow::bail!("WiFiAccessPoint password too short (min 8)");
    }
    if !(1..=14).contains(&s.channel) {
        anyhow::bail!("WiFiAccessPoint channel out of range: {}", s.channel);
    }
    if !(1..=10).contains(&s.max_clients) {
        anyhow::bail!("WiFiAccessPoint maxClients out of range: {}", s.max_clients);
    }
    if s.static_config.len() != 1 {
        anyhow::bail!(
            "WiFiAccessPoint static must have 1 string (AP IP), got {}",
            s.static_config.len()
        );
    }
    Ok(())
}

#[allow(dead_code)]
fn validate_ethernet(s: &EthernetSettings) -> Result<()> {
    if s.dhcp {
        return Ok(());
    }
    let Some(st) = &s.static_config else {
        anyhow::bail!("Ethernet static config required when dhcp=false");
    };
    if st.len() != 5 {
        anyhow::bail!("Ethernet static must have 5 strings, got {}", st.len());
    }
    Ok(())
}

