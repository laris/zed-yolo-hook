//! Hook for `AcpThread::request_tool_call_authorization` (external ACP agents).
//!
//! After the original function creates a oneshot channel and stores `respond_tx`
//! in `AcpThread.entries`, we walk the entries Vec to find it, then schedule
//! sending `PermissionOptionId("allow")` via `dispatch_async_f` on the main queue
//! (to avoid re-entrancy).
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
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::TOOL_AUTHORIZATION_COUNT;
use crate::ffi::dispatch;

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

// ---- Deferred send via dispatch_async_f ----

/// Data passed to the deferred callback.
struct DeferredSend {
    respond_tx: u64, // pointer to Sender<PermissionOptionId> on stack/heap
    count: u64,
}

/// Deferred callback: reconstruct the oneshot::Sender from the raw Arc pointer
/// and call .send() using the actual futures-channel Sender API.
///
/// The respond_tx value at entry+0x40 is the Arc<Inner<T>> pointer inside the
/// Sender<PermissionOptionId>. We reconstruct a Sender from it and call .send().
///
/// Key: Sender<T> is just `{ inner: Arc<Inner<T>> }`. Since we have the Arc pointer,
/// we can reconstruct the Sender via transmute. We bump the Arc refcount first to
/// avoid double-free (the entry still holds its copy).
extern "C" fn deferred_send(ctx: *mut c_void) {
    let data = unsafe { Box::from_raw(ctx as *mut DeferredSend) };
    let sender_arc_ptr = data.respond_tx;

    tracing::info!(
        "tool_authorization #{}: deferred_send executing (sender_arc_ptr={:#x})",
        data.count, sender_arc_ptr
    );

    // Build PermissionOptionId("allow") — same type as what Zed uses.
    // PermissionOptionId is #[repr(transparent)] around Arc<str>.
    let option_id: Arc<str> = Arc::from("allow");

    // Reconstruct a Sender<Arc<str>> from the raw Arc pointer.
    // Sender<T> = { inner: Arc<Inner<T>> }, which is a single pointer.
    //
    // We must bump the Arc refcount because:
    // - The entry still holds one Sender (refcount contribution)
    // - We're creating a second Sender from the same Arc
    // - When our Sender drops (after .send()), it decrements refcount
    // - The entry's Sender will also eventually be dropped
    unsafe {
        // Bump strong refcount: ArcInner.strong is at offset 0
        let strong = sender_arc_ptr as *const std::sync::atomic::AtomicUsize;
        (*strong).fetch_add(1, Ordering::Relaxed);

        // Transmute the Arc pointer into a Sender<Arc<str>>
        // Sender<T> is repr(Rust) with one field, same layout as the Arc pointer
        let sender: futures_channel::oneshot::Sender<Arc<str>> =
            std::mem::transmute(sender_arc_ptr);

        tracing::info!("tool_authorization #{}: calling sender.send(\"allow\")", data.count);

        match sender.send(option_id) {
            Ok(()) => {
                tracing::info!("tool_authorization #{}: send succeeded!", data.count);
            }
            Err(_) => {
                tracing::warn!("tool_authorization #{}: send failed (receiver dropped?)", data.count);
            }
        }
        // sender is dropped here, which calls drop_tx:
        //   sets complete=true, wakes rx_task
    }

    tracing::info!("tool_authorization #{}: deferred_send complete", data.count);
}

// ---- InvocationListener ----

pub struct Listener;

impl frida_gum::interceptor::InvocationListener for Listener {
    fn on_enter(&mut self, context: frida_gum::interceptor::InvocationContext) {
        let cpu = context.cpu_context();
        SAVED_SELF.with(|c| c.set(cpu.reg(0)));
    }

    fn on_leave(&mut self, _context: frida_gum::interceptor::InvocationContext) {
        let count = TOOL_AUTHORIZATION_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let self_ptr = SAVED_SELF.with(|c| c.get());

        tracing::info!("tool_authorization #{}: on_leave (self={:#x})", count, self_ptr);

        if self_ptr == 0 {
            tracing::warn!("tool_authorization #{}: self_ptr is null, skipping", count);
            return;
        }

        // Walk self.entries to find the last WaitingForConfirmation entry
        let (entries_ptr, entries_len) = unsafe {
            let p = self_ptr as *const u64;
            let ptr = *p.byte_add(ENTRIES_PTR_OFFSET);
            let len = *p.byte_add(ENTRIES_LEN_OFFSET);
            (ptr, len)
        };

        tracing::info!(
            "tool_authorization #{}: entries ptr={:#x}, len={}",
            count, entries_ptr, entries_len
        );

        if entries_ptr == 0 || entries_len == 0 {
            tracing::warn!("tool_authorization #{}: no entries, skipping", count);
            return;
        }

        // Dump last 3 entries for diagnostic
        let dump_count = std::cmp::min(3, entries_len as usize);
        for i in (entries_len as usize - dump_count)..entries_len as usize {
            let entry = entries_ptr + (i as u64 * ENTRY_SIZE as u64);
            unsafe {
                let p = entry as *const u64;
                tracing::info!(
                    "tool_authorization #{}: entry[{}] words: [{:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}]",
                    count, i,
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

            // Log last 3 entries' discriminant and status for debugging
            if i >= entries_len - 3 {
                tracing::info!(
                    "tool_authorization #{}: entry[{}] disc={:#x} status={:#x}",
                    count, i, discriminant, status
                );
            }

            if discriminant != TOOLCALL_VARIANT {
                continue;
            }

            if status == WAITING_VARIANT {
                let tx = unsafe { *(entry as *const u64).byte_add(ENTRY_RESPOND_TX_OFFSET) };
                // Validate respond_tx is a valid heap pointer (not null/garbage)
                if tx > 0x1_0000_0000 {
                    respond_tx = tx;
                    tracing::info!(
                        "tool_authorization #{}: found WaitingForConfirmation at entry[{}], respond_tx={:#x}",
                        count, i, respond_tx
                    );
                    break;
                }
            }
        }

        if respond_tx == 0 {
            tracing::warn!("tool_authorization #{}: no WaitingForConfirmation entry found", count);
            return;
        }

        // Schedule sending through respond_tx on the main queue
        // This avoids re-entrancy — we're outside request_tool_call_authorization's stack
        let data = Box::new(DeferredSend {
            respond_tx,
            count,
        });

        unsafe {
            let queue = dispatch::get_main_queue();
            dispatch::dispatch_async_f(
                queue,
                Box::into_raw(data) as *mut c_void,
                deferred_send,
            );
        }

        tracing::info!("tool_authorization #{}: deferred send scheduled", count);
    }
}

/// Symbol search patterns for locating `AcpThread::request_tool_call_authorization` in Zed's binary.
pub const SYMBOL_INCLUDE: &[&str] = &["acp_thread", "AcpThread", "request_tool_call_authorization"];
pub const SYMBOL_EXCLUDE: &[&str] = &["drop_in_place", "closure", "vtable", "island", "spawn"];
