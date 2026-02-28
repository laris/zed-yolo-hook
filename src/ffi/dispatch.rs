//! macOS libdispatch FFI bindings.
//!
//! Provides `dispatch_async_f` for scheduling work on the main queue.
//! Reusable for any hook that needs deferred execution outside the intercepted call stack.

use std::ffi::c_void;

unsafe extern "C" {
    #[link_name = "_dispatch_main_q"]
    static _dispatch_main_q: c_void;
    pub fn dispatch_async_f(
        queue: *const c_void,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

/// Returns a pointer to the main dispatch queue.
///
/// # Safety
/// Must be called from a process that links libdispatch (all macOS apps).
pub unsafe fn get_main_queue() -> *const c_void {
    &raw const _dispatch_main_q
}
