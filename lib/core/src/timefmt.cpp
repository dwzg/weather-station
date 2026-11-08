#include "timefmt.h"

#include <stdio.h>
#include <stdlib.h>

namespace ws {

int64_t daysFromCivil(int64_t y, unsigned m, unsigned d) {
  y -= m <= 2;
  const int64_t era = (y >= 0 ? y : y - 399) / 400;
  const unsigned yoe = static_cast<unsigned>(y - era * 400);
  const unsigned doy = (153 * (m + (m > 2 ? -3 : 9)) + 2) / 5 + d - 1;
  const unsigned doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
  return era * 146097 + static_cast<int64_t>(doe) - 719468;
}

namespace {
int64_t secondsOf(const struct tm& tm) {
  return daysFromCivil(tm.tm_year + 1900, static_cast<unsigned>(tm.tm_mon + 1), static_cast<unsigned>(tm.tm_mday)) *
             86400 +
         tm.tm_hour * 3600 + tm.tm_min * 60 + tm.tm_sec;
}
}  // namespace

int32_t utcOffsetSeconds(time_t t) {
  struct tm local, utc;
  localtime_r(&t, &local);
  gmtime_r(&t, &utc);
  return static_cast<int32_t>(secondsOf(local) - secondsOf(utc));
}

size_t formatIsoLocal(time_t t, char* buf, size_t len) {
  struct tm local;
  localtime_r(&t, &local);
  const int32_t off = utcOffsetSeconds(t);
  const int32_t absMin = abs(off) / 60;
  int n = snprintf(buf, len, "%04d-%02d-%02dT%02d:%02d:%02d%c%02d:%02d", local.tm_year + 1900, local.tm_mon + 1,
                   local.tm_mday, local.tm_hour, local.tm_min, local.tm_sec, off < 0 ? '-' : '+',
                   static_cast<int>(absMin / 60), static_cast<int>(absMin % 60));
  return (n > 0 && static_cast<size_t>(n) < len) ? static_cast<size_t>(n) : 0;
}

}  // namespace ws
