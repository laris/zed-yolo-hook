//! Hook implementations for auto-approving Zed tool calls.
//!
//! Module naming convention (Option A — named after hooked Zed symbols):
//!   - `permission_decision`  — hooks `ToolPermissionDecision::from_input`
//!   - `tool_authorization`   — hooks `request_tool_call_authorization`
//!
//! Alternative naming options considered:
//!   - Option B (by caller type): `builtin_tools`, `acp_agents`
//!   - Option C (with _hook suffix): `decision_hook`, `authorization_hook`

pub mod permission_decision;
pub mod tool_authorization;

use std::sync::atomic::AtomicU64;

/// Counter for permission_decision hook invocations (PATH 1).
pub static PERMISSION_DECISION_COUNT: AtomicU64 = AtomicU64::new(0);

/// Counter for tool_authorization hook invocations (PATH 2).
pub static TOOL_AUTHORIZATION_COUNT: AtomicU64 = AtomicU64::new(0);
