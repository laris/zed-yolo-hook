//! Hook implementations for auto-approving Zed tool calls.
//!
//! Modules:
//!   - `permission_decision`   — hooks `ToolPermissionDecision::from_input` (native tools)
//!   - `tool_authorization`    — hooks `request_tool_call_authorization` (ACP agents, primary)
//!   - `upsert_hook`           — hooks `upsert_tool_call_inner` (approach 1: catch all insertions)
//!   - `session_update_hook`   — hooks `handle_session_update` (approach 2: catch session restore)
//!   - `push_entry_hook`       — hooks `push_entry` (catch-all: every entry insertion path)
//!   - `stale_scanner`         — periodic background scan (approach 3: catch stale entries)
//!   - `entry_scanner`         — shared scanning logic used by approaches 1-3

pub mod entry_scanner;
pub mod permission_decision;
pub mod push_entry_hook;
pub mod session_update_hook;
pub mod stale_scanner;
pub mod tool_authorization;
pub mod upsert_hook;

use std::sync::atomic::AtomicU64;

/// Counter for permission_decision hook invocations (PATH 1).
pub static PERMISSION_DECISION_COUNT: AtomicU64 = AtomicU64::new(0);

/// Counter for tool_authorization hook invocations (PATH 2).
pub static TOOL_AUTHORIZATION_COUNT: AtomicU64 = AtomicU64::new(0);

/// Counter for tool_authorization misses (no WaitingForConfirmation found).
pub static TOOL_AUTHORIZATION_MISS_COUNT: AtomicU64 = AtomicU64::new(0);

/// Counter for misses recovered by retry.
pub static TOOL_AUTHORIZATION_RETRY_SUCCESS_COUNT: AtomicU64 = AtomicU64::new(0);
