#include "upload.h"

#include <math.h>
#include <stdio.h>

#include "pressure.h"
#include "timefmt.h"

namespace ws {

Outcome classifyResponse(int s) {
  if (s >= 200 && s < 300) return Outcome::Accepted;
  if (s == 400 || s == 422) return Outcome::Rejected;
  if (s == 401 || s == 403 || s == 404 || s == 405) return Outcome::Halt;
  return Outcome::Retry;  // <= 0 (no connection, TLS, timeout), 408, 429, 5xx, anything unexpected
}

int32_t seaLevelCentiHpa(const Reading& r, const Calibration& cal) {
  const double station = r.stationDeciPa / 1000.0 + cal.pressureOffsetHpa;
  const double p0 = seaLevelPressureDwd(station, r.tempCenti / 100.0, r.humCenti / 100.0, cal.altitudeM);
  return static_cast<int32_t>(lround(p0 * 100.0));
}

size_t formatCenti(int32_t v, char* buf, size_t len) {
  const bool neg = v < 0;
  const uint32_t a = neg ? static_cast<uint32_t>(-static_cast<int64_t>(v)) : static_cast<uint32_t>(v);
  int n = snprintf(buf, len, "%s%lu.%02lu", neg ? "-" : "", static_cast<unsigned long>(a / 100),
                   static_cast<unsigned long>(a % 100));
  return (n > 0 && static_cast<size_t>(n) < len) ? static_cast<size_t>(n) : 0;
}

size_t buildBody(const Reading& r, const Calibration& cal, char* buf, size_t len) {
  char ts[32], t[16], h[16], p[16];
  if (!formatIsoLocal(static_cast<time_t>(r.epoch), ts, sizeof ts)) return 0;
  if (!formatCenti(r.tempCenti, t, sizeof t) || !formatCenti(r.humCenti, h, sizeof h) ||
      !formatCenti(seaLevelCentiHpa(r, cal), p, sizeof p)) {
    return 0;
  }
  int n = snprintf(buf, len, "{\"timestamp\":\"%s\",\"temperature\":%s,\"humidity\":%s,\"pressure\":%s}", ts, t, h, p);
  return (n > 0 && static_cast<size_t>(n) < len) ? static_cast<size_t>(n) : 0;
}

}  // namespace ws
