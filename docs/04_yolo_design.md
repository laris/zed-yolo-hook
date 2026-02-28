# YOLO Mode Design: Auto-Approve Dylib for Zed Preview

> Date: 2026-02-26
> Status: Design
> Crate: `zed-yolo-hook` (cdylib, injected via insert_dylib)
> No MCP server required — pure in-process dylib

---

## Table of Contents

1. [Overview](#1-overview)
2. [Architecture](#2-architecture)
3. [Technical Design](#3-technical-design)
4. [Implementation Plan](#4-implementation-plan)
5. [Milestones](#5-milestones)
6. [Test Plan](#6-test-plan)
7. [Deploy & Test](#7-deploy--test)
8. [Configuration](#8-configuration)
9. [Safety & Rollback](#9-safety--rollback)

---

## 1. Overview

### What

A cdylib (`libzed_yolo_hook.dylib`) injected into Zed Preview that automatically approves ACP tool call permission requests — eliminating the "Always Allow / Allow / Reject" dialog.

### Why

- Zed's `tool_permissions.default: "allow"` only works for the **native** agent
- External agents (Claude, Gemini, Codex) via ACP always trigger the permission dialog
- No setting or config option exists to bypass this for ACP agents
- The approval dialog breaks autonomous agent workflows

### How

Hook Zed's internal Rust functions via Frida-gum `attach` to intercept permission decisions and auto-send "allow" through the oneshot channel before the UI renders.

### Design Principles

1. **No MCP server** — pure in-process dylib, zero external dependencies
2. **Follow existing pattern** — `#[ctor]` + frida-gum attach hooks (pure in-process dylib)
3. **On by default** — YOLO active when dylib loaded; set `ZED_YOLO_MODE=0` to disable
4. **Safe** — hardcoded security rules (rm -rf /) are NEVER bypassed (they're checked server-side in Claude Agent)
5. **Logged** — all auto-approvals written to `~/Library/Logs/Zed/zed-yolo-hook.*.log`

---

## 2. Architecture

```
┌──────────────────────────────────────────────────────────────────┐
│ Zed Preview Process                                              │
│                                                                  │
│  ┌─────────────────┐    ┌──────────────────┐                    │
│  │ Claude Agent     │    │ ACP Protocol      │                    │
│  │ (subprocess)     │───>│ request_permission │                    │
│  └─────────────────┘    └────────┬─────────┘                    │
│                                  │                               │
│                          ┌───────▼────────┐                      │
│                          │ ClientDelegate  │                      │
│                          │ ::request_     │                      │
│                          │  permission()  │                      │
│                          └───────┬────────┘                      │
│                                  │                               │
│                          ┌───────▼────────┐                      │
│                          │ AcpThread      │                      │
│                          │ ::request_tool │                      │
│                          │ _call_author-  │                      │
│                          │  ization()    │                      │
│                          └───────┬────────┘                      │
│                                  │                               │
│                     ┌────────────▼────────────┐                  │
│                     │ ToolCallStatus::         │                  │
│                     │ WaitingForConfirmation   │                  │
│                     │ { options, respond_tx }  │                  │
│                     └────────────┬────────────┘                  │
│                                  │                               │
│  ┌──────────────────────┐       │                               │
│  │ libzed_yolo_hook.dylib   │       │                               │
│  │                      │       │                               │
│  │  Frida attach on     │───────┘                               │
│  │  request_tool_call_  │  Auto-sends "allow"                   │
│  │  authorization +     │  through respond_tx                   │
│  │  from_input hooks    │  via transmute Sender                 │
│  └──────────────────────┘                                       │
│                                                                  │
│  ┌──────────────────────┐                                       │
│  │ libzed_workspace_sync_hook.dylib │  (existing, workspace sync)           │
│  └──────────────────────┘                                       │
└──────────────────────────────────────────────────────────────────┘
```

### ~~Approach: GPUI Action Dispatch Hook~~ [DOES NOT WORK]

> **This approach was abandoned.** GPUI doesn't process keystrokes through `[NSApp sendEvent:]`, and Obj-C swizzling cannot reach Rust-level GPUI internals. See `05_yolo_implementation_log.md` for details.

<details>
<summary>Old approach (for reference)</summary>

Zed's GPUI framework dispatches actions through a well-defined system. The key actions are:

```rust
// From crates/agent_ui/src/acp/thread_view/active_thread.rs
pub fn allow_always(&mut self, _: &AllowAlways, window: &mut Window, cx: &mut Context<Self>) {
    self.authorize_pending_tool_call(acp::PermissionOptionKind::AllowAlways, window, cx);
}
```

The `AllowAlways` action is a GPUI action struct. When dispatched on the focused view, it triggers auto-approval.

**Strategy**: We hook at the Objective-C layer where NSNotification or NSEvent dispatch occurs, detect when `AcpThreadEvent::ToolAuthorizationRequired` is emitted, and dispatch `AllowAlways` on the relevant view.

</details>

### ~~Practical Approach: Objective-C Swizzle on NSWindow Key Events~~ [DOES NOT WORK]

> **This approach was abandoned.** Keystroke simulation is fundamentally broken because the main thread is blocked waiting for the oneshot channel — any delivery method that requires the main thread's event loop will deadlock. See Section 7 of `03_yolo_research.md`.

<details>
<summary>Old approach (for reference)</summary>

Since direct Rust symbol hooking is fragile, we use a more robust approach:

1. **On dylib load** (`#[ctor]`): Start a background thread
2. **Background thread**: Monitors Zed's process for pending tool authorizations
3. **Detection method**: Hook `objc_msgSend` for specific Objective-C selectors related to GPUI's event processing, OR use a simpler **polling approach** that scans GPUI's pending action queue
4. **Auto-approve**: When a pending authorization is detected, synthesize and dispatch a keyboard shortcut event (the `AllowAlways` keybinding)

</details>

### Current Working Approach (v0.17.1): Attach + Memory Walk + Transmute Sender

After v0.1–v0.16 all failed in various ways (see `05_yolo_implementation_log.md`), v0.17.1 uses:

1. **Frida `attach`** (not replace) on `request_tool_call_authorization`
2. **`on_enter`**: saves `self` pointer (x0 register)
3. **`on_leave`**: walks `self.entries` Vec using hardcoded offsets from disassembly to find the last `WaitingForConfirmation` entry and extract `respond_tx`
4. **`dispatch_async_f`**: defers the send to after the current call stack unwinds (avoids re-entrancy)
5. **`transmute`**: reconstructs a real `futures_channel::oneshot::Sender<Arc<str>>` from the raw Arc pointer
6. **`.send(Arc::from("allow"))`**: uses the actual futures-channel API — handles locking, value writing, and waker notification correctly

Key properties:
- **No ABI matching needed** — we don't call any Rust functions with Rust ABI
- **No keystroke simulation** — sends directly through the Rust oneshot channel
- **No cx/Context needed** — oneshot send is independent of GPUI
- **No re-entrancy** — deferred via dispatch_async_f
- **Correct waking** — Sender::drop calls drop_tx which wakes the receiver
- **No Obj-C at all** — pure Rust-level interception

### Bug History (Full)

See `05_yolo_implementation_log.md` for exhaustive details. Summary:

| Version | Approach | Result |
|---------|----------|--------|
| v0.1–0.3 | Background thread + keystroke/event simulation | Crash/hang (child process chaos, Obj-C in ctor) |
| v0.4–0.6 | GCD dispatch + CGEvent/NSEvent | Hung or no effect (GPUI doesn't use NSApp events) |
| v0.7 | Hook `from_input()` | Wrong code path (native agent only, not ACP) |
| v0.11 | Inline ASM to call authorize_tool_call | Crash (double-free, ABI mismatch) |
| v0.12 | Remove double-free | Crash (heap corruption from wrong ABI) |
| v0.13 | extern "C" fn pointer | Hang (re-entrancy: cx.emit deadlock) |
| v0.14 | dispatch_async deferred call | No effect (wrong tool_call_id, stale cx) |
| v0.15–0.15.1 | Cmd+Y keystroke to contentView | No effect (focus-dependent keybinding) |
| v0.16–0.16.1 | Correct register layout + inline ASM | Crash then hang (fat ptr swap, re-entrancy) |
| v0.17.0 | Memory walk + manual oneshot write | Hang (wrong discriminant, wrong offsets, no waker) |
| **v0.17.1** | **Memory walk + transmute Sender + .send() API** | **SUCCESS** |

---

## 3. Technical Design

### Crate Structure

```
zed-yolo-hook/
├── Cargo.toml
├── docs/
│   ├── 01_yolo_background.md        # Project background + scope
│   ├── 02_yolo_quickstart.md        # Build/inject/restore workflow
│   ├── 03_yolo_research.md          # Research & lessons learned
│   ├── 04_yolo_design.md            # This file
│   ├── 05_yolo_implementation_log.md # Full version history
│   └── 06_yolo_upgrade_guide.md     # Offset recalibration guide
└── src/
    ├── lib.rs                        # Entry point: #[ctor] init, hook orchestration
    ├── config.rs                     # YoloMode enum, env var parsing
    ├── logging.rs                    # Tracing file appender setup
    ├── symbols.rs                    # Generic Frida symbol search by pattern
    ├── ffi/
    │   ├── mod.rs                    # FFI module re-exports
    │   └── dispatch.rs              # macOS libdispatch (dispatch_async_f)
    └── hooks/
        ├── mod.rs                    # Hook module docs, shared counters
        ├── permission_decision.rs    # Hook 1: ToolPermissionDecision::from_input
        └── tool_authorization.rs     # Hook 2: AcpThread::request_tool_call_authorization
```

### Core Logic

```rust
// src/lib.rs — orchestration only
#[ctor]
fn init() {
    // 1. config::YoloMode::from_env() — check ZED_YOLO_MODE
    // 2. logging::init() — set up tracing to ~/Library/Logs/Zed/
    // 3. Gum::obtain() + Process::obtain() — init Frida
    // 4. symbols::find_by_pattern() — locate hook targets
    // 5. Interceptor::attach() — install both hooks
}

// src/hooks/permission_decision.rs — Hook 1 (built-in tools)
// on_leave: zeroes sret buffer (x8) → forces Allow return

// src/hooks/tool_authorization.rs — Hook 2 (ACP agents)
// on_enter: saves self pointer (x0)
// on_leave: walks self.entries Vec → finds respond_tx
//           → dispatch_async_f → transmute Sender → .send("allow")
```

### Architecture (v0.17.1)

```
ACP Agent                  Zed Process                    yolo-hook
   |                           |                              |
   |-- request_permission ---->|                              |
   |                           |                              |
   |                           |-- [ATTACHED via Frida] ----->|
   |                           |                              |
   |                           |   on_enter: save self (x0)   |
   |                           |                              |
   |                           |   ORIGINAL fn executes:      |
   |                           |      → creates (tx, rx)      |
   |                           |      → stores WaitForConfirm |
   |                           |      → emits ToolAuthReq     |
   |                           |                              |
   |                           |   on_leave:                  |
   |                           |   1. Walk self.entries Vec    |
   |                           |   2. Find respond_tx ptr     |
   |                           |   3. dispatch_async_f:       |
   |                           |      bump Arc refcount       |
   |                           |      transmute → Sender      |
   |                           |      .send("allow")          |
   |                           |      Sender drop → wake rx   |
   |                           |                              |
   |<-- permission granted ----|   (dialog auto-resolved)     |
```

No threads. No polling. No Obj-C. No keystrokes. Direct channel send.

### ~~Architecture (v0.8.0): Interceptor::replace~~ [DOES NOT WORK]

> **This approach was abandoned.** `Interceptor::replace` requires matching the unstable Rust ABI exactly, which is fragile and caused crashes/hangs in v0.8–v0.16. The working approach uses `attach` (non-invasive) instead. See `05_yolo_implementation_log.md`.

<details>
<summary>Old approach (for reference)</summary>

```
ACP Agent                  Zed Process                    yolo-hook
   |                           |                              |
   |-- request_permission ---->|                              |
   |                           |                              |
   |                           |-- [REPLACED FUNCTION] ------>|
   |                           |                              |
   |                           |   1. Calls ORIGINAL fn       |
   |                           |      → creates (tx, rx)      |
   |                           |      → stores WaitForConfirm |
   |                           |      → emits ToolAuthReq     |
   |                           |                              |
   |                           |   2. Extracts respond_tx     |
   |                           |      from WaitForConfirm     |
   |                           |                              |
   |                           |   3. Sends "allow" thru tx   |
   |                           |      → rx resolves instantly  |
   |                           |                              |
   |                           |<-- returns resolved future --|
   |                           |                              |
   |<-- permission granted ----|   (dialog never renders)     |
```

</details>

---

## 4. Implementation Plan

### Step 1: Create `zed-yolo-hook` crate ✅

- Project root — cdylib with ctor, Frida attach hooks, file logging
- YOLO on by default; `ZED_YOLO_MODE=0` to disable

### ~~Step 2: Implement NSEvent dispatch~~ [DOES NOT WORK]

> **Abandoned.** NSEvent/keystroke simulation doesn't work because GPUI doesn't use `[NSApp sendEvent:]` and the main thread is blocked. See Section 7 of `03_yolo_research.md`.

### ~~Step 3: Add process guard (v0.3.0)~~ [SUPERSEDED]

> **No longer needed.** The current approach (Frida `attach`) doesn't spawn background threads or call Obj-C. The hook only activates when the hooked function is actually called.

### Step 4: Patch workflow (xtask) ✅

- `cargo patch` builds the dylib and injects it into Zed (Preview by default)
- `cargo patch --stable` targets Zed stable
- `cargo patch restore` restores the original binary from the `.original` backup

### Step 5: Integration test

- Patch Zed Preview or Stable
- Launch and verify ACP tool calls run without the approval dialog
- Verify logs are written to `~/Library/Logs/Zed/zed-yolo-hook.*.log`
- Verify `ZED_YOLO_MODE=0` disables hooks without restoring
- Verify `ZED_YOLO_MODE=allow_safe` skips the native permission hook

---

## 5. Milestones

### M1: Crate Scaffolding ✅
- [x] Create `Cargo.toml`
- [x] Create `src/lib.rs` with ctor skeleton
- [x] Verify it compiles as cdylib
- [x] Verify it loads into Zed without crash

### ~~M2: Auto-Approval via NSEvent~~ [DOES NOT WORK]
- [x] ~~Implement cmd-y NSEvent creation and `[NSApp sendEvent:]` dispatch~~ — abandoned, GPUI doesn't use NSApp events
- [x] ~~Background thread with 500ms interval~~ — abandoned, not needed with Frida attach

### ~~M3: Process Guard (v0.3.0)~~ [SUPERSEDED]
- [x] ~~`is_main_ui_process()` check~~ — no longer needed, Frida attach is passive
- [x] ~~Background thread waits up to 30s~~ — no background thread in current approach

### M4: Configuration ✅
- [x] `ZED_YOLO_MODE=0/off/disabled` to disable
- [x] `ZED_YOLO_MODE=allow_safe/safe` for selective mode
- [x] `ZED_YOLO_LOG` for log level control

### M5: Patch Workflow ✅
- [x] `cargo patch` builds + injects into Zed
- [x] `cargo patch restore` rolls back quickly
- [x] `cargo patch test` validates injection in a temp copy

---

## 6. Test Plan

### Unit Tests (in crate)

| Area | What it covers |
|------|----------------|
| `src/config.rs` | `ZED_YOLO_MODE` parsing and defaults |

### Integration Tests (manual)

| Test | Steps | Expected |
|------|-------|----------|
| **Basic auto-approve** | 1. Build zed-yolo-hook 2. Patch Zed 3. Launch 4. Ask Claude to run `ls` | Terminal command runs without dialog |
| **Multi-tool approve** | Ask Claude to edit a file, then run a command | Both auto-approved |
| **MCP tool approve** | Ask Claude to use an MCP tool (e.g., brave_web_search) | MCP tool runs without dialog |
| **Safety preserved** | Ask Claude to run `rm -rf /` | Claude's own safety should reject (server-side) |
| **Disable via env** | Launch with `ZED_YOLO_MODE=0` | Dialog appears normally |
| **Logging** | Run several tool calls | All appear in zed-yolo-hook.*.log with timestamps |
| **Crash resilience** | Kill/restart Zed | No corruption, hook re-initializes |

### Verification Commands

```bash
# Build (from this repo root)
cargo build --release -p zed-yolo-hook

# Patch Zed Preview (default)
cargo patch

# Or patch Zed Stable
cargo patch --stable

# Verify injection
otool -L "/Applications/Zed Preview.app/Contents/MacOS/zed" | grep -i yolo

# Watch logs
tail -f ~/Library/Logs/Zed/zed-yolo-hook.*.log

# Restore original
cargo patch restore
```

---

## 7. Deploy & Test

### Prerequisites

- macOS aarch64 (Apple Silicon)
- Zed installed at `/Applications/Zed.app` and/or `/Applications/Zed Preview.app`
- Rust toolchain per `rust-toolchain.toml`
- Xcode Command Line Tools (`codesign`, `otool`)

### Supported Versions

| App | Version | Offsets verified | Status |
|-----|---------|-----------------|--------|
| Zed Preview | v0.226.0 | All offsets match | Working (v0.17.1) |
| Zed Stable | v0.225.9 | All offsets match | Working (same dylib) |

The same `libzed_yolo_hook.dylib` binary works for both Zed Stable and Zed Preview — no rebuild needed. The struct layout offsets are identical between v0.225.9 and v0.226.0.

### Quick Deploy (copy-paste)

```bash
# 1. Quit Zed if running
osascript -e 'quit app "Zed Preview"' 2>/dev/null; sleep 1
osascript -e 'quit app "Zed"' 2>/dev/null; sleep 1

# 2. Patch (build + inject)
cd /path/to/zed-yolo-hook
cargo patch        # Zed Preview
# cargo patch --stable

# 3. Launch
open "/Applications/Zed Preview.app"

# 4. Watch the log in another terminal
tail -f ~/Library/Logs/Zed/zed-yolo-hook.*.log
```

### Verify Injection

```bash
# Check the binary has the dylib load commands
otool -l "/Applications/Zed Preview.app/Contents/MacOS/zed" | grep -A5 "libyolo"
otool -l "/Applications/Zed.app/Contents/MacOS/zed" | grep -A5 "libyolo"
```

### Test Checklist

After launching Zed Preview with the hook:

| # | Test | How | Expected |
|---|------|-----|----------|
| 1 | **Hook loads** | Check log: `tail ~/Library/Logs/Zed/zed-yolo-hook.*.log` | "YOLO mode ACTIVE" message |
| 2 | **Zed starts normally** | Open Zed Preview, open a project | No crash, normal UI |
| 3 | **Auto-approve terminal** | Open Agent Panel, ask Claude to run `ls` | Command executes without dialog |
| 4 | **Auto-approve file edit** | Ask Claude to edit a file | Edit happens without dialog |
| 5 | **Auto-approve MCP tool** | Ask Claude to use a search tool | Tool runs without dialog |
| 6 | **Log entries** | `tail ~/Library/Logs/Zed/zed-yolo-hook.*.log` | Auto-approve entries logged |
| 7 | **Disable test** | `ZED_YOLO_MODE=0 open "/Applications/Zed Preview.app"` | Dialog appears normally |

### Uninstall / Restore

```bash
# Restore original Zed binary from the .original backup
cargo patch restore           # Zed Preview
cargo patch restore --stable  # Zed Stable

# Manual fallback:
#   cp "/Applications/Zed Preview.app/Contents/MacOS/zed.original" \
#      "/Applications/Zed Preview.app/Contents/MacOS/zed"
#   codesign -fs - --deep "/Applications/Zed Preview.app"
```

### Troubleshooting

| Problem | Solution |
|---------|----------|
| Zed hangs on launch | Restore binary: `cargo patch restore`. Check zed-yolo-hook.*.log for the hang point. v0.4-v0.6 all caused hangs due to main thread deadlock — keystroke simulation is impossible |
| Zed crashes on launch | Check zed-yolo-hook.*.log for errors; restore with `cargo patch restore`. v0.3 crashed from Obj-C calls during `#[ctor]` |
| "code signature invalid" | Re-sign: `codesign -fs - --deep "/Applications/Zed Preview.app"` |
| Dialog still appears | Check `otool -L` to confirm dylib injected; check log for "YOLO hook installed". v0.7 hooked wrong function (`from_input` — native agent only, not ACP) |
| Multiple PIDs in log | Normal — `#[ctor]` fires in all processes. Hook only activates in processes that call the hooked function |
| Auto-update replaces binary | Re-run `cargo patch` after Zed updates |
| Hook installed but never called | Wrong hook target. ACP agents use `request_tool_call_authorization`, NOT `ToolPermissionDecision::from_input` |

---

## 8. Configuration

### Environment Variables

| Variable | Values | Default | Description |
|----------|--------|---------|-------------|
| `ZED_YOLO_MODE` | `0`/`off`/`disabled` | **(enabled)** | Set to disable YOLO mode |
| `ZED_YOLO_MODE` | `allow_safe`/`safe` | — | Enable ACP-only mode (skip native tool permission hook) |
| `ZED_YOLO_LOG` | `debug`, `info`, `warn` | `info` | Log level |

### Modes

| Mode | Behavior |
|------|----------|
| **(default / unset)** | **Auto-approve ALL tool calls** |
| `allow_safe` / `safe` | Auto-approve ACP dialogs only (native tool permissions unchanged) |
| `0` / `off` / `disabled` | Hook loads but does nothing — normal behavior |

YOLO mode is **enabled by default** when the dylib is loaded. To disable without unpatching, launch Zed with:

```bash
ZED_YOLO_MODE=0 open "/Applications/Zed Preview.app"
```

---

## 9. Safety & Rollback

### Safety Guarantees

1. **Hardcoded security rules are server-side**: Claude Agent checks `rm -rf /` BEFORE sending to Zed. The YOLO hook only affects Zed's UI-side approval — it cannot bypass server-side safety.

2. **Full audit log**: Every auto-approval is logged to `~/Library/Logs/Zed/zed-yolo-hook.*.log`.

3. **Instant rollback**: `cargo patch restore` restores the original binary in seconds.

4. **Disable without unpatching**: Set `ZED_YOLO_MODE=0` to make the dylib inert.

### Rollback Procedure

```bash
# Method 1: Patcher restore
cargo patch restore

# Method 2: Manual restore
cp "/Applications/Zed Preview.app/Contents/MacOS/zed.original" \
   "/Applications/Zed Preview.app/Contents/MacOS/zed"
codesign -fs - --deep "/Applications/Zed Preview.app"

# Method 3: Disable without unpatching
ZED_YOLO_MODE=0 open "/Applications/Zed Preview.app"
```
