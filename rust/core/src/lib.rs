//! Everything about the weather station that is not hardware: the flash queue
//! and its record format, time zones, pressure, the upload policy, the sensor
//! drivers (over embedded-hal) and the self-healing rules. It builds and is
//! tested on the host (`cargo test`); `../firmware` is the ESP32-S2 glue.

pub mod backoff;
pub mod bme280;
pub mod boot;
pub mod crc;
pub mod json;
pub mod median;
pub mod ota;
pub mod pressure;
pub mod queue;
pub mod reading;
pub mod sensors;
pub mod sht4x;
pub mod timefmt;
pub mod upload;
pub mod uploader;
