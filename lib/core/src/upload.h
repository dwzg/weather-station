#pragma once
#include <stddef.h>
#include <stdint.h>

#include "reading.h"

namespace ws {

struct Calibration {
  double altitudeM;          // height of the sensor above sea level
  double pressureOffsetHpa;  // added to the measured station pressure
};

enum class Outcome {
  Accepted,  // stored by the app: drop it from the queue
  Rejected,  // the app will never take this reading (400/422): drop it, or it blocks the queue
  Retry,     // transient (network, 5xx, 429...): keep it, try again later
  Halt,      // configuration problem (bad key, wrong URL): keep everything, wait long
};

Outcome classifyResponse(int httpStatus);

// Sea-level pressure in 0.01 hPa for a stored reading.
int32_t seaLevelCentiHpa(const Reading& r, const Calibration& cal);

// The JSON the weather app expects, built with integer arithmetic only:
// {"timestamp":"2026-11-08T10:00:00+01:00","temperature":7.12,"humidity":99.80,"pressure":1025.43}
// Uses the process time zone for the timestamp. Returns the length, 0 on overflow.
size_t buildBody(const Reading& r, const Calibration& cal, char* buf, size_t len);

// Writes a signed fixed-point value with two decimals, e.g. -123 -> "-1.23".
size_t formatCenti(int32_t v, char* buf, size_t len);

}  // namespace ws
