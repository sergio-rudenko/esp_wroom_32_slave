use anyhow::Result;
use esp_idf_hal::delay::FreeRtos;
use esp_idf_sys::TickType_t;
use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use log::*;
use std::sync::mpsc;
use std::thread;

mod master;
mod network;

fn main() -> Result<()> {
    const UART_TX_TASK_STACK_BYTES: usize = 8 * 1024;
    const UART_RX_TASK_STACK_BYTES: usize = 8 * 1024;

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
    let (ethernet_interface_settings_sender, ethernet_interface_settings_receiver) =
        mpsc::channel::<master::messages::interface_settings::EthernetSettings>();

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

    network::wifi_station::spawn_task(
        modem,
        sysloop.clone(),
        nvs,
        wifi_station_interface_settings_receiver,
        uart_tx_queue_sender.clone(),
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
    )?;
    
    // Mock interface event until real master-side config flow is fully integrated.
    let _ = wifi_station_interface_settings_sender.send(network::wifi_station::mock_settings());
    let _ = ethernet_interface_settings_sender.send(network::ethernet::mock_settings());

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
                    if packet.cmd == master::protocol::MessageType::InterfaceSettings.as_u8() {
                        match master::messages::interface_settings::decode(&packet) {
                            Ok(msg) => {
                                info!("RX InterfaceSettings: {:?}", msg);
                                match msg.settings {
                                    master::messages::interface_settings::InterfaceSettings::WiFiStation(
                                        station_cfg,
                                    ) => {
                                        if let Err(err) =
                                            wifi_station_interface_settings_sender.send(station_cfg)
                                        {
                                            warn!("Failed to enqueue WiFiStation interface event: {err}");
                                        } else {
                                            info!("Enqueued WiFiStation interface event");
                                        }
                                    }
                                    master::messages::interface_settings::InterfaceSettings::Ethernet(
                                        ethernet_cfg,
                                    ) => {
                                        if let Err(err) =
                                            ethernet_interface_settings_sender.send(ethernet_cfg)
                                        {
                                            warn!("Failed to enqueue Ethernet interface event: {err}");
                                        } else {
                                            info!("Enqueued Ethernet interface event");
                                        }
                                    }
                                    master::messages::interface_settings::InterfaceSettings::WiFiAccessPoint(_) => {}
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
