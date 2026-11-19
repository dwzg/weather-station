//! A persistent first-in-first-out queue of readings that survives power loss.
//!
//! Delivery is at-least-once: [`ReadingQueue::pop`] only moves a cursor in RAM,
//! [`ReadingQueue::commit`] makes it durable. A reading popped but not committed
//! before a reset is sent again, which the weather app absorbs because it
//! upserts on the timestamp.
//!
//! Readings live in segments of `records_per_segment` records. Appending never
//! rewrites existing data; a fully delivered segment is deleted on commit. When
//! more than `max_segments` exist the oldest is dropped, so a long outage loses
//! the oldest data rather than the newest, and the flash can never fill up.

use std::collections::{BTreeMap, BTreeSet};

use log::warn;

use crate::crc::crc16;
use crate::reading::{self, Reading, RECORD_SIZE};

/// Where the queue keeps its bytes. The firmware implements this on LittleFS,
/// the tests (and the RAM fallback) in memory. A segment is an append-only file
/// of fixed-size records.
pub trait SegmentStore {
    /// Ids of all segments, in any order. None if the store cannot be listed.
    fn list_segments(&mut self) -> Option<Vec<u32>>;
    /// Size in bytes, None if the segment does not exist.
    fn segment_size(&mut self, id: u32) -> Option<u32>;
    /// Appends to the segment, creating it. False if not everything was written.
    fn append(&mut self, id: u32, data: &[u8]) -> bool;
    fn read(&mut self, id: u32, offset: u32, buf: &mut [u8]) -> bool;
    fn remove_segment(&mut self, id: u32) -> bool;
    /// Fills `buf` exactly; false if there is no cursor of that size.
    fn load_cursor(&mut self, buf: &mut [u8]) -> bool;
    /// Must replace the previous cursor atomically.
    fn save_cursor(&mut self, data: &[u8]) -> bool;
}

impl<T: SegmentStore + ?Sized> SegmentStore for Box<T> {
    fn list_segments(&mut self) -> Option<Vec<u32>> {
        (**self).list_segments()
    }
    fn segment_size(&mut self, id: u32) -> Option<u32> {
        (**self).segment_size(id)
    }
    fn append(&mut self, id: u32, data: &[u8]) -> bool {
        (**self).append(id, data)
    }
    fn read(&mut self, id: u32, offset: u32, buf: &mut [u8]) -> bool {
        (**self).read(id, offset, buf)
    }
    fn remove_segment(&mut self, id: u32) -> bool {
        (**self).remove_segment(id)
    }
    fn load_cursor(&mut self, buf: &mut [u8]) -> bool {
        (**self).load_cursor(buf)
    }
    fn save_cursor(&mut self, data: &[u8]) -> bool {
        (**self).save_cursor(data)
    }
}

const CURSOR_MAGIC: [u8; 4] = *b"WSQ1";
/// Cursor layout: magic[4] | segment u32 | index u32 | crc16 over bytes 0..11.
const CURSOR_SIZE: usize = 14;
/// Reads failing this often in a row at the same record make it a bad spot in
/// the flash (if a later record can be read), not a store that is down.
pub const READ_FAILS_BEFORE_SKIP: u32 = 5;

#[derive(Clone, Copy, Debug)]
struct Seg {
    id: u32,
    /// Whole records.
    count: u32,
    /// No more appends (full, or ends in a torn record).
    sealed: bool,
}

pub struct ReadingQueue<S: SegmentStore> {
    store: S,
    per_segment: u32,
    max_segments: usize,
    /// Ascending ids; the last one is being written.
    segs: Vec<Seg>,
    last_id: u32,
    /// 0 = no segments yet.
    cursor_seg: u32,
    cursor_idx: u32,
    dropped: u32,
    corrupt: u32,
    read_errors: u32,
    unreadable: u32,
    /// Where reads last failed, and how often in a row.
    read_fail_at: Option<(u32, u32)>,
    read_fails: u32,
    front_epoch: u32,
    front_valid: bool,
    dirty: bool,
}

impl<S: SegmentStore> ReadingQueue<S> {
    pub fn new(store: S, records_per_segment: u16, max_segments: u16) -> Self {
        Self {
            store,
            per_segment: u32::from(records_per_segment.max(1)),
            max_segments: usize::from(max_segments.max(2)),
            segs: Vec::new(),
            last_id: 0,
            cursor_seg: 0,
            cursor_idx: 0,
            dropped: 0,
            corrupt: 0,
            read_errors: 0,
            unreadable: 0,
            read_fail_at: None,
            read_fails: 0,
            front_epoch: 0,
            front_valid: false,
            dirty: false,
        }
    }

    pub fn store(&self) -> &S {
        &self.store
    }
    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    /// Reads the segments and the cursor. False if the store cannot be listed.
    pub fn begin(&mut self) -> bool {
        self.segs.clear();
        let Some(mut ids) = self.store.list_segments() else { return false };
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let Some(size) = self.store.segment_size(id) else { continue };
            let count = size / RECORD_SIZE as u32;
            let torn = size % RECORD_SIZE as u32 != 0;
            self.segs.push(Seg { id, count, sealed: torn || count >= self.per_segment });
            self.last_id = self.last_id.max(id);
        }
        // Everything but the newest segment is closed for writing.
        let n = self.segs.len();
        for s in self.segs.iter_mut().take(n.saturating_sub(1)) {
            s.sealed = true;
        }

        let mut buf = [0u8; CURSOR_SIZE];
        let have_cursor = self.store.load_cursor(&mut buf)
            && buf[..4] == CURSOR_MAGIC
            && u16::from_le_bytes([buf[12], buf[13]]) == crc16(&buf[..12]);
        if have_cursor {
            self.cursor_seg = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
            self.cursor_idx = u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
            self.last_id = self.last_id.max(self.cursor_seg);
        }

        // Segments before the cursor were delivered; a crash between saving the
        // cursor and deleting them leaves them behind.
        while have_cursor && self.segs.first().is_some_and(|s| s.id < self.cursor_seg) {
            let id = self.segs.remove(0).id;
            self.store.remove_segment(id);
        }
        match self.segs.first() {
            None => {
                self.cursor_seg = 0;
                self.cursor_idx = 0;
            }
            Some(first) => {
                if !have_cursor || first.id != self.cursor_seg {
                    // No cursor, or its segment is gone: start with the oldest data there is.
                    self.cursor_seg = first.id;
                    self.cursor_idx = 0;
                }
                self.cursor_idx = self.cursor_idx.min(first.count);
            }
        }
        self.front_valid = false;
        self.dirty = false;
        true
    }

    fn cursor_index(&self) -> Option<usize> {
        self.segs.iter().position(|s| s.id == self.cursor_seg)
    }

    fn drop_oldest(&mut self) {
        let oldest = self.segs[0];
        if oldest.id == self.cursor_seg {
            self.dropped += oldest.count - self.cursor_idx.min(oldest.count);
            self.cursor_seg = self.segs.get(1).map_or(0, |s| s.id);
            self.cursor_idx = 0;
            self.dirty = true;
        }
        self.store.remove_segment(oldest.id);
        self.segs.remove(0);
        self.front_valid = false;
    }

    /// Appends a reading. False if the store did not take it.
    pub fn push(&mut self, r: &Reading) -> bool {
        match self.segs.last_mut() {
            Some(last) if last.sealed && last.count == 0 => {
                // The newest segment holds nothing usable (its first append failed).
                // Start it over instead of opening another one: empty segments must
                // never count towards the cap and push real readings out.
                let id = last.id;
                last.sealed = false;
                self.store.remove_segment(id);
            }
            last => {
                if last.as_ref().is_none_or(|s| s.sealed || s.count >= self.per_segment) {
                    if let Some(s) = last {
                        s.sealed = true;
                    }
                    self.last_id += 1;
                    self.segs.push(Seg { id: self.last_id, count: 0, sealed: false });
                    while self.segs.len() > self.max_segments {
                        self.drop_oldest();
                    }
                }
            }
        }
        let rec = reading::encode(r);
        let per_segment = self.per_segment;
        let seg = self.segs.last_mut().expect("a segment to write");
        if !self.store.append(seg.id, &rec) {
            // The file may now end in a partial record; never append behind it.
            seg.sealed = true;
            return false;
        }
        seg.count += 1;
        if seg.count >= per_segment {
            seg.sealed = true;
        }
        if self.cursor_seg == 0 {
            self.cursor_seg = self.segs[0].id;
            self.cursor_idx = 0;
        }
        true
    }

    /// Oldest undelivered reading. Skips records that fail their CRC; returns
    /// None (without skipping anything) when the store cannot be read.
    pub fn front(&mut self) -> Option<Reading> {
        self.front_valid = false;
        let mut i = self.cursor_index()?;
        loop {
            let seg = self.segs[i];
            if self.cursor_idx >= seg.count {
                if i + 1 >= self.segs.len() {
                    return None; // caught up
                }
                i += 1;
                self.cursor_seg = self.segs[i].id;
                self.cursor_idx = 0;
                self.dirty = true;
                continue;
            }
            let mut rec = [0u8; RECORD_SIZE];
            if !self.store.read(seg.id, self.cursor_idx * RECORD_SIZE as u32, &mut rec) {
                // An I/O error says nothing about the data: stop here and let the
                // caller retry later. Skipping on it would walk the cursor through,
                // and the next commit would delete, the whole backlog. Only a
                // record that keeps failing while later ones read fine is skipped.
                self.read_errors += 1;
                if self.skip_unreadable(i) {
                    i = self.cursor_index()?;
                    continue;
                }
                return None;
            }
            self.read_fail_at = None;
            if let Some(r) = reading::decode(&rec) {
                self.front_valid = true;
                self.front_epoch = r.epoch;
                return Some(r);
            }
            // Readable but damaged (torn write, flipped bits): skip just this record.
            self.corrupt += 1;
            self.cursor_idx += 1;
            self.dirty = true;
        }
    }

    fn readable(&mut self, id: u32, idx: u32) -> bool {
        let mut rec = [0u8; RECORD_SIZE];
        self.store.read(id, idx * RECORD_SIZE as u32, &mut rec)
    }

    /// Called when the record at the cursor (in segment index `i`) cannot be
    /// read. After READ_FAILS_BEFORE_SKIP failures in a row there, moves the
    /// cursor past it, if what follows can be read: past the record if the next
    /// one in its segment reads, past the rest of the segment if the next
    /// segment does. If nothing after it reads either, the store itself is down
    /// and nothing is skipped. True if the cursor moved.
    fn skip_unreadable(&mut self, i: usize) -> bool {
        let seg = self.segs[i];
        let here = (seg.id, self.cursor_idx);
        if self.read_fail_at == Some(here) {
            self.read_fails += 1;
        } else {
            self.read_fail_at = Some(here);
            self.read_fails = 1;
        }
        if self.read_fails < READ_FAILS_BEFORE_SKIP {
            return false;
        }
        let skipped = if self.cursor_idx + 1 < seg.count && self.readable(seg.id, self.cursor_idx + 1) {
            self.cursor_idx += 1;
            1
        } else if let Some(next) = self.segs.get(i + 1).copied().filter(|n| n.count > 0) {
            if !self.readable(next.id, 0) {
                return false;
            }
            let n = seg.count - self.cursor_idx;
            self.cursor_seg = next.id;
            self.cursor_idx = 0;
            n
        } else {
            return false;
        };
        warn!("queue: {skipped} unreadable reading(s) in segment {:08x} skipped", seg.id);
        self.unreadable += skipped;
        self.read_fail_at = None;
        self.dirty = true;
        true
    }

    /// The newest pending reading, without moving the cursor or touching what
    /// [`front`](Self::front) returned. None if there is none or it cannot be read.
    pub fn newest(&mut self) -> Option<Reading> {
        let ci = self.cursor_index()?;
        for i in (ci..self.segs.len()).rev() {
            let seg = self.segs[i];
            let start = if i == ci { self.cursor_idx } else { 0 };
            for idx in (start..seg.count).rev() {
                let mut rec = [0u8; RECORD_SIZE];
                if !self.store.read(seg.id, idx * RECORD_SIZE as u32, &mut rec) {
                    return None;
                }
                if let Some(r) = reading::decode(&rec) {
                    return Some(r);
                }
            }
        }
        None
    }

    /// Marks the reading [`front`](Self::front) returned as delivered.
    pub fn pop(&mut self) {
        if !self.front_valid {
            return;
        }
        self.cursor_idx += 1;
        self.front_valid = false;
        self.dirty = true;
    }

    /// Persists the cursor and deletes delivered segments.
    pub fn commit(&mut self) -> bool {
        if !self.dirty {
            return true;
        }
        let mut buf = [0u8; CURSOR_SIZE];
        buf[..4].copy_from_slice(&CURSOR_MAGIC);
        buf[4..8].copy_from_slice(&self.cursor_seg.to_le_bytes());
        buf[8..12].copy_from_slice(&self.cursor_idx.to_le_bytes());
        let c = crc16(&buf[..12]);
        buf[12..14].copy_from_slice(&c.to_le_bytes());
        if !self.store.save_cursor(&buf) {
            return false;
        }
        self.dirty = false;
        // Only now that the cursor is durable may delivered segments go.
        while self.segs.first().is_some_and(|s| s.id < self.cursor_seg) {
            let id = self.segs.remove(0).id;
            self.store.remove_segment(id);
        }
        true
    }

    pub fn pending(&self) -> u32 {
        self.segs
            .iter()
            .filter(|s| s.id >= self.cursor_seg)
            .map(|s| if s.id == self.cursor_seg { s.count - self.cursor_idx.min(s.count) } else { s.count })
            .sum()
    }
    /// Readings lost because the queue was full.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }
    pub fn corrupt(&self) -> u32 {
        self.corrupt
    }
    pub fn read_errors(&self) -> u32 {
        self.read_errors
    }
    /// Readings skipped because their place in the flash could not be read.
    pub fn unreadable(&self) -> u32 {
        self.unreadable
    }
    pub fn segments(&self) -> usize {
        self.segs.len()
    }
    /// Epoch of the last `front()`, 0 if none.
    pub fn oldest_epoch_hint(&self) -> u32 {
        self.front_epoch
    }
}

/// Segments in RAM. The queue's fallback when the flash file system is unusable
/// (short outages are still bridged while the station runs, not across a
/// reset), and the tests' store, with fault injection.
#[derive(Default)]
pub struct MemStore {
    pub segs: BTreeMap<u32, Vec<u8>>,
    pub cursor: Vec<u8>,
    /// Keep the cursor (tests "reboot" with it); the RAM fallback has nothing to keep it for.
    pub persist_cursor: bool,
    pub fail_append: bool,
    pub fail_read: bool,
    /// (segment, byte offset) of records whose reads fail.
    pub fail_read_at: BTreeSet<(u32, u32)>,
    /// If > 0: the next append writes only this many bytes and fails.
    pub tear_append_at: usize,
}

impl MemStore {
    /// A store that keeps its cursor, as the flash does.
    pub fn persistent() -> Self {
        Self { persist_cursor: true, ..Self::default() }
    }
}

impl SegmentStore for MemStore {
    fn list_segments(&mut self) -> Option<Vec<u32>> {
        Some(self.segs.keys().copied().collect())
    }
    fn segment_size(&mut self, id: u32) -> Option<u32> {
        self.segs.get(&id).map(|v| v.len() as u32)
    }
    fn append(&mut self, id: u32, data: &[u8]) -> bool {
        if self.fail_append {
            return false;
        }
        let v = self.segs.entry(id).or_default();
        if self.tear_append_at > 0 {
            v.extend_from_slice(&data[..self.tear_append_at.min(data.len())]);
            self.tear_append_at = 0;
            return false;
        }
        v.extend_from_slice(data);
        true
    }
    fn read(&mut self, id: u32, offset: u32, buf: &mut [u8]) -> bool {
        if self.fail_read || self.fail_read_at.contains(&(id, offset)) {
            return false;
        }
        let Some(v) = self.segs.get(&id) else { return false };
        let start = offset as usize;
        match v.get(start..start + buf.len()) {
            Some(src) => {
                buf.copy_from_slice(src);
                true
            }
            None => false,
        }
    }
    fn remove_segment(&mut self, id: u32) -> bool {
        self.segs.remove(&id).is_some()
    }
    fn load_cursor(&mut self, buf: &mut [u8]) -> bool {
        if !self.persist_cursor || self.cursor.len() != buf.len() {
            return false;
        }
        buf.copy_from_slice(&self.cursor);
        true
    }
    fn save_cursor(&mut self, data: &[u8]) -> bool {
        if self.persist_cursor {
            self.cursor = data.to_vec();
        }
        true
    }
}
