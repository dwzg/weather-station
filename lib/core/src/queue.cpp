#include "queue.h"

#include <algorithm>

#include "crc.h"

namespace ws {
namespace {

constexpr uint8_t kCursorMagic[4] = {'W', 'S', 'Q', '1'};
constexpr size_t kCursorSize = 14;

void put32(uint8_t* p, uint32_t v) {
  for (int i = 0; i < 4; ++i) p[i] = static_cast<uint8_t>(v >> (8 * i));
}
uint32_t get32(const uint8_t* p) {
  return static_cast<uint32_t>(p[0]) | (static_cast<uint32_t>(p[1]) << 8) |
         (static_cast<uint32_t>(p[2]) << 16) | (static_cast<uint32_t>(p[3]) << 24);
}

}  // namespace

ReadingQueue::ReadingQueue(SegmentStore& store, uint16_t recordsPerSegment, uint16_t maxSegments)
    : store_(store), perSegment_(recordsPerSegment), maxSegments_(maxSegments < 2 ? 2 : maxSegments) {}

bool ReadingQueue::begin() {
  segs_.clear();
  std::vector<uint32_t> ids;
  if (!store_.listSegments(ids)) return false;
  std::sort(ids.begin(), ids.end());
  for (uint32_t id : ids) {
    int32_t size = store_.segmentSize(id);
    if (size < 0) continue;
    uint32_t count = static_cast<uint32_t>(size) / kRecordSize;
    bool torn = (static_cast<uint32_t>(size) % kRecordSize) != 0;
    segs_.push_back({id, count, torn || count >= perSegment_});
    lastId_ = std::max(lastId_, id);
  }
  // Everything but the newest segment is closed for writing.
  for (size_t i = 0; i + 1 < segs_.size(); ++i) segs_[i].sealed = true;

  uint8_t buf[kCursorSize];
  // Cursor layout: magic[4] | segment u32 | index u32 | crc16 over bytes 0..11.
  bool haveCursor = store_.loadCursor(buf, sizeof buf) && std::equal(kCursorMagic, kCursorMagic + 4, buf) &&
                    static_cast<uint16_t>(buf[12] | (buf[13] << 8)) == crc16(buf, 12);
  if (haveCursor) {
    cursorSeg_ = get32(buf + 4);
    cursorIdx_ = get32(buf + 8);
    lastId_ = std::max(lastId_, cursorSeg_);
  }

  // Segments before the cursor were delivered; a crash between saving the
  // cursor and deleting them leaves them behind.
  while (!segs_.empty() && haveCursor && segs_.front().id < cursorSeg_) {
    store_.removeSegment(segs_.front().id);
    segs_.erase(segs_.begin());
  }
  if (segs_.empty()) {
    cursorSeg_ = 0;
    cursorIdx_ = 0;
  } else if (!haveCursor || segs_.front().id != cursorSeg_) {
    // No cursor, or its segment is gone: start with the oldest data there is.
    cursorSeg_ = segs_.front().id;
    cursorIdx_ = 0;
  }
  if (!segs_.empty()) cursorIdx_ = std::min(cursorIdx_, segs_.front().count);
  frontValid_ = false;
  dirty_ = false;
  return true;
}

int ReadingQueue::cursorIndex() const {
  for (size_t i = 0; i < segs_.size(); ++i) {
    if (segs_[i].id == cursorSeg_) return static_cast<int>(i);
  }
  return -1;
}

void ReadingQueue::dropOldest() {
  const Seg oldest = segs_.front();
  if (oldest.id == cursorSeg_) {
    dropped_ += oldest.count - std::min(cursorIdx_, oldest.count);
    cursorSeg_ = segs_.size() > 1 ? segs_[1].id : 0;
    cursorIdx_ = 0;
    dirty_ = true;
  }
  store_.removeSegment(oldest.id);
  segs_.erase(segs_.begin());
  frontValid_ = false;
}

bool ReadingQueue::push(const Reading& r) {
  if (!segs_.empty() && segs_.back().sealed && segs_.back().count == 0) {
    // The newest segment holds nothing usable (its first append failed). Start
    // it over instead of opening another one: empty segments must never count
    // towards the cap and push real readings out.
    store_.removeSegment(segs_.back().id);
    segs_.back().sealed = false;
  } else if (segs_.empty() || segs_.back().sealed || segs_.back().count >= perSegment_) {
    if (!segs_.empty()) segs_.back().sealed = true;
    segs_.push_back({++lastId_, 0, false});
    while (segs_.size() > maxSegments_) dropOldest();
  }
  Seg& seg = segs_.back();
  uint8_t rec[kRecordSize];
  encodeReading(r, rec);
  if (!store_.append(seg.id, rec, sizeof rec)) {
    // The file may now end in a partial record; never append behind it.
    seg.sealed = true;
    return false;
  }
  ++seg.count;
  if (seg.count >= perSegment_) seg.sealed = true;
  if (cursorSeg_ == 0) {
    cursorSeg_ = segs_.front().id;
    cursorIdx_ = 0;
  }
  return true;
}

bool ReadingQueue::front(Reading& out) {
  frontValid_ = false;
  int i = cursorIndex();
  if (i < 0) return false;
  while (true) {
    const Seg& seg = segs_[static_cast<size_t>(i)];
    if (cursorIdx_ >= seg.count) {
      if (static_cast<size_t>(i) + 1 >= segs_.size()) return false;  // caught up
      ++i;
      cursorSeg_ = segs_[static_cast<size_t>(i)].id;
      cursorIdx_ = 0;
      dirty_ = true;
      continue;
    }
    uint8_t rec[kRecordSize];
    if (!store_.read(seg.id, cursorIdx_ * kRecordSize, rec, sizeof rec)) {
      // An I/O error says nothing about the data: stop here and let the caller
      // retry later. Skipping on it would walk the cursor through, and the next
      // commit would delete, the whole backlog. Only a record that keeps
      // failing while what follows reads fine is skipped.
      ++readErrors_;
      if (skipUnreadable(static_cast<size_t>(i))) {
        i = cursorIndex();
        if (i < 0) return false;
        continue;
      }
      return false;
    }
    readFailValid_ = false;
    if (decodeReading(rec, out)) {
      frontValid_ = true;
      frontEpoch_ = out.epoch;
      return true;
    }
    ++corrupt_;  // readable but damaged (torn write, flipped bits): skip just this record
    ++cursorIdx_;
    dirty_ = true;
  }
}

bool ReadingQueue::readable(uint32_t id, uint32_t idx) {
  uint8_t rec[kRecordSize];
  return store_.read(id, idx * kRecordSize, rec, sizeof rec);
}

bool ReadingQueue::skipUnreadable(size_t i) {
  const Seg seg = segs_[i];
  if (readFailValid_ && readFailSeg_ == seg.id && readFailIdx_ == cursorIdx_) {
    ++readFails_;
  } else {
    readFailValid_ = true;
    readFailSeg_ = seg.id;
    readFailIdx_ = cursorIdx_;
    readFails_ = 1;
  }
  if (readFails_ < kReadFailsBeforeSkip) return false;
  uint32_t skipped;
  if (cursorIdx_ + 1 < seg.count && readable(seg.id, cursorIdx_ + 1)) {
    skipped = 1;  // just this record
    ++cursorIdx_;
  } else if (i + 1 < segs_.size() && segs_[i + 1].count > 0 && readable(segs_[i + 1].id, 0)) {
    skipped = seg.count - cursorIdx_;  // the rest of its segment
    cursorSeg_ = segs_[i + 1].id;
    cursorIdx_ = 0;
  } else {
    return false;  // nothing after it reads either: the store is down
  }
  unreadable_ += skipped;
  readFailValid_ = false;
  dirty_ = true;
  return true;
}

bool ReadingQueue::newest(Reading& out) {
  const int ci = cursorIndex();
  if (ci < 0) return false;
  for (int i = static_cast<int>(segs_.size()) - 1; i >= ci; --i) {
    const Seg& seg = segs_[static_cast<size_t>(i)];
    const uint32_t start = i == ci ? cursorIdx_ : 0;
    for (uint32_t idx = seg.count; idx > start; --idx) {
      uint8_t rec[kRecordSize];
      if (!store_.read(seg.id, (idx - 1) * kRecordSize, rec, sizeof rec)) return false;
      if (decodeReading(rec, out)) return true;
    }
  }
  return false;
}

void ReadingQueue::pop() {
  if (!frontValid_) return;
  ++cursorIdx_;
  frontValid_ = false;
  dirty_ = true;
}

bool ReadingQueue::commit() {
  if (!dirty_) return true;
  uint8_t buf[kCursorSize];
  std::copy(kCursorMagic, kCursorMagic + 4, buf);
  put32(buf + 4, cursorSeg_);
  put32(buf + 8, cursorIdx_);
  uint16_t c = crc16(buf, 12);
  buf[12] = static_cast<uint8_t>(c);
  buf[13] = static_cast<uint8_t>(c >> 8);
  if (!store_.saveCursor(buf, sizeof buf)) return false;
  dirty_ = false;
  // Only now that the cursor is durable may delivered segments go.
  while (!segs_.empty() && segs_.front().id < cursorSeg_) {
    store_.removeSegment(segs_.front().id);
    segs_.erase(segs_.begin());
  }
  return true;
}

uint32_t ReadingQueue::pending() const {
  uint32_t n = 0;
  for (const Seg& s : segs_) {
    if (s.id < cursorSeg_) continue;
    n += s.count;
    if (s.id == cursorSeg_) n -= std::min(cursorIdx_, s.count);
  }
  return n;
}

}  // namespace ws
