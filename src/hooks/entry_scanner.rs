//! Shared entry scanner for finding and auto-approving WaitingForConfirmation entries.
//!
//! Used by the stale_scanner background thread ONLY.
//! The upsert_hook and session_update_hook NO LONGER use this module to avoid
//! Mutex contention with Frida interceptor context (which causes deadlocks).
//!
//! Thread safety: all Mutex access happens on the dedicated scanner thread only.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

use super::tool_authorization;

// Re-export layout constants from tool_authorization (v0.233.0)
pub(crate) const ENTRIES_PTR_OFFSET: usize = 0xb0;
pub(crate) const ENTRIES_LEN_OFFSET: usize = 0xb8;

/// Tracks all AcpThread pointers we've seen, for periodic scanning.
/// Written by register_thread (from interceptor context via atomic CAS-style),
/// read by stale_scanner thread.
///
/// Uses a fixed-size array + atomic length to avoid Mutex in interceptor context.
const MAX_THREADS: usize = 64;
static THREAD_PTRS: [AtomicU64; MAX_THREADS] = {
    // Initialize all to 0
    const ZERO: AtomicU64 = AtomicU64::new(0);
    [ZERO; MAX_THREADS]
};
static THREAD_COUNT: AtomicU64 = AtomicU64::new(0);

// Tracks entry indices we've already attempted, to avoid infinite retries.
// Only accessed from the stale_scanner thread via thread_local.
thread_local! {
    static SCANNER_ATTEMPTED: std::cell::RefCell<HashSet<(u64, u64)>> =
        std::cell::RefCell::new(HashSet::new());
}

/// Counter for approvals made by the stale scanner.
pub static SCANNER_APPROVAL_COUNT: AtomicU64 = AtomicU64::new(0);

/// Register an AcpThread pointer for periodic scanning.
/// Lock-free — safe to call from Frida interceptor context.
pub fn register_thread(self_ptr: u64) {
    if self_ptr == 0 {
        return;
    }

    // Check if already registered
    let count = THREAD_COUNT.load(Ordering::Relaxed) as usize;
    for i in 0..count.min(MAX_THREADS) {
        if THREAD_PTRS[i].load(Ordering::Relaxed) == self_ptr {
            return; // Already registered
        }
    }

    // Try to add
    let idx = THREAD_COUNT.fetch_add(1, Ordering::Relaxed) as usize;
    if idx < MAX_THREADS {
        THREAD_PTRS[idx].store(self_ptr, Ordering::Relaxed);
        tracing::info!("entry_scanner: registered AcpThread {self_ptr:#x} (slot={idx})");
    }
    // If full, silently ignore — 64 threads is far more than any real scenario
}

/// Get all known AcpThread pointers. Only call from scanner thread.
pub fn known_threads() -> Vec<u64> {
    let count = THREAD_COUNT.load(Ordering::Relaxed) as usize;
    let mut result = Vec::with_capacity(count.min(MAX_THREADS));
    for i in 0..count.min(MAX_THREADS) {
        let ptr = THREAD_PTRS[i].load(Ordering::Relaxed);
        if ptr != 0 {
            result.push(ptr);
        }
    }
    result
}

/// Read entries ptr and len from an AcpThread pointer.
///
/// # Safety
/// `self_ptr` must be a valid AcpThread pointer.
pub unsafe fn read_entries(self_ptr: u64) -> (u64, u64) {
    let p = self_ptr as *const u64;
    let ptr = unsafe { *p.byte_add(ENTRIES_PTR_OFFSET) };
    let len = unsafe { *p.byte_add(ENTRIES_LEN_OFFSET) };
    (ptr, len)
}

/// Scan all entries of an AcpThread for WaitingForConfirmation entries and auto-approve them.
/// ONLY call from the stale_scanner thread (accesses SCANNER_ATTEMPTED without lock).
///
/// Returns the number of entries approved.
///
/// # Safety
/// `self_ptr` must be a valid AcpThread pointer with a live entries Vec.
pub unsafe fn scan_and_approve_from_scanner(self_ptr: u64) -> u64 {
    let (entries_ptr, entries_len) = unsafe { read_entries(self_ptr) };

    if entries_ptr == 0 || entries_len == 0 {
        return 0;
    }

    let layout = &tool_authorization::ENTRY_LAYOUTS[0]; // v0.230.x
    let mut approved = 0;

    SCANNER_ATTEMPTED.with(|attempted| {
        let mut attempted = attempted.borrow_mut();

        for i in 0..entries_len {
            // Skip already-attempted entries
            if attempted.contains(&(self_ptr, i)) {
                continue;
            }

            let entry = entries_ptr + (i * layout.entry_size as u64);
            let discriminant = unsafe { *(entry as *const u64).byte_add(0) };

            if let tool_authorization::MatchStyle::Preview230 {
                toolcall_variant,
                waiting_payload_niche_start,
                ..
            } = layout.match_style
            {
                if discriminant != toolcall_variant {
                    continue;
                }

                let status_head = unsafe { *(entry as *const u64).byte_add(layout.status_offset) };

                // Check if WaitingForConfirmation (niche-encoded)
                if status_head >= waiting_payload_niche_start {
                    continue; // Not waiting
                }

                let tx = unsafe { *(entry as *const u64).byte_add(layout.respond_tx_offset) };
                if !unsafe { tool_authorization::looks_like_sender_arc_pub(tx) } {
                    continue; // Invalid sender
                }

                // Check plan mode
                let is_plan = unsafe { tool_authorization::detect_plan_mode_pub(entry, layout) };
                let count_val = SCANNER_APPROVAL_COUNT.load(Ordering::Relaxed) + approved + 1;

                let session_tag = format!("{:04x}", self_ptr & 0xFFFF);
                tracing::info!(
                    "stale_scanner [s:{session_tag}]: found WaitingForConfirmation at entry[{i}], approving..."
                );

                let ok = unsafe { tool_authorization::send_allow_pub(layout.send_style, tx, is_plan, count_val) };

                if ok {
                    // Force status to InProgress so the UI dismisses the dialog
                    unsafe { tool_authorization::force_status_in_progress(entry, layout) };
                }

                // Mark as attempted (avoid infinite retry)
                attempted.insert((self_ptr, i));

                if ok {
                    approved += 1;
                    tracing::info!(
                        "stale_scanner [s:{session_tag}]: approved entry[{i}] (plan_mode={is_plan})"
                    );
                } else {
                    tracing::debug!(
                        "stale_scanner [s:{session_tag}]: entry[{i}] send failed (already consumed)"
                    );
                }
            }
        }
    });

    approved
}
