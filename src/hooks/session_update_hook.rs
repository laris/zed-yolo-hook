//! Approach 2: Hook `AcpThread::handle_session_update` to register AcpThread pointers.
//!
//! Ensures the stale_scanner knows about AcpThread instances used by the
//! session_notification path (restored/resumed sessions).
//!
//! NOTE: Does NOT call scan_and_approve inline (would deadlock in Frida context).
//! The stale_scanner thread handles the actual approval.

use std::cell::Cell;

use super::entry_scanner;

thread_local! {
    static SAVED_SELF: Cell<u64> = const { Cell::new(0) };
}

pub struct Listener;

impl frida_gum::interceptor::InvocationListener for Listener {
    fn on_enter(&mut self, context: frida_gum::interceptor::InvocationContext) {
        let self_ptr = context.cpu_context().reg(0);
        SAVED_SELF.with(|c| c.set(self_ptr));
        // Lock-free registration — safe in interceptor context
        entry_scanner::register_thread(self_ptr);
    }

    fn on_leave(&mut self, _context: frida_gum::interceptor::InvocationContext) {
        // No-op: approval is handled by stale_scanner thread to avoid Mutex deadlock.
    }
}

/// Symbol patterns for `AcpThread::handle_session_update`.
pub const SYMBOL_INCLUDE: &[&str] = &["acp_thread", "AcpThread", "handle_session_update"];
pub const SYMBOL_EXCLUDE: &[&str] = &["drop_in_place", "closure", "vtable"];
