// Port of Queue.h / Queue.c
//
// Fixed-size ring buffer. Enqueue on a full queue silently drops the item,
// same as the C version. Generic over the item type instead of `void*`.

#![allow(dead_code)]

pub const QUEUE_CAPACITY: usize = 1024;

pub struct Queue<T> {
    items: Box<[Option<T>]>,
    head: u16,
    tail: u16,
    count: u16,
}

impl<T> Queue<T> {
    pub fn new() -> Queue<T> {
        Queue {
            items: (0..QUEUE_CAPACITY).map(|_| None).collect(),
            head: 0,
            tail: 0,
            count: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn peek(&self) -> Option<&T> {
        self.items[self.head as usize].as_ref()
    }

    /// Returns the item back if the queue is full (C drops it silently; the
    /// caller decides what dropping means).
    pub fn enqueue(&mut self, data: T) -> Result<(), T> {
        if self.count as usize == QUEUE_CAPACITY {
            return Err(data);
        }
        self.items[self.tail as usize] = Some(data);
        self.tail = ((self.tail as usize + 1) % QUEUE_CAPACITY) as u16;
        self.count += 1;
        Ok(())
    }

    pub fn dequeue(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let data = self.items[self.head as usize].take();
        self.head = ((self.head as usize + 1) % QUEUE_CAPACITY) as u16;
        self.count -= 1;
        data
    }

    pub fn count(&self) -> u16 {
        self.count
    }
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Queue::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_order_and_wraparound() {
        let mut q = Queue::new();
        assert!(q.is_empty());
        assert_eq!(q.dequeue(), None);
        // push/pop enough to wrap head and tail several times
        for round in 0..3 * QUEUE_CAPACITY {
            q.enqueue(round + 1).unwrap();
            q.enqueue(round + 2).unwrap();
            assert_eq!(q.peek(), Some(&(round + 1)));
            assert_eq!(q.dequeue(), Some(round + 1));
            assert_eq!(q.dequeue(), Some(round + 2));
        }
        assert!(q.is_empty());
    }

    #[test]
    fn full_queue_rejects_new_items() {
        let mut q = Queue::new();
        for i in 0..QUEUE_CAPACITY {
            q.enqueue(i + 1).unwrap();
        }
        assert_eq!(q.enqueue(9999), Err(9999)); // dropped, like the C version
        assert_eq!(q.count() as usize, QUEUE_CAPACITY);
        for i in 0..QUEUE_CAPACITY {
            assert_eq!(q.dequeue(), Some(i + 1));
        }
        assert!(q.is_empty());
    }
}
