#pragma once
#include <stddef.h>

namespace ws {

// Median of a few samples (n <= 8); the input is left untouched.
template <typename T>
T median(const T* v, size_t n) {
  T s[8];
  if (n > 8) n = 8;
  for (size_t i = 0; i < n; ++i) {
    T x = v[i];
    size_t j = i;
    for (; j > 0 && s[j - 1] > x; --j) s[j] = s[j - 1];
    s[j] = x;
  }
  return (n % 2) ? s[n / 2] : static_cast<T>((s[n / 2 - 1] + s[n / 2]) / 2);
}

}  // namespace ws
