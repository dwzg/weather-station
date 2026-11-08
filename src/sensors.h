#pragma once
#include <stdint.h>

#include "reading.h"

struct SensorStatus {
  bool bme280 = false;
  bool sht4x = false;
  uint32_t sht4xSerial = 0;
  uint32_t failures = 0;     // measurements that produced no reading
  uint32_t busRecoveries = 0;
  uint32_t heaterPulses = 0;
};

// BME280 for pressure (and temperature/humidity while no SHT4x is fitted),
// optional SHT4x for temperature and humidity. Both are found at runtime, so
// fitting the SHT45 needs no new firmware.
class Sensors {
 public:
  void begin();
  // Takes one reading for `slotEpoch`. False if any value is missing or implausible.
  bool measure(uint32_t slotEpoch, ws::Reading& out);
  // Runs the SHT4x heater after a measurement in near-saturated air, so
  // condensation does not leave the humidity stuck at 100 %.
  void maybeDeCondense();
  const SensorStatus& status() const { return st_; }
  float lastHumidity() const { return lastHum_; }

 private:
  void recoverBus();
  bool initBme280();
  bool probeSht4x();
  bool readSht4x(uint8_t cmd, uint16_t waitMs, int32_t& tCenti, int32_t& hCenti);

  SensorStatus st_;
  float lastHum_ = 0;
  bool shtHumid_ = false;
  uint32_t lastProbeMs_ = 0;
};
