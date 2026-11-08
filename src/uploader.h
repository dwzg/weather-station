#pragma once
#include <stdint.h>

#include "backoff.h"
#include "queue.h"
#include "upload.h"

struct UploadStats {
  uint32_t accepted = 0;
  uint32_t rejected = 0;
  uint32_t failedBatches = 0;
  uint32_t lastOkEpoch = 0;  // wall clock of the last accepted reading
  int lastStatus = 0;        // HTTP status or negative HTTPClient/TLS error
  char lastError[64] = "";
};

// Sends the queue, oldest first, to the weather app over HTTPS. Certificates are
// checked against Let's Encrypt's roots; a reading leaves the queue only once
// the app has answered 2xx (or rejected it for good).
class Uploader {
 public:
  void begin(ws::ReadingQueue* q, const ws::Calibration& cal) {
    q_ = q;
    cal_ = cal;
  }
  void loop(bool wifiUp, bool timeValid);
  const UploadStats& stats() const { return st_; }
  uint32_t retryInMs() const;

 private:
  void sendBatch();
  void scheduleRetry(uint32_t ms);

  ws::ReadingQueue* q_ = nullptr;
  ws::Calibration cal_{};
  ws::Backoff backoff_{15000, 10UL * 60 * 1000};
  uint32_t nextMs_ = 0;
  UploadStats st_;
};
