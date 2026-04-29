mod config;
mod ip;
mod scan;
mod state;

use anyhow::Result;
use embedded_svc::wifi::{AuthMethod, ClientConfiguration};
use esp_idf_hal::modem::Modem;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::netif::IpEvent;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi, WifiDeviceId, WifiEvent};
use log::*;
use esp_idf_sys::{
    esp_netif_dns_info_t, esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP, esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::master::messages::interface_settings::{InterfaceType, WiFiAccessPointSettings, WiFiStationSettings};
use crate::master::messages::interface_state;
use crate::master::messages::wifi_scan::WifiScanRequestMessage;

use self::config::{build_ap_configuration, compose_wifi_mode_configuration, default_ap_settings, default_sta_settings};
use self::ip::{apply_ap_ip_settings, apply_sta_ip_settings};
use self::scan::process_wifi_scan_requests;
use self::state::{
    ap_error_text, disconnect_reason_text, format_mac, recv_next_or_stop, report_rssi_once,
    resolve_connect_error_code, send_interface_state, WifiDisconnectReason,
};

enum NetifWaitOutcome {
    Up,
    Disconnected(i32),
    Timeout,
}

pub fn spawn_task(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    wifi_station_interface_settings_receiver: mpsc::Receiver<WiFiStationSettings>,
    wifi_ap_interface_settings_receiver: mpsc::Receiver<WiFiAccessPointSettings>,
    wifi_scan_receiver: mpsc::Receiver<WifiScanRequestMessage>,
    _tcp_server_network_reset_sender: mpsc::Sender<()>,
    uart_tx_queue_sender: mpsc::Sender<Vec<u8>>,
    // Incremented once after Wi-Fi / lwIP init (success or fatal error) so UDP can avoid racing
    // `EspWifi::new` with `std::net::UdpSocket::bind`.
    lwip_socket_gate: Arc<AtomicU8>,
    connected_links: Arc<AtomicU8>,
) -> Result<()> {
    const WIFI_STATION_TASK_STACK_BYTES: usize = 32 * 1024;
    const LINK_MONITOR_POLL_MS: u32 = 1000;
    const RSSI_REPORT_PERIOD_SECS: u64 = 20;

    thread::Builder::new()
        .name("wifi-task".into())
        .stack_size(WIFI_STATION_TASK_STACK_BYTES)
        .spawn(move || {
            crate::system::wdt::subscribe_current_task("wifi-task");
            let disconnect_reason = Arc::new(AtomicI32::new(0));
            let disconnect_reason_for_event = disconnect_reason.clone();
            let uart_tx_for_events = uart_tx_queue_sender.clone();
            let ap_client_ips = Arc::new(Mutex::new(HashMap::<String, String>::new()));
            let ap_client_ips_for_wifi = ap_client_ips.clone();
            let ap_client_ips_for_ip = ap_client_ips.clone();

            let _wifi_subscription = match sysloop.subscribe::<WifiEvent<'static>, _>(move |event| {
                match event {
                    WifiEvent::StaDisconnected(disconnected) => {
                        let reason = disconnected.reason() as i32;
                        disconnect_reason_for_event.store(reason, Ordering::Relaxed);
                        info!(
                            "WiFiStation event: StaDisconnected reason={} ({})",
                            reason,
                            disconnect_reason_text(reason)
                        );
                    }
                    WifiEvent::ApStaDisconnected(disconnected) => {
                        let mac = format_mac(disconnected.mac());
                        if let Ok(mut map) = ap_client_ips_for_wifi.lock() {
                            map.remove(&mac);
                        }
                        send_interface_state(
                            &uart_tx_for_events,
                            interface_state::encode_ap_client_disconnected(
                                InterfaceType::WiFiAccessPoint,
                                mac.clone(),
                            ),
                            "ap_client_disconnected",
                        );
                        info!("WiFi AP client disconnected: mac={mac}");
                    }
                    _ => {}
                }
            }) {
                Ok(sub) => Some(sub),
                Err(err) => {
                    warn!("WiFiStation event subscription failed: {err:#}");
                    None
                }
            };

            let uart_tx_for_ip_events = uart_tx_queue_sender.clone();
            let _ip_subscription = match sysloop.subscribe::<IpEvent<'static>, _>(move |event| {
                if let IpEvent::ApStaIpAssigned(assignment) = event {
                    let mac = format_mac(assignment.mac());
                    let ip = assignment.ip().to_string();
                    if let Ok(mut map) = ap_client_ips_for_ip.lock() {
                        map.insert(mac.clone(), ip.clone());
                    }
                    send_interface_state(
                        &uart_tx_for_ip_events,
                        interface_state::encode_ap_client_connected(
                            InterfaceType::WiFiAccessPoint,
                            mac.clone(),
                            ip.clone(),
                        ),
                        "ap_client_connected",
                    );
                    info!("WiFi AP client connected: mac={mac}, ip={ip}");
                }
            }) {
                Ok(sub) => Some(sub),
                Err(err) => {
                    warn!("WiFi AP IP-event subscription failed: {err:#}");
                    None
                }
            };

            let mut wifi = match EspWifi::new(modem, sysloop.clone(), Some(nvs))
                .and_then(|wifi| BlockingWifi::wrap(wifi, sysloop))
            {
                Ok(wifi) => {
                    info!("WiFi task started");
                    wifi
                }
                Err(err) => {
                    error!("WiFi task failed to initialize Wi-Fi stack: {err:#}");
                    lwip_socket_gate.fetch_add(1, Ordering::SeqCst);
                    crate::system::wdt::unsubscribe_current_task("wifi-task");
                    return;
                }
            };
            lwip_socket_gate.fetch_add(1, Ordering::SeqCst);

            let mut sta_settings = default_sta_settings();
            let mut ap_settings = default_ap_settings();
            let mut link_registered = false;
            let mut ap_started_reported = false;
            let mut ap_disabled_reported = false;
            let mut sta_disconnected_reported = false;
            let mut wifi_mode_active_logged = false;

            loop {
                crate::system::wdt::feed("wifi-task");
                while let Ok(next_sta_settings) = wifi_station_interface_settings_receiver.try_recv() {
                    sta_settings = next_sta_settings;
                    info!(
                        "WiFiStation settings updated: enabled={}, ssid='{}', dhcp={}, reconnectPeriod={}s",
                        sta_settings.enabled,
                        sta_settings.ssid,
                        sta_settings.dhcp,
                        sta_settings.reconnect_period
                    );
                }
                while let Ok(next_ap_settings) = wifi_ap_interface_settings_receiver.try_recv() {
                    ap_settings = next_ap_settings;
                    info!(
                        "WiFiAccessPoint settings updated: enabled={}, ssid='{}', channel={}, maxClients={}",
                        ap_settings.enabled, ap_settings.ssid, ap_settings.channel, ap_settings.max_clients
                    );
                }

                let auth_method = if sta_settings.password.is_empty() {
                    AuthMethod::None
                } else {
                    AuthMethod::WPA2Personal
                };

                let effective_ssid = if sta_settings.enabled {
                    sta_settings.ssid.as_str()
                } else {
                    "disabled"
                };
                let ssid = match effective_ssid.try_into() {
                    Ok(v) => v,
                    Err(_) => {
                        warn!("WiFiStation event skipped: SSID is invalid for ESP-IDF");
                        sta_settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                            Ok(next_sta_settings) => next_sta_settings,
                            Err(()) => return,
                        };
                        continue;
                    }
                };

                let effective_password = if sta_settings.enabled {
                    sta_settings.password.as_str()
                } else {
                    ""
                };
                let password = match effective_password.try_into() {
                    Ok(v) => v,
                    Err(_) => {
                        warn!("WiFiStation event skipped: password is invalid for ESP-IDF");
                        sta_settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                            Ok(next_sta_settings) => next_sta_settings,
                            Err(()) => return,
                        };
                        continue;
                    }
                };

                let sta_client_cfg = ClientConfiguration {
                    ssid,
                    password,
                    auth_method,
                    ..Default::default()
                };
                let ap_cfg = match build_ap_configuration(&ap_settings) {
                    Ok(cfg) => cfg,
                    Err(err) => {
                        warn!(
                            "WiFiAccessPoint config build failed: code={} ({}) err={err:#}",
                            err.code(),
                            ap_error_text(err.code())
                        );
                        send_interface_state(
                            &uart_tx_queue_sender,
                            interface_state::encode_ap_start_error(
                                InterfaceType::WiFiAccessPoint,
                                err.code(),
                            ),
                            "ap_start_error",
                        );
                        sta_settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                            Ok(next_sta_settings) => next_sta_settings,
                            Err(()) => return,
                        };
                        continue;
                    }
                };
                let wifi_mode_cfg = compose_wifi_mode_configuration(
                    &sta_settings,
                    &sta_client_cfg,
                    &ap_settings,
                    &ap_cfg,
                );
                if let Err(err) = wifi.set_configuration(&wifi_mode_cfg) {
                    warn!("WiFiStation set_configuration failed: {err:#}");
                    sta_settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                        Ok(next_sta_settings) => next_sta_settings,
                        Err(()) => return,
                    };
                    continue;
                }
                if sta_settings.enabled {
                    if let Err(err) = apply_sta_ip_settings(&mut wifi, &sta_settings) {
                        warn!("WiFiStation IP settings apply failed: {err:#}");
                        send_interface_state(
                            &uart_tx_queue_sender,
                            interface_state::encode_disconnected(InterfaceType::WiFiStation, err.code()),
                            "disconnected",
                        );
                        sta_settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                            Ok(next_sta_settings) => next_sta_settings,
                            Err(()) => return,
                        };
                        continue;
                    }
                }
                if let Err(err) = apply_ap_ip_settings(&mut wifi, &ap_settings) {
                    warn!(
                        "WiFiAccessPoint IP settings apply failed: code={} ({}) err={err:#}",
                        err.code(),
                        ap_error_text(err.code())
                    );
                    send_interface_state(
                        &uart_tx_queue_sender,
                        interface_state::encode_ap_start_error(
                            InterfaceType::WiFiAccessPoint,
                            err.code(),
                        ),
                        "ap_start_error",
                    );
                    sta_settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                        Ok(next_sta_settings) => next_sta_settings,
                        Err(()) => return,
                    };
                    continue;
                }

                let (started_ok, start_err_code) = match wifi.is_started() {
                    Ok(true) => (true, None),
                    Ok(false) => match wifi.start() {
                        Ok(()) => (true, None),
                        Err(err) => {
                            warn!("WiFiStation start failed: {err:#}");
                            (false, Some(err.code()))
                        }
                    },
                    Err(err) => {
                        warn!("WiFi is_started failed: {err:#}");
                        (false, Some(err.code()))
                    }
                };
                if !started_ok {
                    send_interface_state(
                        &uart_tx_queue_sender,
                        interface_state::encode_disconnected(
                            InterfaceType::WiFiStation,
                            resolve_connect_error_code(
                                start_err_code.unwrap_or(esp_idf_sys::ESP_ERR_INVALID_STATE),
                                &disconnect_reason,
                            ),
                        ),
                        "disconnected",
                    );
                    if ap_settings.enabled {
                        let ap_err = start_err_code.unwrap_or(esp_idf_sys::ESP_ERR_INVALID_STATE);
                        warn!(
                            "WiFiAccessPoint start failed: code={} ({})",
                            ap_err,
                            ap_error_text(ap_err)
                        );
                        send_interface_state(
                            &uart_tx_queue_sender,
                            interface_state::encode_ap_start_error(
                                InterfaceType::WiFiAccessPoint,
                                ap_err,
                            ),
                            "ap_start_error",
                        );
                    }
                } else {
                    if !wifi_mode_active_logged {
                        info!("WiFi AP+STA mode active");
                        wifi_mode_active_logged = true;
                    }
                    if ap_settings.enabled {
                        if !ap_started_reported {
                            send_interface_state(
                                &uart_tx_queue_sender,
                                interface_state::encode_ap_started(InterfaceType::WiFiAccessPoint),
                                "ap_started",
                            );
                            ap_started_reported = true;
                            ap_disabled_reported = false;
                        }
                    }
                    if !ap_settings.enabled {
                        if !ap_disabled_reported {
                            info!("WiFiAccessPoint disabled: code=0 ({})", ap_error_text(0));
                            send_interface_state(
                                &uart_tx_queue_sender,
                                interface_state::encode_ap_start_error(InterfaceType::WiFiAccessPoint, 0),
                                "ap_disabled",
                            );
                            ap_disabled_reported = true;
                            ap_started_reported = false;
                        }
                    }

                    if !sta_settings.enabled {
                        if link_registered {
                            connected_links.fetch_sub(1, Ordering::SeqCst);
                            link_registered = false;
                        }
                        let _ = wifi.disconnect();
                        if !sta_disconnected_reported {
                            send_interface_state(
                                &uart_tx_queue_sender,
                                interface_state::encode_disconnected(
                                    InterfaceType::WiFiStation,
                                    disconnect_reason.swap(0, Ordering::Relaxed),
                                ),
                                "disconnected",
                            );
                            sta_disconnected_reported = true;
                        }
                        process_wifi_scan_requests(&mut wifi, &wifi_scan_receiver, &uart_tx_queue_sender);
                        thread::sleep(Duration::from_millis(250));
                        continue;
                    }
                    wifi_mode_active_logged = false;
                    sta_disconnected_reported = false;

                    crate::system::wdt::unsubscribe_current_task("wifi-task");
                    let connect_result = match wifi.is_connected() {
                        Ok(true) => Ok(()),
                        _ => wifi.connect(),
                    };
                    crate::system::wdt::subscribe_current_task("wifi-task");
                    crate::system::wdt::feed("wifi-task");

                    match connect_result {
                        Ok(()) => {
                            info!("WiFiStation connect requested");
                            let mac = match wifi.wifi().get_mac(WifiDeviceId::Sta) {
                                Ok(mac) => {
                                    let mac = format_mac(mac);
                                    send_interface_state(
                                        &uart_tx_queue_sender,
                                        interface_state::encode_connected(InterfaceType::WiFiStation),
                                        "connected",
                                    );
                                    mac
                                }
                                Err(err) => {
                                    warn!("WiFiStation get_mac failed: {err:#}");
                                    String::from("00:00:00:00:00:00")
                                }
                            };
                            info!("WiFiStation connected with MAC {mac}");

                            match wait_for_netif_up_or_disconnect(
                                &mut wifi,
                                &disconnect_reason,
                                Duration::from_secs(15),
                            ) {
                                NetifWaitOutcome::Up => {
                                    info!("WiFiStation netif is up");
                                    if !link_registered {
                                        connected_links.fetch_add(1, Ordering::SeqCst);
                                        link_registered = true;
                                    }
                                    match wifi.wifi().sta_netif().get_ip_info() {
                                        Ok(ip) => {
                                            let netmask = core::net::Ipv4Addr::from(ip.subnet.mask);
                                            let ip_config = [
                                                ip.ip.to_string(),
                                                netmask.to_string(),
                                                ip.subnet.gateway.to_string(),
                                                get_dns_for_sta(&wifi, esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN),
                                                get_dns_for_sta(
                                                    &wifi,
                                                    esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP,
                                                ),
                                            ];
                                            send_interface_state(
                                                &uart_tx_queue_sender,
                                                interface_state::encode_got_ip(
                                                    InterfaceType::WiFiStation,
                                                    mac.clone(),
                                                    ip_config,
                                                ),
                                                "got_ip",
                                            );
                                        }
                                        Err(err) => warn!("WiFiStation get_ip_info failed: {err:#}"),
                                    }
                                    report_rssi_once(&wifi, &uart_tx_queue_sender);

                                    match monitor_connected_state(
                                        &mut wifi,
                                        &wifi_station_interface_settings_receiver,
                                        &wifi_ap_interface_settings_receiver,
                                        &mut ap_settings,
                                        &wifi_scan_receiver,
                                        &uart_tx_queue_sender,
                                        &disconnect_reason,
                                        RSSI_REPORT_PERIOD_SECS,
                                        LINK_MONITOR_POLL_MS,
                                        &sta_settings,
                                    ) {
                                        Ok(Some(next_sta_settings)) => {
                                            if link_registered {
                                                connected_links.fetch_sub(1, Ordering::SeqCst);
                                                link_registered = false;
                                            }
                                            sta_settings = next_sta_settings;
                                            continue;
                                        }
                                        Ok(None) => {
                                            if link_registered {
                                                connected_links.fetch_sub(1, Ordering::SeqCst);
                                                link_registered = false;
                                            }
                                            // disconnected, retry current STA settings after reconnect period
                                        }
                                        Err(()) => {
                                            if link_registered {
                                                connected_links.fetch_sub(1, Ordering::SeqCst);
                                            }
                                            return;
                                        }
                                    }
                                }
                                NetifWaitOutcome::Disconnected(reason) => {
                                    warn!(
                                        "WiFiStation disconnected before netif-up: reason={} ({})",
                                        reason,
                                        disconnect_reason_text(reason)
                                    );
                                    send_interface_state(
                                        &uart_tx_queue_sender,
                                        interface_state::encode_disconnected(
                                            InterfaceType::WiFiStation,
                                            if reason != 0 {
                                                reason
                                            } else {
                                                WifiDisconnectReason::Timeout as i32
                                            },
                                        ),
                                        "disconnected",
                                    );
                                    send_interface_state(
                                        &uart_tx_queue_sender,
                                        interface_state::encode_disconnected(InterfaceType::WiFiStation, reason),
                                        "disconnected",
                                    );
                                }
                                NetifWaitOutcome::Timeout => {
                                    warn!("WiFiStation netif-up wait timed out");
                                    send_interface_state(
                                        &uart_tx_queue_sender,
                                        interface_state::encode_disconnected(
                                            InterfaceType::WiFiStation,
                                            WifiDisconnectReason::Timeout as i32,
                                        ),
                                        "disconnected",
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            warn!("WiFiStation connect failed: {err:#}");
                            send_interface_state(
                                &uart_tx_queue_sender,
                                interface_state::encode_disconnected(
                                    InterfaceType::WiFiStation,
                                    resolve_connect_error_code(err.code(), &disconnect_reason),
                                ),
                                "disconnected",
                            );
                        }
                    }
                }

                let wait_secs = sta_settings.reconnect_period.clamp(1, 600) as u64;
                info!("WiFiStation retry in {wait_secs}s unless new config arrives");
                let deadline = Instant::now() + Duration::from_secs(wait_secs);
                loop {
                    process_wifi_scan_requests(&mut wifi, &wifi_scan_receiver, &uart_tx_queue_sender);
                    match wifi_station_interface_settings_receiver.try_recv() {
                        Ok(next_sta_settings) => {
                            info!("WiFiStation got updated config before retry");
                            sta_settings = next_sta_settings;
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => {}
                        Err(mpsc::TryRecvError::Disconnected) => {
                            if link_registered {
                                connected_links.fetch_sub(1, Ordering::SeqCst);
                            }
                            info!("WiFiStation task stopped: event channel closed");
                            return;
                        }
                    }
                    if let Ok(next_ap_settings) = wifi_ap_interface_settings_receiver.try_recv() {
                        ap_settings = next_ap_settings;
                        info!("WiFiAccessPoint got updated config before retry");
                        break;
                    }
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(250));
                    crate::system::wdt::feed("wifi-task");
                }
            }
        })
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("failed to spawn WiFiStation task: {err}"))
}

fn monitor_connected_state(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    wifi_station_interface_settings_receiver: &mpsc::Receiver<WiFiStationSettings>,
    wifi_ap_interface_settings_receiver: &mpsc::Receiver<WiFiAccessPointSettings>,
    ap_settings: &mut WiFiAccessPointSettings,
    wifi_scan_receiver: &mpsc::Receiver<WifiScanRequestMessage>,
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    disconnect_reason: &AtomicI32,
    rssi_report_period_secs: u64,
    poll_ms: u32,
    sta_settings: &WiFiStationSettings,
) -> Result<Option<WiFiStationSettings>, ()> {
    let mut last_rssi_report = Instant::now();

    loop {
        crate::system::wdt::feed("wifi-task");
        process_wifi_scan_requests(wifi, wifi_scan_receiver, uart_tx_queue_sender);
        match wifi_ap_interface_settings_receiver.try_recv() {
            Ok(next_ap_settings) => {
                *ap_settings = next_ap_settings;
                info!("WiFiAccessPoint got updated config");
                return Ok(Some(sta_settings.clone()));
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                info!("WiFi task stopped: WiFiAccessPoint event channel closed");
                return Err(());
            }
        }
        match wifi_station_interface_settings_receiver.try_recv() {
            Ok(next_sta_settings) => {
                info!("WiFiStation got updated config");
                return Ok(Some(next_sta_settings));
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                info!("WiFi task stopped: event channel closed");
                return Err(());
            }
        }

        let connected = match wifi.is_connected() {
            Ok(v) => v,
            Err(err) => {
                warn!("WiFiStation is_connected check failed: {err:#}");
                false
            }
        };
        let up = match wifi.is_up() {
            Ok(v) => v,
            Err(err) => {
                warn!("WiFiStation is_up check failed: {err:#}");
                false
            }
        };

        if !connected || !up {
            let reason = disconnect_reason.swap(0, Ordering::Relaxed);
            info!(
                "WiFiStation disconnected (connected={connected}, up={up}), reason={} ({})",
                reason,
                disconnect_reason_text(reason)
            );
            send_interface_state(
                uart_tx_queue_sender,
                interface_state::encode_disconnected(InterfaceType::WiFiStation, reason),
                "disconnected",
            );
            return Ok(None);
        }

        if last_rssi_report.elapsed() >= Duration::from_secs(rssi_report_period_secs) {
            report_rssi_once(wifi, uart_tx_queue_sender);
            last_rssi_report = Instant::now();
        }

        thread::sleep(Duration::from_millis(poll_ms as u64));
    }
}

fn wait_for_netif_up_or_disconnect(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    disconnect_reason: &AtomicI32,
    timeout: Duration,
) -> NetifWaitOutcome {
    let deadline = Instant::now() + timeout;
    loop {
        crate::system::wdt::feed("wifi-task");

        let up = match wifi.is_up() {
            Ok(v) => v,
            Err(err) => {
                warn!("WiFiStation is_up check failed during netif-up wait: {err:#}");
                false
            }
        };
        if up {
            return NetifWaitOutcome::Up;
        }

        let connected = match wifi.is_connected() {
            Ok(v) => v,
            Err(err) => {
                warn!("WiFiStation is_connected check failed during netif-up wait: {err:#}");
                false
            }
        };
        if !connected {
            let reason = disconnect_reason.swap(0, Ordering::Relaxed);
            return NetifWaitOutcome::Disconnected(reason);
        }

        if Instant::now() >= deadline {
            return NetifWaitOutcome::Timeout;
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn get_dns_for_sta(wifi: &BlockingWifi<EspWifi<'_>>, dns_type: u32) -> String {
    let mut dns_info: esp_netif_dns_info_t = Default::default();
    let netif_handle = wifi.wifi().sta_netif().handle();
    if esp_idf_hal::sys::EspError::convert(unsafe {
        esp_idf_sys::esp_netif_get_dns_info(netif_handle, dns_type, &mut dns_info)
    })
    .is_ok()
    {
        let raw = unsafe { u32::from_be(dns_info.ip.u_addr.ip4.addr) };
        if raw != 0 {
            return std::net::Ipv4Addr::from(raw).to_string();
        }
    }
    String::new()
}
