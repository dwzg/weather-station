//! The sensors' I2C bus: the ESP-IDF driver with a timeout on every transfer,
//! and bus recovery (which needs the pins as plain GPIOs, so the driver is
//! taken down and set up again around it).

use embedded_hal::i2c::{ErrorKind, ErrorType, I2c, NoAcknowledgeSource, Operation};
use esp_idf_svc::hal::delay::{Ets, TickType};
use esp_idf_svc::hal::gpio::{Gpio33, Gpio35};
use esp_idf_svc::hal::i2c::{I2cConfig, I2cDriver, I2C0};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys;
use ws_core::sensors::Bus;

use crate::config;

pub struct I2cBus {
    drv: Option<I2cDriver<'static>>,
}

impl I2cBus {
    /// Takes the peripherals, so nothing else can use them.
    pub fn new(_i2c: I2C0<'static>, _sda: Gpio33<'static>, _scl: Gpio35<'static>) -> Self {
        let mut bus = Self { drv: None };
        bus.open();
        bus
    }

    fn open(&mut self) {
        let cfg = I2cConfig::new().baudrate(Hertz(config::I2C_HZ)).sda_enable_pullup(true).scl_enable_pullup(true);
        // SAFETY: new() consumed the only handles; the previous driver (if any)
        // was dropped before this, so exactly one instance uses them.
        self.drv = unsafe { I2cDriver::new(I2C0::steal(), Gpio33::steal(), Gpio35::steal(), &cfg) }.ok();
    }
}

impl ErrorType for I2cBus {
    type Error = ErrorKind;
}

impl I2c for I2cBus {
    fn transaction(&mut self, addr: u8, ops: &mut [Operation<'_>]) -> Result<(), ErrorKind> {
        let drv = self.drv.as_mut().ok_or(ErrorKind::Other)?;
        drv.transaction(addr, ops, TickType::new_millis(config::I2C_TIMEOUT_MS).ticks()).map_err(|e| {
            if e.code() == sys::ESP_FAIL {
                ErrorKind::NoAcknowledge(NoAcknowledgeSource::Unknown)
            } else {
                ErrorKind::Bus
            }
        })
    }
}

impl Bus for I2cBus {
    fn recover(&mut self) -> bool {
        self.drv = None; // uninstalls the driver, frees the pins
        let (sda, scl) = (config::PIN_SDA, config::PIN_SCL);
        let stuck;
        // SAFETY: the driver is gone, the pins are ours.
        unsafe {
            for p in [sda, scl] {
                sys::gpio_reset_pin(p);
                sys::gpio_set_pull_mode(p, sys::gpio_pull_mode_t_GPIO_PULLUP_ONLY);
                sys::gpio_set_direction(p, sys::gpio_mode_t_GPIO_MODE_INPUT_OUTPUT_OD);
                sys::gpio_set_level(p, 1);
            }
            Ets::delay_us(10);
            stuck = sys::gpio_get_level(sda) == 0;
            if stuck {
                // Clock out whatever the sensor is still sending, then STOP.
                for _ in 0..9 {
                    if sys::gpio_get_level(sda) != 0 {
                        break;
                    }
                    sys::gpio_set_level(scl, 0);
                    Ets::delay_us(5);
                    sys::gpio_set_level(scl, 1);
                    Ets::delay_us(5);
                }
                sys::gpio_set_level(sda, 0);
                Ets::delay_us(5);
                sys::gpio_set_level(sda, 1);
            }
        }
        self.open();
        stuck
    }
}
