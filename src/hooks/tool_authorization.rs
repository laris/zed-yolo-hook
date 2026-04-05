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
//! ## ExitPlanMode detection
//!
//! ExitPlanMode ("Ready to code?") and regular tool permissions both flow through
//! this function but expect different option_ids:
//!
//! - Regular tools: option_id="allow" or "allow_always"
//! - ExitPlanMode: option_id="acceptEdits", "bypassPermissions", "default", or "plan"
//!
//! We attempt to detect ExitPlanMode by reading the first PermissionOption's
//! option_id from the WaitingForConfirmation entry. If it looks like a mode name
//! (not "allow"/"allow_always"/"reject"), we send the configured `plan_option`.
//!
//! ## Memory layout (from disassembly of Zed Preview v0.230.0 aarch64):
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
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::{
    TOOL_AUTHORIZATION_COUNT, TOOL_AUTHORIZATION_MISS_COUNT,
    TOOL_AUTHORIZATION_RETRY_SUCCESS_COUNT,
};
use crate::config::{PlanOption, ToolOption};
use crate::CONFIG;

// ---- AcpThread offsets (stable across v0.228.x -> v0.230.0) ----
const ENTRIES_PTR_OFFSET: usize = 0x90; // Vec<AgentThreadEntry>.ptr
const ENTRIES_LEN_OFFSET: usize = 0x98; // Vec<AgentThreadEntry>.len
const ENTRY_DISCRIMINANT_OFFSET: usize = 0x00; // AgentThreadEntry variant tag
const ARC_INNER_DATA_OFFSET: usize = 0x10; // ArcInner<T> header = strong + weak
const TOOL_CALL_UPDATE_ID_PTR_OFFSET_V230: usize = 0x128; // ToolCallUpdate.tool_call_id.ptr
const TOOL_CALL_UPDATE_ID_LEN_OFFSET_V230: usize = 0x130; // ToolCallUpdate.tool_call_id.len

#[derive(Clone, Copy, Debug)]
pub(crate) enum SendStyle {
    LegacyOptionId,
    SelectedOutcome,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum MatchStyle {
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
pub(crate) struct EntryLayout {
    pub(crate) name: &'static str,
    pub(crate) entry_size: usize,
    pub(crate) status_offset: usize,
    pub(crate) respond_tx_offset: usize,
    pub(crate) send_style: SendStyle,
    pub(crate) match_style: MatchStyle,
}

pub(crate) const ENTRY_LAYOUTS: &[EntryLayout] = &[
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

/// Try to read the Arc<str> content as a UTF-8 string (for diagnostics).
unsafe fn arc_str_to_string(value: ArcStrRef) -> Option<String> {
    if !unsafe { looks_like_arc_str(value) } {
        return None;
    }
    let len = value.len as usize;
    let bytes = unsafe {
        slice::from_raw_parts(
            (value.ptr + ARC_INNER_DATA_OFFSET as u64) as *const u8,
            len,
        )
    };
    std::str::from_utf8(bytes).ok().map(|s| s.to_string())
}

// ---- Option-aware outcome builders ----

/// Build the outcome for a regular tool permission (Scenario C).
fn build_tool_outcome(tool_option: ToolOption) -> SelectedPermissionOutcome {
    match tool_option {
        ToolOption::Allow => SelectedPermissionOutcome {
            option_id: acp::PermissionOptionId::new("allow"),
            option_kind: acp::PermissionOptionKind::AllowOnce,
            params: None,
        },
        ToolOption::AllowAlways => SelectedPermissionOutcome {
            option_id: acp::PermissionOptionId::new("allow_always"),
            option_kind: acp::PermissionOptionKind::AllowAlways,
            params: None,
        },
    }
}

/// Build the outcome for an ExitPlanMode prompt (Scenario A).
fn build_plan_outcome(plan_option: PlanOption) -> SelectedPermissionOutcome {
    match plan_option {
        PlanOption::AcceptEdits => SelectedPermissionOutcome {
            option_id: acp::PermissionOptionId::new("acceptEdits"),
            option_kind: acp::PermissionOptionKind::AllowAlways,
            params: None,
        },
        PlanOption::BypassPermissions => SelectedPermissionOutcome {
            option_id: acp::PermissionOptionId::new("bypassPermissions"),
            option_kind: acp::PermissionOptionKind::AllowAlways,
            params: None,
        },
        PlanOption::Default => SelectedPermissionOutcome {
            option_id: acp::PermissionOptionId::new("default"),
            option_kind: acp::PermissionOptionKind::AllowOnce,
            params: None,
        },
        PlanOption::Plan => SelectedPermissionOutcome {
            option_id: acp::PermissionOptionId::new("plan"),
            option_kind: acp::PermissionOptionKind::RejectOnce,
            params: None,
        },
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
/// send the given outcome.
///
/// # Safety
/// `sender_arc_ptr` must point to a valid Arc<Inner<Sender<SelectedPermissionOutcome>>>.
unsafe fn send_outcome(sender_arc_ptr: u64, outcome: SelectedPermissionOutcome, count: u64) -> bool {
    unsafe { bump_sender_refcount(sender_arc_ptr) };

    let sender: futures_channel::oneshot::Sender<SelectedPermissionOutcome> =
        unsafe { std::mem::transmute(sender_arc_ptr) };

    let option_id_str = format!("{:?}", outcome.option_id);
    match sender.send(outcome) {
        Ok(()) => {
            tracing::info!("tool_authorization #{count}: send succeeded (option_id={option_id_str})");
            true
        }
        Err(_) => {
            tracing::warn!("tool_authorization #{count}: send failed (receiver dropped?)");
            false
        }
    }
}

unsafe fn send_allow(layout: EntryLayout, sender_arc_ptr: u64, is_plan_mode: bool, count: u64) -> bool {
    match layout.send_style {
        SendStyle::LegacyOptionId => unsafe { send_allow_legacy(sender_arc_ptr, count) },
        SendStyle::SelectedOutcome => {
            let config = CONFIG.get();
            let outcome = if is_plan_mode {
                let plan_opt = config.map(|c| c.plan_option).unwrap_or(PlanOption::AcceptEdits);
                tracing::info!("tool_authorization #{count}: ExitPlanMode detected, plan_option={:?}", plan_opt);
                build_plan_outcome(plan_opt)
            } else {
                let tool_opt = config.map(|c| c.tool_option).unwrap_or(ToolOption::Allow);
                build_tool_outcome(tool_opt)
            };
            unsafe { send_outcome(sender_arc_ptr, outcome, count) }
        }
    }
}

/// Attempt to detect if this is an ExitPlanMode prompt by reading the first
/// PermissionOption's option_id from the WaitingForConfirmation entry.
///
/// ExitPlanMode options start with "bypassPermissions" or "acceptEdits" (never "allow").
/// Regular tool options start with "allow_always" (never "acceptEdits").
///
/// Returns `true` if this appears to be an ExitPlanMode prompt.
///
/// This is best-effort: if pointer validation fails, returns `false` (conservative,
/// falls back to regular tool behavior).
unsafe fn detect_plan_mode(entry: u64, layout: &EntryLayout, count: u64) -> bool {
    // Only supported for v0.230.x layout with known status structure.
    // The PermissionOptions enum is at the start of the WaitingForConfirmation payload.
    // PermissionOptions::Flat(Vec<PermissionOption>) has discriminant 0, followed by Vec.
    //
    // Status layout within WaitingForConfirmation (entry + status_offset):
    //   +0x00: niche/status head (used for waiting detection)
    //   +0x08: PermissionOptions discriminant (0 = Flat)
    //   +0x10: Vec<PermissionOption>.ptr
    //   +0x18: Vec<PermissionOption>.len
    //   ...
    //   +0x48: respond_tx
    //
    // This is speculative — based on typical Rust enum layout for:
    //   enum PermissionOptions { Flat(Vec<T>), Dropdown(Vec<U>), DropdownWithPatterns{...} }
    // where the largest variant determines the enum size.
    //
    // If the offsets are wrong, pointer validation will fail and we'll return false.

    let status_base = entry + layout.status_offset as u64;

    // Read what we think is the PermissionOptions discriminant
    let perm_opts_disc = unsafe { *((status_base + 0x08) as *const u64) };

    // Flat variant should be discriminant 0
    if perm_opts_disc != 0 {
        tracing::debug!(
            "tool_authorization #{count}: PermissionOptions discriminant={perm_opts_disc:#x} (not Flat), skipping plan detection"
        );
        return false;
    }

    // Read Vec<PermissionOption> ptr and len
    let vec_ptr = unsafe { *((status_base + 0x10) as *const u64) };
    let vec_len = unsafe { *((status_base + 0x18) as *const u64) };

    if vec_ptr == 0 || vec_len == 0 || vec_len > 10 {
        tracing::debug!(
            "tool_authorization #{count}: options vec ptr={vec_ptr:#x} len={vec_len} — invalid, skipping plan detection"
        );
        return false;
    }

    // Read the first PermissionOption's option_id (Arc<str>).
    // PermissionOption layout (speculative, based on field types):
    //   +0x00: option_id: PermissionOptionId (= Arc<str>, 16 bytes: ptr + len)
    //   +0x10: name: String (24 bytes: ptr + len + cap)
    //   +0x28: kind: PermissionOptionKind (1-4 bytes + padding)
    //   +0x30: meta: Option<Map<String,Value>> (...)
    //
    // We only need the first option_id to distinguish tool vs plan.
    let first_opt_id = unsafe { read_arc_str(vec_ptr, 0x00, 0x08) };

    if let Some(id_str) = unsafe { arc_str_to_string(first_opt_id) } {
        tracing::debug!(
            "tool_authorization #{count}: first option_id = \"{id_str}\""
        );
        // ExitPlanMode options: "bypassPermissions", "acceptEdits", "default", "plan"
        // Regular tool options: "allow_always", "allow", "reject"
        let is_plan = matches!(
            id_str.as_str(),
            "bypassPermissions" | "acceptEdits" | "default" | "plan"
        );
        if is_plan {
            tracing::info!(
                "tool_authorization #{count}: detected ExitPlanMode (first_option=\"{id_str}\")"
            );
        }
        return is_plan;
    }

    tracing::debug!(
        "tool_authorization #{count}: could not read first option_id, assuming regular tool"
    );
    false
}

fn find_waiting_sender(
    entries_ptr: u64,
    entries_len: u64,
    layout: EntryLayout,
    current_call_id: ArcStrRef,
    count: u64,
) -> Option<(u64, bool)> {
    // Returns (respond_tx, is_plan_mode)
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
                    // Attempt ExitPlanMode detection
                    let is_plan = unsafe { detect_plan_mode(entry, &layout, count) };

                    tracing::info!(
                        "tool_authorization #{count}: matched {} entry[{i}] by ToolCallId, respond_tx={tx:#x}, plan_mode={is_plan}",
                        layout.name
                    );
                    return Some((tx, is_plan));
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
                    // Legacy layout: no plan detection, assume regular tool
                    return Some((tx, false));
                }
            }
        }
    }

    None
}

// ---- Diagnostics for missed approvals ----

fn diagnose_miss(
    entries_ptr: u64,
    entries_len: u64,
    current_call_id: ArcStrRef,
    count: u64,
) {
    // Collect diagnostic info about why the entry wasn't found
    let mut toolcall_count: u64 = 0;
    let mut id_matched_count: u64 = 0;
    let mut id_matched_statuses: Vec<u64> = Vec::new();
    let mut disc_counts: std::collections::HashMap<u64, u64> = std::collections::HashMap::new();

    // Only scan with the primary layout (v0.230.x)
    let layout = &ENTRY_LAYOUTS[0];

    for i in 0..entries_len {
        let entry = entries_ptr + (i * layout.entry_size as u64);
        let discriminant = unsafe { *(entry as *const u64).byte_add(ENTRY_DISCRIMINANT_OFFSET) };
        *disc_counts.entry(discriminant).or_insert(0) += 1;

        if let MatchStyle::Preview230 {
            toolcall_variant,
            id_ptr_offset,
            id_len_offset,
            ..
        } = layout.match_style
        {
            if discriminant == toolcall_variant {
                toolcall_count += 1;
                let entry_id = unsafe { read_arc_str(entry, id_ptr_offset, id_len_offset) };
                if unsafe { arc_str_eq(entry_id, current_call_id) } {
                    id_matched_count += 1;
                    let status_head =
                        unsafe { *(entry as *const u64).byte_add(layout.status_offset) };
                    id_matched_statuses.push(status_head);
                }
            }
        }
    }

    let call_id_str = unsafe { arc_str_to_string(current_call_id) }
        .unwrap_or_else(|| "<unreadable>".to_string());

    if id_matched_count > 0 {
        tracing::warn!(
            "tool_authorization #{count}: MISS — entries={entries_len} toolcalls={toolcall_count} \
             call_id=\"{call_id_str}\" id_matched={id_matched_count} statuses={id_matched_statuses:?} \
             → call_id found but status is not WaitingForConfirmation (already resolved?)"
        );
    } else if toolcall_count > 0 {
        tracing::warn!(
            "tool_authorization #{count}: MISS — entries={entries_len} toolcalls={toolcall_count} \
             call_id=\"{call_id_str}\" → call_id not found in any ToolCall entry (race condition: not yet upserted)"
        );
    } else {
        let disc_summary: Vec<String> = disc_counts
            .iter()
            .map(|(d, c)| format!("{d:#x}:{c}"))
            .collect();
        tracing::warn!(
            "tool_authorization #{count}: MISS — entries={entries_len} toolcalls=0 \
             discriminants=[{}] → no ToolCall entries found (layout drift?)",
            disc_summary.join(",")
        );
    }
}

// ---- InvocationListener ----

pub struct Listener;

impl frida_gum::interceptor::InvocationListener for Listener {
    fn on_enter(&mut self, context: frida_gum::interceptor::InvocationContext) {
        let cpu = context.cpu_context();
        let self_ptr = cpu.reg(0);
        let tool_call_update_ptr = cpu.reg(1);

        SAVED_SELF.with(|c| c.set(self_ptr));
        // Register this AcpThread for periodic scanning
        super::entry_scanner::register_thread(self_ptr);
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

        // Session tag: short identifier derived from AcpThread pointer.
        // Each workspace's agent session gets its own AcpThread instance,
        // so this distinguishes multi-workspace tool calls in the log.
        let session_tag = format!("{:04x}", self_ptr & 0xFFFF);

        // Read tool_call_id string for log correlation
        let call_id_str = unsafe { arc_str_to_string(current_call_id) }
            .unwrap_or_default();
        // Truncate long IDs for log readability (tool_call_ids are often UUIDs)
        let call_id_short = if call_id_str.len() > 12 {
            &call_id_str[..12]
        } else {
            &call_id_str
        };

        tracing::debug!(
            "tool_authorization #{count} [s:{session_tag}]: on_leave (self={self_ptr:#x}, call_id=\"{call_id_short}\")"
        );

        if self_ptr == 0 {
            tracing::warn!("tool_authorization #{count} [s:{session_tag}]: self_ptr is null, skipping");
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
            "tool_authorization #{count} [s:{session_tag}]: entries ptr={entries_ptr:#x}, len={entries_len}"
        );

        if entries_ptr == 0 || entries_len == 0 {
            tracing::warn!("tool_authorization #{count} [s:{session_tag}]: no entries, skipping");
            return;
        }

        // First attempt
        if let Some((layout, respond_tx, is_plan)) = try_find_sender(entries_ptr, entries_len, current_call_id, count) {
            let ok = unsafe { send_allow(layout, respond_tx, is_plan, count) };
            let elapsed_us = t0.elapsed().as_micros();
            if ok {
                tracing::info!(
                    "tool_authorization #{count} [s:{session_tag}]: approved in {elapsed_us}us via {} call_id=\"{call_id_short}\"",
                    layout.name
                );
            }
            log_stats(count);
            return;
        }

        // Single retry after configurable delay
        let retry_delay = CONFIG
            .get()
            .map(|c| c.retry_delay_us)
            .unwrap_or(1500);

        if retry_delay > 0 {
            std::thread::sleep(std::time::Duration::from_micros(retry_delay));

            // Re-read entries (Vec may have grown)
            let (entries_ptr2, entries_len2) = unsafe {
                let p = self_ptr as *const u64;
                let ptr = *p.byte_add(ENTRIES_PTR_OFFSET);
                let len = *p.byte_add(ENTRIES_LEN_OFFSET);
                (ptr, len)
            };

            if let Some((layout, respond_tx, is_plan)) = try_find_sender(entries_ptr2, entries_len2, current_call_id, count) {
                TOOL_AUTHORIZATION_RETRY_SUCCESS_COUNT.fetch_add(1, Ordering::Relaxed);
                let ok = unsafe { send_allow(layout, respond_tx, is_plan, count) };
                let elapsed_us = t0.elapsed().as_micros();
                if ok {
                    tracing::info!(
                        "tool_authorization #{count} [s:{session_tag}]: approved on RETRY in {elapsed_us}us via {} call_id=\"{call_id_short}\" (delay={retry_delay}us)",
                        layout.name
                    );
                }
                log_stats(count);
                return;
            }
        }

        // Both attempts failed — log diagnostics
        TOOL_AUTHORIZATION_MISS_COUNT.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            "tool_authorization #{count} [s:{session_tag}]: MISS for call_id=\"{call_id_short}\""
        );
        diagnose_miss(entries_ptr, entries_len, current_call_id, count);
        log_stats(count);
    }
}

fn try_find_sender(
    entries_ptr: u64,
    entries_len: u64,
    current_call_id: ArcStrRef,
    count: u64,
) -> Option<(EntryLayout, u64, bool)> {
    ENTRY_LAYOUTS.iter().copied().find_map(|layout| {
        find_waiting_sender(entries_ptr, entries_len, layout, current_call_id, count)
            .map(|(tx, is_plan)| (layout, tx, is_plan))
    })
}

fn log_stats(count: u64) {
    // Log summary stats every 50 approvals or on any miss
    if count % 50 == 0 {
        let approved = count
            - TOOL_AUTHORIZATION_MISS_COUNT.load(Ordering::Relaxed);
        let missed = TOOL_AUTHORIZATION_MISS_COUNT.load(Ordering::Relaxed);
        let retried = TOOL_AUTHORIZATION_RETRY_SUCCESS_COUNT.load(Ordering::Relaxed);
        tracing::info!(
            "tool_authorization stats: total={count} approved={approved} missed={missed} retry_recovered={retried}"
        );
    }
}

/// Symbol search patterns for locating `AcpThread::request_tool_call_authorization` in Zed's binary.
pub const SYMBOL_INCLUDE: &[&str] = &["acp_thread", "AcpThread", "request_tool_call_authorization"];
pub const SYMBOL_EXCLUDE: &[&str] = &["drop_in_place", "closure", "vtable", "island", "spawn"];

// ---- pub(crate) wrappers for entry_scanner ----

pub(crate) unsafe fn looks_like_sender_arc_pub(ptr: u64) -> bool {
    unsafe { looks_like_sender_arc(ptr) }
}

pub(crate) unsafe fn detect_plan_mode_pub(entry: u64, layout: &EntryLayout) -> bool {
    // Simplified: only call for v0.230.x layout
    unsafe { detect_plan_mode(entry, layout, 0) }
}

pub(crate) unsafe fn send_allow_pub(send_style: SendStyle, sender_arc_ptr: u64, is_plan: bool, count: u64) -> bool {
    // Create a minimal layout to pass to send_allow
    let layout = ENTRY_LAYOUTS[0]; // v0.230.x
    // Override send_style if needed
    let mut l = layout;
    l.send_style = send_style;
    unsafe { send_allow(l, sender_arc_ptr, is_plan, count) }
}
