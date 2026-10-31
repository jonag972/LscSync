//! A bounded "latest wins" queue between a real-time producer and a consumer.
//!
//! Audio capture callbacks (or an engine's frame loop) produce much faster
//! than the consumer reads: when the queue is full, the oldest item is
//! dropped instead of letting memory grow without limit. Only the most recent
//! item matters to the consumer, so nothing is lost semantically.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How many items may wait at most. Audio blocks arrive ~50-100 times per
/// second and the engine consumes one per frame, so 8 blocks is roughly
/// 100-160 ms of buffer: enough to absorb a slow frame, never a leak.
pub const MAX_QUEUED: usize = 8;

struct Inner<T> {
    queue: VecDeque<T>,
    /// Live producers; when it reaches 0, the channel is closed.
    producers: usize,
}

/// The shared, bounded queue. Keep the `Arc<Latest<T>>` to consume, hand out
/// [`LatestSender`]s to produce.
pub struct Latest<T> {
    inner: Mutex<Inner<T>>,
    ready: Condvar,
}

/// Returned by [`Latest::recv_timeout`] once every producer was dropped and
/// the queue is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed;

/// The producing end of a [`Latest`] queue. Never blocks: when the queue is
/// full, the oldest item is dropped.
pub struct LatestSender<T> {
    latest: Arc<Latest<T>>,
}

impl<T> Latest<T> {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                producers: 0,
            }),
            ready: Condvar::new(),
        })
    }

    /// A new producing end. Cloning a [`LatestSender`] counts as one more.
    pub fn sender(self: &Arc<Self>) -> LatestSender<T> {
        self.inner.lock().unwrap().producers += 1;
        LatestSender {
            latest: Arc::clone(self),
        }
    }

    /// Takes the oldest queued item. `Ok(None)` on timeout, `Err(Closed)`
    /// once every producer is gone and nothing is left.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<T>, Closed> {
        let deadline = Instant::now() + timeout;
        let mut inner = self.inner.lock().unwrap();
        loop {
            if let Some(item) = inner.queue.pop_front() {
                return Ok(Some(item));
            }
            if inner.producers == 0 {
                return Err(Closed);
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Ok(None);
            };
            let (guard, _) = self.ready.wait_timeout(inner, remaining).unwrap();
            inner = guard;
        }
    }
}

impl<T> LatestSender<T> {
    /// Pushes an item, dropping the oldest one when the queue is full.
    pub fn send(&self, item: T) {
        let mut inner = self.latest.inner.lock().unwrap();
        if inner.queue.len() >= MAX_QUEUED {
            inner.queue.pop_front();
        }
        inner.queue.push_back(item);
        drop(inner);
        self.latest.ready.notify_one();
    }
}

impl<T> Clone for LatestSender<T> {
    fn clone(&self) -> Self {
        self.latest.inner.lock().unwrap().producers += 1;
        Self {
            latest: Arc::clone(&self.latest),
        }
    }
}

impl<T> Drop for LatestSender<T> {
    fn drop(&mut self) {
        {
            let mut inner = self.latest.inner.lock().unwrap();
            inner.producers -= 1;
        }
        self.latest.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivers_in_order_then_times_out() {
        let queue = Latest::new();
        let sender = queue.sender();
        sender.send(1);
        sender.send(2);
        assert_eq!(queue.recv_timeout(Duration::from_millis(10)), Ok(Some(1)));
        assert_eq!(queue.recv_timeout(Duration::from_millis(10)), Ok(Some(2)));
        assert_eq!(queue.recv_timeout(Duration::from_millis(5)), Ok(None));
    }

    #[test]
    fn full_queue_drops_the_oldest() {
        let queue = Latest::new();
        let sender = queue.sender();
        for i in 0..(MAX_QUEUED + 5) {
            sender.send(i);
        }
        let mut got = Vec::new();
        while let Ok(Some(value)) = queue.recv_timeout(Duration::ZERO) {
            got.push(value);
        }
        assert_eq!(got, (5..MAX_QUEUED + 5).collect::<Vec<_>>());
    }

    #[test]
    fn closes_once_the_last_sender_drops() {
        let queue = Latest::new();
        let sender = queue.sender();
        sender.send(1);
        drop(sender);
        // What was queued is still delivered, then the channel is closed.
        assert_eq!(queue.recv_timeout(Duration::ZERO), Ok(Some(1)));
        assert_eq!(queue.recv_timeout(Duration::ZERO), Err(Closed));
    }

    #[test]
    fn blocks_until_an_item_arrives() {
        let queue = Latest::new();
        let sender = queue.sender();
        let waiting = Arc::clone(&queue);
        let handle = std::thread::spawn(move || waiting.recv_timeout(Duration::from_secs(2)));
        std::thread::sleep(Duration::from_millis(50));
        sender.send(42);
        assert_eq!(handle.join().unwrap(), Ok(Some(42)));
    }
}
