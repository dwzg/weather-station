#include "uploader.h"

#include <ESP8266HTTPClient.h>
#include <ESP8266WiFi.h>
#include <WiFiClientSecureBearSSL.h>
#include <time.h>

#include "config.h"
#include "log.h"
#include "secrets.h"
#include "trust_anchors.h"

#ifndef FW_VERSION
#define FW_VERSION "dev"
#endif

namespace {
BearSSL::Session tlsSession;  // resumption makes every connection after the first cheap

uint32_t secondsToNextSlot() {
  const time_t now = time(nullptr);
  return cfg::kSlotS - static_cast<uint32_t>(now % cfg::kSlotS);
}
}  // namespace

uint32_t Uploader::retryInMs() const {
  const int32_t d = static_cast<int32_t>(nextMs_ - millis());
  return d > 0 ? static_cast<uint32_t>(d) : 0;
}

void Uploader::scheduleRetry(uint32_t ms) { nextMs_ = millis() + ms; }

void Uploader::loop(bool wifiUp, bool timeValid) {
  if (!q_ || !wifiUp || !timeValid || q_->pending() == 0) return;
  if (static_cast<int32_t>(millis() - nextMs_) < 0) return;
  if (secondsToNextSlot() < 5 || secondsToNextSlot() > cfg::kSlotS - 3) return;  // keep slot boundaries free
  sendBatch();
}

void Uploader::sendBatch() {
  if (ESP.getMaxFreeBlockSize() < cfg::kMinHeapBlockForTls) {
    snprintf(st_.lastError, sizeof st_.lastError, "heap too fragmented (%lu)",
             static_cast<unsigned long>(ESP.getMaxFreeBlockSize()));
    logf("upload: %s", st_.lastError);
    scheduleRetry(backoff_.next(os_random()));
    return;
  }

  // Built per batch so the parsed roots do not sit on the heap between uploads.
  BearSSL::X509List anchors(kTrustAnchors);
  BearSSL::WiFiClientSecure client;
  client.setTrustAnchors(&anchors);
  client.setSession(&tlsSession);
  client.setBufferSizes(16384, 1024);  // the server does not do max-fragment negotiation
  client.setX509Time(time(nullptr));
  client.setTimeout(cfg::kHttpTimeoutMs);

  HTTPClient http;
  http.setReuse(true);
  http.setTimeout(cfg::kHttpTimeoutMs);
  http.setUserAgent(F("weather-station/" FW_VERSION));

  const uint32_t started = millis();
  uint32_t sent = 0;
  bool failed = false;
  ws::Reading r;
  while (sent < cfg::kBatchMax && millis() - started < cfg::kBatchBudgetMs && secondsToNextSlot() >= 5 &&
         q_->front(r)) {
    char body[192];
    if (!ws::buildBody(r, cal_, body, sizeof body)) {
      logf("upload: cannot encode reading %lu, dropping it", static_cast<unsigned long>(r.epoch));
      q_->pop();
      ++st_.rejected;
      continue;
    }
    if (!http.begin(client, API_URL)) {
      snprintf(st_.lastError, sizeof st_.lastError, "bad API_URL");
      failed = true;
      break;
    }
    http.addHeader(F("Content-Type"), F("application/json"));
    http.addHeader(F("X-API-Key"), F(API_KEY));
    const int code = http.POST(reinterpret_cast<uint8_t*>(body), strlen(body));
    st_.lastStatus = code;
    String resp = code > 0 ? http.getString() : String();

    switch (ws::classifyResponse(code)) {
      case ws::Outcome::Accepted:
        q_->pop();
        ++sent;
        ++st_.accepted;
        st_.lastOkEpoch = static_cast<uint32_t>(time(nullptr));
        st_.lastError[0] = '\0';
        continue;
      case ws::Outcome::Rejected:
        logf("upload: app rejected %s -> %d %.60s", body, code, resp.c_str());
        q_->pop();
        ++st_.rejected;
        continue;
      case ws::Outcome::Halt:
        snprintf(st_.lastError, sizeof st_.lastError, "HTTP %d, check API_URL/API_KEY", code);
        logf("upload: %s; pausing %lu min", st_.lastError, static_cast<unsigned long>(cfg::kHaltMs / 60000));
        q_->commit();
        http.end();
        scheduleRetry(cfg::kHaltMs);
        ++st_.failedBatches;
        return;
      case ws::Outcome::Retry:
        if (code > 0) {
          snprintf(st_.lastError, sizeof st_.lastError, "HTTP %d", code);
        } else {
          char tls[48] = "";
          const int tlsErr = client.getLastSSLError(tls, sizeof tls);
          if (tlsErr) {
            snprintf(st_.lastError, sizeof st_.lastError, "TLS %d: %.40s", tlsErr, tls);
          } else {
            snprintf(st_.lastError, sizeof st_.lastError, "%s", HTTPClient::errorToString(code).c_str());
          }
        }
        failed = true;
        break;
    }
    break;
  }
  http.end();
  client.stop();
  q_->commit();

  if (failed) {
    ++st_.failedBatches;
    const uint32_t wait = backoff_.next(os_random());
    scheduleRetry(wait);
    logf("upload: %s after %lu sent, %lu pending, retry in %lus", st_.lastError, static_cast<unsigned long>(sent),
         static_cast<unsigned long>(q_->pending()), static_cast<unsigned long>(wait / 1000));
  } else {
    backoff_.reset();
    if (sent > 1 || q_->pending() > 0) {
      logf("upload: %lu sent in %lums, %lu pending", static_cast<unsigned long>(sent),
           static_cast<unsigned long>(millis() - started), static_cast<unsigned long>(q_->pending()));
    }
  }
}
