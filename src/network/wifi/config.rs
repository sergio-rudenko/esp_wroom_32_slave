use embedded_svc::wifi::{AccessPointConfiguration, AuthMethod, ClientConfiguration, Configuration};
use esp_idf_hal::sys::EspError;

use crate::master::messages::interface_settings::{WiFiAccessPointSettings, WiFiStationSettings};

pub fn default_sta_settings() -> WiFiStationSettings {
    WiFiStationSettings {
        enabled: false,
        ssid: String::from("disabled"),
        password: String::new(),
        reconnect_period: 15,
        dhcp: true,
        static_config: None,
    }
}

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
    if !settings.enabled {
        let ssid = "ap-disabled"
            .try_into()
            .map_err(|_| EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>())?;
        let password = "disabled-ap-profile"
            .try_into()
            .map_err(|_| EspError::from_infallible::<{ esp_idf_sys::ESP_ERR_INVALID_ARG }>())?;
        return Ok(AccessPointConfiguration {
            ssid,
            ssid_hidden: true,
            channel: 1,
            secondary_channel: None,
            protocols: Default::default(),
            auth_method: AuthMethod::WPA2Personal,
            password,
            max_connections: 1,
        });
    }

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
    let _ = (station_settings, ap_settings);
    Configuration::Mixed(station_cfg.clone(), ap_cfg.clone())
}
