// Port of Channel.h / Channel.c
//
// C11 mtx_t/cnd_t + Queue become a Mutex<Queue> + Condvar.

#![allow(dead_code)]

use libc::c_void;
use std::sync::{Condvar, Mutex, MutexGuard};

use crate::queue::Queue;

pub struct Channel {
    q: Mutex<Queue>,
    cond: Condvar,
}

impl Channel {
    pub fn new() -> Channel {
        Channel {
            q: Mutex::new(Queue::new()),
            cond: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.q.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn send(&self, data: *mut c_void) {
        self.lock().enqueue(data);
        // Notify after unlocking (C signals while holding the mutex, so the
        // woken worker immediately blocks on it again). recv() re-checks
        // the queue under the lock, so no wakeup can be lost.
        self.cond.notify_one();
    }

    pub fn recv(&self) -> *mut c_void {
        let mut q = self.lock();
        while q.is_empty() {
            q = self.cond.wait(q).unwrap_or_else(|e| e.into_inner());
        }
        q.dequeue()
    }

    pub fn try_recv(&self) -> *mut c_void {
        // dequeue() on an empty queue already returns NULL.
        self.lock().dequeue()
    }

    /// Removes and returns everything still queued. ChannelDestroy in C
    /// free()d these; the caller now decides what to do with them.
    pub fn drain(&self) -> Vec<*mut c_void> {
        let mut q = self.lock();
        let mut items = Vec::with_capacity(q.count() as usize);
        while !q.is_empty() {
            items.push(q.dequeue());
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recv_blocks_until_send_from_other_thread() {
        let ch = Channel::new();
        assert!(ch.try_recv().is_null());
        std::thread::scope(|s| {
            let ch = &ch;
            let rx = s.spawn(move || (0..100).map(|_| ch.recv() as usize).collect::<Vec<_>>());
            for i in 1..=100usize {
                ch.send(i as *mut c_void);
            }
            assert_eq!(rx.join().unwrap(), (1..=100).collect::<Vec<_>>());
        });
        assert!(ch.drain().is_empty());
    }
}
