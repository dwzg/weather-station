#pragma once
#include <Print.h>

// printf-style logging to the serial port and a small RAM ring shown at /log.
void logf(const char* fmt, ...) __attribute__((format(printf, 1, 2)));
void logDump(Print& out);
