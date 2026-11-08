#include "pressure.h"

#include <math.h>

namespace ws {
namespace {
constexpr double kG = 9.80665;     // standard gravity, m/s^2
constexpr double kR = 287.05;      // gas constant of dry air, J/(kg K)
constexpr double kLapse = 0.0065;  // standard temperature gradient, K/m
constexpr double kCh = 0.12;       // DWD humidity coefficient, K/hPa
}  // namespace

double vapourPressureHpa(double tempC, double humidityPct) {
  return humidityPct / 100.0 * 6.112 * exp(17.62 * tempC / (243.12 + tempC));
}

double seaLevelPressureDwd(double stationHpa, double tempC, double humidityPct, double altitudeM) {
  const double tK = tempC + 273.15;
  const double th = tK + kCh * vapourPressureHpa(tempC, humidityPct) + kLapse * altitudeM / 2.0;
  return stationHpa * exp(kG / (kR * th) * altitudeM);
}

}  // namespace ws
