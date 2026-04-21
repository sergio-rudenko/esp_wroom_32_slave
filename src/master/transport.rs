use anyhow::Result;
use esp_idf_hal::gpio::{AnyInputPin, AnyOutputPin};
use esp_idf_hal::peripheral::Peripheral;
use esp_idf_hal::prelude::*;
use esp_idf_hal::uart;
use esp_idf_hal::uart::UartDriver;

use super::config::{UART1_BAUDRATE, UART1_RX_GPIO, UART1_TX_GPIO};

pub fn init_uart1_link<UART>(uart: impl Peripheral<P = UART> + 'static) -> Result<UartDriver<'static>>
where
    UART: uart::Uart,
{
    // Pin numbers are centralized in config for quick board remapping.
    let tx = unsafe { AnyOutputPin::new(UART1_TX_GPIO) };
    let rx = unsafe { AnyInputPin::new(UART1_RX_GPIO) };

    let config = uart::config::Config::new().baudrate(Hertz(UART1_BAUDRATE));
    let driver = UartDriver::new(uart, tx, rx, AnyInputPin::none(), AnyOutputPin::none(), &config)?;

    Ok(driver)
}
