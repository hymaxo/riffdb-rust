// Port of ThreadPool.h / ThreadPool.c

use libc::c_void;
use std::mem::size_of;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread::JoinHandle;

use crate::channel::Channel;
use crate::xmalloc::xmalloc;

pub type WorkerFn = unsafe fn(*mut c_void) -> i32;

pub struct ThreadPool {
    pub mail_boxes: Vec<Channel>,
    pub workers: Vec<JoinHandle<i32>>,

    // Only touched by the network thread; atomic so `process` can take &self
    // while workers hold raw pointers into the pool.
    pub current_worker: AtomicU8,

    pub working: AtomicBool,
}

impl ThreadPool {
    pub const fn new() -> ThreadPool {
        ThreadPool {
            mail_boxes: Vec::new(),
            workers: Vec::new(),
            current_worker: AtomicU8::new(0),
            working: AtomicBool::new(false),
        }
    }

    pub fn process(&self, message: *mut c_void) {
        let index = self.current_worker.load(Ordering::Relaxed) as usize;
        self.current_worker
            .store(((index + 1) % self.mail_boxes.len()) as u8, Ordering::Relaxed);
        self.mail_boxes[index].send(message);
    }
}

#[repr(C)]
pub struct ThreadPoolWorker {
    pub pool: *const ThreadPool,
    pub mail_box: *const Channel,
}

/// Lets a raw pointer cross into a spawned thread (the C code just hands
/// `void*` to thrd_create).
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}

/// Workers keep raw pointers to `self_` and its mail boxes: the pool must not
/// move, and must outlive the worker threads.
pub unsafe fn thread_pool_start(self_: *mut ThreadPool, workers_amount: u8, worker: WorkerFn) -> i8 {
    (*self_).mail_boxes = (0..workers_amount).map(|_| Channel::new()).collect();
    (*self_).workers = Vec::with_capacity(workers_amount as usize);
    (*self_).current_worker.store(0, Ordering::Relaxed);

    (*self_).working.store(true, Ordering::SeqCst);
    for i in 0..workers_amount as usize {
        let arg = xmalloc(size_of::<ThreadPoolWorker>()) as *mut ThreadPoolWorker;

        ptr::write(
            arg,
            ThreadPoolWorker {
                mail_box: (*self_).mail_boxes.as_ptr().add(i),
                pool: self_,
            },
        );

        let arg = SendPtr(arg as *mut c_void);
        let handle = std::thread::spawn(move || {
            let arg = arg;
            unsafe { worker(arg.0) }
        });
        (*self_).workers.push(handle);
    }

    0
}

pub unsafe fn thread_pool_stop(self_: *mut ThreadPool) {
    if !(*self_).working.load(Ordering::SeqCst) {
        return;
    }

    (*self_).working.store(false, Ordering::SeqCst);
    for handle in (*self_).workers.drain(..) {
        let _ = handle.join();
    }

    // All workers are gone, nothing else points into the pool any more.
    for mail_box in (*self_).mail_boxes.drain(..) {
        for item in mail_box.drain() {
            // queued messages are Requests from socket_actions_on_connect (xmalloc)
            libc::free(item);
        }
    }

    (*self_).current_worker.store(0, Ordering::Relaxed);
}
