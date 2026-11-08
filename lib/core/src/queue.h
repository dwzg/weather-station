#pragma once
#include <stddef.h>
#include <stdint.h>

#include <vector>

#include "reading.h"

namespace ws {

// Where the queue keeps its bytes. The firmware implements this on LittleFS,
// the tests in memory. A segment is an append-only file of fixed-size records.
class SegmentStore {
 public:
  virtual ~SegmentStore() = default;
  virtual bool listSegments(std::vector<uint32_t>& ids) = 0;
  virtual int32_t segmentSize(uint32_t id) = 0;  // bytes, or -1 if it does not exist
  virtual bool append(uint32_t id, const uint8_t* data, size_t len) = 0;  // creates the segment
  virtual bool read(uint32_t id, uint32_t offset, uint8_t* data, size_t len) = 0;
  virtual bool removeSegment(uint32_t id) = 0;
  virtual bool loadCursor(uint8_t* data, size_t len) = 0;
  virtual bool saveCursor(const uint8_t* data, size_t len) = 0;  // must replace atomically
};

// A persistent first-in-first-out queue of readings that survives power loss.
//
// Delivery is at-least-once: pop() only moves a cursor in RAM, commit() makes it
// durable. A reading popped but not committed before a reset is sent again,
// which the weather app absorbs because it upserts on the timestamp.
//
// Readings live in segments of `recordsPerSegment` records. Appending never
// rewrites existing data; a fully delivered segment is deleted on commit. When
// more than `maxSegments` exist the oldest is dropped, so a long outage loses
// the oldest data rather than the newest, and the flash can never fill up.
class ReadingQueue {
 public:
  ReadingQueue(SegmentStore& store, uint16_t recordsPerSegment, uint16_t maxSegments);

  bool begin();
  bool push(const Reading& r);
  // Oldest undelivered reading. Skips records that fail their CRC; returns
  // false (without skipping anything) when the store cannot be read.
  bool front(Reading& out);
  void pop();                // marks the reading front() returned as delivered
  bool commit();             // persists the cursor and deletes delivered segments

  uint32_t pending() const;
  uint32_t dropped() const { return dropped_; }
  uint32_t corrupt() const { return corrupt_; }
  uint32_t readErrors() const { return readErrors_; }
  size_t segments() const { return segs_.size(); }
  uint32_t oldestEpochHint() const { return frontEpoch_; }  // epoch of the last front(), 0 if none

 private:
  struct Seg {
    uint32_t id;
    uint32_t count;  // whole records
    bool sealed;     // no more appends (full, or ends in a torn record)
  };

  int cursorIndex() const;  // index into segs_ of the cursor segment, -1 if none
  void dropOldest();

  SegmentStore& store_;
  const uint16_t perSegment_;
  const uint16_t maxSegments_;
  std::vector<Seg> segs_;  // ascending ids; back() is the one being written
  uint32_t lastId_ = 0;
  uint32_t cursorSeg_ = 0;  // 0 = no segments yet
  uint32_t cursorIdx_ = 0;
  uint32_t dropped_ = 0;
  uint32_t corrupt_ = 0;
  uint32_t readErrors_ = 0;
  uint32_t frontEpoch_ = 0;
  bool frontValid_ = false;
  bool dirty_ = false;
};

}  // namespace ws
