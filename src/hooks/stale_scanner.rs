//! Approach 3: Periodic background scanner for stale WaitingForConfirmation entries.
//!
//! Spawns a background thread that periodically scans all known AcpThread instances
//! for WaitingForConfirmation entries and auto-approves them.
//!
//! This is the primary fallback — it catches ALL cases regardless of which
//! Zed code path created the entry. The upsert_hook and session_update_hook
//! ensure AcpThread pointers are registered; this thread does the actual approval.
//!
//! Thread safety: NO Mutex is held during scan. AcpThread registration uses
//! lock-free atomics. The attempted-entries set is only accessed from this thread.

use std::sync::atomic::Ordering;
use std::time::Duration;

use super::entry_scanner;

/// Start the periodic scanner thread.
pub fn start(scan_interval_ms: u64) {
    let interval = Duration::from_millis(scan_interval_ms.max(500));

    std::thread::Builder::new()
        .name("yolo-stale-scanner".to_string())
        .spawn(move || {
            tracing::info!("stale_scanner: started (interval={}ms)", interval.as_millis());
            // Initial delay — wait for Zed to fully initialize
            std::thread::sleep(Duration::from_secs(5));

            loop {
                std::thread::sleep(interval);
                scan_all_threads();
            }
        })
        .expect("failed to spawn stale scanner thread");
}

fn scan_all_threads() {
    let threads = entry_scanner::known_threads();
    if threads.is_empty() {
        return;
    }

    let mut total_approved: u64 = 0;

    for self_ptr in &threads {
        // scan_and_approve_from_scanner is only called from this thread — no lock contention
        let approved = unsafe { entry_scanner::scan_and_approve_from_scanner(*self_ptr) };
        total_approved += approved;
    }

    if total_approved > 0 {
        entry_scanner::SCANNER_APPROVAL_COUNT.fetch_add(total_approved, Ordering::Relaxed);
        tracing::info!(
            "stale_scanner: sweep complete — approved {total_approved} entries across {} threads",
            threads.len()
        );
    }
}
