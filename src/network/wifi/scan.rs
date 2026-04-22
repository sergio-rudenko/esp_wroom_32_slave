use anyhow::Result;
use embedded_svc::wifi::AuthMethod;
use esp_idf_svc::wifi::{BlockingWifi, EspWifi};
use std::sync::mpsc;

use crate::master::messages::wifi_scan::{self, WifiScanRequestMessage, WifiScanResultItem};

use super::state::format_mac;

pub fn process_wifi_scan_requests(
    wifi: &mut BlockingWifi<EspWifi<'_>>,
    wifi_scan_receiver: &mpsc::Receiver<WifiScanRequestMessage>,
    uart_tx_queue_sender: &mpsc::Sender<Vec<u8>>,
) {
    while let Ok(request) = wifi_scan_receiver.try_recv() {
        log::info!("WiFiStation scan requested: limit={}", request.limit);
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
                log::warn!("WiFiStation scan failed: {err:#}");
                Vec::new()
            }
        };

        if request.limit > 0 && entries.len() > request.limit as usize {
            entries.truncate(request.limit as usize);
        }

        if let Err(err) = send_wifi_scan_chunks(uart_tx_queue_sender, &entries) {
            log::warn!("WiFiStation scan response encode/send failed: {err:#}");
        } else {
            log::info!("WiFiStation scan response sent: {} AP entries", entries.len());
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
            log::warn!("WiFiStation scan: single AP item exceeds MAX_PAYLOAD_SIZE, skipping");
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
