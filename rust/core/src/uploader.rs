//! Sends the queue, oldest first, to the weather app. A reading leaves the
//! queue only once the app has answered 2xx (or rejected it for good).
//!
//! The HTTP client is behind [`Transport`], so the policy (batches, the upload
//! window, what each answer means, backoff) is tested on the host.

use log::{info, warn};

use crate::backoff::Backoff;
use crate::queue::{ReadingQueue, SegmentStore};
use crate::timefmt::TimeZone;
use crate::upload::{build_body, classify_response, Calibration, Outcome};

/// An HTTP answer.
#[derive(Clone, Debug, Default)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// One connection to the app (kept alive across the requests of a batch).
pub trait Transport {
    /// POSTs one JSON body. Err with a short description when there is no HTTP
    /// answer at all (DNS, TCP, TLS, timeout).
    fn post(&mut self, body: &str) -> Result<Response, String>;
}

impl<T: Transport + ?Sized> Transport for &mut T {
    fn post(&mut self, body: &str) -> Result<Response, String> {
        (**self).post(body)
    }
}

/// The clock and randomness the uploader needs.
pub trait Env {
    /// Monotonic milliseconds since boot.
    fn now_ms(&self) -> u64;
    /// Wall clock, seconds since 1970 (only called while the clock is valid).
    fn epoch(&self) -> u32;
    fn random(&mut self) -> u32;
}

#[derive(Clone, Debug)]
pub struct UploadConfig {
    pub cal: Calibration,
    pub tz: TimeZone,
    pub slot_s: u32,
    /// Readings per connection...
    pub batch_max: u32,
    /// ...and at most this long.
    pub batch_budget_ms: u64,
    pub retry_min_ms: u32,
    pub retry_max_ms: u32,
    /// After 3xx/401/403/404/...: someone has to fix the config.
    pub halt_ms: u32,
    /// The slowest single request (DNS, connect, handshake, headers, body, each
    /// with its own timeout) takes about a minute. Never start one later than
    /// this before the next slot, so an upload cannot push a measurement out of
    /// its window.
    pub stop_before_slot_s: u32,
    pub start_after_slot_s: u32,
    /// After this many rejected readings in a row the app is more likely broken
    /// than the readings: stop dropping them and wait for a person.
    pub rejects_before_halt: u8,
}

#[derive(Clone, Debug, Default)]
pub struct UploadStats {
    pub accepted: u32,
    pub rejected: u32,
    pub failed_batches: u32,
    /// Wall clock of the last accepted reading.
    pub last_ok_epoch: u32,
    /// HTTP status of the last request, 0 if it got no answer.
    pub last_status: u16,
    pub last_error: String,
    pub last_batch_ms: u64,
}

pub struct Uploader {
    cfg: UploadConfig,
    backoff: Backoff,
    paused_until_ms: Option<u64>,
    rejects_in_a_row: u8,
    stats: UploadStats,
}

fn clip(s: &str, n: usize) -> &str {
    s.char_indices().nth(n).map_or(s, |(i, _)| &s[..i])
}

impl Uploader {
    pub fn new(cfg: UploadConfig) -> Self {
        let backoff = Backoff::new(cfg.retry_min_ms, cfg.retry_max_ms);
        Self { cfg, backoff, paused_until_ms: None, rejects_in_a_row: 0, stats: UploadStats::default() }
    }

    pub fn stats(&self) -> &UploadStats {
        &self.stats
    }

    pub fn retry_in_ms(&self, now_ms: u64) -> u64 {
        self.paused_until_ms.map_or(0, |t| t.saturating_sub(now_ms))
    }

    fn pause(&mut self, now_ms: u64, ms: u32) {
        self.paused_until_ms = Some(now_ms + u64::from(ms));
    }

    /// True between `start_after_slot_s` after a slot and `stop_before_slot_s` before the next.
    pub fn in_window(&self, epoch: u32) -> bool {
        let to_next = self.cfg.slot_s - epoch % self.cfg.slot_s;
        to_next >= self.cfg.stop_before_slot_s && to_next <= self.cfg.slot_s - self.cfg.start_after_slot_s
    }

    /// Whether [`poll`](Self::poll) would send now. Lets the caller skip setting
    /// up a connection.
    pub fn due<S: SegmentStore>(&self, q: &ReadingQueue<S>, ready: bool, env: &impl Env) -> bool {
        ready
            && q.pending() > 0
            && self.paused_until_ms.is_none_or(|t| env.now_ms() >= t)
            && self.in_window(env.epoch())
    }

    /// Sends a batch if one is due. `ready`: network up and clock valid.
    /// `connect` sets up the transport for one batch.
    pub fn poll<S, T, F>(&mut self, q: &mut ReadingQueue<S>, ready: bool, env: &mut impl Env, connect: F)
    where
        S: SegmentStore,
        T: Transport,
        F: FnOnce() -> Result<T, String>,
    {
        if !self.due(q, ready, env) {
            return;
        }
        self.paused_until_ms = None;
        let started = env.now_ms();
        match connect() {
            Ok(mut t) => self.send_batch(q, env, &mut t, started),
            Err(e) => {
                self.stats.failed_batches += 1;
                let wait = self.backoff.next(env.random());
                self.pause(started, wait);
                warn!("upload: {e}, retry in {}s", wait / 1000);
                self.stats.last_error = e;
            }
        }
    }

    fn send_batch<S: SegmentStore, T: Transport>(
        &mut self,
        q: &mut ReadingQueue<S>,
        env: &mut impl Env,
        t: &mut T,
        started: u64,
    ) {
        let mut sent = 0u32;
        let mut failed = false;
        let mut halted = false;
        let mut read_failed = false;
        while sent < self.cfg.batch_max
            && env.now_ms() - started < self.cfg.batch_budget_ms
            && self.in_window(env.epoch())
        {
            let Some(r) = q.front() else {
                read_failed = q.pending() > 0; // front() refuses without skipping on a read error
                break;
            };
            let body = build_body(&r, &self.cfg.cal, &self.cfg.tz);
            let resp = match t.post(&body) {
                Ok(resp) => resp,
                Err(e) => {
                    self.stats.last_status = 0;
                    self.stats.last_error = e;
                    failed = true;
                    break;
                }
            };
            self.stats.last_status = resp.status;
            let mut outcome = classify_response(i32::from(resp.status));
            if outcome == Outcome::Rejected && self.rejects_in_a_row + 1 >= self.cfg.rejects_before_halt {
                outcome = Outcome::Halt; // keep this one and everything behind it
            }
            match outcome {
                Outcome::Accepted => {
                    q.pop();
                    sent += 1;
                    self.stats.accepted += 1;
                    self.rejects_in_a_row = 0;
                    self.stats.last_ok_epoch = env.epoch();
                    self.stats.last_error.clear();
                }
                Outcome::Rejected => {
                    warn!("upload: app rejected {} -> {} {}", clip(&body, 90), resp.status, clip(&resp.body, 60));
                    q.pop();
                    self.stats.rejected += 1;
                    self.rejects_in_a_row += 1;
                }
                Outcome::Halt => {
                    self.stats.last_error =
                        format!("HTTP {} {}", resp.status, clip(&resp.body, 40)).trim_end().to_string();
                    halted = true;
                    break;
                }
                Outcome::Retry => {
                    self.stats.last_error = format!("HTTP {}", resp.status);
                    failed = true;
                    break;
                }
            }
        }
        if !q.commit() {
            warn!("upload: could not save the queue cursor (readings will be sent again)");
        }
        let now = env.now_ms();
        self.stats.last_batch_ms = now - started;

        if halted {
            self.stats.failed_batches += 1;
            self.pause(now, self.cfg.halt_ms);
            warn!(
                "upload: {}; needs a fix (URL, key or app), pausing {} min",
                self.stats.last_error,
                self.cfg.halt_ms / 60_000
            );
        } else if failed {
            self.stats.failed_batches += 1;
            let wait = self.backoff.next(env.random());
            self.pause(now, wait);
            warn!(
                "upload: {} after {sent} sent, {} pending, retry in {}s",
                self.stats.last_error,
                q.pending(),
                wait / 1000
            );
        } else if read_failed {
            self.stats.last_error = format!("queue read error ({})", q.read_errors());
            let wait = self.backoff.next(env.random());
            self.pause(now, wait);
            warn!("upload: {}, retry in {}s", self.stats.last_error, wait / 1000);
        } else {
            self.backoff.reset();
            if sent > 1 || q.pending() > 0 {
                info!("upload: {sent} sent in {}ms, {} pending", now - started, q.pending());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;
    use crate::queue::MemStore;
    use crate::reading::Reading;

    /// Starts 10 s into a slot; every request takes `step_ms`.
    struct FakeEnv {
        ms: Rc<Cell<u64>>,
        epoch0: u32,
    }
    impl Env for FakeEnv {
        fn now_ms(&self) -> u64 {
            self.ms.get()
        }
        fn epoch(&self) -> u32 {
            self.epoch0 + (self.ms.get() / 1000) as u32
        }
        fn random(&mut self) -> u32 {
            0
        }
    }

    struct FakeTransport {
        answers: Vec<Result<u16, String>>,
        bodies: Vec<String>,
        ms: Rc<Cell<u64>>,
        step_ms: u64,
    }
    impl Transport for FakeTransport {
        fn post(&mut self, body: &str) -> Result<Response, String> {
            self.ms.set(self.ms.get() + self.step_ms);
            self.bodies.push(body.to_string());
            let a = if self.answers.is_empty() { Ok(200) } else { self.answers.remove(0) };
            a.map(|status| Response { status, body: String::new() })
        }
    }

    fn cfg() -> UploadConfig {
        UploadConfig {
            cal: Calibration { altitude_m: 298.0, pressure_offset_hpa: -3.2 },
            tz: TimeZone::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap(),
            slot_s: 300,
            batch_max: 40,
            batch_budget_ms: 25_000,
            retry_min_ms: 15_000,
            retry_max_ms: 600_000,
            halt_ms: 1_800_000,
            stop_before_slot_s: 70,
            start_after_slot_s: 3,
            rejects_before_halt: 3,
        }
    }

    const SLOT: u32 = 1_794_128_400;

    fn setup(n: u32) -> (Uploader, ReadingQueue<MemStore>, FakeEnv, Rc<Cell<u64>>) {
        let mut q = ReadingQueue::new(MemStore::persistent(), 288, 60);
        assert!(q.begin());
        for i in 0..n {
            q.push(&Reading {
                epoch: SLOT - 300 * (n - i),
                temp_centi: 700,
                hum_centi: 8000,
                station_deci_pa: 990_000,
                source: 0,
            });
        }
        let ms = Rc::new(Cell::new(0));
        (Uploader::new(cfg()), q, FakeEnv { ms: ms.clone(), epoch0: SLOT + 10 }, ms)
    }

    fn transport(ms: &Rc<Cell<u64>>, answers: Vec<Result<u16, String>>) -> FakeTransport {
        FakeTransport { answers, bodies: Vec::new(), ms: ms.clone(), step_ms: 100 }
    }

    #[test]
    fn sends_oldest_first_and_stops_at_batch_max() {
        let (mut u, mut q, mut env, ms) = setup(50);
        let mut t = transport(&ms, vec![]);
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert_eq!(t.bodies.len(), 40);
        assert!(t.bodies[0].contains("\"timestamp\":\"2026-11-08T05:50:00+01:00\""), "{}", t.bodies[0]);
        assert_eq!(q.pending(), 10);
        assert_eq!(u.stats().accepted, 40);
        // Delivered readings are durable: a "reboot" resumes behind them.
        let store = std::mem::take(q.store_mut());
        let mut q2 = ReadingQueue::new(store, 288, 60);
        q2.begin();
        assert_eq!(q2.pending(), 10);
    }

    #[test]
    fn nothing_without_network_or_outside_the_window() {
        let (mut u, mut q, mut env, ms) = setup(3);
        let mut t = transport(&ms, vec![]);
        u.poll(&mut q, false, &mut env, || Ok(&mut t));
        assert!(t.bodies.is_empty());
        env.epoch0 = SLOT + 300 - 60; // a minute before the next measurement
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert!(t.bodies.is_empty());
        env.epoch0 = SLOT + 1; // right at the measurement
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert!(t.bodies.is_empty());
        assert_eq!(q.pending(), 3);
    }

    #[test]
    fn batch_ends_when_the_window_closes() {
        let (mut u, mut q, mut env, ms) = setup(40);
        env.epoch0 = SLOT + 300 - 75; // requests may start until 70 s before the slot
        let mut t = transport(&ms, vec![]);
        t.step_ms = 1000;
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert_eq!(t.bodies.len(), 6);
    }

    #[test]
    fn retry_keeps_the_reading_and_backs_off() {
        let (mut u, mut q, mut env, ms) = setup(3);
        let mut t = transport(&ms, vec![Ok(200), Ok(503)]);
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert_eq!(q.pending(), 2);
        assert_eq!(u.stats().last_error, "HTTP 503");
        assert_eq!(u.retry_in_ms(ms.get()), 11_250); // 15 s - 25 % (random = 0)
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert_eq!(t.bodies.len(), 2, "paused");
        ms.set(ms.get() + 11_250);
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert_eq!(q.pending(), 0);
        assert!(t.bodies[1] == t.bodies[2], "the failed reading is sent again");
        assert!(u.stats().last_error.is_empty());
    }

    #[test]
    fn transport_error_is_a_retry() {
        let (mut u, mut q, mut env, ms) = setup(2);
        let mut t = transport(&ms, vec![Err("TLS handshake failed".into())]);
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert_eq!(q.pending(), 2);
        assert_eq!(u.stats().last_status, 0);
        assert_eq!(u.stats().failed_batches, 1);
        assert!(u.retry_in_ms(ms.get()) > 0);
        let (mut u, mut q, mut env, _) = setup(2);
        u.poll::<_, &mut FakeTransport, _>(&mut q, true, &mut env, || Err("out of memory".into()));
        assert_eq!(u.stats().last_error, "out of memory");
        assert!(u.retry_in_ms(0) > 0);
    }

    #[test]
    fn halt_keeps_everything_and_waits_long() {
        let (mut u, mut q, mut env, ms) = setup(3);
        let mut t = transport(&ms, vec![Ok(401)]);
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert_eq!(q.pending(), 3);
        assert_eq!(u.retry_in_ms(ms.get()), 1_800_000);
        assert_eq!(u.stats().last_error, "HTTP 401");
    }

    #[test]
    fn rejected_readings_are_dropped_until_three_in_a_row() {
        let (mut u, mut q, mut env, ms) = setup(6);
        let mut t = transport(&ms, vec![Ok(422), Ok(200), Ok(422), Ok(400), Ok(422)]);
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        // 422 dropped, 200 sent, 422 and 400 dropped, the third reject in a row halts.
        assert_eq!(u.stats().rejected, 3);
        assert_eq!(u.stats().accepted, 1);
        assert_eq!(q.pending(), 2);
        assert_eq!(u.retry_in_ms(ms.get()), 1_800_000);
    }

    #[test]
    fn read_error_backs_off_without_losing_data() {
        let (mut u, mut q, mut env, ms) = setup(3);
        q.store_mut().fail_read = true;
        let mut t = transport(&ms, vec![]);
        u.poll(&mut q, true, &mut env, || Ok(&mut t));
        assert!(t.bodies.is_empty());
        assert!(u.stats().last_error.starts_with("queue read error"));
        assert!(u.retry_in_ms(ms.get()) > 0);
        assert_eq!(q.pending(), 3);
    }
}
