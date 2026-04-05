# ExitPlanMode Gap Analysis & Permission Matrix

> Date: 2026-04-04
> Scope: Analysis of the ExitPlanMode bug, complete ACP permission matrix, and config-file-based fix

---

## 1. The Bug: ExitPlanMode Sends Wrong option_id

### Symptom

The YOLO hook reports "approved" in logs, but the agent stays stuck in plan mode after the "Ready to code?" dialog.

### Root Cause

`request_tool_call_authorization` handles **two distinct types** of ACP permission requests:

| Type | Dialog | Valid option_ids | Hook was sending |
|------|--------|-----------------|------------------|
| Regular tool | "Run Command" / "Allow" | `allow`, `allow_always`, `reject` | `"allow"` ✅ |
| ExitPlanMode | "Ready to code?" | `acceptEdits`, `bypassPermissions`, `default`, `plan` | `"allow"` ❌ |

The hook was sending `"allow"` for both types. Claude Code's ExitPlanMode handler (`acp-agent.ts:1114-1118`) only accepts mode names as option_ids:

```typescript
if (optionId === "default" || optionId === "acceptEdits" || optionId === "bypassPermissions")
  → sets mode, returns { behavior: "allow" }
else
  → returns { behavior: "deny", message: "User rejected request to exit plan mode." }
```

Sending `"allow"` → Claude Code denies → agent stays in plan mode.

### Fix

The hook now detects ExitPlanMode by reading the first PermissionOption's option_id from memory. If it matches a mode name (not "allow"/"allow_always"/"reject"), the hook sends the configured `plan_option` instead.

---

## 2. Complete Permission Matrix

### ExitPlanMode Options (Scenario A — "Ready to code?")

| option_id | kind | UI Label | Claude Code Behavior |
|-----------|------|----------|---------------------|
| `bypassPermissions` | AllowAlways | "Yes, and bypass permissions" | Sets mode, allows all tools. **Warning:** has hallucination bug ([zed#48992](https://github.com/zed-industries/zed/issues/48992)) |
| `acceptEdits` | AllowAlways | "Yes, and auto-accept edits" | Sets mode, auto-accepts file edits, prompts for commands |
| `default` | AllowOnce | "Yes, and manually approve edits" | Sets mode, prompts for every action |
| `plan` | RejectOnce | "No, keep planning" | Stays in plan mode (deny) |

Note: `bypassPermissions` is only offered when not running as root.

### Regular Tool Options (Scenario C)

| option_id | kind | UI Label | Claude Code Behavior |
|-----------|------|----------|---------------------|
| `allow_always` | AllowAlways | "Always Allow" | Allows + creates persistent rule (stops asking for this tool type in session) |
| `allow` | AllowOnce | "Allow" | One-time allow |
| `reject` | RejectOnce | "Reject" | Denies tool |

### Session Modes (6 modes)

| Mode | Aliases | Effect |
|------|---------|--------|
| `default` | — | Standard, prompts for dangerous operations |
| `acceptEdits` | `acceptedits` | Auto-accept file edits, prompt for commands |
| `plan` | — | Planning mode, no tool execution |
| `dontAsk` | `dontask` | Don't prompt, deny if not pre-approved |
| `bypassPermissions` | `bypasspermissions`, `bypass` | Bypass all checks (non-root only, has hallucination bug) |
| `auto` | — | ML classifier auto-approves (experimental) |

### PermissionOptionKind (4 variants)

| Kind | Zed Effect | Claude Code Effect |
|------|-----------|-------------------|
| AllowOnce | status → InProgress | One-time grant |
| AllowAlways | status → InProgress | Persistent rule for session |
| RejectOnce | status → Rejected | One-time denial |
| RejectAlways | status → Rejected | Persistent deny rule |

---

## 3. Configuration

### Config file

Located at `~/.config/dylib-hooks/{app_id}/zed-yolo-hook.json` (same directory as the hook registry).

Works with Finder/Dock launches (unlike env vars which are stripped by macOS LaunchServices).

```json
{
  "mode": "allow_all",
  "tool_option": "allow",
  "plan_option": "acceptEdits",
  "log_level": "info",
  "retry_delay_us": 1500
}
```

### Config fields

| Field | Default | Values | Effect |
|-------|---------|--------|--------|
| `mode` | `allow_all` | `allow_all`, `allow_safe`, `disabled` | Which hooks to install |
| `tool_option` | `allow` | `allow`, `allow_always` | option_id for regular tool permissions |
| `plan_option` | `acceptEdits` | `acceptEdits`, `bypassPermissions`, `default`, `plan` | option_id for ExitPlanMode |
| `log_level` | `info` | `trace`, `debug`, `info`, `warn`, `error` | Tracing filter level |
| `retry_delay_us` | `1500` | 0–10000 | Retry delay on miss (microseconds) |

### Precedence (highest wins)

1. Environment variable (`ZED_YOLO_MODE`, `ZED_YOLO_TOOL_OPTION`, etc.)
2. Config file
3. Built-in defaults

### CLI management

```bash
cargo patch config                    # Show current config
cargo patch config set plan_option bypassPermissions
cargo patch config set tool_option allow_always
cargo patch config reset              # Reset to defaults
cargo patch config path               # Print config file path
cargo patch config --stable           # Target Zed Stable instead of Preview
```

---

## 4. ExitPlanMode Detection

### How it works

The hook detects ExitPlanMode by reading the first `PermissionOption`'s `option_id` from the `PermissionOptions::Flat(Vec<PermissionOption>)` stored in the `WaitingForConfirmation` entry.

- If first option_id is `"allow_always"`, `"allow"`, or `"reject"` → regular tool → send `tool_option`
- If first option_id is `"bypassPermissions"`, `"acceptEdits"`, `"default"`, or `"plan"` → ExitPlanMode → send `plan_option`
- If pointer validation fails → conservative fallback to regular tool behavior

### Speculative memory layout

The PermissionOptions is stored within `ToolCallStatus::WaitingForConfirmation` at `entry + status_offset`:

```
status_base = entry + 0x118

status_base + 0x00: niche/status head (used for waiting detection)
status_base + 0x08: PermissionOptions discriminant (0 = Flat)
status_base + 0x10: Flat Vec<PermissionOption>.ptr
status_base + 0x18: Flat Vec<PermissionOption>.len
...
status_base + 0x48: respond_tx
```

First PermissionOption at `vec_ptr + 0x00`:
```
+0x00: option_id.ptr (Arc<str> pointer)
+0x08: option_id.len (Arc<str> length)
+0x10: name (String)
+0x28: kind (PermissionOptionKind)
+0x30: meta (Option<Map<String,Value>>)
```

These offsets are speculative (based on type analysis, not binary disassembly). The detection is best-effort with pointer validation — if wrong, it safely falls back.

### Binary RE procedure (for verification)

```bash
# Disassemble functions that access PermissionOptions:
xcrun llvm-objdump --disassemble-symbols='<first_option_of_kind_symbol>' \
  '/Applications/Zed Preview.app/Contents/MacOS/zed' | head -200

# Look for ldr instructions that access the Vec inside Flat variant:
# ldr xN, [xSTATUS, #0x10]  → Vec.ptr
# ldr xN, [xSTATUS, #0x18]  → Vec.len
```

---

## 5. How to Trigger Each Scenario

### Scenario C — Regular Tool Permission
1. Open Zed → Agent Panel → ask "list files in current directory"
2. Agent calls Bash/Read/etc → permission dialog appears
3. Hook sends configured `tool_option` → approved instantly
4. **Log:** `tool_authorization #N: approved in Xus`

### Scenario A — ExitPlanMode / "Ready to code?"
1. Open Zed → Agent Panel with Claude Code
2. Give a coding task: "add error handling to server.rs"
3. Agent enters plan mode, creates a plan
4. Agent calls `ExitPlanMode` → "Ready to code?" dialog appears
5. Hook detects ExitPlanMode (first option_id = "bypassPermissions" or "acceptEdits")
6. Hook sends configured `plan_option` (default: "acceptEdits")
7. **Log:** `tool_authorization #N: ExitPlanMode detected, plan_option=AcceptEdits`

### Scenario B — Phase Continuation (NOT hookable)
1. Agent pauses between phases: "Shall I continue with Phase N?"
2. This is NOT a permission dialog — agent stopped generating and awaits text input
3. No hook can intercept this — requires CLAUDE.md instructions or `dontAsk` mode

---

## 6. ACP Protocol Types (agent-client-protocol 0.10.2)

### Structs passed through `request_tool_call_authorization`

```rust
// The function signature:
pub fn request_tool_call_authorization(
    &mut self,
    tool_call: acp::ToolCallUpdate,  // passed via x1 on ARM64
    options: PermissionOptions,       // passed via x2 (by-pointer, > 16 bytes)
    cx: &mut Context<Self>,
)

// ToolCallUpdate (ACP protocol):
struct ToolCallUpdate {
    pub tool_call_id: ToolCallId,       // Arc<str>
    pub fields: ToolCallUpdateFields,   // Contains Option<ToolKind>
    pub meta: Option<Map<String, Value>>,
}

// ToolKind — distinguishes ExitPlanMode at the protocol level:
enum ToolKind {
    Read, Edit, Delete, Move, Search,
    Execute, Think, Fetch,
    SwitchMode,  // ← ExitPlanMode
    Other,
}

// PermissionOption — what each option in the dialog contains:
struct PermissionOption {
    pub option_id: PermissionOptionId,  // Arc<str>
    pub name: String,
    pub kind: PermissionOptionKind,
    pub meta: Option<Map<String, Value>>,
}

// The response payload sent through the oneshot channel:
struct SelectedPermissionOutcome {  // size = 0x30
    pub params: Option<SelectedPermissionParams>,   // +0x00
    pub option_id: PermissionOptionId,              // +0x18
    pub option_kind: PermissionOptionKind,          // +0x28
}
```

### Claude Code's ExitPlanMode handler

From `claude-agent-acp/src/acp-agent.ts:1079-1141`:

```typescript
if (toolName === "ExitPlanMode") {
    const options = [
        { kind: "allow_always", name: "Yes, and auto-accept edits", optionId: "acceptEdits" },
        { kind: "allow_once", name: "Yes, and manually approve edits", optionId: "default" },
        { kind: "reject_once", name: "No, keep planning", optionId: "plan" },
    ];
    if (ALLOW_BYPASS) {
        options.unshift({
            kind: "allow_always", name: "Yes, and bypass permissions", optionId: "bypassPermissions",
        });
    }
    // ... requestPermission(options) ...
    // Response: optionId must be "default" | "acceptEdits" | "bypassPermissions" to proceed
    // Otherwise: returns { behavior: "deny" }
}
```

### Claude Code's regular tool handler

From `claude-agent-acp/src/acp-agent.ts:1154-1206`:

```typescript
const response = await this.client.requestPermission({
    options: [
        { kind: "allow_always", name: "Always Allow", optionId: "allow_always" },
        { kind: "allow_once", name: "Allow", optionId: "allow" },
        { kind: "reject_once", name: "Reject", optionId: "reject" },
    ],
    // ...
});
// Response: optionId must be "allow" | "allow_always" to proceed
```

---

## 7. Known Gap: Restored/Resumed Sessions

### Discovery (2026-04-05)

When Zed restarts, saved ACP sessions are restored via `load_session` or `resume_session` (acp.rs:700-820). Tool calls in these restored sessions arrive via `session_notification()` → `handle_session_update()` → `upsert_tool_call()` with their original status. This path does **NOT** call `request_tool_call_authorization`.

If a tool call was `Pending` when Zed quit, it gets re-inserted as `Pending` on restore. Claude Code then re-issues `requestPermission()` for the new prompt, but the restored tool call may render the "Awaiting Confirmation" dialog before `requestPermission()` arrives — creating a window where the dialog is visible but the hook hasn't been triggered.

Additionally, if the ACP agent connection drops and reconnects (e.g., Claude Code process restarts), the agent may re-send tool call state via `session_notification` rather than `request_permission`, bypassing the authorization hook entirely.

### Verified: ACP v0.24.2 vs v0.25.0

Compared the compiled `canUseTool()` function between claude-agent-acp v0.24.2 and v0.25.0 — the permission flow is **identical**. The v0.25.0 changes are only:
- Added "auto" permission mode
- Auth method improvements (split claude-ai-login / console-login)
- Context window size calculation fixes

The `requestPermission()` call path is unchanged between versions.

### Two code paths for tool calls in Zed

| Path | Source | Calls `request_tool_call_authorization`? | Hook intercepts? |
|------|--------|----------------------------------------|-----------------|
| `request_permission` (acp.rs:1445) | Claude Code calls `requestPermission()` | **Yes** | **Yes** |
| `session_notification` (acp.rs:1584) | Claude Code sends status update | **No** | **No** |
| `load_session` restore (acp.rs:700) | Zed restores saved session | **No** (tool calls arrive via session updates) | **No** |

### Impact

- Fresh tool calls in active sessions: **always intercepted** (100% of the time)
- Restored/resumed sessions after restart: **may be missed** if tool call state arrives via session_notification before requestPermission
- This explains why the "nixos" workspace showed "Awaiting Confirmation" while other workspaces worked fine — the nixos session was likely restored from a previous Zed run

### Potential fix directions

1. **Hook `upsert_tool_call_inner`** — intercepts ALL tool call insertions regardless of source. Would need to detect which ones need auto-approval (check for WaitingForConfirmation status after insertion).
2. **Hook `handle_session_update`** — intercept the session_notification path specifically.
3. **Periodic scan** — background thread that scans `AcpThread.entries` for stale WaitingForConfirmation entries and auto-approves them.

## 8. References

- [zed-industries/zed#48992](https://github.com/zed-industries/zed/issues/48992) — bypassPermissions causes hallucination
- [zed-industries/zed#30313](https://github.com/zed-industries/zed/issues/30313) — "Always allow" scoping bug (61 reactions)
- `claude-agent-acp/src/acp-agent.ts` — canUseTool function (lines 1068-1208)
- `zed/crates/acp_thread/src/acp_thread.rs` — request_tool_call_authorization (line 2000)
- `zed/crates/agent_servers/src/acp.rs` — ACP bridge (line 1445)
