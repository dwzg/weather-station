#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unity.h>

#include <map>
#include <string>
#include <vector>

#include "backoff.h"
#include "crc.h"
#include "median.h"
#include "pressure.h"
#include "queue.h"
#include "reading.h"
#include "sht4x.h"
#include "timefmt.h"
#include "upload.h"

using namespace ws;

// ---- in-memory store with fault injection -------------------------------------------------

struct MemStore : SegmentStore {
  std::map<uint32_t, std::vector<uint8_t>> segs;
  std::vector<uint8_t> cursor;
  bool failAppend = false;
  size_t tearAppendAt = 0;  // if > 0: the next append writes only this many bytes and fails

  bool listSegments(std::vector<uint32_t>& ids) override {
    for (auto& kv : segs) ids.push_back(kv.first);
    return true;
  }
  int32_t segmentSize(uint32_t id) override {
    auto it = segs.find(id);
    return it == segs.end() ? -1 : static_cast<int32_t>(it->second.size());
  }
  bool append(uint32_t id, const uint8_t* d, size_t n) override {
    if (failAppend) return false;
    auto& v = segs[id];
    if (tearAppendAt) {
      v.insert(v.end(), d, d + tearAppendAt);
      tearAppendAt = 0;
      return false;
    }
    v.insert(v.end(), d, d + n);
    return true;
  }
  bool read(uint32_t id, uint32_t off, uint8_t* d, size_t n) override {
    auto it = segs.find(id);
    if (it == segs.end() || off + n > it->second.size()) return false;
    memcpy(d, it->second.data() + off, n);
    return true;
  }
  bool removeSegment(uint32_t id) override { return segs.erase(id) > 0; }
  bool loadCursor(uint8_t* d, size_t n) override {
    if (cursor.size() != n) return false;
    memcpy(d, cursor.data(), n);
    return true;
  }
  bool saveCursor(const uint8_t* d, size_t n) override {
    cursor.assign(d, d + n);
    return true;
  }
};

static Reading mk(uint32_t epoch) {
  Reading r;
  r.epoch = epoch;
  r.tempCenti = static_cast<int16_t>(epoch % 1000) - 300;
  r.humCenti = 5000;
  r.stationDeciPa = 9920000;
  return r;
}

static uint32_t drain(ReadingQueue& q, std::vector<uint32_t>* got = nullptr, uint32_t max = 1u << 30) {
  uint32_t n = 0;
  Reading r;
  while (n < max && q.front(r)) {
    if (got) got->push_back(r.epoch);
    q.pop();
    ++n;
  }
  q.commit();
  return n;
}

void setUp() {
  setenv("TZ", "CET-1CEST,M3.5.0,M10.5.0/3", 1);
  tzset();
}
void tearDown() {}

// ---- record ---------------------------------------------------------------------------

void test_record_roundtrip_and_corruption() {
  Reading r = mk(1794128400), out;
  r.tempCenti = -1234;
  r.source = kSourceSht4x;
  uint8_t b[kRecordSize];
  encodeReading(r, b);
  TEST_ASSERT_TRUE(decodeReading(b, out));
  TEST_ASSERT_EQUAL_UINT32(r.epoch, out.epoch);
  TEST_ASSERT_EQUAL_INT16(-1234, out.tempCenti);
  TEST_ASSERT_EQUAL_UINT8(kSourceSht4x, out.source);
  b[5] ^= 0x01;
  TEST_ASSERT_FALSE(decodeReading(b, out));
  uint8_t zero[kRecordSize] = {0};
  TEST_ASSERT_FALSE(decodeReading(zero, out));
}

void test_crc_reference_values() {
  const uint8_t s[] = {'1', '2', '3', '4', '5', '6', '7', '8', '9'};
  TEST_ASSERT_EQUAL_HEX16(0x29B1, crc16(s, 9));        // CRC-16/CCITT-FALSE check value
  TEST_ASSERT_EQUAL_HEX32(0xCBF43926, crc32(s, 9));    // CRC-32 check value
  const uint8_t beef[] = {0xBE, 0xEF};
  TEST_ASSERT_EQUAL_HEX8(0x92, crc8Sensirion(beef, 2));  // example from the Sensirion datasheet
}

// ---- queue ----------------------------------------------------------------------------

void test_queue_fifo_and_persistence() {
  MemStore s;
  {
    ReadingQueue q(s, 4, 10);
    TEST_ASSERT_TRUE(q.begin());
    for (uint32_t i = 1; i <= 10; ++i) TEST_ASSERT_TRUE(q.push(mk(i * 300)));
    TEST_ASSERT_EQUAL_UINT32(10, q.pending());
    TEST_ASSERT_EQUAL_UINT32(3, drain(q, nullptr, 3));
    TEST_ASSERT_EQUAL_UINT32(7, q.pending());
  }
  ReadingQueue q(s, 4, 10);  // "reboot"
  TEST_ASSERT_TRUE(q.begin());
  TEST_ASSERT_EQUAL_UINT32(7, q.pending());
  std::vector<uint32_t> got;
  TEST_ASSERT_EQUAL_UINT32(7, drain(q, &got));
  for (size_t i = 0; i < got.size(); ++i) TEST_ASSERT_EQUAL_UINT32((i + 4) * 300, got[i]);
  TEST_ASSERT_EQUAL_UINT32(0, q.pending());
  TEST_ASSERT_EQUAL(1, (int)s.segs.size());  // delivered segments deleted, write segment kept
}

void test_queue_uncommitted_pops_are_redelivered() {
  MemStore s;
  {
    ReadingQueue q(s, 4, 10);
    q.begin();
    for (uint32_t i = 1; i <= 5; ++i) q.push(mk(i * 300));
    Reading r;
    q.front(r);
    q.pop();
    q.front(r);
    q.pop();  // no commit: power is lost here
  }
  ReadingQueue q(s, 4, 10);
  q.begin();
  TEST_ASSERT_EQUAL_UINT32(5, q.pending());
  Reading r;
  TEST_ASSERT_TRUE(q.front(r));
  TEST_ASSERT_EQUAL_UINT32(300, r.epoch);
}

void test_queue_cap_drops_oldest() {
  MemStore s;
  ReadingQueue q(s, 2, 3);  // at most 6 readings
  q.begin();
  for (uint32_t i = 1; i <= 9; ++i) q.push(mk(i * 300));
  TEST_ASSERT_EQUAL(3, (int)q.segments());
  TEST_ASSERT_EQUAL_UINT32(5, q.pending());  // the newest segment holds 1, two full ones hold 4
  TEST_ASSERT_EQUAL_UINT32(4, q.dropped());
  std::vector<uint32_t> got;
  drain(q, &got);
  TEST_ASSERT_EQUAL_UINT32(5 * 300, got.front());
  TEST_ASSERT_EQUAL_UINT32(9 * 300, got.back());
}

void test_queue_skips_corrupt_record() {
  MemStore s;
  ReadingQueue q(s, 10, 10);
  q.begin();
  for (uint32_t i = 1; i <= 3; ++i) q.push(mk(i * 300));
  s.segs.begin()->second[kRecordSize + 2] ^= 0xFF;  // damage the second record
  std::vector<uint32_t> got;
  drain(q, &got);
  TEST_ASSERT_EQUAL(2, (int)got.size());
  TEST_ASSERT_EQUAL_UINT32(300, got[0]);
  TEST_ASSERT_EQUAL_UINT32(900, got[1]);
  TEST_ASSERT_EQUAL_UINT32(1, q.corrupt());
}

void test_queue_torn_append_starts_new_segment() {
  MemStore s;
  {
    ReadingQueue q(s, 10, 10);
    q.begin();
    q.push(mk(300));
    s.tearAppendAt = 7;  // power fails halfway through writing
    TEST_ASSERT_FALSE(q.push(mk(600)));
    TEST_ASSERT_TRUE(q.push(mk(900)));
  }
  ReadingQueue q(s, 10, 10);
  q.begin();
  q.push(mk(1200));
  std::vector<uint32_t> got;
  drain(q, &got);
  TEST_ASSERT_EQUAL(3, (int)got.size());
  TEST_ASSERT_EQUAL_UINT32(300, got[0]);
  TEST_ASSERT_EQUAL_UINT32(900, got[1]);
  TEST_ASSERT_EQUAL_UINT32(1200, got[2]);
}

void test_queue_bad_cursor_starts_from_oldest() {
  MemStore s;
  {
    ReadingQueue q(s, 4, 10);
    q.begin();
    for (uint32_t i = 1; i <= 6; ++i) q.push(mk(i * 300));
    drain(q, nullptr, 5);
  }
  s.cursor[5] ^= 0xFF;
  ReadingQueue q(s, 4, 10);
  q.begin();
  Reading r;
  TEST_ASSERT_TRUE(q.front(r));
  TEST_ASSERT_EQUAL_UINT32(5 * 300, r.epoch);  // first segment was deleted; resend the rest
}

void test_queue_push_after_full_drain_and_reboot() {
  MemStore s;
  {
    ReadingQueue q(s, 4, 10);
    q.begin();
    for (uint32_t i = 1; i <= 4; ++i) q.push(mk(i * 300));
    drain(q);
  }
  ReadingQueue q(s, 4, 10);
  q.begin();
  TEST_ASSERT_EQUAL_UINT32(0, q.pending());
  q.push(mk(5 * 300));
  Reading r;
  TEST_ASSERT_TRUE(q.front(r));
  TEST_ASSERT_EQUAL_UINT32(5 * 300, r.epoch);
}

void test_queue_failed_append_is_reported() {
  MemStore s;
  ReadingQueue q(s, 4, 10);
  q.begin();
  s.failAppend = true;
  TEST_ASSERT_FALSE(q.push(mk(300)));
  s.failAppend = false;
  TEST_ASSERT_TRUE(q.push(mk(600)));
  TEST_ASSERT_EQUAL_UINT32(1, q.pending());
}

// ---- time -----------------------------------------------------------------------------

void test_iso_local_with_offset() {
  char b[40];
  TEST_ASSERT_TRUE(formatIsoLocal(1794128400, b, sizeof b) > 0);  // 2026-11-08 09:00:00Z
  TEST_ASSERT_EQUAL_STRING("2026-11-08T10:00:00+01:00", b);
  formatIsoLocal(1784538000, b, sizeof b);  // 2026-07-20 09:00:00Z
  TEST_ASSERT_EQUAL_STRING("2026-07-20T11:00:00+02:00", b);
}

void test_iso_dst_fold_is_unambiguous() {
  char b[40];
  formatIsoLocal(1792888200, b, sizeof b);  // 2026-10-25 00:30Z, first 02:30
  TEST_ASSERT_EQUAL_STRING("2026-10-25T02:30:00+02:00", b);
  formatIsoLocal(1792891800, b, sizeof b);  // 2026-10-25 01:30Z, second 02:30
  TEST_ASSERT_EQUAL_STRING("2026-10-25T02:30:00+01:00", b);
}

void test_days_from_civil() {
  TEST_ASSERT_EQUAL_INT64(0, daysFromCivil(1970, 1, 1));
  TEST_ASSERT_EQUAL_INT64(20765, daysFromCivil(2026, 11, 8));
}

// ---- pressure and body ----------------------------------------------------------------

void test_pressure_matches_reference_implementation() {
  // Values from the Python calibration script (pressure_lib.msl_dwd).
  TEST_ASSERT_DOUBLE_WITHIN(0.005, 1028.3828, seaLevelPressureDwd(992.0, 7.3, 100.0, 298.0));
  TEST_ASSERT_DOUBLE_WITHIN(0.005, 1007.0917, seaLevelPressureDwd(973.6, 25.0, 50.0, 298.0));
  TEST_ASSERT_DOUBLE_WITHIN(1e-9, 1013.25, seaLevelPressureDwd(1013.25, 15.0, 50.0, 0.0));
}

void test_body() {
  Reading r = mk(1794128400);
  r.tempCenti = -5;
  r.humCenti = 10000;
  r.stationDeciPa = 9920000;  // 992.000 hPa
  Calibration cal{298.0, -3.2};
  char b[160];
  TEST_ASSERT_TRUE(buildBody(r, cal, b, sizeof b) > 0);
  char expect[160];
  char p[16];
  formatCenti(seaLevelCentiHpa(r, cal), p, sizeof p);
  snprintf(expect, sizeof expect,
           "{\"timestamp\":\"2026-11-08T10:00:00+01:00\",\"temperature\":-0.05,\"humidity\":100.00,\"pressure\":%s}", p);
  TEST_ASSERT_EQUAL_STRING(expect, b);
  TEST_ASSERT_EQUAL(0, (int)buildBody(r, cal, b, 20));  // too small: refuse, never truncate
}

void test_format_centi() {
  char b[16];
  formatCenti(0, b, sizeof b);
  TEST_ASSERT_EQUAL_STRING("0.00", b);
  formatCenti(-1, b, sizeof b);
  TEST_ASSERT_EQUAL_STRING("-0.01", b);
  formatCenti(102543, b, sizeof b);
  TEST_ASSERT_EQUAL_STRING("1025.43", b);
  formatCenti(-32768, b, sizeof b);
  TEST_ASSERT_EQUAL_STRING("-327.68", b);
}

void test_classify() {
  TEST_ASSERT_TRUE(classifyResponse(200) == Outcome::Accepted);
  TEST_ASSERT_TRUE(classifyResponse(201) == Outcome::Accepted);
  TEST_ASSERT_TRUE(classifyResponse(422) == Outcome::Rejected);
  TEST_ASSERT_TRUE(classifyResponse(401) == Outcome::Halt);
  TEST_ASSERT_TRUE(classifyResponse(404) == Outcome::Halt);
  TEST_ASSERT_TRUE(classifyResponse(429) == Outcome::Retry);
  TEST_ASSERT_TRUE(classifyResponse(502) == Outcome::Retry);
  TEST_ASSERT_TRUE(classifyResponse(-1) == Outcome::Retry);
}

// ---- misc -----------------------------------------------------------------------------

void test_backoff_grows_and_caps() {
  Backoff b(1000, 8000);
  uint32_t d1 = b.next(0), d2 = b.next(0), d3 = b.next(0), d4 = b.next(0), d5 = b.next(0);
  TEST_ASSERT_EQUAL_UINT32(750, d1);
  TEST_ASSERT_EQUAL_UINT32(1500, d2);
  TEST_ASSERT_EQUAL_UINT32(3000, d3);
  TEST_ASSERT_EQUAL_UINT32(6000, d4);
  TEST_ASSERT_EQUAL_UINT32(6000, d5);
  TEST_ASSERT_TRUE(b.next(0xFFFFFFFF) < 10000);
  b.reset();
  TEST_ASSERT_EQUAL_UINT32(750, b.next(0));
}

void test_median() {
  int a[] = {5, 1, 9};
  TEST_ASSERT_EQUAL_INT(5, median(a, 3));
  int b[] = {4, 1, 3, 2};
  TEST_ASSERT_EQUAL_INT(2, median(b, 4));
  TEST_ASSERT_EQUAL_INT(4, b[0]);  // input untouched
}

void test_sht4x_decode() {
  uint8_t f[6] = {0x66, 0x66, 0, 0xCC, 0xCC, 0};
  f[2] = crc8Sensirion(f, 2);
  f[5] = crc8Sensirion(f + 3, 2);
  int32_t t, h;
  TEST_ASSERT_TRUE(decodeSht4x(f, t, h));
  TEST_ASSERT_EQUAL_INT32(2500, t);  // -45 + 175 * 0.4
  TEST_ASSERT_EQUAL_INT32(9400, h);  // -6 + 125 * 0.8
  f[5] ^= 1;
  TEST_ASSERT_FALSE(decodeSht4x(f, t, h));
  uint8_t g[6] = {0, 0, 0, 0xFF, 0xFF, 0};
  g[2] = crc8Sensirion(g, 2);
  g[5] = crc8Sensirion(g + 3, 2);
  decodeSht4x(g, t, h);
  TEST_ASSERT_EQUAL_INT32(10000, h);  // clamped
}

int main() {
  UNITY_BEGIN();
  RUN_TEST(test_record_roundtrip_and_corruption);
  RUN_TEST(test_crc_reference_values);
  RUN_TEST(test_queue_fifo_and_persistence);
  RUN_TEST(test_queue_uncommitted_pops_are_redelivered);
  RUN_TEST(test_queue_cap_drops_oldest);
  RUN_TEST(test_queue_skips_corrupt_record);
  RUN_TEST(test_queue_torn_append_starts_new_segment);
  RUN_TEST(test_queue_bad_cursor_starts_from_oldest);
  RUN_TEST(test_queue_push_after_full_drain_and_reboot);
  RUN_TEST(test_queue_failed_append_is_reported);
  RUN_TEST(test_iso_local_with_offset);
  RUN_TEST(test_iso_dst_fold_is_unambiguous);
  RUN_TEST(test_days_from_civil);
  RUN_TEST(test_pressure_matches_reference_implementation);
  RUN_TEST(test_body);
  RUN_TEST(test_format_centi);
  RUN_TEST(test_classify);
  RUN_TEST(test_backoff_grows_and_caps);
  RUN_TEST(test_median);
  RUN_TEST(test_sht4x_decode);
  return UNITY_END();
}
