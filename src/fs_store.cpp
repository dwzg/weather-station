#include "fs_store.h"

#include <LittleFS.h>
#include <stdlib.h>

#include "log.h"

namespace {
void segPath(uint32_t id, char* buf) { snprintf(buf, 20, "/q/%08lx", static_cast<unsigned long>(id)); }
constexpr char kCursor[] = "/q/cursor";
constexpr char kCursorTmp[] = "/q/cursor.tmp";
}  // namespace

bool LittleFsStore::begin() {
  LittleFSConfig c;
  c.setAutoFormat(false);
  LittleFS.setConfig(c);
  if (!LittleFS.begin()) {
    logf("fs: mount failed, formatting");
    if (!LittleFS.format() || !LittleFS.begin()) {
      logf("fs: format failed");
      return false;
    }
  }
  if (!LittleFS.exists("/q")) LittleFS.mkdir("/q");
  return true;
}

bool LittleFsStore::listSegments(std::vector<uint32_t>& ids) {
  Dir d = LittleFS.openDir("/q");
  while (d.next()) {
    const String name = d.fileName();
    if (name.length() != 8) continue;  // skips cursor files
    char* end = nullptr;
    unsigned long id = strtoul(name.c_str(), &end, 16);
    if (end && *end == '\0' && id > 0) ids.push_back(static_cast<uint32_t>(id));
  }
  return true;
}

int32_t LittleFsStore::segmentSize(uint32_t id) {
  char p[20];
  segPath(id, p);
  File f = LittleFS.open(p, "r");
  if (!f) return -1;
  int32_t s = static_cast<int32_t>(f.size());
  f.close();
  return s;
}

bool LittleFsStore::append(uint32_t id, const uint8_t* data, size_t len) {
  char p[20];
  segPath(id, p);
  File f = LittleFS.open(p, "a");
  if (!f) return false;
  size_t n = f.write(data, len);
  f.close();  // LittleFS commits on close
  return n == len;
}

bool LittleFsStore::read(uint32_t id, uint32_t offset, uint8_t* data, size_t len) {
  char p[20];
  segPath(id, p);
  File f = LittleFS.open(p, "r");
  if (!f) return false;
  bool ok = f.seek(offset) && static_cast<size_t>(f.read(data, len)) == len;
  f.close();
  return ok;
}

bool LittleFsStore::removeSegment(uint32_t id) {
  char p[20];
  segPath(id, p);
  return LittleFS.remove(p);
}

bool LittleFsStore::loadCursor(uint8_t* data, size_t len) {
  File f = LittleFS.open(kCursor, "r");
  if (!f) return false;
  bool ok = f.size() == len && static_cast<size_t>(f.read(data, len)) == len;
  f.close();
  return ok;
}

bool LittleFsStore::saveCursor(const uint8_t* data, size_t len) {
  File f = LittleFS.open(kCursorTmp, "w");
  if (!f) return false;
  bool ok = f.write(data, len) == len;
  f.close();
  return ok && LittleFS.rename(kCursorTmp, kCursor);
}

void LittleFsStore::usage(size_t& used, size_t& total) {
  FSInfo i;
  if (LittleFS.info(i)) {
    used = i.usedBytes;
    total = i.totalBytes;
  } else {
    used = total = 0;
  }
}
