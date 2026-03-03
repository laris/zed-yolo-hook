//! Hook for `AcpThread::request_tool_call_authorization` (external ACP agents).
//!
//! After the original function creates a oneshot channel and stores `respond_tx`
//! in `AcpThread.entries`, we walk the entries Vec to find it, then send
//! `PermissionOptionId("allow")` directly in `on_leave`.
//!
//! Sending inline is safe: `oneshot::Sender::send()` is an atomic write + receiver
//! wake. The receiver Future is polled later by Zed's async executor — it never
//! synchronously re-enters `request_tool_call_authorization`.
//!
//! Memory layout (from disassembly of Zed v0.226.0 aarch64):
//!   AcpThread + 0x60 = entries.ptr
//!   AcpThread + 0x68 = entries.len
//!   Each entry = 0x1b0 (432) bytes
//!   entry[0x00] = discriminant (0x7 = ToolCall)
//!   entry[0x20] = ToolCallStatus discriminant (0x1 = WaitingForConfirmation)
//!   entry[0x40] = respond_tx (pointer to oneshot::Sender on heap)
//!
//! oneshot::Sender internal layout (futures 0.3):
//!   sender+0x20 = lock (atomic u8)
//!   sender+0x58 = closed flag (atomic u8)
//!   The value (PermissionOptionId) is written at sender+0x28 (after lock)
//!   Then sender+0x20 is atomically set to signal the receiver.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::TOOL_AUTHORIZATION_COUNT;

// ---- Memory layout offsets (Zed v0.226.0 aarch64) ----
const ENTRIES_PTR_OFFSET: usize = 0x60; // Vec<AgentThreadEntry>.ptr
const ENTRIES_LEN_OFFSET: usize = 0x68; // Vec<AgentThreadEntry>.len
const ENTRY_SIZE: usize = 0x1b0;        // sizeof(AgentThreadEntry) = 432 bytes
const ENTRY_DISCRIMINANT_OFFSET: usize = 0x00; // AgentThreadEntry variant tag
const ENTRY_STATUS_OFFSET: usize = 0x20; // ToolCallStatus within ToolCall
const ENTRY_RESPOND_TX_OFFSET: usize = 0x40; // respond_tx ptr within WaitingForConfirmation
const TOOLCALL_VARIANT: u64 = 0x07;     // AgentThreadEntry::ToolCall discriminant
const WAITING_VARIANT: u64 = 0x00;      // ToolCallStatus::WaitingForConfirmation (niche-optimized)

// ---- Thread-local for capturing `self` pointer ----
thread_local! {
    static SAVED_SELF: Cell<u64> = const { Cell::new(0) };
}

// ---- Inline send helper ----

/// Reconstruct the oneshot::Sender from the raw Arc pointer and send "allow".
///
/// The respond_tx value at entry+0x40 is the Arc<Inner<T>> pointer inside the
/// Sender<PermissionOptionId>. We reconstruct a Sender from it and call .send().
///
/// Key: Sender<T> is just `{ inner: Arc<Inner<T>> }`. Since we have the Arc pointer,
/// we can reconstruct the Sender via transmute. We bump the Arc refcount first to
/// avoid double-free (the entry still holds its copy).
///
/// # Safety
/// `sender_arc_ptr` must point to a valid Arc<Inner<Sender<Arc<str>>>>.
unsafe fn send_allow(sender_arc_ptr: u64, count: u64) -> bool {
    // Build PermissionOptionId("allow") — same type as what Zed uses.
    // PermissionOptionId is #[repr(transparent)] around Arc<str>.
    let option_id: Arc<str> = Arc::from("allow");

    // Bump strong refcount: ArcInner.strong is at offset 0
    // We must bump because:
    // - The entry still holds one Sender (refcount contribution)
    // - We're creating a second Sender from the same Arc
    // - When our Sender drops (after .send()), it decrements refcount
    // - The entry's Sender will also eventually be dropped
    let strong = sender_arc_ptr as *const std::sync::atomic::AtomicUsize;
    unsafe { (*strong).fetch_add(1, Ordering::Relaxed) };

    // Transmute the Arc pointer into a Sender<Arc<str>>
    // Sender<T> is repr(Rust) with one field, same layout as the Arc pointer
    let sender: futures_channel::oneshot::Sender<Arc<str>> =
        unsafe { std::mem::transmute(sender_arc_ptr) };

    match sender.send(option_id) {
        Ok(()) => {
            tracing::info!("tool_authorization #{count}: send succeeded");
            true
        }
        Err(_) => {
            tracing::warn!("tool_authorization #{count}: send failed (receiver dropped?)");
            false
        }
    }
}

// ---- InvocationListener ----

pub struct Listener;

impl frida_gum::interceptor::InvocationListener for Listener {
    fn on_enter(&mut self, context: frida_gum::interceptor::InvocationContext) {
        let cpu = context.cpu_context();
        SAVED_SELF.with(|c| c.set(cpu.reg(0)));
    }

    fn on_leave(&mut self, _context: frida_gum::interceptor::InvocationContext) {
        let t0 = Instant::now();
        let count = TOOL_AUTHORIZATION_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let self_ptr = SAVED_SELF.with(|c| c.get());

        tracing::debug!("tool_authorization #{count}: on_leave (self={self_ptr:#x})");

        if self_ptr == 0 {
            tracing::warn!("tool_authorization #{count}: self_ptr is null, skipping");
            return;
        }

        // Walk self.entries to find the last WaitingForConfirmation entry
        let (entries_ptr, entries_len) = unsafe {
            let p = self_ptr as *const u64;
            let ptr = *p.byte_add(ENTRIES_PTR_OFFSET);
            let len = *p.byte_add(ENTRIES_LEN_OFFSET);
            (ptr, len)
        };

        tracing::debug!(
            "tool_authorization #{count}: entries ptr={entries_ptr:#x}, len={entries_len}"
        );

        if entries_ptr == 0 || entries_len == 0 {
            tracing::warn!("tool_authorization #{count}: no entries, skipping");
            return;
        }

        // Dump last 3 entries for diagnostic (debug level only)
        let dump_count = std::cmp::min(3, entries_len as usize);
        for i in (entries_len as usize - dump_count)..entries_len as usize {
            let entry = entries_ptr + (i as u64 * ENTRY_SIZE as u64);
            unsafe {
                let p = entry as *const u64;
                tracing::debug!(
                    "tool_authorization #{count}: entry[{i}] words: [{:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}]",
                    *p, *p.add(1), *p.add(2), *p.add(3),
                    *p.add(4), *p.add(5), *p.add(6), *p.add(7), *p.add(8)
                );
            }
        }

        // Iterate entries in reverse to find last WaitingForConfirmation
        let mut respond_tx: u64 = 0;
        for i in (0..entries_len).rev() {
            let entry = entries_ptr + (i * ENTRY_SIZE as u64);
            let discriminant = unsafe { *(entry as *const u64).byte_add(ENTRY_DISCRIMINANT_OFFSET) };
            let status = unsafe { *(entry as *const u64).byte_add(ENTRY_STATUS_OFFSET) };

            tracing::debug!(
                "tool_authorization #{count}: entry[{i}] disc={discriminant:#x} status={status:#x}"
            );

            if discriminant != TOOLCALL_VARIANT {
                continue;
            }

            if status == WAITING_VARIANT {
                let tx = unsafe { *(entry as *const u64).byte_add(ENTRY_RESPOND_TX_OFFSET) };
                // Validate respond_tx is a valid heap pointer (not null/garbage)
                if tx > 0x1_0000_0000 {
                    respond_tx = tx;
                    tracing::debug!(
                        "tool_authorization #{count}: found WaitingForConfirmation at entry[{i}], respond_tx={respond_tx:#x}"
                    );
                    break;
                }
            }
        }

        if respond_tx == 0 {
            tracing::warn!("tool_authorization #{count}: no WaitingForConfirmation entry found");
            return;
        }

        // Send directly — no dispatch_async_f needed.
        // oneshot::Sender::send() is an atomic write + wake; the receiver Future
        // is polled later by Zed's executor, no re-entrancy risk.
        let ok = unsafe { send_allow(respond_tx, count) };
        let elapsed_us = t0.elapsed().as_micros();

        if ok {
            tracing::info!("tool_authorization #{count}: approved in {elapsed_us}us");
        }
    }
}

/// Symbol search patterns for locating `AcpThread::request_tool_call_authorization` in Zed's binary.
pub const SYMBOL_INCLUDE: &[&str] = &["acp_thread", "AcpThread", "request_tool_call_authorization"];
pub const SYMBOL_EXCLUDE: &[&str] = &["drop_in_place", "closure", "vtable", "island", "spawn"];
