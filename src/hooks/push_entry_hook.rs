//! Hook `AcpThread::push_entry` to register AcpThread pointers.
//!
//! `push_entry` is the lowest-level entry insertion function, called by ALL
//! code paths including session restore from the workspace DB. This catches
//! AcpThread instances that never go through `upsert_tool_call_inner` or
//! `handle_session_update` — e.g., restored sessions before the ACP server
//! reconnects.
//!
//! NOTE: Does NOT call scan_and_approve inline (would deadlock in Frida context).
//! The stale_scanner thread handles the actual approval.

use super::entry_scanner;

pub struct Listener;

impl frida_gum::interceptor::InvocationListener for Listener {
    fn on_enter(&mut self, context: frida_gum::interceptor::InvocationContext) {
        let self_ptr = context.cpu_context().reg(0);
        // Lock-free registration — safe in interceptor context
        entry_scanner::register_thread(self_ptr);
    }

    fn on_leave(&mut self, _context: frida_gum::interceptor::InvocationContext) {
        // No-op: approval is handled by stale_scanner thread to avoid Mutex deadlock.
    }
}

/// Symbol patterns for `AcpThread::push_entry`.
pub const SYMBOL_INCLUDE: &[&str] = &["acp_thread", "AcpThread", "push_entry"];
pub const SYMBOL_EXCLUDE: &[&str] = &["drop_in_place", "closure", "vtable"];
