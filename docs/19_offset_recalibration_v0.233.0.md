# Offset Recalibration: Zed Preview v0.233.0

> Date: 2026-04-17
> Installed app: `/Applications/Zed Preview.app`
> App version: `0.233.0` (build `20260415.163537`)
> Zed source commit: `b8b7aad70a8127fa6deb8e83ba80725fca04c0fd`
> Previous verified version: `v0.232.0` (commit `957fa4d9e3`, docs/18)

---

## 1. Summary

Zed Preview 0.233.0 requires a recalibration of the `AcpThread.entries` Vec
offset. The `entries.ptr` / `entries.len` fields **shifted by 0x20 (32 bytes)**
because a new `cost: Option<SessionCost>` field was added to `AcpThread`, and
Rust's `repr(Rust)` layout freely reorders fields to optimize for alignment.

| Constant              | v0.228.x – v0.232.0 | v0.233.0 |
|-----------------------|---------------------|----------|
| `ENTRIES_PTR_OFFSET`  | `0x90`              | **`0xb0`** |
| `ENTRIES_LEN_OFFSET`  | `0x98`              | **`0xb8`** |

All other offsets are unchanged (entry_size `0x1c0`, status_offset `0x118`,
respond_tx_offset `0x160`, id_ptr/len `0x168`/`0x170`).

The 5 exported symbol patterns (`request_tool_call_authorization`,
`upsert_tool_call_inner`, `handle_session_update`, `push_entry`,
`ToolPermissionDecision::from_input`) all resolve correctly via hash-agnostic
pattern matching.

---

## 2. Source-Level Change

Diff between `957fa4d9e3` (v0.232.0) and `b8b7aad70a` (v0.233.0) in
`crates/acp_thread/src/acp_thread.rs`:

```diff
 pub struct AcpThread {
     session_id: acp::SessionId,
     work_dirs: Option<PathList>,
     parent_session_id: Option<acp::SessionId>,
     title: Option<SharedString>,
     provisional_title: Option<SharedString>,
     entries: Vec<AgentThreadEntry>,
     plan: Plan,
     project: Entity<Project>,
     action_log: Entity<ActionLog>,
     shared_buffers: HashMap<Entity<Buffer>, BufferSnapshot>,
     turn_id: u32,
     running_turn: Option<RunningTurn>,
     connection: Rc<dyn AgentConnection>,
     token_usage: Option<TokenUsage>,
+    cost: Option<SessionCost>,         // ← NEW in v0.233.0
     prompt_capabilities: acp::PromptCapabilities,
     available_commands: Vec<acp::AvailableCommand>,
     ...
 }
```

`SessionCost` is defined as:
```rust
pub struct SessionCost {
    pub amount: f64,
    pub currency: SharedString,
}
```

Everything else in the hook's target structs is byte-for-byte identical:
- `AgentThreadEntry` enum — unchanged
- `ToolCall` struct — unchanged
- `ToolCallStatus` enum — unchanged
- `SelectedPermissionOutcome` struct — unchanged
- `PermissionOptions` enum — unchanged
- `tool_permissions.rs` (Hook 1 target) — unchanged
- `agent-client-protocol` crate — unchanged (0.10.2)

---

## 3. Binary Offset Discovery

Disassembled `AcpThread::push_entry` in the v0.233.0 binary
(`/Applications/Zed Preview.app/Contents/MacOS/zed.original`):

```asm
__RNvMsp_CshIajxtHV62U_10acp_threadNtB5_9AcpThread10push_entry:
    ...
    ldr  x8, [x0, #0xa8]!      ; cap   = *(self + 0xa8)   — pre-index, x0 += 0xa8
    ldr  x22, [x0, #0x10]      ; len   = *(self + 0xb8)
    cmp  x22, x8
    b.ne +0x8                  ; skip grow_one
    bl   grow_one
    ldr  x8, [x21, #0xb0]      ; ptr   = *(self + 0xb0)
    mov  w9, #0x1c0            ; entry_size = 0x1c0 (unchanged)
    madd x0, x22, x9, x8       ; slot  = len * entry_size + ptr
    ...
    str  x8, [x21, #0xb8]      ; len++ (written at 0xb8)
```

This gives the Vec<AgentThreadEntry> layout (repr(Rust) reorders fields from
source order):

| Offset | Field |
|--------|-------|
| `0xa8` | `entries.cap` |
| `0xb0` | `entries.ptr` |
| `0xb8` | `entries.len` |

Note that Rust's `repr(Rust)` chose the order (cap, ptr, len) in memory even
though the source declares `struct Vec { buf: RawVec { ptr, cap }, len }`.
Previous Zed versions happened to produce (ptr, cap, len) at offsets
`0x90`/`0x98`/`0xa0`; the compiler chose a different permutation for v0.233.0.

---

## 4. Runtime Verification

### Before (v0.232.0 offsets on v0.233.0 — broken):

```
tool_authorization #1 [s:d180]: on_leave (self=0x81adad180, call_id="toolu_01Sca1")
tool_authorization #1 [s:d180]: entries ptr=0x0, len=2     ← reading wrong offset
tool_authorization #1 [s:d180]: no entries, skipping
```

The old `0x90` / `0x98` offsets landed inside unrelated fields, giving a null
pointer + garbage length, which made all approvals silently fall through.

### After (v0.233.0 offsets — working):

```
tool_authorization #1 [s:2300]: on_leave (self=0xaf6d02300, call_id="toolu_01WnzS")
tool_authorization #1 [s:2300]: entries ptr=0xafc67c000, len=97
tool_authorization #1: layout=v0.230.x entry[96] disc=0x2 status_head=0x8000000000000000 id_len=30
tool_authorization #1: matched v0.230.x entry[96] by ToolCallId, respond_tx=0xb032a6d00, plan_mode=false
tool_authorization #1: send succeeded (option_id=PermissionOptionId("allow_always"))
tool_authorization #1 [s:2300]: approved in 47us via v0.230.x call_id="toolu_01WnzS"
```

Valid entries pointer, correct length (97), entry matched by ToolCallId,
oneshot send succeeded, approval latency ~47µs — identical behavior to prior
versions.

---

## 5. Files Changed

| File | Change |
|------|--------|
| `src/hooks/tool_authorization.rs` | `ENTRIES_PTR_OFFSET`: `0x90` → `0xb0`, `ENTRIES_LEN_OFFSET`: `0x98` → `0xb8`, doc comments refreshed |
| `src/hooks/entry_scanner.rs` | Mirror constants updated to `0xb0` / `0xb8` |

All hooks attach at load time on v0.233.0 (5/5 symbols resolved), and the
ACP auto-approve path works end-to-end. No changes to xtask, Cargo.toml, or
scripts.

---

## 6. Watchlist

Because `repr(Rust)` can reorder fields whenever the struct changes, any future
Zed release that adds/removes/resizes a field in `AcpThread` may again shift
the `entries` offset. The automation that reliably detects this is
`cargo patch --verify` plus a manual tool-call smoke test — a source-only
comparison is necessary but not sufficient.
