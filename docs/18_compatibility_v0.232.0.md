# Compatibility Verification: Zed Preview v0.232.0

> Date: 2026-04-11 / 2026-04-12
> Installed app: `/Applications/Zed Preview.app`
> App version: `0.232.0`
> App build: `0.232.0+preview.219.957fa4d9e3530ba0c8773a92943f42263b25ca1f`
> Zed source commit: `957fa4d9e3530ba0c8773a92943f42263b25ca1f`
> CI verification: [Run 24283318357](https://github.com/laris/zed-yolo-hook/actions/runs/24283318357) (PASS)

---

## 1. Scope

This note records the 2026-04-11 verification that `zed-yolo-hook` is compatible
with Zed Preview 0.232.0. Includes source-level struct comparison, local binary
verification, and the first-ever automated CI verification on GitHub Actions.

**Result: fully compatible, no code changes required for hook functionality.**

Additional changes made in this session:
- Converted `dylib-kit` dependencies from local paths to git dependencies
- Added GitHub Actions CI verification workflow
- Added `push_entry_hook` (Hook 5: catch-all AcpThread registration)
- Fixed registry race condition with `locked_register` (in dylib-kit)

---

## 2. Source-Level Struct Comparison (v0.230.0 → v0.232.0)

Zed source compared between commits `9437a84390` (v0.230.0) and `957fa4d9e3` (v0.232.0).

### 2.1 AgentThreadEntry enum — NEW VARIANT ADDED

```diff
 pub enum AgentThreadEntry {
     UserMessage(UserMessage),        // variant 0
     AssistantMessage(AssistantMessage), // variant 1
     ToolCall(ToolCall),              // variant 2
+    CompletedPlan(Vec<PlanEntry>),   // variant 3   ← NEW
 }
```

**Impact on hook:** None. ToolCall discriminant stays at `0x02`. The new variant
`CompletedPlan(Vec<PlanEntry>)` adds 24 bytes (Vec = ptr + len + cap), which is
far smaller than the ToolCall variant (~448 bytes), so the enum size stays `0x1c0`.

### 2.2 AcpThread struct — NEW FIELD ADDED

```diff
 pub struct AcpThread {
     session_id: acp::SessionId,
     work_dirs: Option<PathList>,
     parent_session_id: Option<acp::SessionId>,
     title: Option<SharedString>,
     provisional_title: Option<SharedString>,
     entries: Vec<AgentThreadEntry>,     // ← our target (offset 0x90)
     plan: Plan,
     project: Entity<Project>,
     ...
     prompt_capabilities: acp::PromptCapabilities,
+    available_commands: Vec<acp::AvailableCommand>,  ← NEW
     _observe_prompt_capabilities: Task<...>,
     ...
 }
```

**Impact on hook:** The `entries` field is declared BEFORE the new
`available_commands` field. Since `repr(Rust)` can reorder fields, this is not
conclusive from source alone — but the binary confirmed no change to `entries`
offset at `0x90`.

### 2.3 ToolCall struct — UNCHANGED

All 12 fields remain identical between v0.230.0 and v0.232.0:
```
id, label, kind, content, status, locations, resolved_locations,
raw_input, raw_input_markdown, raw_output, tool_name, subagent_session_info
```

### 2.4 ToolCallStatus enum — UNCHANGED

```rust
enum ToolCallStatus {
    Pending,
    WaitingForConfirmation { options, respond_tx },
    InProgress,
    Completed,
    Failed,
    Rejected,
    Canceled,
}
```

### 2.5 SelectedPermissionOutcome struct — UNCHANGED

```rust
pub struct SelectedPermissionOutcome {
    pub option_id: acp::PermissionOptionId,
    pub option_kind: acp::PermissionOptionKind,
    pub params: Option<SelectedPermissionParams>,
}
```

This struct already had `option_kind` added in v0.230.0 (PR #52050). No change
between v0.230.0 and v0.232.0.

### 2.6 PermissionOptions enum — UNCHANGED

All three variants present in both versions:
- `Flat(Vec<acp::PermissionOption>)`
- `Dropdown(Vec<PermissionOptionChoice>)`
- `DropdownWithPatterns { choices, patterns, tool_name }`

---

## 3. Binary Offset Verification

### 3.1 Local verification (2026-04-11)

```
$ cargo patch --verify
[verify] PASS zed-yolo-hook — all markers found
[verify] ALL HOOKS VERIFIED
```

### 3.2 Runtime verification from hook log

```
tool_authorization #1: layout=v0.230.x entry[42] disc=0x2 status_head=0x8000000000000000
tool_authorization #1: matched v0.230.x entry[42] by ToolCallId, respond_tx=0x8b92a9e80
tool_authorization #1: send succeeded (option_id=PermissionOptionId("allow_always"))
tool_authorization #1: approved in 86us via v0.230.x call_id="toolu_01Qe3J"
```

All offsets from the v0.230.x layout work correctly on v0.232.0:

| Offset | Field | Value | Status |
|--------|-------|-------|--------|
| `+0x90` | `entries.ptr` | Valid pointer | OK |
| `+0x98` | `entries.len` | Correct count | OK |
| Entry `+0x00` | discriminant | `0x02` (ToolCall) | OK |
| Entry size | | `0x1c0` (448 bytes) | OK |
| Entry `+0x118` | status_head | `0x8000000000000000` (WaitingForConfirmation) | OK |
| Entry `+0x160` | respond_tx | Valid Arc pointer | OK |
| Entry `+0x168` | ToolCallId.ptr | Valid Arc<str> | OK |
| Entry `+0x170` | ToolCallId.len | 30 (matches) | OK |

### 3.3 CI verification (2026-04-11)

First automated CI run on GitHub Actions `macos-15` ARM64 runner.

**Run:** https://github.com/laris/zed-yolo-hook/actions/runs/24283318357
**Result:** PASS (1m30s)

All 6 hooks verified on the CI runner (downloaded fresh Zed Preview v0.232.0-pre from GitHub Releases):

```
=== zed-yolo-hook v0.1.0 ===
permission_decision: Found ...from_input at NativePointer(0x102e2be10)
permission_decision: hook installed
tool_authorization: Found ...request_tool_call_authorization at NativePointer(0x102b13214)
tool_authorization: hook installed
upsert_hook: Found ...upsert_tool_call_inner at NativePointer(0x102b0e75c)
upsert_hook: hook installed (approach 1)
session_update_hook: Found ...handle_session_update at NativePointer(0x102b0c8ec)
session_update_hook: hook installed (approach 2)
push_entry_hook: Found ...push_entry at NativePointer(0x102b07da8)
push_entry_hook: hook installed (catch-all registration)
stale_scanner: started (interval=2000ms)
Registered in hook registry (app_id=zed-preview)
YOLO mode ACTIVE (pid=6845)
```

---

## 4. Symbol Names (v0.232.0)

Crate hashes changed again (as expected with each Zed release), but the
pattern-based symbol search finds them correctly:

| Symbol | v0.231.1 Hash | v0.232.0 Hash |
|--------|--------------|--------------|
| `acp_thread` | `CseS5viS5XUxq` | `Cs64YZFm6XLjH` |
| `agent` (tool_permissions) | `CslQbgH0e8eCG` | `CsbVohzhw9q4J` |

The hook's pattern matching (`["acp_thread", "AcpThread", "request_tool_call_authorization"]`)
is hash-agnostic and works correctly.

---

## 5. CI Verification Workflow

This verification session also established the CI verification infrastructure:

### 5.1 Files created

| File | Purpose |
|------|---------|
| `.github/workflows/verify-hook.yml` | GitHub Actions workflow |
| `scripts/ci-verify.sh` | Standalone verification script |
| `docs/16_free_macos_ci_research.md` | Research on free macOS CI environments |
| `docs/17_ci_verification_workflow.md` | Workflow design and details |

### 5.2 CI run history

| Run | Date | Result | Issue | Fix |
|-----|------|--------|-------|-----|
| 24283228452 | 2026-04-11 | FAIL | `nicokoch/insert-dylib` repo dead | Use crates.io |
| 24283282605 | 2026-04-11 | FAIL | `insert-dylib` "Failed to create" | Use `--inplace` flag |
| 24283318357 | 2026-04-11 | **PASS** | — | — |
| 24307265961 | 2026-04-12 | **PASS** | — | After `locked_register` fix |

### 5.3 Dependency changes for CI

```toml
# Before (local paths, CI incompatible):
dylib-hook-registry = { path = "/Users/lqiao/dev/codes/dylib-kit/crates/dylib-hook-registry" }

# After (git dependencies, works everywhere):
dylib-hook-registry = { git = "https://github.com/laris/dylib-kit" }
```

---

## 6. Registry Race Condition Fix

### 6.1 Problem discovered

When running `cargo patch` from one hook project, the other hook's registry entry
was being lost. Investigation revealed a **concurrent `#[ctor]` race condition**:

Zed spawns multiple processes. Each process loads all injected dylibs and runs
their `#[ctor]` constructors simultaneously. Each hook does:
1. `HookRegistry::load()` → reads registry file
2. `.register(entry)` → adds itself
3. `.save()` → writes file

Without locking, if process A reads the file, then process B reads the same file
before A writes, A writes (adding hook A), then B writes (adding hook B but
losing hook A's entry from its stale copy).

### 6.2 Fix applied

Added `HookRegistry::locked_register()` in `dylib-kit` (`crates/dylib-hook-registry/src/lib.rs`):
- Uses `fs2::FileExt::lock_exclusive()` on a `.lock` file
- Atomically: lock → load → register → save → unlock
- Both hooks updated to use this method

### 6.3 Commits

| Repo | Commit | Description |
|------|--------|-------------|
| `dylib-kit` | `11c764a` | Add `locked_register` with fs2 file locking |
| `zed-yolo-hook` | `8f20299` | Use `locked_register` in `register_in_registry` |
| `zed-project-workspace` | `5fae6a2` | Use `locked_register` in `register_in_registry` |

---

## 7. Conclusion

Zed Preview 0.232.0 is fully compatible with `zed-yolo-hook`. The v0.230.x
entry layout continues to work correctly. The source-level changes
(`CompletedPlan` variant, `available_commands` field) do not affect any offsets
used by the hook.

The CI verification infrastructure is now in place for automated weekly checks
and push-triggered verification.
