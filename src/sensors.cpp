#include "sensors.h"

#include <Adafruit_BME280.h>
#include <Arduino.h>
#include <Wire.h>

#include "config.h"
#include "log.h"
#include "median.h"
#include "sht4x.h"

namespace {
Adafruit_BME280 bme;
constexpr int kSamples = 3;

constexpr uint8_t kShtMeasureHigh = 0xFD;      // high repeatability, 8.3 ms
constexpr uint8_t kShtSerial = 0x89;
constexpr uint8_t kShtSoftReset = 0x94;
constexpr uint8_t kShtHeat110mW1s = 0x2F;      // heater, then a measurement
constexpr float kHeaterAboveRh = 95.0f;
constexpr uint32_t kReprobeMs = 10UL * 60 * 1000;
}  // namespace

void Sensors::recoverBus() {
  // A sensor that was reset mid-transfer can hold SDA low forever. Clock it out
  // (up to 9 pulses), then issue a STOP; this is what I2C section 3.1.16 prescribes.
  pinMode(cfg::kPinSda, INPUT_PULLUP);
  pinMode(cfg::kPinScl, INPUT_PULLUP);
  delayMicroseconds(10);
  if (digitalRead(cfg::kPinSda) == LOW) {
    ++st_.busRecoveries;
    pinMode(cfg::kPinScl, OUTPUT_OPEN_DRAIN);
    for (int i = 0; i < 9 && digitalRead(cfg::kPinSda) == LOW; ++i) {
      digitalWrite(cfg::kPinScl, LOW);
      delayMicroseconds(5);
      digitalWrite(cfg::kPinScl, HIGH);
      delayMicroseconds(5);
    }
    pinMode(cfg::kPinSda, OUTPUT_OPEN_DRAIN);
    digitalWrite(cfg::kPinSda, LOW);
    delayMicroseconds(5);
    digitalWrite(cfg::kPinSda, HIGH);
    logf("i2c: bus recovered (SDA was stuck low)");
  }
  Wire.begin(cfg::kPinSda, cfg::kPinScl);
  Wire.setClock(100000);
  Wire.setClockStretchLimit(150000);
}

bool Sensors::initBme280() {
  if (!bme.begin(cfg::kBme280Addr, &Wire)) return false;
  // Forced mode: the sensor sleeps between readings, so it does not warm itself
  // up (the datasheet's "weather monitoring" setting, with more oversampling).
  bme.setSampling(Adafruit_BME280::MODE_FORCED, Adafruit_BME280::SAMPLING_X4, Adafruit_BME280::SAMPLING_X4,
                  Adafruit_BME280::SAMPLING_X4, Adafruit_BME280::FILTER_OFF);
  return true;
}

bool Sensors::readSht4x(uint8_t cmd, uint16_t waitMs, int32_t& tCenti, int32_t& hCenti) {
  Wire.beginTransmission(cfg::kSht4xAddr);
  Wire.write(cmd);
  if (Wire.endTransmission() != 0) return false;
  delay(waitMs);
  if (Wire.requestFrom(cfg::kSht4xAddr, static_cast<uint8_t>(6)) != 6) return false;
  uint8_t f[6];
  for (uint8_t& b : f) b = Wire.read();
  return ws::decodeSht4x(f, tCenti, hCenti);
}

bool Sensors::probeSht4x() {
  lastProbeMs_ = millis();
  Wire.beginTransmission(cfg::kSht4xAddr);
  Wire.write(kShtSoftReset);
  if (Wire.endTransmission() != 0) return false;
  delay(2);
  Wire.beginTransmission(cfg::kSht4xAddr);
  Wire.write(kShtSerial);
  if (Wire.endTransmission() != 0) return false;
  delay(2);
  if (Wire.requestFrom(cfg::kSht4xAddr, static_cast<uint8_t>(6)) != 6) return false;
  uint8_t f[6];
  for (uint8_t& b : f) b = Wire.read();
  int32_t a, b;
  if (!ws::decodeSht4x(f, a, b)) return false;  // same framing and CRCs as a measurement
  st_.sht4xSerial = (static_cast<uint32_t>(f[0]) << 24) | (f[1] << 16) | (f[3] << 8) | f[4];
  return true;
}

void Sensors::begin() {
  recoverBus();
  st_.bme280 = initBme280();
  st_.sht4x = probeSht4x();
  logf("sensors: BME280 %s, SHT4x %s", st_.bme280 ? "ok" : "MISSING", st_.sht4x ? "ok" : "not fitted");
  if (st_.sht4x) logf("sensors: SHT4x serial %08lx", static_cast<unsigned long>(st_.sht4xSerial));
}

bool Sensors::measure(uint32_t slotEpoch, ws::Reading& out) {
  if (!st_.bme280 || (!st_.sht4x && millis() - lastProbeMs_ > kReprobeMs)) {
    recoverBus();
    if (!st_.bme280) st_.bme280 = initBme280();
    if (!st_.sht4x && probeSht4x()) {
      st_.sht4x = true;
      logf("sensors: SHT4x found (serial %08lx), using it for temperature and humidity",
           static_cast<unsigned long>(st_.sht4xSerial));
    }
  }
  if (!st_.bme280) {
    ++st_.failures;
    logf("sensors: BME280 not responding");
    return false;
  }

  float t[kSamples], h[kSamples], p[kSamples];
  for (int i = 0; i < kSamples; ++i) {
    if (!bme.takeForcedMeasurement()) {
      st_.bme280 = false;  // re-initialise next time
      ++st_.failures;
      logf("sensors: BME280 measurement timed out");
      return false;
    }
    t[i] = bme.readTemperature();
    p[i] = bme.readPressure() / 100.0f;
    h[i] = bme.readHumidity();
  }
  const float tm = ws::median(t, kSamples), hm = ws::median(h, kSamples), pm = ws::median(p, kSamples);
  if (!(pm > 300.0f && pm < 1100.0f) || !(tm > -40.0f && tm < 85.0f) || !(hm >= 0.0f && hm <= 100.0f)) {
    st_.bme280 = false;
    ++st_.failures;
    logf("sensors: BME280 implausible (T=%.2f RH=%.2f p=%.2f)", tm, hm, pm);
    return false;
  }

  out.epoch = slotEpoch;
  out.stationDeciPa = static_cast<uint32_t>(lroundf(pm * 1000.0f));
  out.tempCenti = static_cast<int16_t>(lroundf(tm * 100.0f));
  out.humCenti = static_cast<uint16_t>(lroundf(hm * 100.0f));
  out.source = ws::kSourceBme280;

  if (st_.sht4x) {
    int32_t ts[kSamples], hs[kSamples];
    int n = 0;
    for (int i = 0; i < kSamples; ++i) {
      if (readSht4x(kShtMeasureHigh, 10, ts[n], hs[n])) ++n;
    }
    if (n > 0 && ts[0] > -4000 && ts[0] < 8500) {
      out.tempCenti = static_cast<int16_t>(ws::median(ts, n));
      out.humCenti = static_cast<uint16_t>(ws::median(hs, n));
      out.source = ws::kSourceSht4x;
    } else {
      st_.sht4x = false;  // probe again later; this slot uses the BME280
      logf("sensors: SHT4x read failed, falling back to BME280 for this slot");
    }
  }
  lastHum_ = out.humCenti / 100.0f;
  shtHumid_ = out.source == ws::kSourceSht4x && lastHum_ >= kHeaterAboveRh;
  return true;
}

void Sensors::maybeDeCondense() {
  if (!shtHumid_) return;
  shtHumid_ = false;
  int32_t t, h;
  // 110 mW for 1 s, then the sensor measures (that reading is discarded: it is hot).
  // Five minutes until the next slot is plenty for it to cool down again.
  if (readSht4x(kShtHeat110mW1s, 1100, t, h)) ++st_.heaterPulses;
}
