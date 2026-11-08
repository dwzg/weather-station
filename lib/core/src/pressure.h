#pragma once

namespace ws {

// Saturation-weighted water vapour pressure in hPa (Magnus formula, over water).
double vapourPressureHpa(double tempC, double humidityPct);

// Reduces station pressure to mean sea level with the formula of the German
// Weather Service (DWD), which accounts for the actual air temperature and
// humidity of the column below the station:
//
//   p0 = p * exp(g / (R * (T + Ch*E + a*h/2)) * h)
//
// Compared against reanalysis data at this station it has a third less scatter
// than the standard-atmosphere formula and no seasonal bias (that formula
// assumes 15 degC and drifts ~2 hPa between summer and winter).
double seaLevelPressureDwd(double stationHpa, double tempC, double humidityPct, double altitudeM);

}  // namespace ws
