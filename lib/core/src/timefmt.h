#pragma once
#include <stddef.h>
#include <stdint.h>
#include <time.h>

namespace ws {

// Local time minus UTC at instant `t`, in seconds, using the process time zone (TZ).
int32_t utcOffsetSeconds(time_t t);

// ISO-8601 local time with an explicit offset, e.g. "2026-11-08T10:00:00+01:00".
// The offset is what lets the weather app tell the two 02:30s of the autumn
// DST change apart. Returns the length written, 0 if the buffer is too small.
size_t formatIsoLocal(time_t t, char* buf, size_t len);

// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
int64_t daysFromCivil(int64_t y, unsigned m, unsigned d);

}  // namespace ws
