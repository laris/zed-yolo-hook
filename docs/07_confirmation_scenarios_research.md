# Research: LLM Agent Confirmation Scenarios & Improvement Opportunities

> Date: 2026-02-28
> Scope: Analysis of three confirmation/blocking scenarios that interrupt autonomous LLM agent workflows in Zed

---

## Table of Contents

1. [Problem Statement](#1-problem-statement)
2. [Scenario Analysis](#2-scenario-analysis)
3. [Current Hook Coverage](#3-current-hook-coverage)
4. [Zed Source Code Analysis](#4-zed-source-code-analysis)
5. [Public Discussions & Evolution](#5-public-discussions--evolution)
6. [Improvement Opportunities](#6-improvement-opportunities)
7. [References](#7-references)

---

## 1. Problem Statement

When using LLM coding agents (Claude Code, Gemini, Codex) in Zed's Agent Panel, there are **three distinct types of confirmation dialogs** that block autonomous operation:

| # | Scenario | Trigger | Frequency |
|---|----------|---------|-----------|
| A | "Ready to code?" plan confirmation | Agent presents plan, asks to proceed | Once per task |
| B | "Shall I continue with Phase N?" | Agent pauses between phases | Multiple per task |
| C | "Run Command" tool permission | Each tool call needs approval | Many per task |

The current `zed-yolo-hook` (v0.17.1) handles **Scenario C** via `request_tool_call_authorization` interception. This document researches the remaining gaps.

---

## 2. Scenario Analysis

### Scenario A: "Ready to code?" (Plan Confirmation)

**What the user sees:**
```
┌─────────────────────────────────────┐
│  ⇄  Ready to code?                 │
│                                     │
│     View Raw Input                  │
│                                     │
│  ✓✓ Yes, and auto-accept edits      │
│  ✓  Yes, and manually approve edits │
│  ✗  No, keep planning               │
│                                     │
│  ⌛ Awaiting Confirmation.           │
└─────────────────────────────────────┘
```

**Mechanism:** This is an **ACP permission prompt** (`request_permission`) sent by Claude Code. It flows through the **same** `request_tool_call_authorization` code path as tool permissions. However, the option IDs are **mode identifiers** (e.g., `"acceptEdits"`, `"dontAsk"`, `"plan"`), NOT `"allow"`.

**Key insight:** The current hook sends `"allow"` as the response. For this dialog, the valid option IDs are likely:
- `"Yes, and auto-accept edits"` → maps to `acceptEdits` or `dontAsk` mode
- `"Yes, and manually approve edits"` → maps to `default` mode
- `"No, keep planning"` → maps to `plan` mode

**The option_id values are opaque strings defined by Claude Code**, not Zed. They are passed through as `PermissionOptionId` (which is `Arc<str>`). If the hook sends `"allow"` but the valid options are mode-specific strings, the response may be silently dropped or cause unexpected behavior.

**Code path:**
```
Claude Code Process              Zed Process
    |                                |
    |--- request_permission -------->|
    |    { options: [                |
    |      {id: "opt1",              |
    |       name: "Yes, auto-accept",|--- request_tool_call_authorization()
    |       kind: AllowOnce},        |--- creates oneshot channel
    |      {id: "opt2", ...},        |--- WaitingForConfirmation state
    |      {id: "opt3", ...}         |--- UI renders buttons
    |    ]}                          |
    |                                |
    |<-- Selected("opt1") ----------|  ← hook sends "allow" here
    |                                |     but valid IDs are "opt1"/"opt2"/"opt3"
```

**Source:** `crates/agent_servers/src/acp.rs:1147-1173` (request_permission handler)
**Source:** `crates/acp_thread/src/acp_thread.rs:1756-1787` (request_tool_call_authorization)

### Scenario B: "Shall I continue with Phase N?" (Phase Continuation)

**What the user sees:**
```
Shall I continue with Phase 5 (the DB writer hook for paths_order pinning)?

  ▼ Plan                                              6/10
  ✅ Phase 4: Update hook/src/discovery.rs
  ○  Phase 5: Add DB writer hook for paths_order...
  ○  Phase 6: Update MCP (mcp/src/main.rs)...
  ○  Phase 7: Deprecate mapping file, add cleanup
  ○  Run cargo check + cargo test to verify

  [Message Claude Agent — @ to include context]
```

**Mechanism:** This is **NOT** a permission prompt. This is the LLM agent **deciding to pause** and asking the user a question in the chat. The agent stops generating and waits for user text input. There is no oneshot channel, no `WaitingForConfirmation` state, no permission dialog.

**Why it happens:** Claude Code (and other agents) are instructed to check in with the user between major phases. This is a safety/oversight feature built into the agent's system prompt, not into the Zed UI.

**Key insight:** This cannot be solved by intercepting `request_tool_call_authorization`. It requires one of:
1. Configuring Claude Code to not pause between phases (via `~/.claude/settings.json` or ACP config)
2. Using `bypassPermissions` / `dontAsk` mode (which tells Claude Code to skip confirmations)
3. Auto-injecting a continuation message when the agent pauses
4. A completely new hook targeting the message input/send mechanism

**NOT in Zed source code:** Searched for "Ready to code", "Shall I continue", "keep planning", "manually approve" — none found in Zed codebase. These strings originate from Claude Code's internal prompts.

### Scenario C: "Run Command" Tool Permission (Already Handled)

**What the user sees:**
```
┌──────────────────────────┐
│  Run Command             │
│  find /Users...          │
│                          │
│  ✓ Allow                 │
│  ✗ Reject                │
└──────────────────────────┘
```

**Mechanism:** Standard ACP tool call permission. Goes through `request_tool_call_authorization` with options like `{id: "allow", kind: AllowOnce}`.

**Status:** Fully handled by the existing `tool_authorization` hook (v0.17.1). The hook sends `"allow"` which matches the expected option_id.

---

## 3. Current Hook Coverage

| Scenario | Code Path | Hook Coverage | Gap |
|----------|-----------|---------------|-----|
| A: Plan confirmation | `request_tool_call_authorization` | **Partial** — sends `"allow"` but valid option_id may differ | Need to detect plan prompts and send correct option_id |
| B: Phase continuation | User text input (chat) | **None** — not a permission prompt | Need entirely new mechanism |
| C: Tool permission | `request_tool_call_authorization` | **Full** — `"allow"` matches expected option_id | Already working |

---

## 4. Zed Source Code Analysis

### 4.1 The Unified Permission Path

All ACP permission prompts (both plan confirmations and tool permissions) flow through one function:

```rust
// crates/acp_thread/src/acp_thread.rs:1756
pub fn request_tool_call_authorization(
    &mut self,
    tool_call: acp::ToolCallUpdate,
    options: PermissionOptions,
    cx: &mut Context<Self>,
) -> Result<Task<acp::RequestPermissionOutcome>> {
    let (tx, rx) = oneshot::channel();
    let status = ToolCallStatus::WaitingForConfirmation {
        options,
        respond_tx: tx,  // ← this is what the hook intercepts
    };
    self.upsert_tool_call_inner(tool_call, status, cx)?;
    cx.emit(AcpThreadEvent::ToolAuthorizationRequested(tool_call_id.clone()));
    // ...
}
```

The `options` parameter contains the actual option IDs. For tool permissions, options include `"allow"` / `"reject"`. For plan confirmations, options may include different IDs matching the Claude Code mode names.

### 4.2 ACP Session Modes

Zed configures the default ACP mode via `agent_servers` settings:

```rust
// crates/settings_content/src/agent.rs:349-354
/// The default mode to use for this agent.
/// Note: Not all agents support modes.
default_mode: Option<String>,
```

```rust
// crates/agent_servers/src/acp.rs:394-417
if let Some(default_mode) = self.default_mode.clone() {
    if modes_ref.available_modes.iter().any(|mode| mode.id == default_mode) {
        conn.set_session_mode(SetSessionModeRequest::new(session_id, default_mode)).await;
    }
}
```

Claude Code modes (from ACP protocol):
| Mode ID | Name | Effect |
|---------|------|--------|
| `default` | Default | Ask for all confirmations |
| `acceptEdits` | Accept Edits | Auto-accept file edits, still ask for commands |
| `plan` | Plan | Read-only analysis, no edits |
| `dontAsk` / `bypassPermissions` | Bypass Permissions | Skip all confirmation prompts |

### 4.3 Native Tool Permission System (Separate)

Zed also has a settings-based permission system for the native agent:

```json
{
  "agent": {
    "tool_permissions": {
      "default": "allow",
      "tools": {
        "terminal": {
          "default": "confirm",
          "always_allow": [{"pattern": "^cargo\\s+(build|test|check)"}],
          "always_deny": [{"pattern": "^rm\\s+-rf"}]
        }
      }
    }
  }
}
```

**Important:** This ONLY applies to the native agent. ACP agents (Claude Code) bypass this entirely — their permissions are always handled via `request_tool_call_authorization`.

### 4.4 "Awaiting Confirmation" UI Label

```rust
// crates/agent_ui/src/connection_view/thread_view.rs:4245
LoadingLabel::new("Awaiting Confirmation")
    .size(LabelSize::Small)
    .color(Color::Muted),
```

Rendered when `tool_call.status` matches `ToolCallStatus::WaitingForConfirmation { .. }`.

---

## 5. Public Discussions & Evolution

### 5.1 Key GitHub Issues

| Issue | Title | Status | Reactions | Key Insight |
|-------|-------|--------|-----------|-------------|
| [#17799](https://github.com/zed-industries/zed/issues/17799) | "AI: transform & accept all" (original YOLO request) | Closed | 10 | The first explicit "YOLO mode" request (Sep 2024) |
| [#30313](https://github.com/zed-industries/zed/issues/30313) | "Always allow" should only apply to current tool | Open | **61** | "Always Allow" accidentally enabled ALL tools globally |
| [#43322](https://github.com/zed-industries/zed/issues/43322) | Claude Agent ignores permission settings after change | Open | — | Bug: downgrading from bypass mode doesn't take effect |
| [#39392](https://github.com/zed-industries/zed/issues/39392) | "Bypass Permissions" reverts automatically | Closed | 8 | Mode didn't persist across sessions (fixed by PR #39816) |
| [#40041](https://github.com/zed-industries/zed/issues/40041) | Unable to switch to bypassPermissions | Closed | — | Requires `~/.claude/settings.json` to be configured |
| [#48992](https://github.com/zed-industries/zed/issues/48992) | bypassPermissions causes severe hallucination | Open | — | `bypassPermissions` mode causes Claude sub-agents to hallucinate |

### 5.2 Key Pull Requests

| PR | Title | Status | Author | Significance |
|----|-------|--------|--------|-------------|
| [#46284](https://github.com/zed-industries/zed/pull/46284) | Granular Tool Permission Buttons | **Merged** | @rtfeldman | Per-tool regex permissions for native agent |
| [#48553](https://github.com/zed-industries/zed/pull/48553) | Replace `always_allow_tool_actions` with `tool_permissions.default` | **Merged** | @rtfeldman | New settings architecture |
| [#48436](https://github.com/zed-industries/zed/pull/48436) | Fix shell quote bypass in terminal permissions | **Merged** | @rtfeldman | Security fix for permission system |
| [#48640](https://github.com/zed-industries/zed/pull/48640) | Strengthen hardcoded rm security rules | **Merged** | @rtfeldman | Path normalization, multi-arg rm expansion |
| [#37582](https://github.com/zed-industries/zed/pull/37582) | Add AcceptEdits mode for Claude Code | Closed | @EtanHey | Community attempt, rejected in favor of granular approach |
| [#39816](https://github.com/zed-industries/zed/pull/39816) | Persist agent permission mode across sessions | **Merged** | @jefflouisma | Fixed mode persistence bug |

### 5.3 Timeline of Permission System Evolution

```
Sep 2024  Issue #17799 — "YOLO mode" first requested
May 2025  Boolean flag: always_allow_tool_actions (all-or-nothing)
Sep 2025  ACP modes: bypassPermissions, acceptEdits for Claude Code
Oct 2025  Bug fixes: mode persistence (PR #39816), bypass error display
Jan 2026  Granular per-tool permissions with regex (PR #46284)
Feb 2026  Migration to tool_permissions.default (PR #48553)
Feb 2026  Security hardening: shell quote bypass, path normalization
Feb 2026  Bug: bypassPermissions causes hallucination (#48992)
```

### 5.4 Community Workarounds

- **`claude --dangerously-skip-permissions`**: CLI flag for Claude Code standalone
- **`~/.claude/settings.json`**: Configure `permissions.allow` patterns for bypass mode
- **Third-party ACP bridges**: e.g., `@mrtkrcm/acp-claude-code` with `ACP_PERMISSION_MODE` env var
- **Zed settings**: `"agent_servers": {"claude": {"default_mode": "bypassPermissions"}}` (has hallucination bug)

---

## 6. Improvement Opportunities

### 6.1 Scenario A Fix: Smart Option Selection for Plan Confirmations

**Problem:** The hook always sends `"allow"` but plan confirmation dialogs have different option IDs.

**Solution:** When intercepting `request_tool_call_authorization`, inspect the `PermissionOptions` to determine the option_id dynamically:

```
Approach 1 — Read option IDs from memory:
  - Walk the PermissionOptions struct in the WaitingForConfirmation entry
  - Find the first option with PermissionOptionKind::AllowOnce or AllowAlways
  - Send that option's ID instead of hardcoded "allow"

Approach 2 — Pattern match on known option types:
  - If options contain "allow"/"reject" → tool permission → send "allow"
  - If options contain mode-like IDs → plan confirmation → send first "allow" variant
  - Fallback: send first option's ID

Approach 3 — Configure desired option_id per scenario:
  - ZED_YOLO_PLAN_OPTION env var (e.g., "acceptEdits", "dontAsk")
  - Default to first option with AllowOnce/AllowAlways kind
```

**Complexity:** Medium. Requires reading the `PermissionOptions` struct from memory, which means additional offset calibration.

**Alternative (simpler):** Configure Claude Code mode via Zed settings:
```json
{
  "agent_servers": {
    "claude": {
      "default_mode": "acceptEdits"
    }
  }
}
```
This tells Claude Code to start in acceptEdits mode, which should reduce plan confirmations. However, this has known issues (mode persistence, hallucination in bypass mode).

### 6.2 Scenario B Fix: Auto-Continue Phase Prompts

**Problem:** The agent pauses between phases and waits for user text input. No permission dialog is shown.

**Potential approaches:**

```
Approach 1 — Configure Claude Code to not pause:
  - Add instructions to Claude Code's config to continue without asking
  - Use ~/.claude/CLAUDE.md or project-level instructions
  - Limitation: Claude Code may still pause for safety reasons

Approach 2 — Auto-inject continuation message:
  - Hook the message display/render path
  - Detect patterns like "Shall I continue" in agent output
  - Automatically queue a "yes, continue" user message
  - Risk: Complex, fragile, could cause infinite loops

Approach 3 — Hook the ACP session message path:
  - Intercept outgoing messages from the agent
  - When a message ends with a question about continuing, immediately send a response
  - Requires hooking a different code path than tool_authorization

Approach 4 — Use Zed's "Available Commands" ACP feature:
  - Claude Code may advertise commands via acp::AvailableCommand
  - If "continue" is an available command, auto-invoke it
  - Depends on Claude Code implementation
```

**Complexity:** High. This is fundamentally different from tool authorization interception.

**Recommended investigation:** Check if Claude Code's `dontAsk` mode prevents phase pausing, or if it only affects tool permissions. If `dontAsk` prevents pausing, the simplest fix is to set that mode and address the hallucination bug separately.

### 6.3 Scenario C: Already Handled (Maintenance Only)

The existing hook works correctly. Future improvements:
- Monitor for Zed struct layout changes (offset drift)
- Consider reading option_id from the actual options list rather than hardcoding `"allow"`

### 6.4 Configuration-Based Approach (No Hook Required)

For users who don't need full YOLO mode, Zed now offers significant native configuration:

```json
{
  "agent": {
    "tool_permissions": {
      "default": "allow",
      "tools": {
        "terminal": {
          "always_deny": [{"pattern": "^sudo"}, {"pattern": "^rm\\s+-rf"}]
        }
      }
    }
  },
  "agent_servers": {
    "claude": {
      "default_mode": "acceptEdits"
    }
  }
}
```

**Limitation:** `tool_permissions` only applies to the native agent, NOT ACP agents. The `default_mode` setting for ACP agents has bugs (hallucination in bypass mode, mode persistence issues).

---

## 7. References

### Zed Source Code (key files)

| File | Purpose |
|------|---------|
| `crates/acp_thread/src/acp_thread.rs` | Core ACP thread: tool authorization, plan tracking, events |
| `crates/agent_servers/src/acp.rs` | ACP server connection: request_permission, mode management |
| `crates/agent/src/agent.rs` | Native agent: tool call authorization bridge |
| `crates/agent/src/tool_permissions.rs` | Native tool permission rules engine |
| `crates/agent_ui/src/connection_view/thread_view.rs` | UI: "Awaiting Confirmation" label, buttons |
| `crates/settings_content/src/agent.rs` | Settings schema: tool_permissions, agent_servers |

### GitHub Issues

- [#17799](https://github.com/zed-industries/zed/issues/17799) — Original "YOLO mode" request
- [#30313](https://github.com/zed-industries/zed/issues/30313) — "Always allow" scoping bug (61 reactions)
- [#43322](https://github.com/zed-industries/zed/issues/43322) — Permission settings ignored after change
- [#39392](https://github.com/zed-industries/zed/issues/39392) — Bypass mode revert bug
- [#40041](https://github.com/zed-industries/zed/issues/40041) — Unable to switch to bypass mode
- [#48992](https://github.com/zed-industries/zed/issues/48992) — bypassPermissions causes hallucination

### GitHub Pull Requests

- [#46284](https://github.com/zed-industries/zed/pull/46284) — Granular tool permission buttons (merged)
- [#48553](https://github.com/zed-industries/zed/pull/48553) — tool_permissions.default migration (merged)
- [#48436](https://github.com/zed-industries/zed/pull/48436) — Shell quote bypass fix (merged)
- [#48640](https://github.com/zed-industries/zed/pull/48640) — Hardened rm security rules (merged)
- [#37582](https://github.com/zed-industries/zed/pull/37582) — AcceptEdits mode (closed, not merged)
- [#39816](https://github.com/zed-industries/zed/pull/39816) — Mode persistence fix (merged)

### Documentation

- [Zed Tool Permissions](https://zed.dev/docs/ai/tool-permissions.html) — Official configuration docs
- [Zed Agent Settings](https://zed.dev/docs/ai/agent-settings) — Agent configuration reference

### Related Projects

- [anthropics/claude-code#25152](https://github.com/anthropics/claude-code/issues/25152) — bypassPermissions hallucination (cross-filed)
- [@mrtkrcm/acp-claude-code](https://www.npmjs.com/package/@mrtkrcm/acp-claude-code) — Third-party ACP bridge with permission mode support
