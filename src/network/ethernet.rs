use anyhow::Result;
use esp_idf_hal::sys::EspError;
use esp_idf_hal::gpio::{
    AnyOutputPin, Gpio0, Gpio16, Gpio17, Gpio18, Gpio19, Gpio21, Gpio22, Gpio23, Gpio25, Gpio26,
    Gpio27,
};
use esp_idf_hal::mac::MAC;
use esp_idf_svc::handle::RawHandle;
use esp_idf_sys::{
    esp_ip4_addr_t, esp_netif_dhcpc_start, esp_netif_dhcpc_stop, esp_netif_dns_info_t,
    esp_netif_dns_type_t_ESP_NETIF_DNS_BACKUP, esp_netif_dns_type_t_ESP_NETIF_DNS_MAIN,
    esp_netif_ip_info_t, esp_netif_set_dns_info, esp_netif_set_ip_info,
};
use esp_idf_svc::eth::{BlockingEth, EspEth, EthDriver, RmiiClockConfig, RmiiEth, RmiiEthChipset};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use log::*;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::master::messages::interface_settings::{EthernetSettings, InterfaceType};
use crate::master::messages::interface_state;

pub fn spawn_task(
    mac: MAC,
    rmii_txd0: Gpio19,
    rmii_tx_en: Gpio21,
    rmii_txd1: Gpio22,
    rmii_mdc: Gpio23,
    rmii_rxd0: Gpio25,
    rmii_rxd1: Gpio26,
    rmii_crs_dv: Gpio27,
    rmii_mdio: Gpio18,
    rmii_clk_out_gpio17: Gpio17,
    sysloop: EspSystemEventLoop,
    ethernet_interface_settings_receiver: mpsc::Receiver<EthernetSettings>,
    uart_tx_queue_sender: mpsc::Sender<Vec<u8>>,
    // Incremented once after Ethernet / lwIP init (success or fatal error).
    lwip_socket_gate: Arc<AtomicU8>,
    connected_links: Arc<AtomicU8>,
) -> Result<()> {
    const ETHERNET_TASK_STACK_BYTES: usize = 24 * 1024;
    const LINK_MONITOR_POLL_MS: u32 = 1000;

    thread::Builder::new()
        .name("eth-task".into())
        .stack_size(ETHERNET_TASK_STACK_BYTES)
        .spawn(move || {
            crate::system::wdt::subscribe_current_task("eth-task");
            let mut eth = match EthDriver::new(
                mac,
                rmii_rxd0,
                rmii_rxd1,
                rmii_crs_dv,
                rmii_mdc,
                rmii_txd1,
                rmii_tx_en,
                rmii_txd0,
                rmii_mdio,
                RmiiClockConfig::<Gpio0, Gpio16, Gpio17>::OutputInvertedGpio17(
                    rmii_clk_out_gpio17,
                ),
                Option::<AnyOutputPin>::None,
                RmiiEthChipset::LAN87XX,
                None,
                sysloop.clone(),
            )
            .and_then(EspEth::wrap)
            .and_then(|eth| BlockingEth::wrap(eth, sysloop))
            {
                Ok(eth) => {
                    info!("Ethernet task started");
                    eth
                }
                Err(err) => {
                    error!("Ethernet task failed to initialize stack: {err:#}");
                    lwip_socket_gate.fetch_add(1, Ordering::SeqCst);
                    crate::system::wdt::unsubscribe_current_task("eth-task");
                    return;
                }
            };
            lwip_socket_gate.fetch_add(1, Ordering::SeqCst);

            let mut settings = loop {
                match ethernet_interface_settings_receiver.recv_timeout(Duration::from_secs(1)) {
                    Ok(settings) => break settings,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        crate::system::wdt::feed("eth-task");
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        info!("Ethernet task stopped: event channel closed before first config");
                        return;
                    }
                }
            };

            let mut link_registered = false;

            loop {
                crate::system::wdt::feed("eth-task");
                info!("Ethernet settings: enabled={}, dhcp={}", settings.enabled, settings.dhcp);

                if !settings.enabled {
                    if link_registered {
                        connected_links.fetch_sub(1, Ordering::SeqCst);
                        link_registered = false;
                    }
                    if let Err(err) = eth.stop() {
                        warn!("Ethernet stop failed: {err:#}");
                    } else {
                        info!("Ethernet disabled");
                        send_interface_state(
                            &uart_tx_queue_sender,
                            interface_state::encode_disconnected(InterfaceType::Ethernet, 0),
                            "ethernet_disconnected",
                        );
                    }

                    settings = match recv_next_or_stop(&ethernet_interface_settings_receiver) {
                        Ok(next_settings) => next_settings,
                        Err(()) => return,
                    };
                    continue;
                }

                if let Err(err) = apply_eth_ip_settings(&mut eth, &settings) {
                    warn!("Ethernet IP settings apply failed: {err:#}");
                    send_interface_state(
                        &uart_tx_queue_sender,
                        interface_state::encode_connect_error(InterfaceType::Ethernet, err.code()),
                        "ethernet_connect_error",
                    );
                    settings = match recv_next_or_stop(&ethernet_interface_settings_receiver) {
                        Ok(next_settings) => next_settings,
                        Err(()) => return,
                    };
                    continue;
                }

                if let Err(err) = eth.start() {
                    warn!("Ethernet start failed: {err:#}");
                    send_interface_state(
                        &uart_tx_queue_sender,
                        interface_state::encode_connect_error(InterfaceType::Ethernet, err.code()),
                        "ethernet_connect_error",
                    );
                } else {
                    info!("Ethernet started");
                    match eth.wait_connected() {
                        Ok(()) => {
                            info!("Ethernet link connected");
                            let mac = match eth.eth().netif().get_mac() {
                                Ok(mac) => format_mac(mac),
                                Err(err) => {
                                    warn!("Ethernet get_mac failed: {err:#}");
                                    String::from("00:00:00:00:00:00")
                                }
                            };
                            send_interface_state(
                                &uart_tx_queue_sender,
                                interface_state::encode_connected(InterfaceType::Ethernet, mac),
                                "ethernet_connected",
                            );

                            match eth.wait_netif_up() {
                                Ok(()) => {
                                    info!("Ethernet netif is up");
                                    if !link_registered {
                                        connected_links.fetch_add(1, Ordering::SeqCst);
                                        link_registered = true;
                                    }
                                    match eth.eth().netif().get_ip_info() {
                                        Ok(ip) => {
                                            let netmask =
                                                core::net::Ipv4Addr::from(ip.subnet.mask).to_string();
                                            let ip_config = [
                                                ip.ip.to_string(),
                                                netmask,
                                                ip.subnet.gateway.to_string(),
                                            ];
                                            send_interface_state(
                                                &uart_tx_queue_sender,
                                                interface_state::encode_got_ip(
                                                    InterfaceType::Ethernet,
                                                    ip_config,
                                                ),
                                                "ethernet_got_ip",
                                            );
                                        }
                                        Err(err) => warn!("Ethernet get_ip_info failed: {err:#}"),
                                    }

                                    match monitor_link_state(
                                        &eth,
                                        &ethernet_interface_settings_receiver,
                                        &uart_tx_queue_sender,
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
                                            // disconnected, keep current config and retry quickly
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
                                    warn!("Ethernet netif-up wait failed: {err:#}");
                                    send_interface_state(
                                        &uart_tx_queue_sender,
                                        interface_state::encode_connect_error(
                                            InterfaceType::Ethernet,
                                            err.code(),
                                        ),
                                        "ethernet_connect_error",
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            warn!("Ethernet wait_connected failed: {err:#}");
                            send_interface_state(
                                &uart_tx_queue_sender,
                                interface_state::encode_connect_error(
                                    InterfaceType::Ethernet,
                                    err.code(),
                                ),
                                "ethernet_connect_error",
                            );
                        }
                    }
                }

                match ethernet_interface_settings_receiver.recv_timeout(Duration::from_secs(5)) {
                    Ok(next_settings) => {
                        info!("Ethernet got updated config");
                        settings = next_settings;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // keep current config and retry
                        crate::system::wdt::feed("eth-task");
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        if link_registered {
                            connected_links.fetch_sub(1, Ordering::SeqCst);
                        }
                        info!("Ethernet task stopped: event channel closed");
                        return;
                    }
                }
            }
        })
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("failed to spawn Ethernet task: {err}"))
}

fn recv_next_or_stop(
    rx: &mpsc::Receiver<EthernetSettings>,
) -> Result<EthernetSettings, ()> {
    loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
            Ok(next_settings) => return Ok(next_settings),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                crate::system::wdt::feed("eth-task");
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                info!("Ethernet task stopped: event channel closed");
                return Err(());
            }
        }
    }
}

fn monitor_link_state(
    eth: &BlockingEth<EspEth<'_, RmiiEth>>,
    ethernet_interface_settings_receiver: &mpsc::Receiver<EthernetSettings>,
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    poll_ms: u32,
) -> Result<Option<EthernetSettings>, ()> {
    let mut last_report = Instant::now();
    loop {
        crate::system::wdt::feed("eth-task");
        match ethernet_interface_settings_receiver.try_recv() {
            Ok(next_settings) => return Ok(Some(next_settings)),
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => return Err(()),
        }

        let connected = eth.is_connected().unwrap_or(false);
        let up = eth.is_up().unwrap_or(false);
        if !connected || !up {
            info!("Ethernet disconnected (connected={connected}, up={up})");
            send_interface_state(
                uart_tx_queue_sender,
                interface_state::encode_disconnected(InterfaceType::Ethernet, 0),
                "ethernet_disconnected",
            );
            return Ok(None);
        }

        // Keep-alive heartbeat-like RSSI equivalent is not available for Ethernet.
        if last_report.elapsed() >= Duration::from_secs(30) {
            info!("Ethernet link still up");
            last_report = Instant::now();
        }

        thread::sleep(Duration::from_millis(poll_ms as u64));
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

#[derive(Clone, Debug)]
struct StaticClientSettings {
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
    gateway: Ipv4Addr,
    dns: Option<Ipv4Addr>,
    secondary_dns: Option<Ipv4Addr>,
}

fn apply_eth_ip_settings(
    eth: &mut BlockingEth<EspEth<'_, RmiiEth>>,
    settings: &EthernetSettings,
) -> Result<(), EspError> {
    let netif_handle = eth.eth().netif().handle();

    if settings.dhcp {
        let _ = EspError::convert(unsafe { esp_netif_dhcpc_start(netif_handle) });
        info!("Ethernet IP mode: DHCP");
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
        "Ethernet IP mode: static ip={}, mask={}, gw={}, dns={:?}, dns2={:?}",
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
