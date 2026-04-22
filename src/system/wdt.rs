use esp_idf_hal::sys::EspError;
use esp_idf_sys::{
    esp_task_wdt_add, esp_task_wdt_config_t, esp_task_wdt_delete, esp_task_wdt_init,
    esp_task_wdt_reconfigure, esp_task_wdt_reset,
    ESP_ERR_INVALID_STATE,
};
use log::*;
use std::sync::atomic::{AtomicBool, Ordering};

static WDT_ENABLED: AtomicBool = AtomicBool::new(false);

pub fn init(timeout_secs: u32) {
    let cfg = esp_task_wdt_config_t {
        timeout_ms: timeout_secs.saturating_mul(1000),
        idle_core_mask: 0,
        trigger_panic: true,
    };

    match EspError::convert(unsafe { esp_task_wdt_init(&cfg) }) {
        Ok(()) => {
            WDT_ENABLED.store(true, Ordering::Relaxed);
            info!("Task WDT initialized: timeout={}s", timeout_secs);
        }
        Err(err) => {
            if err.code() == ESP_ERR_INVALID_STATE {
                // ESP-IDF may initialize TWDT earlier with its own timeout.
                match EspError::convert(unsafe { esp_task_wdt_reconfigure(&cfg) }) {
                    Ok(()) => {
                        WDT_ENABLED.store(true, Ordering::Relaxed);
                        info!(
                            "Task WDT already initialized; reconfigured timeout={}s",
                            timeout_secs
                        );
                    }
                    Err(recfg_err) => {
                        WDT_ENABLED.store(true, Ordering::Relaxed);
                        warn!(
                            "Task WDT already initialized, but reconfigure failed: {recfg_err:#}; using existing runtime settings"
                        );
                    }
                }
            } else {
                warn!("Task WDT init failed: {err:#}");
            }
        }
    }
}

pub fn subscribe_current_task(task_name: &str) {
    if !WDT_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Err(err) = EspError::convert(unsafe { esp_task_wdt_add(core::ptr::null_mut()) }) {
        warn!("Task WDT subscribe failed for {task_name}: {err:#}");
    } else {
        info!("Task WDT subscribed: {task_name}");
    }
}

pub fn feed(task_name: &str) {
    if !WDT_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Err(err) = EspError::convert(unsafe { esp_task_wdt_reset() }) {
        warn!("Task WDT feed failed for {task_name}: {err:#}");
    }
}

#[allow(dead_code)]
pub fn unsubscribe_current_task(task_name: &str) {
    if !WDT_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    if let Err(err) = EspError::convert(unsafe { esp_task_wdt_delete(core::ptr::null_mut()) }) {
        warn!("Task WDT unsubscribe failed for {task_name}: {err:#}");
    } else {
        info!("Task WDT unsubscribed: {task_name}");
    }
}
