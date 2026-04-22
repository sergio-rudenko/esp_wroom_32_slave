use anyhow::Result;
use embedded_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};
use esp_idf_hal::sys::EspError;
use esp_idf_svc::handle::RawHandle;
use esp_idf_hal::modem::Modem;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi, WifiDeviceId, WifiEvent};
use esp_idf_sys::{
    esp_ip4_addr_t, esp_netif_dhcpc_start, esp_netif_dhcpc_stop, esp_netif_dns_info_t,
    esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP, esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN,
    esp_netif_ip_info_t, esp_netif_set_dns_info, esp_netif_set_ip_info,
};
use log::*;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::master::messages::interface_settings::InterfaceType;
use crate::master::messages::interface_settings::WiFiStationSettings;
use crate::master::messages::interface_state;
use crate::master::messages::wifi_scan::{self, WifiScanRequestMessage, WifiScanResultItem};

pub fn spawn_task(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs: EspDefaultNvsPartition,
    wifi_station_interface_settings_receiver: mpsc::Receiver<WiFiStationSettings>,
    wifi_scan_receiver: mpsc::Receiver<WifiScanRequestMessage>,
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

        let mut settings = loop {
            match wifi_station_interface_settings_receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(settings) => break settings,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    process_wifi_scan_requests(&mut wifi, &wifi_scan_receiver, &uart_tx_queue_sender);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    info!("WiFiStation task stopped: event channel closed before first config");
                    return;
                }
            }
        };

        let mut link_registered = false;

        loop {
            info!(
                "WiFiStation settings: enabled={}, ssid='{}', dhcp={}, reconnectPeriod={}s",
                settings.enabled, settings.ssid, settings.dhcp, settings.reconnect_period
            );

            if !settings.enabled {
                process_wifi_scan_requests(&mut wifi, &wifi_scan_receiver, &uart_tx_queue_sender);
                if link_registered {
                    connected_links.fetch_sub(1, Ordering::SeqCst);
                    link_registered = false;
                }
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

                settings = loop {
                    match wifi_station_interface_settings_receiver.recv_timeout(Duration::from_millis(250)) {
                        Ok(next_settings) => break next_settings,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            process_wifi_scan_requests(
                                &mut wifi,
                                &wifi_scan_receiver,
                                &uart_tx_queue_sender,
                            );
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            info!("WiFiStation task stopped: event channel closed");
                            return;
                        }
                    }
                };
                continue;
            }

            let auth_method = if settings.password.is_empty() {
                AuthMethod::None
            } else {
                AuthMethod::WPA2Personal
            };

            let ssid = match settings.ssid.as_str().try_into() {
                Ok(v) => v,
                Err(_) => {
                    warn!("WiFiStation event skipped: SSID is invalid for ESP-IDF");
                    settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                        Ok(next_settings) => next_settings,
                        Err(()) => return,
                    };
                    continue;
                }
            };

            let password = match settings.password.as_str().try_into() {
                Ok(v) => v,
                Err(_) => {
                    warn!("WiFiStation event skipped: password is invalid for ESP-IDF");
                    settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                        Ok(next_settings) => next_settings,
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
                settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                    Ok(next_settings) => next_settings,
                    Err(()) => return,
                };
                continue;
            }
            if let Err(err) = apply_sta_ip_settings(&mut wifi, &settings) {
                warn!("WiFiStation IP settings apply failed: {err:#}");
                send_interface_state(
                    &uart_tx_queue_sender,
                    interface_state::encode_connect_error(InterfaceType::WiFiStation, err.code()),
                    "connect_error",
                );
                settings = match recv_next_or_stop(&wifi_station_interface_settings_receiver) {
                    Ok(next_settings) => next_settings,
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
                                    &wifi_scan_receiver,
                                    &uart_tx_queue_sender,
                                    &disconnect_reason,
                                    RSSI_REPORT_PERIOD_SECS,
                                    LINK_MONITOR_POLL_MS,
                                ) {
                                    Ok(Some(next_settings)) => {
                                        if link_registered {
                                            connected_links.fetch_sub(1, Ordering::SeqCst);
                                            link_registered = false;
                                        }
                                        settings = next_settings;
                                        continue;
                                    }
                                    Ok(None) => {
                                        if link_registered {
                                            connected_links.fetch_sub(1, Ordering::SeqCst);
                                            link_registered = false;
                                        }
                                        // disconnected, retry current settings after reconnect period
                                    }
                                    Err(()) => {
                                        if link_registered {
                                            connected_links.fetch_sub(1, Ordering::SeqCst);
                                        }
                                        return;
                                    }
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

            let wait_secs = settings.reconnect_period.clamp(1, 600) as u64;
            info!("WiFiStation retry in {wait_secs}s unless new config arrives");

            match wifi_station_interface_settings_receiver.recv_timeout(Duration::from_secs(wait_secs)) {
                Ok(next_settings) => {
                    info!("WiFiStation got updated config before retry");
                    settings = next_settings;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    process_wifi_scan_requests(&mut wifi, &wifi_scan_receiver, &uart_tx_queue_sender);
                    // Keep current settings and retry connect.
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if link_registered {
                        connected_links.fetch_sub(1, Ordering::SeqCst);
                    }
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
        Ok(next_settings) => Ok(next_settings),
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

fn process_wifi_scan_requests(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    wifi_scan_receiver: &mpsc::Receiver<WifiScanRequestMessage>,
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
) {
    while let Ok(request) = wifi_scan_receiver.try_recv() {
        info!("WiFiStation scan requested: limit={}", request.limit);
        let mut entries: Vec<WifiScanResultItem> = match wifi.scan() {
            Ok(access_points) => access_points
                .into_iter()
                .map(|ap| WifiScanResultItem {
                    ssid: ap.ssid.as_str().to_string(),
                    bssid: format_mac(ap.bssid),
                    channel: ap.channel,
                    rssi: ap.signal_strength,
                    auth_method: auth_method_list(ap.auth_method),
                })
                .collect(),
            Err(err) => {
                warn!("WiFiStation scan failed: {err:#}");
                Vec::new()
            }
        };

        if request.limit > 0 && entries.len() > request.limit as usize {
            entries.truncate(request.limit as usize);
        }

        if let Err(err) = send_wifi_scan_chunks(uart_tx_queue_sender, &entries) {
            warn!("WiFiStation scan response encode/send failed: {err:#}");
        } else {
            info!("WiFiStation scan response sent: {} AP entries", entries.len());
        }
    }
}

fn send_wifi_scan_chunks(
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    entries: &[WifiScanResultItem],
) -> Result<()> {
    if entries.is_empty() {
        // Empty scan result must be encoded as MsgPack empty JSON array: []
        let empty_items: Vec<WifiScanResultItem> = Vec::new();
        let frame = wifi_scan::encode(0, &empty_items)?;
        uart_tx_queue_sender
            .send(frame)
            .map_err(|e| anyhow::anyhow!("enqueue WifiScan chunk 0 failed: {e}"))?;
        return Ok(());
    }

    let mut start = 0usize;
    let mut chunk_index: u8 = 0;
    while start < entries.len() {
        let mut end = start + 1;
        let mut best_end = start;

        while end <= entries.len() {
            if wifi_scan::encode(chunk_index, &entries[start..end]).is_ok() {
                best_end = end;
                end += 1;
            } else {
                break;
            }
        }

        if best_end == start {
            warn!("WiFiStation scan: single AP item exceeds MAX_PAYLOAD_SIZE, skipping");
            start += 1;
            continue;
        }

        let frame = wifi_scan::encode(chunk_index, &entries[start..best_end])?;
        uart_tx_queue_sender
            .send(frame)
            .map_err(|e| anyhow::anyhow!("enqueue WifiScan chunk {chunk_index} failed: {e}"))?;

        start = best_end;
        if chunk_index == u8::MAX && start < entries.len() {
            anyhow::bail!("WifiScan response requires more than 256 chunks");
        }
        chunk_index = chunk_index.saturating_add(1);
    }

    Ok(())
}

fn auth_method_list(auth_method: Option<AuthMethod>) -> Vec<String> {
    match auth_method {
        Some(v) => vec![format!("{v:?}")],
        None => vec![String::from("None")],
    }
}

fn monitor_connected_state(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    wifi_station_interface_settings_receiver: &mpsc::Receiver<WiFiStationSettings>,
    wifi_scan_receiver: &mpsc::Receiver<WifiScanRequestMessage>,
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    disconnect_reason: &AtomicI32,
    rssi_report_period_secs: u64,
    poll_ms: u32,
) -> Result<Option<WiFiStationSettings>, ()> {
    let mut last_rssi_report = Instant::now();

    loop {
        process_wifi_scan_requests(wifi, wifi_scan_receiver, uart_tx_queue_sender);
        match wifi_station_interface_settings_receiver.try_recv() {
            Ok(next_settings) => {
                info!("WiFiStation got updated config");
                return Ok(Some(next_settings));
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

#[derive(Clone, Debug)]
struct StaticClientSettings {
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
    gateway: Ipv4Addr,
    dns: Option<Ipv4Addr>,
    secondary_dns: Option<Ipv4Addr>,
}

fn apply_sta_ip_settings(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    settings: &WiFiStationSettings,
) -> Result<(), EspError> {
    let netif = wifi.wifi().sta_netif();
    let netif_handle = netif.handle();

    if settings.dhcp {
        let _ = EspError::convert(unsafe { esp_netif_dhcpc_start(netif_handle) });
        info!("WiFiStation IP mode: DHCP");
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
    info!(
        "WiFiStation IP mode: static ip={}, mask={}, gw={}, dns={:?}, dns2={:?}",
        static_settings.ip,
        static_settings.netmask,
        static_settings.gateway,
        static_settings.dns,
        static_settings.secondary_dns
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

fn set_dns_info(netif_handle: *mut esp_idf_sys::esp_netif_t, dns: Option<Ipv4Addr>, dns_type: u32) -> Result<(), EspError> {
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
