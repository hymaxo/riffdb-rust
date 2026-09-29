// A bounded multi-producer queue with blocking receive: a Mutex<Queue<T>>
// plus a Condvar. Closing it makes recv() return None once it is empty, which
// is how the thread pool shuts its workers down.

use std::sync::{Condvar, Mutex, MutexGuard};

use crate::queue::Queue;

struct State<T> {
    q: Queue<T>,
    closed: bool,
}

pub struct Channel<T> {
    state: Mutex<State<T>>,
    cond: Condvar,
}

impl<T> Channel<T> {
    pub fn new() -> Channel<T> {
        Channel {
            state: Mutex::new(State {
                q: Queue::new(),
                closed: false,
            }),
            cond: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queues `data`, or hands it back if the queue is full.
    pub fn send(&self, data: T) -> Result<(), T> {
        let rc = self.lock().q.enqueue(data);
        // Notify after unlocking, so the woken receiver doesn't immediately
        // block on the mutex. recv() re-checks the queue under the lock, so
        // no wakeup can be lost.
        self.cond.notify_one();
        rc
    }

    /// Blocks until a message arrives. None once the channel is closed and
    /// empty.
    pub fn recv(&self) -> Option<T> {
        let mut state = self.lock();
        loop {
            if let Some(data) = state.q.dequeue() {
                return Some(data);
            }
            if state.closed {
                return None;
            }
            state = self.cond.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    #[cfg(test)]
    pub fn try_recv(&self) -> Option<T> {
        self.lock().q.dequeue()
    }

    pub fn close(&self) {
        self.lock().closed = true;
        self.cond.notify_all();
    }

    /// Removes and returns everything still queued.
    pub fn drain(&self) -> Vec<T> {
        let mut state = self.lock();
        let mut items = Vec::with_capacity(state.q.count() as usize);
        while let Some(item) = state.q.dequeue() {
            items.push(item);
        }
        items
    }
}

impl<T> Default for Channel<T> {
    fn default() -> Self {
        Channel::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recv_blocks_until_send_from_other_thread() {
        let ch = Channel::new();
        assert_eq!(ch.try_recv(), None);
        std::thread::scope(|s| {
            let ch = &ch;
            let rx = s.spawn(move || (0..100).map(|_| ch.recv().unwrap()).collect::<Vec<usize>>());
            for i in 1..=100usize {
                ch.send(i).unwrap();
            }
            assert_eq!(rx.join().unwrap(), (1..=100).collect::<Vec<_>>());
        });
        assert!(ch.drain().is_empty());
    }

    #[test]
    fn close_wakes_receivers_after_draining() {
        let ch = Channel::new();
        ch.send(1).unwrap();
        std::thread::scope(|s| {
            let ch = &ch;
            let rx = s.spawn(move || (ch.recv(), ch.recv()));
            std::thread::sleep(std::time::Duration::from_millis(20));
            ch.close();
            assert_eq!(rx.join().unwrap(), (Some(1), None));
        });
    }
}
