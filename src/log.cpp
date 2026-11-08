#include "log.h"

#include <Arduino.h>
#include <stdarg.h>
#include <time.h>

namespace {
constexpr size_t kLines = 40;
constexpr size_t kWidth = 100;
char ring[kLines][kWidth];
size_t next = 0;
size_t used = 0;
}  // namespace

void logf(const char* fmt, ...) {
  char* line = ring[next];
  const time_t now = time(nullptr);
  int n;
  if (now > 1700000000) {
    struct tm tm;
    localtime_r(&now, &tm);
    n = strftime(line, kWidth, "%m-%d %H:%M:%S ", &tm);
  } else {
    n = snprintf(line, kWidth, "+%lus ", static_cast<unsigned long>(millis() / 1000));
  }
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(line + n, kWidth - n, fmt, ap);
  va_end(ap);
  Serial.println(line);
  next = (next + 1) % kLines;
  if (used < kLines) ++used;
}

void logDump(Print& out) {
  for (size_t i = 0; i < used; ++i) {
    out.println(ring[(next + kLines - used + i) % kLines]);
  }
}
