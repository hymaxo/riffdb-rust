use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::channel::Channel;

pub struct ThreadPoolWorker<T> {
    shared: Arc<Shared<T>>,
    index: usize,
}

impl<T> ThreadPoolWorker<T> {
    pub fn mailbox(&self) -> &Channel<T> {
        &self.shared.mailboxes[self.index]
    }

    pub fn working(&self) -> bool {
        self.shared.working.load(Ordering::SeqCst)
    }

    pub fn stop_pool(&self) {
        self.shared.working.store(false, Ordering::SeqCst);
    }
}

struct Shared<T> {
    mailboxes: Box<[Channel<T>]>,
    working: AtomicBool,
}

pub struct ThreadPool<T> {
    shared: Arc<Shared<T>>,
    workers: Vec<JoinHandle<()>>,
    current_worker: AtomicU8,
}

impl<T: Send + 'static> ThreadPool<T> {
    pub fn start<F>(worker_count: u8, worker: F) -> ThreadPool<T>
    where
        F: Fn(ThreadPoolWorker<T>) + Send + Sync + Clone + 'static,
    {
        let shared = Arc::new(Shared {
            mailboxes: (0..worker_count).map(|_| Channel::new()).collect(),
            working: AtomicBool::new(true),
        });

        let workers = (0..worker_count as usize)
            .map(|index| {
                let handle = ThreadPoolWorker {
                    shared: Arc::clone(&shared),
                    index,
                };
                let worker = worker.clone();
                std::thread::spawn(move || worker(handle))
            })
            .collect();

        ThreadPool {
            shared,
            workers,
            current_worker: AtomicU8::new(0),
        }
    }

    pub fn process(&self, message: T) -> Result<(), T> {
        let index = self.current_worker.load(Ordering::Relaxed) as usize;
        self.current_worker
            .store(((index + 1) % self.shared.mailboxes.len()) as u8, Ordering::Relaxed);
        self.shared.mailboxes[index].send(message)
    }

    pub fn stop(&mut self) {
        if !self.shared.working.swap(false, Ordering::SeqCst) {
            return;
        }

        for mailbox in self.shared.mailboxes.iter() {
            mailbox.close();
        }
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        for mailbox in self.shared.mailboxes.iter() {
            drop(mailbox.drain());
        }
    }
}

impl<T> Drop for ThreadPool<T> {
    fn drop(&mut self) {
        self.shared.working.store(false, Ordering::SeqCst);
        for mailbox in self.shared.mailboxes.iter() {
            mailbox.close();
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
                while let Some(msg) = w.mailbox().recv() {
                    tx.lock().unwrap().send((w.index, msg)).unwrap();
                }
            }
        });
        for i in 0..9 {
            pool.process(i).unwrap();
        }
        let mut got: Vec<(usize, u32)> = (0..9).map(|_| rx.recv().unwrap()).collect();
        got.sort();
        assert!(got.iter().all(|&(w, m)| w == m as usize % 3));
        pool.stop();
    }
}
