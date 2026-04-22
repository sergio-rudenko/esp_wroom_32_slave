use esp_idf_hal::sys::EspError;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use esp_idf_sys::{
    esp_ip4_addr_t, esp_netif_dhcpc_start, esp_netif_dhcpc_stop, esp_netif_dhcps_start,
    esp_netif_dhcps_stop, esp_netif_dns_info_t,
    esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP, esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN,
    esp_netif_ip_info_t, esp_netif_set_dns_info, esp_netif_set_ip_info,
};
use std::net::Ipv4Addr;

use crate::master::messages::interface_settings::{WiFiAccessPointSettings, WiFiStationSettings};

#[derive(Clone, Debug)]
struct StaticClientSettings {
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
    gateway: Ipv4Addr,
    dns: Option<Ipv4Addr>,
    secondary_dns: Option<Ipv4Addr>,
}

pub fn apply_sta_ip_settings(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    settings: &WiFiStationSettings,
) -> Result<(), EspError> {
    let netif = wifi.wifi().sta_netif();
    let netif_handle = netif.handle();

    if settings.dhcp {
        let _ = EspError::convert(unsafe { esp_netif_dhcpc_start(netif_handle) });
        log::info!("WiFiStation IP mode: DHCP");
        return Ok(());
    }

    let static_settings = parse_static_client_settings(
        settings
            .static_config
            .as_ref()
            .ok_or_else(|| EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>())?,
    )?;

    let ip_info = esp_netif_ip_info_t {
        ip: ipv4_to_esp(static_settings.ip),
        netmask: ipv4_to_esp(static_settings.netmask),
        gw: ipv4_to_esp(static_settings.gateway),
    };

    EspError::convert(unsafe { esp_netif_dhcpc_stop(netif_handle) })?;
    EspError::convert(unsafe { esp_netif_set_ip_info(netif_handle, &ip_info) })?;
    set_dns_info(netif_handle, static_settings.dns, esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN)?;
    set_dns_info(
        netif_handle,
        static_settings.secondary_dns,
        esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP,
    )?;
    log::info!(
        "WiFiStation IP mode: static ip={}, mask={}, gw={}, dns={:?}, dns2={:?}",
        static_settings.ip,
        static_settings.netmask,
        static_settings.gateway,
        static_settings.dns,
        static_settings.secondary_dns
    );
    Ok(())
}

pub fn apply_ap_ip_settings(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    settings: &WiFiAccessPointSettings,
) -> Result<(), EspError> {
    if !settings.enabled {
        return Ok(());
    }
    let static_settings = parse_ap_static_settings(&settings.static_config)?;
    let netif_handle = wifi.wifi().ap_netif().handle();
    let ip_info = esp_netif_ip_info_t {
        ip: ipv4_to_esp(static_settings.ip),
        netmask: ipv4_to_esp(static_settings.netmask),
        gw: ipv4_to_esp(static_settings.ip),
    };
    // AP netif requires DHCP server to be stopped before changing IP info.
    EspError::convert(unsafe { esp_netif_dhcps_stop(netif_handle) })?;
    EspError::convert(unsafe { esp_netif_set_ip_info(netif_handle, &ip_info) })?;
    EspError::convert(unsafe { esp_netif_dhcps_start(netif_handle) })?;
    log::info!(
        "WiFiAccessPoint IP: ip={}, mask={}",
        static_settings.ip,
        static_settings.netmask
    );
    Ok(())
}

fn parse_static_client_settings(values: &[String]) -> Result<StaticClientSettings, EspError> {
    if values.len() != 5 {
        return Err(EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>());
    }

    Ok(StaticClientSettings {
        ip: parse_ipv4(&values[0])?,
        netmask: parse_ipv4(&values[1])?,
        gateway: parse_ipv4(&values[2])?,
        dns: parse_optional_ipv4(&values[3])?,
        secondary_dns: parse_optional_ipv4(&values[4])?,
    })
}

fn parse_ap_static_settings(values: &[String]) -> Result<StaticClientSettings, EspError> {
    if values.len() != 2 {
        return Err(EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>());
    }
    Ok(StaticClientSettings {
        ip: parse_ipv4(&values[0])?,
        netmask: parse_ipv4(&values[1])?,
        gateway: parse_ipv4(&values[0])?,
        dns: None,
        secondary_dns: None,
    })
}

fn parse_ipv4(value: &str) -> Result<Ipv4Addr, EspError> {
    value
        .parse::<Ipv4Addr>()
        .map_err(|_| EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>())
}

fn parse_optional_ipv4(value: &str) -> Result<Option<Ipv4Addr>, EspError> {
    if value.is_empty() {
        Ok(None)
    } else {
        parse_ipv4(value).map(Some)
    }
}

fn set_dns_info(
    netif_handle: *mut esp_idf_sys::esp_netif_t,
    dns: Option<Ipv4Addr>,
    dns_type: u32,
) -> Result<(), EspError> {
    let mut dns_info: esp_netif_dns_info_t = Default::default();
    dns_info.ip.u_addr.ip4 = match dns {
        Some(addr) => ipv4_to_esp(addr),
        None => esp_ip4_addr_t { addr: 0 },
    };
    EspError::convert(unsafe { esp_netif_set_dns_info(netif_handle, dns_type, &mut dns_info) })
}

fn ipv4_to_esp(ip: Ipv4Addr) -> esp_ip4_addr_t {
    esp_ip4_addr_t {
        addr: u32::to_be(u32::from_be_bytes(ip.octets())),
    }
}
