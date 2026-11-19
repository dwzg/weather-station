#pragma once
#include <stdint.h>

#include "backoff.h"
#include "queue.h"
#include "upload.h"

class HTTPClient;
namespace BearSSL {
class WiFiClientSecure;
}

struct UploadStats {
  uint32_t accepted = 0;
  uint32_t rejected = 0;
  uint32_t failedBatches = 0;
  uint32_t lastOkEpoch = 0;   // wall clock of the last accepted reading
  int lastStatus = 0;         // HTTP status or negative HTTPClient error
  uint32_t minHeapInTls = 0;  // lowest free heap seen while a connection was open
  uint32_t tlsStackMax = 0;   // deepest use of the BearSSL stack, bytes
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
  void pause(uint32_t ms);
  // After the app refused a reading: what does it say about the newest one?
  enum class Probe { AppWorks, AppRefuses, NoAnswer };
  Probe probe(HTTPClient& http, BearSSL::WiFiClientSecure& client, const ws::Reading& refused);

  ws::ReadingQueue* q_ = nullptr;
  ws::Calibration cal_{};
  ws::Backoff backoff_{15000, 10UL * 60 * 1000};
  bool paused_ = false;      // elapsed-time form, safe across the 49-day millis() wrap
  uint32_t pausedAtMs_ = 0;
  uint32_t pauseMs_ = 0;
  UploadStats st_;
};
