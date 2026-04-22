use embedded_svc::wifi::{AccessPointConfiguration, AuthMethod, ClientConfiguration, Configuration};
use esp_idf_hal::sys::EspError;

use crate::master::messages::interface_settings::{WiFiAccessPointSettings, WiFiStationSettings};

pub fn default_ap_settings() -> WiFiAccessPointSettings {
    WiFiAccessPointSettings {
        enabled: false,
        ssid: String::from("ESP32-AP"),
        password: String::from("12345678"),
        channel: 1,
        max_clients: 4,
        static_config: vec![String::from("192.168.4.1"), String::from("255.255.255.0")],
    }
}

pub fn build_ap_configuration(
    settings: &WiFiAccessPointSettings,
) -> Result<AccessPointConfiguration, EspError> {
    let ssid = settings
        .ssid
        .as_str()
        .try_into()
        .map_err(|_| EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>())?;
    let password = settings
        .password
        .as_str()
        .try_into()
        .map_err(|_| EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>())?;
    Ok(AccessPointConfiguration {
        ssid,
        ssid_hidden: false,
        channel: settings.channel,
        secondary_channel: None,
        protocols: Default::default(),
        auth_method: if settings.password.is_empty() {
            AuthMethod::None
        } else {
            AuthMethod::WPA2Personal
        },
        password,
        max_connections: settings.max_clients as u16,
    })
}

pub fn compose_wifi_mode_configuration(
    station_settings: &WiFiStationSettings,
    station_cfg: &ClientConfiguration,
    ap_settings: &WiFiAccessPointSettings,
    ap_cfg: &AccessPointConfiguration,
) -> Configuration {
    match (station_settings.enabled, ap_settings.enabled) {
        (true, true) => Configuration::Mixed(station_cfg.clone(), ap_cfg.clone()),
        (true, false) => Configuration::Client(station_cfg.clone()),
        (false, true) => Configuration::AccessPoint(ap_cfg.clone()),
        (false, false) => Configuration::None,
    }
}
