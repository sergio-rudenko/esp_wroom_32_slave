use anyhow::Result;
use esp_idf_hal::gpio::{
    AnyOutputPin, Gpio0, Gpio16, Gpio17, Gpio18, Gpio19, Gpio21, Gpio22, Gpio23, Gpio25, Gpio26,
    Gpio27,
};
use esp_idf_hal::mac::MAC;
use esp_idf_svc::eth::{BlockingEth, EspEth, EthDriver, RmiiClockConfig, RmiiEth, RmiiEthChipset};
use esp_idf_svc::eventloop::EspSystemEventLoop;
use log::*;
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
) -> Result<()> {
    const ETHERNET_TASK_STACK_BYTES: usize = 24 * 1024;
    const LINK_MONITOR_POLL_MS: u32 = 1000;

    thread::Builder::new()
        .name("eth-task".into())
        .stack_size(ETHERNET_TASK_STACK_BYTES)
        .spawn(move || {
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
                    return;
                }
            };
            lwip_socket_gate.fetch_add(1, Ordering::SeqCst);

            let mut cfg = match ethernet_interface_settings_receiver.recv() {
                Ok(cfg) => cfg,
                Err(_) => {
                    info!("Ethernet task stopped: event channel closed before first config");
                    return;
                }
            };

            loop {
                info!("Ethernet settings: enabled={}, dhcp={}", cfg.enabled, cfg.dhcp);

                if !cfg.enabled {
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

                    cfg = match recv_next_or_stop(&ethernet_interface_settings_receiver) {
                        Ok(next) => next,
                        Err(()) => return,
                    };
                    continue;
                }

                if !cfg.dhcp {
                    info!("Ethernet static config requested (not applied yet): {:?}", cfg.static_config);
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
                                        Ok(Some(next_cfg)) => {
                                            cfg = next_cfg;
                                            continue;
                                        }
                                        Ok(None) => {
                                            // disconnected, keep current config and retry quickly
                                        }
                                        Err(()) => return,
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
                    Ok(next_cfg) => {
                        info!("Ethernet got updated config");
                        cfg = next_cfg;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        // keep current config and retry
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        info!("Ethernet task stopped: event channel closed");
                        return;
                    }
                }
            }
        })
        .map(|_| ())
        .map_err(|err| anyhow::anyhow!("failed to spawn Ethernet task: {err}"))
}

pub fn mock_settings() -> EthernetSettings {
    EthernetSettings {
        enabled: true,
        dhcp: true,
        static_config: None,
    }
}

fn recv_next_or_stop(
    rx: &mpsc::Receiver<EthernetSettings>,
) -> Result<EthernetSettings, ()> {
    match rx.recv() {
        Ok(cfg) => Ok(cfg),
        Err(_) => {
            info!("Ethernet task stopped: event channel closed");
            Err(())
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
        match ethernet_interface_settings_receiver.try_recv() {
            Ok(next_cfg) => return Ok(Some(next_cfg)),
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
