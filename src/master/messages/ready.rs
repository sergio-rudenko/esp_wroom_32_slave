use anyhow::Result;
use esp_idf_sys::{
    esp_reset_reason, esp_reset_reason_t, esp_reset_reason_t_ESP_RST_BROWNOUT,
    esp_reset_reason_t_ESP_RST_CPU_LOCKUP, esp_reset_reason_t_ESP_RST_DEEPSLEEP,
    esp_reset_reason_t_ESP_RST_EFUSE, esp_reset_reason_t_ESP_RST_EXT,
    esp_reset_reason_t_ESP_RST_INT_WDT, esp_reset_reason_t_ESP_RST_JTAG,
    esp_reset_reason_t_ESP_RST_PANIC, esp_reset_reason_t_ESP_RST_POWERON,
    esp_reset_reason_t_ESP_RST_PWR_GLITCH, esp_reset_reason_t_ESP_RST_SDIO,
    esp_reset_reason_t_ESP_RST_SW, esp_reset_reason_t_ESP_RST_TASK_WDT,
    esp_reset_reason_t_ESP_RST_USB, esp_reset_reason_t_ESP_RST_WDT,
};
use log::info;

use crate::master::protocol::{MessageType, encode_packet};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BootReason {
    Undefined = 0,
    Power = 1,
    External = 2,
    Software = 3,
    Panic = 4,
    InterruptWatchdog = 5,
    TaskWatchdog = 6,
    OtherWatchdog = 7,
    DeepSleep = 8,
    Brownout = 9,
    Sdio = 10,
    Usb = 11,
    Jtag = 12,
    Efuse = 13,
    PowerGlitch = 14,
    CpuLockup = 15,
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
    } else if reason == esp_reset_reason_t_ESP_RST_EXT {
        BootReason::External
    } else if reason == esp_reset_reason_t_ESP_RST_SW {
        BootReason::Software
    } else if reason == esp_reset_reason_t_ESP_RST_PANIC {
        BootReason::Panic
    } else if reason == esp_reset_reason_t_ESP_RST_INT_WDT {
        BootReason::InterruptWatchdog
    } else if reason == esp_reset_reason_t_ESP_RST_TASK_WDT {
        BootReason::TaskWatchdog
    } else if reason == esp_reset_reason_t_ESP_RST_WDT {
        BootReason::OtherWatchdog
    } else if reason == esp_reset_reason_t_ESP_RST_DEEPSLEEP {
        BootReason::DeepSleep
    } else if reason == esp_reset_reason_t_ESP_RST_BROWNOUT {
        BootReason::Brownout
    } else if reason == esp_reset_reason_t_ESP_RST_SDIO {
        BootReason::Sdio
    } else if reason == esp_reset_reason_t_ESP_RST_USB {
        BootReason::Usb
    } else if reason == esp_reset_reason_t_ESP_RST_JTAG {
        BootReason::Jtag
    } else if reason == esp_reset_reason_t_ESP_RST_EFUSE {
        BootReason::Efuse
    } else if reason == esp_reset_reason_t_ESP_RST_PWR_GLITCH {
        BootReason::PowerGlitch
    } else if reason == esp_reset_reason_t_ESP_RST_CPU_LOCKUP {
        BootReason::CpuLockup
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
