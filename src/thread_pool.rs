// Port of ThreadPool.h / ThreadPool.c
//
// Same shape: one mailbox (Channel) per worker thread, messages handed out
// round-robin. The workers share the mailboxes and the `working` flag through
// an Arc instead of a raw pointer back to the pool.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::channel::Channel;

/// What the C code passes to each worker (ThreadPoolWorker: pool + mailbox).
pub struct ThreadPoolWorker<T> {
    shared: Arc<Shared<T>>,
    index: usize,
}

impl<T> ThreadPoolWorker<T> {
    pub fn mail_box(&self) -> &Channel<T> {
        &self.shared.mail_boxes[self.index]
    }

    pub fn working(&self) -> bool {
        self.shared.working.load(Ordering::SeqCst)
    }

    /// C: `Self->Pool->Working = false` when a worker can't start.
    pub fn stop_pool(&self) {
        self.shared.working.store(false, Ordering::SeqCst);
    }
}

struct Shared<T> {
    mail_boxes: Box<[Channel<T>]>,
    working: AtomicBool,
}

pub struct ThreadPool<T> {
    shared: Arc<Shared<T>>,
    workers: Vec<JoinHandle<i32>>,
    // Only touched by the network thread.
    current_worker: AtomicU8,
}

impl<T: Send + 'static> ThreadPool<T> {
    pub fn start<F>(workers_amount: u8, worker: F) -> ThreadPool<T>
    where
        F: Fn(ThreadPoolWorker<T>) -> i32 + Send + Sync + Clone + 'static,
    {
        let shared = Arc::new(Shared {
            mail_boxes: (0..workers_amount).map(|_| Channel::new()).collect(),
            working: AtomicBool::new(true),
        });

        let workers = (0..workers_amount as usize)
            .map(|index| {
                let arg = ThreadPoolWorker {
                    shared: Arc::clone(&shared),
                    index,
                };
                let worker = worker.clone();
                std::thread::spawn(move || worker(arg))
            })
            .collect();

        ThreadPool {
            shared,
            workers,
            current_worker: AtomicU8::new(0),
        }
    }

    /// Round-robin like ThreadPoolProcess. Returns the message if the target
    /// mailbox is full (C drops it silently).
    pub fn process(&self, message: T) -> Result<(), T> {
        let index = self.current_worker.load(Ordering::Relaxed) as usize;
        self.current_worker
            .store(((index + 1) % self.shared.mail_boxes.len()) as u8, Ordering::Relaxed);
        self.shared.mail_boxes[index].send(message)
    }

    /// Like ThreadPoolStop, except that it can actually return: closing the
    /// mailboxes wakes the workers blocked in recv(). Messages still queued
    /// are dropped (C: ChannelDestroy frees them).
    pub fn stop(&mut self) {
        if !self.shared.working.swap(false, Ordering::SeqCst) {
            return;
        }

        for mail_box in self.shared.mail_boxes.iter() {
            mail_box.close();
        }
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        for mail_box in self.shared.mail_boxes.iter() {
            drop(mail_box.drain());
        }
    }
}

impl<T> Drop for ThreadPool<T> {
    fn drop(&mut self) {
        self.shared.working.store(false, Ordering::SeqCst);
        for mail_box in self.shared.mail_boxes.iter() {
            mail_box.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn round_robin_and_stop() {
        let (tx, rx) = mpsc::channel::<(usize, u32)>();
        let tx = std::sync::Mutex::new(tx);
        let tx = Arc::new(tx);
        let mut pool = ThreadPool::start(3, {
            let tx = Arc::clone(&tx);
            move |w: ThreadPoolWorker<u32>| {
                while let Some(msg) = w.mail_box().recv() {
                    tx.lock().unwrap().send((w.index, msg)).unwrap();
                }
                0
            }
        });
        for i in 0..9 {
            pool.process(i).unwrap();
        }
        let mut got: Vec<(usize, u32)> = (0..9).map(|_| rx.recv().unwrap()).collect();
        got.sort();
        // message i went to worker i % 3
        assert!(got.iter().all(|&(w, m)| w == m as usize % 3));
        pool.stop(); // must not hang
    }
}
