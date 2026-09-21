//! Fixed-duration `Instant`-based tick pacer for the server's main loop.
//!
//! Cadence: a deadline that advances by `tick_duration` per iteration.
//! If a tick takes longer than its budget, we don't sleep — we run
//! the next one immediately so wall-clock time averages out. If we
//! fall behind by more than 10 ticks (~333 ms at the default 30 Hz) we reset the deadline
//! to `now` so the loop doesn't burn CPU "catching up" after a process
//! suspend.
//!
//! Used by `run_server::run_server` directly. Host-client mode in
//! the client crate also runs through `run_server`, so the pacer
//! drives both topologies via the same path.

use std::thread;
use std::time::{Duration, Instant};

pub struct TickPacer {
    next_deadline: Instant,
    tick_duration: Duration,
}

impl TickPacer {
    #[must_use]
    pub fn new(tick_duration: Duration) -> Self {
        Self {
            next_deadline: Instant::now() + tick_duration,
            tick_duration,
        }
    }

    /// Sleep (or skip) until the next tick boundary. Call once per
    /// loop iteration after all per-tick work has finished.
    pub fn sleep_until_next(&mut self) {
        let now = Instant::now();
        if self.next_deadline > now {
            thread::sleep(self.next_deadline - now);
            self.next_deadline += self.tick_duration;
        } else {
            let lag = now - self.next_deadline;
            if lag > self.tick_duration * 10 {
                // Grossly behind — reset the deadline so we don't
                // burn CPU replaying every missed tick at maximum
                // speed.
                self.next_deadline = now + self.tick_duration;
            } else {
                self.next_deadline += self.tick_duration;
            }
        }
    }
}
