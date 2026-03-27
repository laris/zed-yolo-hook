//! Hook for `AcpThread::request_tool_call_authorization` (external ACP agents).
//!
//! After the original function creates a oneshot channel and stores `respond_tx`
//! in `AcpThread.entries`, we walk the entries Vec to find it, then send the
//! matching "allow once" outcome directly in `on_leave`.
//!
//! Sending inline is safe: `oneshot::Sender::send()` is an atomic write + receiver
//! wake. The receiver Future is polled later by Zed's async executor — it never
//! synchronously re-enters `request_tool_call_authorization`.
//!
//! Memory layout (from disassembly of Zed Preview v0.230.0 aarch64):
//!   AcpThread + 0x90 = entries.ptr
//!   AcpThread + 0x98 = entries.len
//!   Each entry = 0x1c0 (448) bytes
//!   entry[0x00] = AgentThreadEntry discriminant (0x2 = ToolCall)
//!   entry[0x118] = ToolCallStatus payload head
//!                WaitingForConfirmation is niche-encoded:
//!                payload head < 0x8000_0000_0000_0002
//!   entry[0x160] = respond_tx (pointer to oneshot::Sender on heap)
//!
//! The v0.228.x layout is retained as a fallback because the exported symbol
//! stayed stable across the upgrade.

use agent_client_protocol as acp;
use std::cell::Cell;
use std::slice;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use super::TOOL_AUTHORIZATION_COUNT;

// ---- AcpThread offsets (stable across v0.228.x -> v0.230.0) ----
const ENTRIES_PTR_OFFSET: usize = 0x90; // Vec<AgentThreadEntry>.ptr
const ENTRIES_LEN_OFFSET: usize = 0x98; // Vec<AgentThreadEntry>.len
const ENTRY_DISCRIMINANT_OFFSET: usize = 0x00; // AgentThreadEntry variant tag
const ARC_INNER_DATA_OFFSET: usize = 0x10; // ArcInner<T> header = strong + weak
const TOOL_CALL_UPDATE_ID_PTR_OFFSET_V230: usize = 0x128; // ToolCallUpdate.tool_call_id.ptr
const TOOL_CALL_UPDATE_ID_LEN_OFFSET_V230: usize = 0x130; // ToolCallUpdate.tool_call_id.len

#[derive(Clone, Copy, Debug)]
enum SendStyle {
    LegacyOptionId,
    SelectedOutcome,
}

#[derive(Clone, Copy, Debug)]
enum MatchStyle {
    // Preview 0.230.x:
    // - AgentThreadEntry::ToolCall discriminant = 0x2
    // - ToolCall.id at entry+0x168 / +0x170
    // - ToolCallStatus::WaitingForConfirmation is niche-encoded:
    //   any payload head < 0x8000_0000_0000_0002
    Preview230 {
        toolcall_variant: u64,
        id_ptr_offset: usize,
        id_len_offset: usize,
        waiting_payload_niche_start: u64,
    },
    LegacyExact {
        toolcall_variant: u64,
        waiting_variant: u64,
    },
}

#[derive(Clone, Copy, Debug)]
struct EntryLayout {
    name: &'static str,
    entry_size: usize,
    status_offset: usize,
    respond_tx_offset: usize,
    send_style: SendStyle,
    match_style: MatchStyle,
}

const ENTRY_LAYOUTS: &[EntryLayout] = &[
    // Zed Preview 0.230.0 9437a84390a396d666f04b38db87d89bb07284c1
    EntryLayout {
        name: "v0.230.x",
        entry_size: 0x1c0,
        status_offset: 0x118,
        respond_tx_offset: 0x160,
        send_style: SendStyle::SelectedOutcome,
        match_style: MatchStyle::Preview230 {
            toolcall_variant: 0x02,
            id_ptr_offset: 0x168,
            id_len_offset: 0x170,
            waiting_payload_niche_start: 0x8000_0000_0000_0002,
        },
    },
    // Zed Preview 0.228.x / 0.229.x
    EntryLayout {
        name: "v0.228.x",
        entry_size: 0x1b0,
        status_offset: 0x48,
        respond_tx_offset: 0x68,
        send_style: SendStyle::LegacyOptionId,
        match_style: MatchStyle::LegacyExact {
            toolcall_variant: 0x07,
            waiting_variant: 0x00,
        },
    },
];

// ---- Thread-local for capturing `self` pointer ----
#[derive(Clone, Copy, Debug, Default)]
struct ArcStrRef {
    ptr: u64,
    len: u64,
}

thread_local! {
    static SAVED_SELF: Cell<u64> = const { Cell::new(0) };
    static SAVED_TOOL_CALL_ID: Cell<ArcStrRef> = const { Cell::new(ArcStrRef { ptr: 0, len: 0 }) };
}

// ---- ACP outcome shim for Zed Preview 0.230.x ----

#[allow(dead_code)]
#[derive(Debug)]
enum SelectedPermissionParams {
    Terminal { patterns: Vec<String> },
}

#[derive(Debug)]
struct SelectedPermissionOutcome {
    option_id: acp::PermissionOptionId,
    option_kind: acp::PermissionOptionKind,
    params: Option<SelectedPermissionParams>,
}

// Match the layout observed in the Preview 0.230.0 binary:
// - sizeof = 0x30
// - params offset = 0x00
// - option_id offset = 0x18
// - option_kind offset = 0x28
const _: [(); 0x30] = [(); std::mem::size_of::<SelectedPermissionOutcome>()];
const _: [(); 0x00] = [(); std::mem::offset_of!(SelectedPermissionOutcome, params)];
const _: [(); 0x18] = [(); std::mem::offset_of!(SelectedPermissionOutcome, option_id)];
const _: [(); 0x28] = [(); std::mem::offset_of!(SelectedPermissionOutcome, option_kind)];

// ---- Inline send helpers ----

unsafe fn bump_sender_refcount(sender_arc_ptr: u64) {
    let strong = sender_arc_ptr as *const std::sync::atomic::AtomicUsize;
    unsafe { (*strong).fetch_add(1, Ordering::Relaxed) };
}

unsafe fn looks_like_arc_allocation(arc_ptr: u64) -> bool {
    if arc_ptr <= 0x1_0000_0000 {
        return false;
    }

    let p = arc_ptr as *const usize;
    let strong = unsafe { *p };
    let weak = unsafe { *p.add(1) };

    (1..=64).contains(&strong) && (1..=64).contains(&weak)
}

unsafe fn looks_like_sender_arc(sender_arc_ptr: u64) -> bool {
    unsafe { looks_like_arc_allocation(sender_arc_ptr) }
}

unsafe fn looks_like_arc_str(value: ArcStrRef) -> bool {
    value.len != 0 && value.len <= 4096 && unsafe { looks_like_arc_allocation(value.ptr) }
}

unsafe fn arc_str_eq(a: ArcStrRef, b: ArcStrRef) -> bool {
    if a.len != b.len || !unsafe { looks_like_arc_str(a) } || !unsafe { looks_like_arc_str(b) } {
        return false;
    }

    let len = a.len as usize;
    let a_bytes =
        unsafe { slice::from_raw_parts((a.ptr + ARC_INNER_DATA_OFFSET as u64) as *const u8, len) };
    let b_bytes =
        unsafe { slice::from_raw_parts((b.ptr + ARC_INNER_DATA_OFFSET as u64) as *const u8, len) };
    a_bytes == b_bytes
}

unsafe fn read_arc_str(ptr: u64, ptr_offset: usize, len_offset: usize) -> ArcStrRef {
    if ptr == 0 {
        return ArcStrRef::default();
    }

    let p = ptr as *const u64;
    ArcStrRef {
        ptr: unsafe { *p.byte_add(ptr_offset) },
        len: unsafe { *p.byte_add(len_offset) },
    }
}

unsafe fn read_tool_call_id_v230(tool_call_update_ptr: u64) -> ArcStrRef {
    unsafe {
        read_arc_str(
            tool_call_update_ptr,
            TOOL_CALL_UPDATE_ID_PTR_OFFSET_V230,
            TOOL_CALL_UPDATE_ID_LEN_OFFSET_V230,
        )
    }
}

/// Reconstruct the legacy `oneshot::Sender<PermissionOptionId>` and send `"allow"`.
///
/// The respond_tx value at entry+0x68 is the Arc<Inner<T>> pointer inside the
/// Sender<PermissionOptionId>. We reconstruct a Sender from it and call .send().
///
/// Key: Sender<T> is just `{ inner: Arc<Inner<T>> }`. Since we have the Arc pointer,
/// we can reconstruct the Sender via transmute. We bump the Arc refcount first to
/// avoid double-free (the entry still holds its copy).
///
/// # Safety
/// `sender_arc_ptr` must point to a valid Arc<Inner<Sender<Arc<str>>>>.
unsafe fn send_allow_legacy(sender_arc_ptr: u64, count: u64) -> bool {
    // Build PermissionOptionId("allow") — same type as what Zed uses.
    // PermissionOptionId is #[repr(transparent)] around Arc<str>.
    let option_id: Arc<str> = Arc::from("allow");

    unsafe { bump_sender_refcount(sender_arc_ptr) };

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

/// Reconstruct the current `oneshot::Sender<SelectedPermissionOutcome>` and
/// synthesize an `AllowOnce` selection.
///
/// # Safety
/// `sender_arc_ptr` must point to a valid Arc<Inner<Sender<SelectedPermissionOutcome>>>.
unsafe fn send_allow_once_outcome(sender_arc_ptr: u64, count: u64) -> bool {
    let outcome = SelectedPermissionOutcome {
        option_id: acp::PermissionOptionId::new("allow"),
        option_kind: acp::PermissionOptionKind::AllowOnce,
        params: None,
    };

    unsafe { bump_sender_refcount(sender_arc_ptr) };

    let sender: futures_channel::oneshot::Sender<SelectedPermissionOutcome> =
        unsafe { std::mem::transmute(sender_arc_ptr) };

    match sender.send(outcome) {
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

unsafe fn send_allow(layout: EntryLayout, sender_arc_ptr: u64, count: u64) -> bool {
    match layout.send_style {
        SendStyle::LegacyOptionId => unsafe { send_allow_legacy(sender_arc_ptr, count) },
        SendStyle::SelectedOutcome => unsafe { send_allow_once_outcome(sender_arc_ptr, count) },
    }
}

fn find_waiting_sender(
    entries_ptr: u64,
    entries_len: u64,
    layout: EntryLayout,
    current_call_id: ArcStrRef,
    count: u64,
) -> Option<u64> {
    for i in (0..entries_len).rev() {
        let entry = entries_ptr + (i * layout.entry_size as u64);
        let discriminant = unsafe { *(entry as *const u64).byte_add(ENTRY_DISCRIMINANT_OFFSET) };

        match layout.match_style {
            MatchStyle::Preview230 {
                toolcall_variant,
                id_ptr_offset,
                id_len_offset,
                waiting_payload_niche_start,
            } => {
                if discriminant != toolcall_variant {
                    continue;
                }

                let status_head = unsafe { *(entry as *const u64).byte_add(layout.status_offset) };
                let entry_id = unsafe { read_arc_str(entry, id_ptr_offset, id_len_offset) };

                tracing::debug!(
                    "tool_authorization #{count}: layout={} entry[{i}] disc={discriminant:#x} status_head={status_head:#x} id_len={}",
                    layout.name,
                    entry_id.len
                );

                if !unsafe { arc_str_eq(entry_id, current_call_id) } {
                    continue;
                }

                if status_head >= waiting_payload_niche_start {
                    tracing::warn!(
                        "tool_authorization #{count}: matched call id in {} entry[{i}] but status_head={status_head:#x} is not WaitingForConfirmation",
                        layout.name
                    );
                    continue;
                }

                let tx = unsafe { *(entry as *const u64).byte_add(layout.respond_tx_offset) };
                if unsafe { looks_like_sender_arc(tx) } {
                    tracing::info!(
                        "tool_authorization #{count}: matched {} entry[{i}] by ToolCallId, respond_tx={tx:#x}",
                        layout.name
                    );
                    return Some(tx);
                }

                tracing::warn!(
                    "tool_authorization #{count}: matched call id in {} entry[{i}] but respond_tx={tx:#x} did not look like a oneshot sender",
                    layout.name
                );
            }
            MatchStyle::LegacyExact {
                toolcall_variant,
                waiting_variant,
            } => {
                let status = unsafe { *(entry as *const u64).byte_add(layout.status_offset) };

                tracing::debug!(
                    "tool_authorization #{count}: layout={} entry[{i}] disc={discriminant:#x} status={status:#x}",
                    layout.name
                );

                if discriminant != toolcall_variant || status != waiting_variant {
                    continue;
                }

                let tx = unsafe { *(entry as *const u64).byte_add(layout.respond_tx_offset) };
                if unsafe { looks_like_sender_arc(tx) } {
                    tracing::info!(
                        "tool_authorization #{count}: matched layout {} at entry[{i}], respond_tx={tx:#x}",
                        layout.name
                    );
                    return Some(tx);
                }
            }
        }
    }

    None
}

// ---- InvocationListener ----

pub struct Listener;

impl frida_gum::interceptor::InvocationListener for Listener {
    fn on_enter(&mut self, context: frida_gum::interceptor::InvocationContext) {
        let cpu = context.cpu_context();
        let self_ptr = cpu.reg(0);
        let tool_call_update_ptr = cpu.reg(1);

        SAVED_SELF.with(|c| c.set(self_ptr));
        SAVED_TOOL_CALL_ID.with(|c| {
            let call_id = unsafe { read_tool_call_id_v230(tool_call_update_ptr) };
            c.set(call_id);
        });
    }

    fn on_leave(&mut self, _context: frida_gum::interceptor::InvocationContext) {
        let t0 = Instant::now();
        let count = TOOL_AUTHORIZATION_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let self_ptr = SAVED_SELF.with(|c| c.get());
        let current_call_id = SAVED_TOOL_CALL_ID.with(|c| c.get());

        tracing::debug!(
            "tool_authorization #{count}: on_leave (self={self_ptr:#x}, call_id_ptr={:#x}, call_id_len={})",
            current_call_id.ptr,
            current_call_id.len
        );

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

        let Some((layout, respond_tx)) = ENTRY_LAYOUTS.iter().copied().find_map(|layout| {
            find_waiting_sender(entries_ptr, entries_len, layout, current_call_id, count)
                .map(|tx| (layout, tx))
        }) else {
            tracing::warn!("tool_authorization #{count}: no WaitingForConfirmation entry found");
            return;
        };

        // Send directly — no dispatch_async_f needed.
        // oneshot::Sender::send() is an atomic write + wake; the receiver Future
        // is polled later by Zed's executor, no re-entrancy risk.
        let ok = unsafe { send_allow(layout, respond_tx, count) };
        let elapsed_us = t0.elapsed().as_micros();

        if ok {
            tracing::info!(
                "tool_authorization #{count}: approved in {elapsed_us}us via {}",
                layout.name
            );
        }
    }
}

/// Symbol search patterns for locating `AcpThread::request_tool_call_authorization` in Zed's binary.
pub const SYMBOL_INCLUDE: &[&str] = &["acp_thread", "AcpThread", "request_tool_call_authorization"];
pub const SYMBOL_EXCLUDE: &[&str] = &["drop_in_place", "closure", "vtable", "island", "spawn"];
