#include "uploader.h"

#include <ESP8266HTTPClient.h>
#include <ESP8266WiFi.h>
#include <StackThunk.h>
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
// The slowest single request (DNS, connect, handshake, headers, body, each with
// its own timeout) takes about a minute. Never start one later than this before
// the next slot, so an upload cannot push a measurement out of its window.
constexpr uint32_t kStopBeforeSlotS = 70;
constexpr uint32_t kStartAfterSlotS = 3;

uint32_t secondsToNextSlot() {
  const time_t now = time(nullptr);
  return cfg::kSlotS - static_cast<uint32_t>(now % cfg::kSlotS);
}
bool inUploadWindow() {
  const uint32_t s = secondsToNextSlot();
  return s >= kStopBeforeSlotS && s <= cfg::kSlotS - kStartAfterSlotS;
}
}  // namespace

uint32_t Uploader::retryInMs() const {
  if (!paused_) return 0;
  const uint32_t gone = millis() - pausedAtMs_;
  return gone < pauseMs_ ? pauseMs_ - gone : 0;
}

void Uploader::pause(uint32_t ms) {
  paused_ = true;
  pausedAtMs_ = millis();
  pauseMs_ = ms;
}

void Uploader::loop(bool wifiUp, bool timeValid) {
  if (!q_ || !wifiUp || !timeValid || q_->pending() == 0) return;
  if (paused_) {
    if (millis() - pausedAtMs_ < pauseMs_) return;
    paused_ = false;
  }
  if (!inUploadWindow()) return;
  sendBatch();
}

Uploader::Probe Uploader::probe(HTTPClient& http, BearSSL::WiFiClientSecure& client, const ws::Reading& refused) {
  // The app upserts on the timestamp, so sending the newest reading early (it
  // is sent again in its turn) does no harm.
  ws::Reading newest;
  if (!q_->newest(newest) || newest.epoch == refused.epoch) {
    snprintf(st_.lastError, sizeof st_.lastError, "app refused the newest reading; waiting for a newer one");
    return Probe::NoAnswer;
  }
  char body[192];
  if (!ws::buildBody(newest, cal_, body, sizeof body) || !http.begin(client, API_URL)) {
    snprintf(st_.lastError, sizeof st_.lastError, "cannot ask the app about the newest reading");
    return Probe::NoAnswer;
  }
  http.addHeader(F("Content-Type"), F("application/json"));
  http.addHeader(F("X-API-Key"), F(API_KEY));
  const int code = http.POST(reinterpret_cast<uint8_t*>(body), strlen(body));
  st_.lastStatus = code;
  if (code > 0) http.getString();  // read it all, so the connection can carry on
  switch (ws::classifyResponse(code)) {
    case ws::Outcome::Accepted:
      st_.lastOkEpoch = static_cast<uint32_t>(time(nullptr));
      return Probe::AppWorks;
    case ws::Outcome::Retry:
      if (code > 0) {
        snprintf(st_.lastError, sizeof st_.lastError, "HTTP %d", code);
      } else {
        snprintf(st_.lastError, sizeof st_.lastError, "%s", HTTPClient::errorToString(code).c_str());
      }
      return Probe::NoAnswer;
    default:
      return Probe::AppRefuses;
  }
}

void Uploader::sendBatch() {
  // Built per batch so the parsed roots do not sit on the heap between uploads.
  BearSSL::X509List anchors(kTrustAnchors);
  // The TLS client needs ~22 KB on top (8.5 KB buffers, ~6.5 KB contexts, the
  // 6 KB BearSSL stack). Its x509 context aborts if it cannot be allocated,
  // so check before trying rather than crash.
  if (ESP.getFreeHeap() < 26000 || ESP.getMaxFreeBlockSize() < 10000) {
    snprintf(st_.lastError, sizeof st_.lastError, "low memory (free %lu, block %lu)",
             static_cast<unsigned long>(ESP.getFreeHeap()), static_cast<unsigned long>(ESP.getMaxFreeBlockSize()));
    logf("upload: %s", st_.lastError);
    pause(backoff_.next(os_random()));
    return;
  }

  BearSSL::WiFiClientSecure client;
  client.setTrustAnchors(&anchors);
  // The server's largest TLS record is the certificate chain, 4.3 KB (measured
  // 2026-11-08); responses are a few hundred bytes. 8 KB halves the buffer.
  client.setBufferSizes(8192, 1024);
  client.setX509Time(time(nullptr));
  client.setTimeout(cfg::kHttpTimeoutMs);

  HTTPClient http;
  http.setReuse(true);
  http.setTimeout(cfg::kHttpTimeoutMs);
  http.setUserAgent(F("weather-station/" FW_VERSION));

  const uint32_t started = millis();
  uint32_t sent = 0;
  bool failed = false;
  bool halted = false;
  ws::Reading r;
  while (sent < cfg::kBatchMax && millis() - started < cfg::kBatchBudgetMs && inUploadWindow() && q_->front(r)) {
    char body[192];
    if (!ws::buildBody(r, cal_, body, sizeof body)) {
      logf("upload: cannot encode reading %lu, dropping it", static_cast<unsigned long>(r.epoch));
      q_->pop();
      ++st_.rejected;
      continue;
    }
    if (!http.begin(client, API_URL)) {
      snprintf(st_.lastError, sizeof st_.lastError, "bad API_URL");
      halted = true;
      break;
    }
    http.addHeader(F("Content-Type"), F("application/json"));
    http.addHeader(F("X-API-Key"), F(API_KEY));
    const int code = http.POST(reinterpret_cast<uint8_t*>(body), strlen(body));
    st_.lastStatus = code;
    const uint32_t heap = ESP.getFreeHeap();
    if (!st_.minHeapInTls || heap < st_.minHeapInTls) st_.minHeapInTls = heap;
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
        // Bad data, or a broken app (or a firmware sending a body the app no
        // longer takes)? Dropping is right only for bad data: ask about another.
        switch (probe(http, client, r)) {
          case Probe::AppWorks:
            logf("upload: app rejected %.80s -> %d %.40s; it takes the newest reading, so this one goes", body, code,
                 resp.c_str());
            q_->pop();
            ++st_.rejected;
            continue;
          case Probe::AppRefuses:
            snprintf(st_.lastError, sizeof st_.lastError, "HTTP %d %.24s (the newest reading too)", code, resp.c_str());
            halted = true;
            break;
          case Probe::NoAnswer:  // lastError says why
            failed = true;
            break;
        }
        break;
      case ws::Outcome::Halt:
        snprintf(st_.lastError, sizeof st_.lastError, "HTTP %d %.40s", code, resp.c_str());
        halted = true;
        break;
      case ws::Outcome::Retry:
        if (code > 0) {
          snprintf(st_.lastError, sizeof st_.lastError, "HTTP %d", code);
        } else {
          char tls[40] = "";
          const int tlsErr = client.getLastSSLError(tls, sizeof tls);
          if (tlsErr) {
            snprintf(st_.lastError, sizeof st_.lastError, "TLS %d: %s", tlsErr, tls);
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
  st_.tlsStackMax = stack_thunk_get_max_usage();

  if (halted) {
    ++st_.failedBatches;
    pause(cfg::kHaltMs);
    logf("upload: %s; needs a fix (URL, key or app), pausing %lu min", st_.lastError,
         static_cast<unsigned long>(cfg::kHaltMs / 60000));
  } else if (failed) {
    ++st_.failedBatches;
    const uint32_t wait = backoff_.next(os_random());
    pause(wait);
    logf("upload: %s after %lu sent, %lu pending, retry in %lus", st_.lastError, static_cast<unsigned long>(sent),
         static_cast<unsigned long>(q_->pending()), static_cast<unsigned long>(wait / 1000));
  } else if (sent == 0 && q_->pending() > 0) {
    // front() refused without a network error: the flash could not be read.
    snprintf(st_.lastError, sizeof st_.lastError, "queue read error (%lu)",
             static_cast<unsigned long>(q_->readErrors()));
    pause(backoff_.next(os_random()));
  } else {
    backoff_.reset();
    if (sent > 1 || q_->pending() > 0) {
      logf("upload: %lu sent in %lums, %lu pending", static_cast<unsigned long>(sent),
           static_cast<unsigned long>(millis() - started), static_cast<unsigned long>(q_->pending()));
    }
  }
}
