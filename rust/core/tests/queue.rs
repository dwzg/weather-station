//! The queue tests of the ESP8266 firmware (test/test_core), ported one to one.

use ws_core::queue::{MemStore, ReadingQueue};
use ws_core::reading::{Reading, RECORD_SIZE};

fn mk(epoch: u32) -> Reading {
    Reading { epoch, temp_centi: (epoch % 1000) as i16 - 300, hum_centi: 5000, station_deci_pa: 992_000, source: 0 }
}

fn drain(q: &mut ReadingQueue<MemStore>, max: u32) -> Vec<u32> {
    let mut got = Vec::new();
    while (got.len() as u32) < max {
        let Some(r) = q.front() else { break };
        got.push(r.epoch);
        q.pop();
    }
    q.commit();
    got
}

fn all(q: &mut ReadingQueue<MemStore>) -> Vec<u32> {
    drain(q, u32::MAX)
}

/// The store survives, the queue object does not: a reset.
fn reboot(q: ReadingQueue<MemStore>, per: u16, max: u16) -> ReadingQueue<MemStore> {
    let mut q = ReadingQueue::new(into_store(q), per, max);
    assert!(q.begin());
    q
}

fn into_store(mut q: ReadingQueue<MemStore>) -> MemStore {
    std::mem::take(q.store_mut())
}

fn new(per: u16, max: u16) -> ReadingQueue<MemStore> {
    let mut q = ReadingQueue::new(MemStore::persistent(), per, max);
    assert!(q.begin());
    q
}

#[test]
fn fifo_and_persistence() {
    let mut q = new(4, 10);
    for i in 1..=10 {
        assert!(q.push(&mk(i * 300)));
    }
    assert_eq!(q.pending(), 10);
    assert_eq!(drain(&mut q, 3).len(), 3);
    assert_eq!(q.pending(), 7);
    let mut q = reboot(q, 4, 10);
    assert_eq!(q.pending(), 7);
    let got = all(&mut q);
    assert_eq!(got, (4..=10).map(|i| i * 300).collect::<Vec<_>>());
    assert_eq!(q.pending(), 0);
    assert_eq!(q.store().segs.len(), 1, "delivered segments deleted, write segment kept");
}

#[test]
fn uncommitted_pops_are_redelivered() {
    let mut q = new(4, 10);
    for i in 1..=5 {
        q.push(&mk(i * 300));
    }
    q.front();
    q.pop();
    q.front();
    q.pop(); // no commit: power is lost here
    let mut q = reboot(q, 4, 10);
    assert_eq!(q.pending(), 5);
    assert_eq!(q.front().unwrap().epoch, 300);
}

#[test]
fn cap_drops_oldest() {
    let mut q = new(2, 3); // at most 6 readings
    for i in 1..=9 {
        q.push(&mk(i * 300));
    }
    assert_eq!(q.segments(), 3);
    assert_eq!(q.pending(), 5, "the newest segment holds 1, two full ones hold 4");
    assert_eq!(q.dropped(), 4);
    let got = all(&mut q);
    assert_eq!((got[0], *got.last().unwrap()), (5 * 300, 9 * 300));
}

#[test]
fn skips_corrupt_record() {
    let mut q = new(10, 10);
    for i in 1..=3 {
        q.push(&mk(i * 300));
    }
    q.store_mut().segs.values_mut().next().unwrap()[RECORD_SIZE + 2] ^= 0xFF; // damage the second record
    assert_eq!(all(&mut q), [300, 900]);
    assert_eq!(q.corrupt(), 1);
}

#[test]
fn torn_append_starts_new_segment() {
    let mut q = new(10, 10);
    q.push(&mk(300));
    q.store_mut().tear_append_at = 7; // power fails halfway through writing
    assert!(!q.push(&mk(600)));
    assert!(q.push(&mk(900)));
    let mut q = reboot(q, 10, 10);
    q.push(&mk(1200));
    assert_eq!(all(&mut q), [300, 900, 1200]);
}

#[test]
fn bad_cursor_starts_from_oldest() {
    let mut q = new(4, 10);
    for i in 1..=6 {
        q.push(&mk(i * 300));
    }
    drain(&mut q, 5);
    let mut s = into_store(q);
    s.cursor[5] ^= 0xFF;
    let mut q = ReadingQueue::new(s, 4, 10);
    q.begin();
    assert_eq!(q.front().unwrap().epoch, 5 * 300, "first segment was deleted; resend the rest");
}

#[test]
fn push_after_full_drain_and_reboot() {
    let mut q = new(4, 10);
    for i in 1..=4 {
        q.push(&mk(i * 300));
    }
    all(&mut q);
    let mut q = reboot(q, 4, 10);
    assert_eq!(q.pending(), 0);
    q.push(&mk(5 * 300));
    assert_eq!(q.front().unwrap().epoch, 5 * 300);
}

#[test]
fn failed_append_is_reported() {
    let mut q = new(4, 10);
    q.store_mut().fail_append = true;
    assert!(!q.push(&mk(300)));
    q.store_mut().fail_append = false;
    assert!(q.push(&mk(600)));
    assert_eq!(q.pending(), 1);
}

#[test]
fn read_error_keeps_backlog() {
    let mut q = new(4, 10);
    for i in 1..=12 {
        q.push(&mk(i * 300));
    }
    q.store_mut().fail_read = true;
    assert!(q.front().is_none());
    q.commit();
    assert_eq!(q.pending(), 12);
    assert_eq!(q.corrupt(), 0);
    assert_eq!(q.store().segs.len(), 3);
    q.store_mut().fail_read = false;
    assert_eq!(all(&mut q).len(), 12);
}

#[test]
fn repeated_write_failures_keep_data() {
    let mut q = new(4, 3);
    for i in 1..=6 {
        q.push(&mk(i * 300));
    }
    q.store_mut().fail_append = true;
    for i in 0..100 {
        assert!(!q.push(&mk(9000 + i)));
    }
    q.store_mut().fail_append = false;
    assert_eq!(q.pending(), 6);
    assert_eq!(q.dropped(), 0);
    assert!(q.push(&mk(99_000)));
    let got = all(&mut q);
    assert_eq!(got.len(), 7);
    assert_eq!((got[0], got[6]), (300, 99_000));
}

#[test]
fn torn_first_append_reuses_segment() {
    let mut q = new(4, 10);
    for i in 1..=4 {
        q.push(&mk(i * 300)); // fills segment 1
    }
    q.store_mut().tear_append_at = 5;
    assert!(!q.push(&mk(1500))); // segment 2 now holds 5 garbage bytes
    assert!(q.push(&mk(1800))); // segment 2 is started over
    assert_eq!(q.store().segs.len(), 2);
    let got = all(&mut q);
    assert_eq!(got.len(), 5);
    assert_eq!(*got.last().unwrap(), 1800);
    assert_eq!(q.corrupt(), 0);
}

#[test]
fn ram_fallback_keeps_no_cursor() {
    // The RAM store (no persist_cursor) still delivers in order while running.
    let mut q = ReadingQueue::new(MemStore::default(), 36, 4);
    assert!(q.begin());
    for i in 1..=200 {
        q.push(&mk(i * 300));
    }
    assert_eq!(q.pending(), 36 * 3 + 200 % 36);
    let got = all(&mut q);
    assert_eq!(*got.last().unwrap(), 200 * 300);
}

// ---- unreadable records and newest() (not in the C++ suite before) ----------------------------

use ws_core::queue::READ_FAILS_BEFORE_SKIP;

/// Calls front() until it returns something, at most `tries` times.
fn front_within(q: &mut ReadingQueue<MemStore>, tries: u32) -> (Option<u32>, u32) {
    for n in 1..=tries {
        if let Some(r) = q.front() {
            return (Some(r.epoch), n);
        }
    }
    (None, tries)
}

#[test]
fn record_that_never_reads_is_skipped_after_retries() {
    let mut q = new(4, 10);
    for i in 1..=6 {
        q.push(&mk(i * 300));
    }
    q.store_mut().fail_read_at.insert((1, RECORD_SIZE as u32)); // the second record
    assert_eq!(drain(&mut q, 1), [300]);
    // The first failures say nothing; the record is skipped only once the next one reads.
    assert_eq!(front_within(&mut q, 10), (Some(900), READ_FAILS_BEFORE_SKIP));
    assert_eq!(q.unreadable(), 1);
    assert_eq!(all(&mut q), [900, 1200, 1500, 1800]);
}

#[test]
fn unreadable_segment_is_skipped_when_the_next_reads() {
    let mut q = new(4, 10);
    for i in 1..=6 {
        q.push(&mk(i * 300));
    }
    for k in 0..4 {
        q.store_mut().fail_read_at.insert((1, k * RECORD_SIZE as u32));
    }
    assert_eq!(front_within(&mut q, 10), (Some(1500), READ_FAILS_BEFORE_SKIP));
    assert_eq!(q.unreadable(), 4);
    assert_eq!(all(&mut q), [1500, 1800]);
    assert!(!q.store().segs.contains_key(&1), "the bad segment is deleted with the commit");
}

#[test]
fn store_that_cannot_read_anything_loses_nothing() {
    let mut q = new(4, 10);
    for i in 1..=12 {
        q.push(&mk(i * 300));
    }
    q.store_mut().fail_read = true;
    assert_eq!(front_within(&mut q, 100).0, None);
    q.commit();
    assert_eq!((q.pending(), q.unreadable()), (12, 0));
    q.store_mut().fail_read = false;
    assert_eq!(all(&mut q).len(), 12);
}

#[test]
fn newest_reading_leaves_the_cursor_alone() {
    let mut q = new(4, 10);
    assert!(q.newest().is_none());
    for i in 1..=6 {
        q.push(&mk(i * 300));
    }
    let f = q.front().unwrap();
    assert_eq!(q.newest().unwrap().epoch, 1800);
    q.pop(); // still pops what front() returned
    assert_eq!(q.front().unwrap().epoch, 600);
    assert_eq!(f.epoch, 300);
    // A damaged last record: the one before it.
    let last = q.store_mut().segs.get_mut(&2).unwrap();
    let n = last.len();
    last[n - 3] ^= 0xFF;
    assert_eq!(q.newest().unwrap().epoch, 1500);
    // Only pending readings count.
    all(&mut q);
    assert!(q.newest().is_none());
}
