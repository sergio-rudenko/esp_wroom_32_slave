use anyhow::Result;
use log::info;
use esp_idf_sys::{
    esp_reset_reason, esp_reset_reason_t, esp_reset_reason_t_ESP_RST_BROWNOUT,
    esp_reset_reason_t_ESP_RST_CPU_LOCKUP, esp_reset_reason_t_ESP_RST_EXT,
    esp_reset_reason_t_ESP_RST_INT_WDT, esp_reset_reason_t_ESP_RST_PANIC,
    esp_reset_reason_t_ESP_RST_POWERON, esp_reset_reason_t_ESP_RST_PWR_GLITCH,
    esp_reset_reason_t_ESP_RST_SW, esp_reset_reason_t_ESP_RST_TASK_WDT, esp_reset_reason_t_ESP_RST_WDT,
};

use crate::master::protocol::{MessageType, encode_packet};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BootReason {
    Undefined = 0,
    Power = 1,
    Reset = 2,
    Watchdog = 3,
}

impl BootReason {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }
}

pub fn current_boot_reason() -> BootReason {
    let reason: esp_reset_reason_t = unsafe { esp_reset_reason() };
    if reason == esp_reset_reason_t_ESP_RST_POWERON {
        BootReason::Power
    } else if reason == esp_reset_reason_t_ESP_RST_INT_WDT
        || reason == esp_reset_reason_t_ESP_RST_TASK_WDT
        || reason == esp_reset_reason_t_ESP_RST_WDT
        || reason == esp_reset_reason_t_ESP_RST_CPU_LOCKUP
    {
        BootReason::Watchdog
    } else if reason == esp_reset_reason_t_ESP_RST_EXT
        || reason == esp_reset_reason_t_ESP_RST_SW
        || reason == esp_reset_reason_t_ESP_RST_PANIC
        || reason == esp_reset_reason_t_ESP_RST_BROWNOUT
        || reason == esp_reset_reason_t_ESP_RST_PWR_GLITCH
    {
        BootReason::Reset
    } else {
        BootReason::Undefined
    }
}

pub fn encode(boot: BootReason) -> Result<Vec<u8>> {
    encode_packet(MessageType::Ready.as_u8(), boot.as_u8(), &[])
}

pub fn encode_current() -> Result<Vec<u8>> {
    let boot = current_boot_reason();
    let frame = encode(boot)?;
    info!("Enqueued READY message, boot_reason={:?}", boot);
    Ok(frame)
}
