// Port of XMalloc.h / XMalloc.c

use libc::c_void;

// The fatal paths are kept out of line so the (inlined) happy path of every
// allocation site is just the call + a null check.
#[cold]
#[inline(never)]
fn alloc_failed(what: &str, count: usize, size: usize) -> ! {
    match what {
        "calloc" => crate::log_fatal!("calloc failed ({} × {} bytes)", count, size),
        _ => crate::log_fatal!("{} failed (requested {} bytes)", what, size),
    }
}

#[inline]
pub unsafe fn xmalloc(size: usize) -> *mut c_void {
    let ptr = libc::malloc(size);
    if ptr.is_null() {
        alloc_failed("malloc", 1, size);
    }
    ptr
}

#[inline]
pub unsafe fn xcalloc(count: usize, size: usize) -> *mut c_void {
    let ptr = libc::calloc(count, size);
    if ptr.is_null() {
        alloc_failed("calloc", count, size);
    }
    ptr
}

#[inline]
pub unsafe fn xrealloc(ptr: *mut c_void, new_size: usize) -> *mut c_void {
    let new_ptr = libc::realloc(ptr, new_size);
    if new_ptr.is_null() {
        alloc_failed("realloc", 1, new_size);
    }
    new_ptr
}

#[inline]
pub unsafe fn xfree(ptr: *mut c_void) {
    libc::free(ptr);
}
