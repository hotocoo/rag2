//! FIFO admission for a decision server: at most `permits` requests in flight, granted strictly in
//! arrival order, each waiting no longer than its deadline.
//!
//! Why: llama.cpp 0.8.0 serves d1 requests one at a time and does not grant queued requests in
//! arrival order, so under saturation a request can starve for minutes (measured: a single request
//! waited over 120 s behind 8 busy clients). Holding the queue on the client side keeps the server
//! queue empty, makes waiting fair, and bounds it by the caller's deadline.

use parking_lot::{Condvar, Mutex};
use std::collections::VecDeque;
use std::time::Instant;

pub struct FairLimiter {
    state: Mutex<State>,
    cv: Condvar,
}

struct State {
    free: usize,
    queue: VecDeque<u64>,
    next: u64,
}

/// Returned to the limiter on drop.
pub struct Permit<'a>(&'a FairLimiter);

impl FairLimiter {
    pub fn new(permits: usize) -> Self {
        FairLimiter {
            state: Mutex::new(State {
                free: permits,
                queue: VecDeque::new(),
                next: 0,
            }),
            cv: Condvar::new(),
        }
    }

    /// Wait for a permit in arrival order; `None` if `deadline` passes first.
    pub fn acquire(&self, deadline: Instant) -> Option<Permit<'_>> {
        let mut s = self.state.lock();
        let me = s.next;
        s.next += 1;
        s.queue.push_back(me);
        loop {
            if s.free > 0 && s.queue.front() == Some(&me) {
                s.queue.pop_front();
                s.free -= 1;
                // The next in line may also fit.
                self.cv.notify_all();
                return Some(Permit(self));
            }
            if Instant::now() >= deadline {
                s.queue.retain(|x| *x != me);
                self.cv.notify_all();
                return None;
            }
            self.cv.wait_until(&mut s, deadline);
        }
    }

    /// Requests waiting for a permit.
    pub fn waiting(&self) -> usize {
        self.state.lock().queue.len()
    }
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.state.lock().free += 1;
        self.0.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    const SHORT: Duration = Duration::from_millis(50);
    const LONG: Duration = Duration::from_secs(10);

    #[test]
    fn permits_bound_concurrency_and_time_out() {
        let l = FairLimiter::new(2);
        let a = l.acquire(Instant::now() + LONG).expect("first");
        let _b = l.acquire(Instant::now() + LONG).expect("second");
        assert!(
            l.acquire(Instant::now() + SHORT).is_none(),
            "third must wait"
        );
        assert_eq!(l.waiting(), 0, "a timed-out waiter leaves the queue");
        drop(a);
        assert!(l.acquire(Instant::now() + SHORT).is_some());
    }

    #[test]
    fn waiters_are_served_in_arrival_order() {
        let l = Arc::new(FairLimiter::new(1));
        let held = l.acquire(Instant::now() + LONG).expect("held");
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut hs = Vec::new();
        for i in 0..5 {
            let (l2, order) = (l.clone(), order.clone());
            hs.push(std::thread::spawn(move || {
                let _p = l2.acquire(Instant::now() + LONG).expect("permit");
                order.lock().push(i);
            }));
            // Make arrival order deterministic.
            while l.waiting() < i + 1 {
                std::thread::yield_now();
            }
        }
        drop(held);
        for h in hs {
            h.join().expect("join");
        }
        assert_eq!(*order.lock(), vec![0, 1, 2, 3, 4]);
    }
}
