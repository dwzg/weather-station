#pragma once
#include "queue.h"

// The queue's segments as files /q/<id> on LittleFS; the cursor in /q/cursor,
// replaced atomically by writing a temporary file and renaming it.
class LittleFsStore : public ws::SegmentStore {
 public:
  bool begin();  // mounts, formats an unreadable file system
  bool listSegments(std::vector<uint32_t>& ids) override;
  int32_t segmentSize(uint32_t id) override;
  bool append(uint32_t id, const uint8_t* data, size_t len) override;
  bool read(uint32_t id, uint32_t offset, uint8_t* data, size_t len) override;
  bool removeSegment(uint32_t id) override;
  bool loadCursor(uint8_t* data, size_t len) override;
  bool saveCursor(const uint8_t* data, size_t len) override;
  void usage(size_t& used, size_t& total);
};
