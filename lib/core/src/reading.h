#pragma once
#include <stddef.h>
#include <stdint.h>

namespace ws {

// Which sensor supplied temperature and humidity (pressure always comes from the BME280).
enum Source : uint8_t {
  kSourceBme280 = 0,
  kSourceSht4x = 1,
};

// One observation for one 5-minute slot, as it is kept in the flash queue.
//
// Everything is fixed-point so a reading has exactly one encoding. The pressure
// is the raw station pressure as the sensor measured it: calibration and the
// reduction to sea level are applied when the reading is sent, so a firmware
// update with a better calibration also corrects readings still in the queue.
struct Reading {
  uint32_t epoch = 0;        // start of the slot, seconds since 1970-01-01 UTC
  int16_t tempCenti = 0;     // air temperature in 0.01 degC
  uint16_t humCenti = 0;     // relative humidity in 0.01 %
  uint32_t stationDeciPa = 0;  // station pressure in 0.1 Pa (= 0.001 hPa)
  uint8_t source = kSourceBme280;
};

constexpr size_t kRecordSize = 16;
constexpr uint8_t kRecordVersion = 1;

// If the format ever changes, bump kRecordVersion and keep decoding version 1:
// otherwise every reading still queued at the update is skipped as corrupt.
//
// Layout (little endian): epoch u32 | temp i16 | hum u16 | pressure u32 | source u8 |
// version u8 | crc16 over bytes 0..13.
void encodeReading(const Reading& r, uint8_t out[kRecordSize]);

// False for a torn, corrupted or unknown record.
bool decodeReading(const uint8_t in[kRecordSize], Reading& out);

}  // namespace ws
