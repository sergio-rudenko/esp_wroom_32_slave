use anyhow::Result;
use embedded_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};
use esp_idf_hal::modem::Modem;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi, WifiDeviceId, WifiEvent};
use log::*;
use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::master::messages::interface_settings::InterfaceType;
use crate::master::messages::interface_settings::WiFiStationSettings;
use crate::master::messages::interface_state;

pub fn spawn_task(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    wifi_station_interface_settings_receiver: mpsc::Receiver<WiFiStationSettings>,
    uart_tx_queue_sender: mpsc::Sender<Vec<u8>>,
    // Incremented once after Wi-Fi / lwIP init (success or fatal error) so UDP can avoid racing
    // `EspWifi::new` with `std::net::UdpSocket::bind`.
    lwip_socket_gate: Arc<AtomicU8>,
) -> Result<()> {
    const WIFI_STATION_TASK_STACK_BYTES: usize = 32 * 1024;
    const LINK_MONITOR_POLL_MS: u32 = 1000;
    const RSSI_REPORT_PERIOD_SECS: u64 = 20;

    thread::Builder::new()
        .name("wifi-sta-task".into())
        .stack_size(WIFI_STATION_TASK_STACK_BYTES)
        .spawn(move || {
        let disconnect_reason = Arc::new(AtomicI32::new(0));
        let disconnect_reason_for_event = disconnect_reason.clone();

        let _wifi_subscription = match sysloop.subscribe::<WifiEvent<'static>, _>(move |event| {
            if let WifiEvent::StaDisconnected(disconnected) = event {
                let reason = disconnected.reason() as i32;
                disconnect_reason_for_event.store(reason, Ordering::Relaxed);
                info!(
                    "WiFiStation event: StaDisconnected reason={} ({})",
                    reason,
                    disconnect_reason_text(reason)
                );
            }
        }) {
            Ok(sub) => Some(sub),
            Err(err) => {
                warn!("WiFiStation event subscription failed: {err:#}");
                None
            }
        };

        let mut wifi = match EspWifi::new(modem, sysloop.clone(), Some(nvs))
            .and_then(|wifi| BlockingWifi::wrap(wifi, sysloop))
        {
            Ok(wifi) => {
                info!("WiFiStation task started");
                wifi
            }
            Err(err) => {
                error!("WiFiStation task failed to initialize Wi-Fi stack: {err:#}");
                lwip_socket_gate.fetch_add(1, Ordering::SeqCst);
                return;
            }
        };
        lwip_socket_gate.fetch_add(1, Ordering::SeqCst);

        let mut cfg = match wifi_station_interface_settings_receiver.recv() {
            Ok(cfg) => cfg,
            Err(_) => {
                info!("WiFiStation task stopped: event channel closed before first config");
                return;
            }
        };

        loop {
            info!(
                "WiFiStation settings: enabled={}, ssid='{}', dhcp={}, reconnectPeriod={}s",
                cfg.enabled, cfg.ssid, cfg.dhcp, cfg.reconnect_period
            );

            if !cfg.enabled {
                if let Err(err) = wifi.stop() {
                    warn!("WiFiStation stop failed: {err:#}");
                } else {
                    info!("WiFiStation disabled");
                    send_interface_state(
                        &uart_tx_queue_sender,
                        interface_state::encode_disconnected(
                            InterfaceType::WiFiStation,
                            disconnect_reason.swap(0, Ordering::Relaxed),
                        ),
                        "disconnected",
                    );
                }

                cfg = match wifi_station_interface_settings_receiver.recv() {
                    Ok(next) => next,
                    Err(_) => {
                        info!("WiFiStation task stopped: event channel closed");
                        return;
                    }
                };
                continue;
            }

            let auth_method = if cfg.password.is_empty() {
                AuthMethod::None
            } else {
                AuthMethod::WPA2Personal
            };

            let ssid = match cfg.ssid.as_str().try_into() {
                Ok(v) => v,
                Err(_) => {
                    warn!("WiFiStation event skipped: SSID is invalid for ESP-IDF");
                    cfg = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                        Ok(next) => next,
                        Err(()) => return,
                    };
                    continue;
                }
            };

            let password = match cfg.password.as_str().try_into() {
                Ok(v) => v,
                Err(_) => {
                    warn!("WiFiStation event skipped: password is invalid for ESP-IDF");
                    cfg = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                        Ok(next) => next,
                        Err(()) => return,
                    };
                    continue;
                }
            };

            let client_cfg = ClientConfiguration {
                ssid,
                password,
                auth_method,
                ..Default::default()
            };

            if let Err(err) = wifi.set_configuration(&Configuration::Client(client_cfg)) {
                warn!("WiFiStation set_configuration failed: {err:#}");
                cfg = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                    Ok(next) => next,
                    Err(()) => return,
                };
                continue;
            }

            if let Err(err) = wifi.start() {
                warn!("WiFiStation start failed: {err:#}");
                send_interface_state(
                    &uart_tx_queue_sender,
                    interface_state::encode_connect_error(
                        InterfaceType::WiFiStation,
                        resolve_connect_error_code(err.code(), &disconnect_reason),
                    ),
                    "connect_error",
                );
            } else {
                info!("WiFiStation started");
                match wifi.connect() {
                    Ok(()) => {
                        info!("WiFiStation connect requested");
                        let mac = match wifi.wifi().get_mac(WifiDeviceId::Sta) {
                            Ok(mac) => {
                                let mac = format_mac(mac);
                                send_interface_state(
                                    &uart_tx_queue_sender,
                                    interface_state::encode_connected(
                                        InterfaceType::WiFiStation,
                                        mac.clone(),
                                    ),
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

                        match wifi.wait_netif_up() {
                            Ok(()) => {
                                info!("WiFiStation netif is up");
                                match wifi.wifi().sta_netif().get_ip_info() {
                                    Ok(ip) => {
                                        let netmask = core::net::Ipv4Addr::from(ip.subnet.mask);
                                        let ip_config = [
                                            ip.ip.to_string(),
                                            netmask.to_string(),
                                            ip.subnet.gateway.to_string(),
                                        ];
                                        send_interface_state(
                                            &uart_tx_queue_sender,
                                            interface_state::encode_got_ip(
                                                InterfaceType::WiFiStation,
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
                                    &uart_tx_queue_sender,
                                    &disconnect_reason,
                                    RSSI_REPORT_PERIOD_SECS,
                                    LINK_MONITOR_POLL_MS,
                                ) {
                                    Ok(Some(next_cfg)) => {
                                        cfg = next_cfg;
                                        continue;
                                    }
                                    Ok(None) => {
                                        // disconnected, retry current cfg after reconnect period
                                    }
                                    Err(()) => return,
                                }
                            }
                            Err(err) => {
                                warn!("WiFiStation netif-up wait failed: {err:#}");
                                send_interface_state(
                                    &uart_tx_queue_sender,
                                    interface_state::encode_connect_error(
                                        InterfaceType::WiFiStation,
                                        resolve_connect_error_code(err.code(), &disconnect_reason),
                                    ),
                                    "connect_error",
                                );
                            }
                        }
                    }
                    Err(err) => {
                        warn!("WiFiStation connect failed: {err:#}");
                        send_interface_state(
                            &uart_tx_queue_sender,
                            interface_state::encode_connect_error(
                                InterfaceType::WiFiStation,
                                resolve_connect_error_code(err.code(), &disconnect_reason),
                            ),
                            "connect_error",
                        );
                    }
                }
            }

            let wait_secs = cfg.reconnect_period.clamp(1, 600) as u64;
            info!("WiFiStation retry in {wait_secs}s unless new config arrives");

            match wifi_station_interface_settings_receiver.recv_timeout(Duration::from_secs(wait_secs)) {
                Ok(next_cfg) => {
                    info!("WiFiStation got updated config before retry");
                    cfg = next_cfg;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Keep current config and retry connect.
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    info!("WiFiStation task stopped: event channel closed");
                    return;
                }
            }
        }
        })
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("failed to spawn WiFiStation task: {err}"))
}

fn recv_next_or_stop(
    rx: &mpsc::Receiver<WiFiStationSettings>,
) -> Result<WiFiStationSettings, ()> {
    match rx.recv() {
        Ok(cfg) => Ok(cfg),
        Err(_) => {
            info!("WiFiStation task stopped: event channel closed");
            Err(())
        }
    }
}

fn send_interface_state(
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    frame_result: Result<Vec<u8>>,
    event_name: &str,
) {
    match frame_result {
        Ok(frame) => {
            if let Err(err) = uart_tx_queue_sender.send(frame) {
                warn!("InterfaceState {event_name} enqueue failed: {err}");
            } else {
                info!("Enqueued InterfaceState event: {event_name}");
            }
        }
        Err(err) => warn!("InterfaceState {event_name} encode failed: {err:#}"),
    }
}

fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn resolve_connect_error_code(esp_error_code: i32, disconnect_reason: &AtomicI32) -> i32 {
    let reason = disconnect_reason.swap(0, Ordering::Relaxed);
    if reason != 0 {
        info!(
            "WiFiStation connect_error mapped from disconnect reason={} ({})",
            reason,
            disconnect_reason_text(reason)
        );
        reason
    } else {
        esp_error_code
    }
}

fn report_rssi_once(wifi: &BlockingWifi<EspWifi<'_>>, uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>) {
    match wifi.wifi().get_rssi() {
        Ok(rssi) => {
            send_interface_state(
                uart_tx_queue_sender,
                interface_state::encode_rssi(InterfaceType::WiFiStation, rssi),
                "rssi",
            );
        }
        Err(err) => warn!("WiFiStation get_rssi failed: {err:#}"),
    }
}

fn monitor_connected_state(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    wifi_station_interface_settings_receiver: &mpsc::Receiver<WiFiStationSettings>,
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    disconnect_reason: &AtomicI32,
    rssi_report_period_secs: u64,
    poll_ms: u32,
) -> Result<Option<WiFiStationSettings>, ()> {
    let mut last_rssi_report = Instant::now();

    loop {
        match wifi_station_interface_settings_receiver.try_recv() {
            Ok(next_cfg) => {
                info!("WiFiStation got updated config");
                return Ok(Some(next_cfg));
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                info!("WiFiStation task stopped: event channel closed");
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

fn disconnect_reason_text(reason: i32) -> &'static str {
    match reason {
        0 => "UNKNOWN_OR_NOT_SET",
        1 => "UNSPECIFIED",
        2 => "AUTH_EXPIRE",
        3 => "AUTH_LEAVE",
        4 => "ASSOC_EXPIRE",
        5 => "ASSOC_TOOMANY",
        6 => "NOT_AUTHED",
        7 => "NOT_ASSOCED",
        8 => "ASSOC_LEAVE",
        14 => "MIC_FAILURE",
        15 => "4WAY_HANDSHAKE_TIMEOUT",
        16 => "GROUP_KEY_UPDATE_TIMEOUT",
        23 => "802_1X_AUTH_FAILED",
        39 => "TIMEOUT",
        46 => "PEER_INITIATED",
        47 => "AP_INITIATED",
        200 => "BEACON_TIMEOUT",
        201 => "NO_AP_FOUND",
        202 => "AUTH_FAIL",
        203 => "ASSOC_FAIL",
        204 => "HANDSHAKE_TIMEOUT",
        205 => "CONNECTION_FAIL",
        206 => "AP_TSF_RESET",
        207 => "ROAMING",
        210 => "NO_AP_FOUND_COMPATIBLE_SECURITY",
        211 => "NO_AP_FOUND_AUTHMODE_THRESHOLD",
        212 => "NO_AP_FOUND_RSSI_THRESHOLD",
        _ => "OTHER",
    }
}
