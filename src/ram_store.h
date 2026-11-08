#pragma once
#include <map>
#include <vector>

#include "queue.h"

// Fallback when the flash file system is unusable: the queue lives in RAM, so
// short outages are still bridged while the station runs (not across a reset).
class RamStore : public ws::SegmentStore {
 public:
  bool listSegments(std::vector<uint32_t>& ids) override {
    for (auto& kv : segs_) ids.push_back(kv.first);
    return true;
  }
  int32_t segmentSize(uint32_t id) override {
    auto it = segs_.find(id);
    return it == segs_.end() ? -1 : static_cast<int32_t>(it->second.size());
  }
  bool append(uint32_t id, const uint8_t* d, size_t n) override {
    auto& v = segs_[id];
    v.insert(v.end(), d, d + n);
    return true;
  }
  bool read(uint32_t id, uint32_t off, uint8_t* d, size_t n) override {
    auto it = segs_.find(id);
    if (it == segs_.end() || off + n > it->second.size()) return false;
    memcpy(d, it->second.data() + off, n);
    return true;
  }
  bool removeSegment(uint32_t id) override { return segs_.erase(id) > 0; }
  bool loadCursor(uint8_t*, size_t) override { return false; }
  bool saveCursor(const uint8_t*, size_t) override { return true; }

 private:
  std::map<uint32_t, std::vector<uint8_t>> segs_;
};
