use anyhow::Result;
use esp_idf_hal::delay::FreeRtos;
use log::*;
use std::io::ErrorKind;
use std::io::Read;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::master::messages::service_settings::TcpServerSettings;
use crate::master::messages::service_state::{self, TcpDisconnectReason};
use crate::master::messages::tcp_data::TcpDataMessage;
use crate::master::messages::tcp_command::TcpCommandMessage;

const TCP_SERVER_TASK_STACK_BYTES: usize = 24 * 1024;
const MAX_CLIENTS: usize = 4;
const MAX_PAYLOAD_SIZE: usize = 1500;
const LOOP_DELAY_MS: u32 = 5;

struct ClientConn {
    stream: TcpStream,
    last_activity: Instant,
}

pub fn spawn_task(
    tcp_settings_receiver: mpsc::Receiver<TcpServerSettings>,
    tcp_command_receiver: mpsc::Receiver<TcpCommandMessage>,
    tcp_data_receiver: mpsc::Receiver<TcpDataMessage>,
    tcp_network_reset_receiver: mpsc::Receiver<()>,
    uart_tx_queue_sender: mpsc::Sender<Vec<u8>>,
    lwip_socket_gate: Arc<AtomicU8>,
    lwip_socket_gate_expected: u8,
) -> Result<()> {
    thread::Builder::new()
        .name("tcp-server".into())
        .stack_size(TCP_SERVER_TASK_STACK_BYTES)
        .spawn(move || {
            run(
                tcp_settings_receiver,
                tcp_command_receiver,
                tcp_data_receiver,
                tcp_network_reset_receiver,
                uart_tx_queue_sender,
                lwip_socket_gate,
                lwip_socket_gate_expected,
            )
        })
        .map_err(|e| anyhow::anyhow!("failed to spawn TCP server task: {e}"))?;
    Ok(())
}

fn run(
    tcp_settings_receiver: mpsc::Receiver<TcpServerSettings>,
    tcp_command_receiver: mpsc::Receiver<TcpCommandMessage>,
    tcp_data_receiver: mpsc::Receiver<TcpDataMessage>,
    tcp_network_reset_receiver: mpsc::Receiver<()>,
    uart_tx_queue_sender: mpsc::Sender<Vec<u8>>,
    lwip_socket_gate: Arc<AtomicU8>,
    lwip_socket_gate_expected: u8,
) {
    info!("TCP server task started; waiting for ServiceSettings (TcpServer)...");

    let mut settings = match tcp_settings_receiver.recv() {
        Ok(settings) => settings,
        Err(_) => {
            info!("TCP server task stopped: settings channel closed before first config");
            return;
        }
    };

    let mut listener: Option<TcpListener> = None;
    let mut clients: Vec<Option<ClientConn>> = (0..MAX_CLIENTS).map(|_| None).collect();
    let mut rx_buf = [0_u8; MAX_PAYLOAD_SIZE];

    loop {
        if listener.is_none() {
            wait_for_lwip_driver_init(&lwip_socket_gate, lwip_socket_gate_expected);
            match open_listener(settings.port) {
                Ok(l) => listener = Some(l),
                Err(err) => {
                    error!("TCP server bind failed: {err:#}; waiting for new settings");
                    settings = match tcp_settings_receiver.recv() {
                        Ok(next_settings) => next_settings,
                        Err(_) => {
                            info!("TCP server task stopped: settings channel closed");
                            return;
                        }
                    };
                    continue;
                }
            }
        }

        while let Ok(next_settings) = tcp_settings_receiver.try_recv() {
            info!(
                "TCP server apply settings: port={}, clientTimeout={}s",
                next_settings.port, next_settings.client_timeout
            );
            disconnect_all_clients(
                &mut clients,
                &uart_tx_queue_sender,
                TcpDisconnectReason::ServerClosedConnection,
            );
            listener = None;
            settings = next_settings;
        }
        drain_network_reset_events(&tcp_network_reset_receiver, &mut clients, &uart_tx_queue_sender);

        drain_tcp_commands(
            &tcp_command_receiver,
            &mut clients,
            &uart_tx_queue_sender,
            settings.client_timeout,
        );
        drain_tcp_data(&tcp_data_receiver, &mut clients, &uart_tx_queue_sender);

        if let Some(ref l) = listener {
            accept_new_clients(l, &mut clients, &uart_tx_queue_sender);
            poll_clients(
                &mut clients,
                &uart_tx_queue_sender,
                &mut rx_buf,
                settings.client_timeout,
            );
        }

        FreeRtos::delay_ms(LOOP_DELAY_MS);
    }
}

fn open_listener(port: u16) -> Result<TcpListener> {
    let addr: SocketAddr = format!("0.0.0.0:{port}").parse()?;
    let listener = TcpListener::bind(addr)?;
    listener.set_nonblocking(true)?;
    info!("TCP server listening on 0.0.0.0:{port} (all interfaces)");
    Ok(listener)
}

fn accept_new_clients(
    listener: &TcpListener,
    clients: &mut [Option<ClientConn>],
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
) {
    loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                if let Some(index) = first_free_slot(clients) {
                    if let Err(err) = stream.set_nonblocking(true) {
                        warn!("TCP client setup failed for {addr}: {err:#}");
                        continue;
                    }
                    clients[index] = Some(ClientConn {
                        stream,
                        last_activity: Instant::now(),
                    });
                    send_service_state(
                        uart_tx_queue_sender,
                        service_state::encode_tcp_connected(index as u8, addr.ip().to_string(), addr.port()),
                        "tcp_connected",
                    );
                    info!("TCP client accepted index={index} from {addr}");
                } else {
                    warn!("TCP client rejected (max {} reached): {}", MAX_CLIENTS, addr);
                }
            }
            Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(err) => {
                warn!("TCP accept failed: {err:#}");
                break;
            }
        }
    }
}

fn poll_clients(
    clients: &mut [Option<ClientConn>],
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    rx_buf: &mut [u8; MAX_PAYLOAD_SIZE],
    client_timeout_secs: u16,
) {
    let timeout = if client_timeout_secs == 0 {
        None
    } else {
        Some(Duration::from_secs(client_timeout_secs as u64))
    };

    for (idx, slot) in clients.iter_mut().enumerate() {
        let Some(conn) = slot.as_mut() else {
            continue;
        };

        let mut should_disconnect: Option<TcpDisconnectReason> = None;
        loop {
            match conn.stream.read(rx_buf) {
                Ok(0) => {
                    should_disconnect = Some(TcpDisconnectReason::ClientClosedConnection);
                    break;
                }
                Ok(n) => {
                    conn.last_activity = Instant::now();
                    send_tcp_data_upstream(uart_tx_queue_sender, idx as u8, &rx_buf[..n]);
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(err) => {
                    warn!("TCP read failed for index={idx}: {err:#}");
                    should_disconnect = Some(TcpDisconnectReason::Undefined);
                    break;
                }
            }
        }

        if should_disconnect.is_none()
            && timeout
                .map(|t| conn.last_activity.elapsed() > t)
                .unwrap_or(false)
        {
            should_disconnect = Some(TcpDisconnectReason::InactivityTimeout);
        }

        if let Some(reason) = should_disconnect {
            *slot = None;
            send_service_state(
                uart_tx_queue_sender,
                service_state::encode_tcp_disconnected(idx as u8, reason),
                "tcp_disconnected",
            );
            info!("TCP client disconnected index={idx} reason={reason:?}");
        }
    }
}

fn drain_tcp_commands(
    tcp_command_receiver: &mpsc::Receiver<TcpCommandMessage>,
    clients: &mut [Option<ClientConn>],
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    _client_timeout_secs: u16,
) {
    while let Ok(cmd) = tcp_command_receiver.try_recv() {
        if !cmd.command.close {
            continue;
        }

        let index = cmd.index as usize;
        if index >= MAX_CLIENTS {
            warn!("TCP close command out of range: {}", cmd.index);
            continue;
        }

        if clients[index].take().is_some() {
            send_service_state(
                uart_tx_queue_sender,
                service_state::encode_tcp_disconnected(
                    cmd.index,
                    TcpDisconnectReason::ServerClosedConnection,
                ),
                "tcp_disconnected",
            );
            info!("TCP client closed by command index={}", cmd.index);
        } else {
            info!("TCP close command for inactive index={}", cmd.index);
        }
    }
}

fn drain_tcp_data(
    tcp_data_receiver: &mpsc::Receiver<TcpDataMessage>,
    clients: &mut [Option<ClientConn>],
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
) {
    while let Ok(msg) = tcp_data_receiver.try_recv() {
        let index = msg.index as usize;
        if index >= MAX_CLIENTS {
            warn!("TcpData index out of range: {}", msg.index);
            continue;
        }
        let Some(conn) = clients[index].as_mut() else {
            warn!("TcpData for inactive index={}", msg.index);
            send_service_state(
                uart_tx_queue_sender,
                service_state::encode_tcp_disconnected(msg.index, TcpDisconnectReason::NotConnected),
                "tcp_disconnected_not_connected",
            );
            continue;
        };
        if let Err(err) = std::io::Write::write_all(&mut conn.stream, &msg.payload) {
            warn!("TcpData write failed for index={}: {err:#}", msg.index);
            continue;
        }
        conn.last_activity = Instant::now();
    }
}

fn drain_network_reset_events(
    tcp_network_reset_receiver: &mpsc::Receiver<()>,
    clients: &mut [Option<ClientConn>],
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
) {
    let mut got_event = false;
    while tcp_network_reset_receiver.try_recv().is_ok() {
        got_event = true;
    }
    if !got_event {
        return;
    }
    info!("TCP server: network reset signal received, closing all clients");
    disconnect_all_clients(
        clients,
        uart_tx_queue_sender,
        TcpDisconnectReason::ServerClosedConnection,
    );
}

fn disconnect_all_clients(
    clients: &mut [Option<ClientConn>],
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    reason: TcpDisconnectReason,
) {
    for (idx, slot) in clients.iter_mut().enumerate() {
        if slot.take().is_some() {
            send_service_state(
                uart_tx_queue_sender,
                service_state::encode_tcp_disconnected(idx as u8, reason),
                "tcp_disconnected",
            );
        }
    }
}

fn first_free_slot(clients: &[Option<ClientConn>]) -> Option<usize> {
    clients.iter().position(|slot| slot.is_none())
}

fn send_service_state(
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
    frame_result: Result<Vec<u8>>,
    event_name: &str,
) {
    match frame_result {
        Ok(frame) => {
            if let Err(err) = uart_tx_queue_sender.send(frame) {
                warn!("ServiceState {event_name} enqueue failed: {err}");
            }
        }
        Err(err) => warn!("ServiceState {event_name} encode failed: {err:#}"),
    }
}

fn send_tcp_data_upstream(uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>, index: u8, payload: &[u8]) {
    let frame_result = crate::master::messages::tcp_data::encode(index, payload);
    match frame_result {
        Ok(frame) => {
            if let Err(err) = uart_tx_queue_sender.send(frame) {
                warn!("TcpData enqueue failed for index={index}: {err}");
            }
        }
        Err(err) => warn!("TcpData encode failed for index={index}: {err:#}"),
    }
}

fn wait_for_lwip_driver_init(counter: &Arc<AtomicU8>, expected: u8) {
    const SLICE_MS: u32 = 50;
    const MAX_WAIT_MS: u32 = 120_000;
    let mut waited = 0_u32;
    while counter.load(Ordering::SeqCst) < expected && waited < MAX_WAIT_MS {
        FreeRtos::delay_ms(SLICE_MS);
        waited += SLICE_MS;
    }
    let n = counter.load(Ordering::SeqCst);
    if n < expected {
        warn!(
            "TCP server: lwIP driver init signals {}/{} after {} ms — binding anyway",
            n, expected, MAX_WAIT_MS
        );
    }
}
