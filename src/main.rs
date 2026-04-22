use anyhow::Result;
use esp_idf_hal::delay::FreeRtos;
use esp_idf_sys::TickType_t;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::*;
use std::sync::atomic::AtomicU8;
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

mod master;
mod network;
mod services;

fn main() -> Result<()> {
    const UART_TX_TASK_STACK_BYTES: usize = 8 * 1024;
    const UART_RX_TASK_STACK_BYTES: usize = 8 * 1024;
    /// WiFi station + Ethernet tasks each bump `lwip_socket_gate` once after lwIP-related init.
    const LWIP_STACK_DRIVER_TASKS: u8 = 2;

    esp_idf_sys::link_patches();
    EspLogger::initialize_default();

    info!("Booting ESP32 UART slave controller...");

    let peripherals = esp_idf_hal::peripherals::Peripherals::take()?;

    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let modem = peripherals.modem;
    let mac = peripherals.mac;

    let uart1 = master::init_uart1_link(peripherals.uart1)?;
    let (mut uart1_tx, uart1_rx) = uart1.into_split();

    let (uart_tx_queue_sender, uart_tx_queue_receiver) = mpsc::channel::<Vec<u8>>();
    let (uart_rx_queue_sender, uart_rx_queue_receiver) = mpsc::channel::<Vec<u8>>();
    let (wifi_station_interface_settings_sender, wifi_station_interface_settings_receiver) =
        mpsc::channel::<master::messages::interface_settings::WiFiStationSettings>();
    let (wifi_ap_interface_settings_sender, wifi_ap_interface_settings_receiver) =
        mpsc::channel::<master::messages::interface_settings::WiFiAccessPointSettings>();
    let (ethernet_interface_settings_sender, ethernet_interface_settings_receiver) =
        mpsc::channel::<master::messages::interface_settings::EthernetSettings>();
    let (udp_listener_settings_sender, udp_listener_settings_receiver) =
        mpsc::channel::<master::messages::service_settings::UdpListenerSettings>();
    let (tcp_server_settings_sender, tcp_server_settings_receiver) =
        mpsc::channel::<master::messages::service_settings::TcpServerSettings>();
    let (ntp_client_settings_sender, ntp_client_settings_receiver) =
        mpsc::channel::<master::messages::service_settings::NtpClientSettings>();
    let (tcp_server_command_sender, tcp_server_command_receiver) =
        mpsc::channel::<master::messages::tcp_command::TcpCommandMessage>();
    let (tcp_server_data_sender, tcp_server_data_receiver) =
        mpsc::channel::<master::messages::tcp_data::TcpDataMessage>();
    let (tcp_server_network_reset_sender, tcp_server_network_reset_receiver) = mpsc::channel::<()>();
    let (wifi_scan_sender, wifi_scan_receiver) =
        mpsc::channel::<master::messages::wifi_scan::WifiScanRequestMessage>();
    let lwip_socket_gate = Arc::new(AtomicU8::new(0));
    let connected_links = Arc::new(AtomicU8::new(0));

    thread::Builder::new()
        .name("uart-tx-task".into())
        .stack_size(UART_TX_TASK_STACK_BYTES)
        .spawn(move || {
            while let Ok(frame) = uart_tx_queue_receiver.recv() {
                let _ = uart1_tx.write(&frame);
            }
        })
        .map_err(|err| anyhow::anyhow!("failed to spawn UART TX task: {err}"))?;

    thread::Builder::new()
        .name("uart-rx-task".into())
        .stack_size(UART_RX_TASK_STACK_BYTES)
        .spawn(move || {
            let mut stream_buf = Vec::with_capacity(1024);
            let mut chunk = [0_u8; 256];

            loop {
                if let Ok(read) = uart1_rx.read(&mut chunk, TickType_t::default()) {
                    if read > 0 {
                        stream_buf.extend_from_slice(&chunk[..read]);
                        while let Some(frame) = master::pop_next_valid_frame(&mut stream_buf) {
                            let _ = uart_rx_queue_sender.send(frame);
                        }
                    }
                }
                FreeRtos::delay_ms(10);
            }
        })
        .map_err(|err| anyhow::anyhow!("failed to spawn UART RX task: {err}"))?;

    uart_tx_queue_sender.send(master::encode_ready()?)?;
    info!("UART1 ready for STM32 controller");

    network::wifi::spawn_task(
        modem,
        sysloop.clone(),
        nvs,
        wifi_station_interface_settings_receiver,
        wifi_ap_interface_settings_receiver,
        wifi_scan_receiver,
        tcp_server_network_reset_sender,
        uart_tx_queue_sender.clone(),
        lwip_socket_gate.clone(),
        connected_links.clone(),
    )?;

    network::ethernet::spawn_task(
        mac,
        peripherals.pins.gpio19,
        peripherals.pins.gpio21,
        peripherals.pins.gpio22,
        peripherals.pins.gpio23,
        peripherals.pins.gpio25,
        peripherals.pins.gpio26,
        peripherals.pins.gpio27,
        peripherals.pins.gpio18,
        peripherals.pins.gpio17,
        sysloop,
        ethernet_interface_settings_receiver,
        uart_tx_queue_sender.clone(),
        lwip_socket_gate.clone(),
        connected_links.clone(),
    )?;

    services::udp_listener::spawn_task(
        udp_listener_settings_receiver,
        lwip_socket_gate.clone(),
        LWIP_STACK_DRIVER_TASKS,
    )?;

    services::tcp_server::spawn_task(
        tcp_server_settings_receiver,
        tcp_server_command_receiver,
        tcp_server_data_receiver,
        tcp_server_network_reset_receiver,
        uart_tx_queue_sender.clone(),
        lwip_socket_gate.clone(),
        LWIP_STACK_DRIVER_TASKS,
    )?;

    services::ntp_client::spawn_task(
        ntp_client_settings_receiver,
        uart_tx_queue_sender.clone(),
        lwip_socket_gate.clone(),
        LWIP_STACK_DRIVER_TASKS,
        connected_links.clone(),
    )?;

    let mut has_master_message = false;
    let mut ready_tick: u32 = 0;

    loop {
        while let Ok(frame) = uart_rx_queue_receiver.try_recv() {
            if !has_master_message {
                has_master_message = true;
                info!("First valid packet from master received; stop READY periodic broadcast");
            }

            match master::protocol::decode_packet(&frame) {
                Ok(packet) => {
                    if packet.cmd == master::protocol::MessageType::ServiceSettings.as_u8() {
                        match master::messages::service_settings::decode(&packet) {
                            Ok(msg) => {
                                info!(
                                    "RX ServiceSettings: service={:?}, settings={:?}",
                                    msg.service, msg.settings
                                );
                                match msg.settings {
                                    master::messages::service_settings::ServiceSettings::UdpListener(
                                        settings,
                                    ) => {
                                        if let Err(err) = udp_listener_settings_sender.send(settings) {
                                            warn!("Failed to enqueue UdpListener settings: {err}");
                                        } else {
                                            info!("Enqueued UdpListener service settings");
                                        }
                                    }
                                    master::messages::service_settings::ServiceSettings::TcpServer(
                                        settings,
                                    ) => {
                                        if let Err(err) = tcp_server_settings_sender.send(settings) {
                                            warn!("Failed to enqueue TcpServer settings: {err}");
                                        } else {
                                            info!("Enqueued TcpServer service settings");
                                        }
                                    }
                                    master::messages::service_settings::ServiceSettings::NtpClient(
                                        settings,
                                    ) => {
                                        if let Err(err) = ntp_client_settings_sender.send(settings) {
                                            warn!("Failed to enqueue NtpClient settings: {err}");
                                        } else {
                                            info!("Enqueued NtpClient service settings");
                                        }
                                    }
                                }
                            }
                            Err(err) => warn!("RX ServiceSettings decode failed: {err:#}"),
                        }
                    } else if packet.cmd == master::protocol::MessageType::TcpCommand.as_u8() {
                        match master::messages::tcp_command::decode(&packet) {
                            Ok(cmd) => {
                                info!("RX TcpCommand: index={}, close={}", cmd.index, cmd.command.close);
                                if let Err(err) = tcp_server_command_sender.send(cmd) {
                                    warn!("Failed to enqueue TcpCommand: {err}");
                                }
                            }
                            Err(err) => warn!("RX TcpCommand decode failed: {err:#}"),
                        }
                    } else if packet.cmd == master::protocol::MessageType::TcpData.as_u8() {
                        match master::messages::tcp_data::decode(&packet) {
                            Ok(msg) => {
                                info!("RX TcpData: index={}, bytes={}", msg.index, msg.payload.len());
                                if let Err(err) = tcp_server_data_sender.send(msg) {
                                    warn!("Failed to enqueue TcpData: {err}");
                                }
                            }
                            Err(err) => warn!("RX TcpData decode failed: {err:#}"),
                        }
                    } else if packet.cmd == master::protocol::MessageType::WifiScan.as_u8() {
                        match master::messages::wifi_scan::decode(&packet) {
                            Ok(request) => {
                                info!("RX WifiScan: limit={}", request.limit);
                                if let Err(err) = wifi_scan_sender.send(request) {
                                    warn!("Failed to enqueue WifiScan request: {err}");
                                }
                            }
                            Err(err) => warn!("RX WifiScan decode failed: {err:#}"),
                        }
                    } else if packet.cmd == master::protocol::MessageType::InterfaceSettings.as_u8() {
                        match master::messages::interface_settings::decode(&packet) {
                            Ok(msg) => {
                                info!("RX InterfaceSettings: {:?}", msg);
                                match msg.settings {
                                    master::messages::interface_settings::InterfaceSettings::WiFiStation(
                                        station_settings,
                                    ) => {
                                        if let Err(err) =
                                            wifi_station_interface_settings_sender.send(station_settings)
                                        {
                                            warn!("Failed to enqueue WiFiStation interface event: {err}");
                                        } else {
                                            info!("Enqueued WiFiStation interface event");
                                        }
                                    }
                                    master::messages::interface_settings::InterfaceSettings::Ethernet(
                                        ethernet_settings,
                                    ) => {
                                        if let Err(err) =
                                            ethernet_interface_settings_sender.send(ethernet_settings)
                                        {
                                            warn!("Failed to enqueue Ethernet interface event: {err}");
                                        } else {
                                            info!("Enqueued Ethernet interface event");
                                        }
                                    }
                                    master::messages::interface_settings::InterfaceSettings::WiFiAccessPoint(
                                        ap_settings,
                                    ) => {
                                        if let Err(err) =
                                            wifi_ap_interface_settings_sender.send(ap_settings)
                                        {
                                            warn!("Failed to enqueue WiFiAccessPoint interface event: {err}");
                                        } else {
                                            info!("Enqueued WiFiAccessPoint interface event");
                                        }
                                    }
                                }
                            }
                            Err(err) => warn!("RX InterfaceSettings decode failed: {err:#}"),
                        }
                    } else {
                        info!(
                            "RX packet cmd={} param={} payload_len={}",
                            packet.cmd,
                            packet.parameter,
                            packet.payload.len()
                        );
                    }
                }
                Err(err) => warn!("RX frame decode failed: {err:#}"),
            }
        }

        if !has_master_message && ready_tick == 0 {
            if let Ok(frame) = master::encode_ready() {
                let _ = uart_tx_queue_sender.send(frame);
            }
        }

        ready_tick = (ready_tick + 1) % 5;
        FreeRtos::delay_ms(1000);
    }
}
