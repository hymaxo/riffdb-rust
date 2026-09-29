// Port of Queue.h / Queue.c
//
// Fixed-size ring buffer. Enqueue on a full queue silently drops the item,
// same as the C version. Items are opaque pointers owned by the caller; the
// queue never dereferences them.

#![allow(dead_code)]

use libc::c_void;
use std::ptr;

pub const QUEUE_CAPACITY: usize = 1024;

pub struct Queue {
    items: [*mut c_void; QUEUE_CAPACITY],
    head: u16,
    tail: u16,
    count: u16,
}

// SAFETY: the queue only stores and hands back pointer values and never
// dereferences them. Whoever dereferences a dequeued pointer is responsible
// for that access (in riffdb: the worker, inside unsafe code).
unsafe impl Send for Queue {}

impl Queue {
    pub const fn new() -> Queue {
        Queue {
            items: [ptr::null_mut(); QUEUE_CAPACITY],
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn peek(&self) -> *mut c_void {
        if self.is_empty() {
            return ptr::null_mut();
        }
        self.items[self.head as usize]
    }

    pub fn enqueue(&mut self, data: *mut c_void) {
        if self.count as usize == QUEUE_CAPACITY {
            return;
        }
        self.items[self.tail as usize] = data;
        self.tail = ((self.tail as usize + 1) % QUEUE_CAPACITY) as u16;
        self.count += 1;
    }

    pub fn dequeue(&mut self) -> *mut c_void {
        if self.is_empty() {
            return ptr::null_mut();
        }
        let data = self.items[self.head as usize];
        self.items[self.head as usize] = ptr::null_mut();
        self.head = ((self.head as usize + 1) % QUEUE_CAPACITY) as u16;
        self.count -= 1;
        data
    }

    pub fn count(&self) -> u16 {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: usize) -> *mut c_void {
        n as *mut c_void
    }

    #[test]
    fn fifo_order_and_wraparound() {
        let mut q = Queue::new();
        assert!(q.is_empty());
        assert!(q.dequeue().is_null());
        // push/pop enough to wrap head and tail several times
        for round in 0..3 * QUEUE_CAPACITY {
            q.enqueue(p(round + 1));
            q.enqueue(p(round + 2));
            assert_eq!(q.peek(), p(round + 1));
            assert_eq!(q.dequeue(), p(round + 1));
            assert_eq!(q.dequeue(), p(round + 2));
        }
        assert!(q.is_empty());
    }

    #[test]
    fn full_queue_drops_new_items() {
        let mut q = Queue::new();
        for i in 0..QUEUE_CAPACITY {
            q.enqueue(p(i + 1));
        }
        q.enqueue(p(9999)); // dropped, like the C version
        assert_eq!(q.count() as usize, QUEUE_CAPACITY);
        for i in 0..QUEUE_CAPACITY {
            assert_eq!(q.dequeue(), p(i + 1));
        }
        assert!(q.is_empty());
    }
}
