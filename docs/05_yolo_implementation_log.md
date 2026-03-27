# YOLO Hook Implementation Log & Methodology

> Date: 2026-02-26, updated 2026-03-27
> Historical working version: v0.17.1 on Zed Preview v0.226.0 / Zed Stable v0.225.9
> Current recalibration pass: Preview v0.230.0
> Latest patch verification: `cargo patch --verify` passed on 2026-03-27

---

## 1. 2026-03-27 Recalibration for Preview v0.230.0

This project had a stable historical solution on the older `0.225.x / 0.226.x` binaries, but the
Preview `0.230.0` app changed enough that the ACP hook had to be re-derived from the installed
binary.

### Ground truth used during this pass

- Installed app under test:
  `0.230.0+preview.205.9437a84390a396d666f04b38db87d89bb07284c1`
- Bundle build:
  `20260325.153514`
- Local source checkout:
  `6bc34ff44f9931a77e5e82cff87dc2aa266a41a4`

The local source checkout was useful for call flow, but the final offsets had to come from binary
analysis because the app and source commits did not match.

### Source call path confirmed during the update

```text
agent_servers::acp::ClientDelegate::request_permission
  -> AcpThread::request_tool_call_authorization
     -> upsert_tool_call_inner
     -> ToolCallStatus::WaitingForConfirmation { options, respond_tx }

agent_ui::Conversation::authorize_tool_call
  -> AcpThread::authorize_tool_call
     -> tool_call_mut(id)
     -> respond_tx.send(outcome)
```

This matters because the correct runtime model is not “find the newest waiting-looking entry”; it
is “find the same `ToolCall` entry that `authorize_tool_call` would find by `ToolCallId`.”

### Phase A — first 0.230 port [loads, but does not approve]

The first port correctly updated:

- `ENTRY_SIZE`: `0x1b0 -> 0x1c0`
- `ENTRY_STATUS_OFFSET`: `0x48 -> 0x118`
- `ENTRY_RESPOND_TX_OFFSET`: `0x68 -> 0x160`
- send payload: legacy string ID -> `SelectedPermissionOutcome`

But it still preserved two stale assumptions from the previous generation of docs:

- `ToolCall` entry discriminant was still treated as `0x07`
- `WaitingForConfirmation` was still treated as an exact discriminant value

Result in logs:

```text
2026-03-27T11:36:10Z  tool_authorization #1: no WaitingForConfirmation entry found
...
2026-03-27T11:57:53Z  tool_authorization #25: no WaitingForConfirmation entry found
```

Interpretation:

- symbol hook install worked
- offset move to `0x118 / 0x160` was not enough by itself
- the entry matching logic was still wrong for Preview `0.230.0`

### Phase B — final 0.230 port [current]

The final 0.230 update changed the hook strategy:

1. capture `ToolCallUpdate.tool_call_id` in `on_enter`
2. walk `self.entries` in `on_leave`
3. match the exact entry by `ToolCallId`
4. treat waiting-state as a payload-bearing niche case:
   `status_head < 0x8000_0000_0000_0002`
5. reconstruct the real `oneshot::Sender<SelectedPermissionOutcome>`
6. send:
   `SelectedPermissionOutcome { option_id: "allow", option_kind: AllowOnce, params: None }`

New binary-derived facts that changed the hook:

| Item | Value |
|------|-------|
| `AcpThread.entries.ptr` | `self + 0x90` |
| `AcpThread.entries.len` | `self + 0x98` |
| `AgentThreadEntry` size | `0x1c0` |
| `AgentThreadEntry::ToolCall` discriminant | `0x02` |
| `ToolCall.status` offset | `0x118` |
| `respond_tx` offset | `0x160` |
| `ToolCall.id.ptr/len` | `0x168 / 0x170` |
| `ToolCallUpdate.id.ptr/len` | `0x128 / 0x130` |
| waiting-state test | `status_head < 0x8000_0000_0000_0002` |

### Operational workflow used in this pass

```bash
# 1. Read source call path
sed -n '1435,1475p' /Users/lqiao/codes-repos/gh-zed-industries__zed/crates/agent_servers/src/acp.rs
sed -n '1880,2065p' /Users/lqiao/codes-repos/gh-zed-industries__zed/crates/acp_thread/src/acp_thread.rs

# 2. Read installed binary symbols / disassembly
nm -nm "/Applications/Zed Preview.app/Contents/MacOS/zed"
xcrun llvm-objdump --disassemble-symbols='__RNvMsn_Cs3xb0dWJrqhb_10acp_threadNtB5_9AcpThread31request_tool_call_authorization' ...
xcrun llvm-objdump --disassemble-symbols='__RNvMsn_Cs3xb0dWJrqhb_10acp_threadNtB5_9AcpThread19authorize_tool_call' ...
xcrun llvm-objdump --disassemble-symbols='__RNvMsn_Cs3xb0dWJrqhb_10acp_threadNtB5_9AcpThread22upsert_tool_call_inner' ...

# 3. Update hook code
cargo check
cargo build --release

# 4. Patch and verify
cargo patch --verify

# 5. Confirm the binary and logs
otool -L "/Applications/Zed Preview.app/Contents/MacOS/zed"
tail -n 80 ~/Library/Logs/Zed/zed-yolo-hook.$(date +%Y-%m-%d).log
```

### Validation state after the final patch

Verified:

- `cargo check` passed
- `cargo build --release` passed
- `cargo patch --verify` passed
- `otool -L` shows
  `/Users/lqiao/codes/zed-yolo-hook/target/release/libzed_yolo_hook.dylib`
- fresh post-patch startup logs show:
  - `permission_decision: hook installed`
  - `tool_authorization: hook installed`
  - `YOLO mode ACTIVE`

Later re-observed during the same pass:

- a fresh post-patch ACP authorization event at `2026-03-27T12:03:12Z` showing:
  - `tool_authorization #1: matched v0.230.x entry[5] by ToolCallId`
  - `tool_authorization #1: send succeeded`
  - `tool_authorization #1: approved in 54us via v0.230.x`

So the final status on 2026-03-27 is:

- patch / injection / startup: confirmed
- pre-fix failure mode: reproduced and explained
- final runtime match logic: updated to align with binary and source
- live post-patch ACP approval log: confirmed

---

## 2. Historical Working Architecture (v0.17.1)

The zed-yolo-hook intercepts TWO code paths via separate modules:

### Hook 1: `permission_decision` — Built-in tools (`ToolPermissionDecision::from_input`)

- **Source**: `src/hooks/permission_decision.rs`
- **Method**: Frida `attach` with `InvocationListener`
- **Hook point**: `on_leave` — zeroes the sret buffer (x8) to force `Allow` return
- **Works because**: Simple return value manipulation, no ABI complexity
- **Symbol**: `_RNvMNtCs..._5agent16tool_permissionsNtB2_22ToolPermissionDecision10from_input`

### Hook 2: `tool_authorization` — ACP agents (`request_tool_call_authorization`)

- **Source**: `src/hooks/tool_authorization.rs`
- **Method**: Frida `attach` with `InvocationListener` + `dispatch_async_f` (from `src/ffi/dispatch.rs`)
- **Hook point**: `on_enter` saves `self` (x0), `on_leave` walks `self.entries` Vec to find `respond_tx` (oneshot::Sender), then schedules `dispatch_async_f` to send `"allow"` through the channel
- **Key insight**: Reconstruct `futures_channel::oneshot::Sender<Arc<str>>` via `transmute` from the raw Arc pointer found in the entry, then call the real `.send()` API
- **Symbol**: `_RNvMsk_Cs..._10acp_threadNtB5_9AcpThread31request_tool_call_authorization`

### Module Structure

```
src/
├── lib.rs                        # Entry point: #[ctor] init, hook orchestration
├── config.rs                     # YoloMode enum, env var parsing
├── logging.rs                    # Tracing file appender setup
├── symbols.rs                    # Generic Frida symbol search by pattern
├── ffi/
│   ├── mod.rs
│   └── dispatch.rs              # macOS libdispatch (dispatch_async_f)
└── hooks/
    ├── mod.rs                    # Hook module docs, shared counters
    ├── permission_decision.rs    # Hook 1: ToolPermissionDecision::from_input
    └── tool_authorization.rs     # Hook 2: AcpThread::request_tool_call_authorization
```

### Memory Layout Constants (Zed v0.226.0 aarch64)

```
AcpThread struct:
  +0x60 = entries.ptr    (Vec<AgentThreadEntry> data pointer)
  +0x68 = entries.len    (Vec<AgentThreadEntry> length)

AgentThreadEntry:
  size = 0x1b0 (432 bytes)
  +0x00 = discriminant   (0x07 = ToolCall variant)
  +0x20 = ToolCallStatus discriminant (0x00 = WaitingForConfirmation, niche-optimized)
  +0x40 = respond_tx     (Arc<Inner<T>> pointer — the oneshot::Sender)
```

---

## 3. Full Version History

### v0.11.0 — Inline ASM call to authorize_tool_call [DOES NOT WORK]
- **Approach**: Save registers in `on_enter`, call `authorize_tool_call` via inline asm in `on_leave`
- **Result**: CRASH — `BUG IN CLIENT OF LIBMALLOC: memory corruption of free block`
- **Root cause**: `Arc::into_raw` + inline asm call (takes ownership) + `Arc::from_raw` = double-free

### v0.12.0 — Remove double-free [DOES NOT WORK]
- **Fix**: Removed `Arc::from_raw` cleanup after asm call
- **Result**: CRASH — memory corruption on Thread 27 (CoreAnimation)
- **Root cause**: Inline asm passed values with wrong ABI, corrupting heap

### v0.13.0 — Typed extern "C" function pointer [DOES NOT WORK]
- **Approach**: Replace inline asm with `extern "C" fn` pointer cast to authorize_tool_call
- **Result**: HANG — app froze
- **Root cause**: Re-entrancy. `authorize_tool_call` calls `cx.emit()` which processes GPUI effects while `&mut self` and `&mut Context` are still borrowed by caller of `request_tool_call_authorization`

### v0.14.0 — dispatch_async_f deferred call [DOES NOT WORK]
- **Approach**: Use `dispatch_async_f` on main queue to call `authorize_tool_call` after call stack unwinds
- **Result**: NO EFFECT — dialog still showed
- **Root cause 1**: Wrong tool_call_id extraction. `ToolCallUpdate` uses `#[repr(Rust)]` which allows field reordering. We read offset 0 but `tool_call_id` was elsewhere
- **Root cause 2**: `cx` (Context) pointer was stale — it's a stack reference, dead after the calling function returns. Function returned early because wrong tool_call_id matched nothing

### v0.15.0 — Cmd+Y keystroke synthesis via NSApp sendEvent [DOES NOT WORK]
- **Approach**: Synthesize Cmd+Y NSEvent via `CGEventCreateKeyboardEvent` + `[NSApp sendEvent:]`
- **Result**: NO EFFECT — event was ignored
- **Root cause**: GPUI doesn't process keystrokes through `[NSApp sendEvent:]`. Events are handled at the `NSView.keyDown:` level

### v0.15.1 — Send keystroke to contentView directly [DOES NOT WORK]
- **Approach**: `[contentView performKeyEquivalent:]` and `[contentView keyDown:]`
- **Result**: NO EFFECT — `performKeyEquivalent` returned YES but action wasn't dispatched
- **Root cause**: `cmd-y → AllowOnce` keybinding only active in `"AgentPanel"` context. If focus is elsewhere, `cmd-y` maps to `git::StageAndNext` instead

### v0.16.0 — Reverse-engineer register layout, call authorize_tool_call with correct args [DOES NOT WORK]
- **Approach**: Disassembled `authorize_tool_call` to find exact register layout:
  - x0=self, x1=ToolCallId.ptr, x2=ToolCallId.len, x3=OptionId.ptr, x4=OptionId.len, x5=kind, x6=cx
- **Approach**: Scan `ToolCallUpdate` struct for `Arc<str>` pattern `(len, ptr)` — Rust stores fat pointers as `(metadata, data_ptr)` on aarch64
- **Result**: CRASH — `BUG IN CLIENT OF LIBMALLOC: memory corruption`
- **Root cause**: v0.16.0 found wrong match in scan (word[4] matched before actual tool_call_id). Also `Arc<str>` in struct is `(len, ptr)` but registers expect `(ptr, len)` — swapped

### v0.16.1 — Fixed scan direction and register mapping [DOES NOT WORK]
- **Fix**: Scan for `(small_len, heap_ptr)` pattern. Added Arc refcount bump
- **Result**: HANG
- **Root cause**: Synchronous call to `authorize_tool_call` from `on_leave` caused re-entrancy deadlock again — `cx.emit()` inside the function deadlocks when called from within the Frida interceptor trampoline

### v0.17.0 — Direct memory walk to find respond_tx, manual oneshot send [DOES NOT WORK]
- **Approach**: Walk `self.entries` Vec using known offsets, find `WaitingForConfirmation` entry, extract `respond_tx` pointer, manually write value into oneshot channel via raw memory writes, use `dispatch_async_f` to defer
- **Result v1**: NO EFFECT — `WAITING_VARIANT` was set to `0x01` but actual value is `0x00` (Rust niche optimization)
- **Result v2**: HANG — found correct entry, wrote value, but receiver not woken. Wrote at wrong offsets in `Inner<T>` because `Lock<T>` field layout assumptions were wrong
- **Result v3**: HANG — wrote correctly but `rx_task` lock was held, couldn't wake receiver

### v0.17.1 — Reconstruct real Sender via transmute, call .send() API [CURRENT WORKING VERSION]
- **Source**: `src/hooks/tool_authorization.rs`
- **Approach**: Instead of manual memory writes, reconstruct a `futures_channel::oneshot::Sender<Arc<str>>` by transmuting the raw Arc pointer. Bump Arc refcount first, then call the real `.send()` API. Drop triggers `drop_tx` which sets complete=true and wakes rx_task automatically
- **Result**: SUCCESS! `send succeeded!` — tool calls auto-approved, no crash, no hang
- **Key insight**: Let the futures-channel library handle its own internal state machine. Don't try to replicate it manually

---

## 4. Key Technical Discoveries

### aarch64 Rust ABI

- `Arc<str>` (fat pointer) stored in memory as **(metadata/len, data_ptr)** — reversed from typical `(ptr, len)` documentation
- In registers, Rust passes fat pointers as **(data_ptr in lower reg, len in higher reg)** — so x1=ptr, x2=len
- `#[repr(Rust)]` allows field reordering — NEVER assume struct field order matches declaration
- Large structs (>16 bytes) passed by hidden pointer on aarch64
- Return values >16 bytes use sret via x8

### Disassembly patterns (Zed v0.226.0)

```
request_tool_call_authorization:
  x0 = &mut self (AcpThread)
  x1 = *const ToolCallUpdate (hidden pointer)
  x2 = *const PermissionOptions (hidden pointer)
  x3 = &mut Context<Self>
  x8 = sret for Result<BoxFuture>

authorize_tool_call:
  x0 = &mut self
  x1 = ToolCallId ArcInner ptr
  x2 = ToolCallId str len
  x3 = PermissionOptionId ArcInner ptr
  x4 = PermissionOptionId str len
  x5 = PermissionOptionKind (u8)
  x6 = &mut Context<Self>
```

### Enum discriminant values (niche-optimized)

- `AgentThreadEntry::ToolCall` = `0x07` (not sequential 2)
- `ToolCallStatus::WaitingForConfirmation` = `0x00` (not sequential 1 — niche optimization because this variant has data while Pending doesn't)
- `ToolCallStatus::InProgress` = `0x07` (written by authorize_tool_call for AllowOnce)
- `ToolCallStatus::Rejected` = `0x04`

### Vec<T> in struct

- `Vec<T>` is `(ptr, len, cap)` = 24 bytes
- `AcpThread.entries` at offset 0x60 from self (confirmed by disassembly: `ldr x9, [x0, #0x60]`)

### futures::channel::oneshot internals

```rust
struct Inner<T> {
    complete: AtomicBool,
    data: Lock<Option<T>>,        // Lock = { locked: AtomicBool, data: UnsafeCell<T> }
    rx_task: Lock<Option<Waker>>,
    tx_task: Lock<Option<Waker>>,
}
struct Sender<T> { inner: Arc<Inner<T>> }
```

- `Sender::send()` acquires data lock, writes value, releases lock
- `Sender::drop()` calls `drop_tx`: sets `complete=true`, then locks `rx_task` and calls `waker.wake()`
- The wake is CRITICAL — without it, the receiver future never re-polls and the app hangs
- **Lesson**: Don't replicate internal state machines manually. Reconstruct the actual type and use its API

### GPUI constraints

- `&mut Context<Self>` is a **stack reference** — invalid after the calling function returns
- `cx.emit()` triggers effect processing — causes re-entrancy deadlock if called from Frida interceptor
- Keybindings are context-scoped (e.g., `cmd-y` only works in `AgentPanel` context)
- GPUI doesn't process keystrokes through `[NSApp sendEvent:]` — uses custom `NSView.keyDown:` responder

### dispatch_async_f

- Safe way to schedule work after current call stack unwinds
- Avoids re-entrancy issues with `&mut self` borrows
- `dispatch_get_main_queue()` is actually a macro — real symbol is `_dispatch_main_q` (a static variable)
- `dispatch_after_f` for delayed execution (used in keystroke approach)

---

## 5. Approaches That DON'T Work (and Why)

| Approach | Why it fails |
|----------|-------------|
| Inline ASM to call Rust functions | ABI mismatch, register corruption, double-free |
| `extern "C"` fn pointer to Rust function | Rust ABI != C ABI for non-trivial types |
| Synchronous call from on_leave | Re-entrancy: cx.emit() deadlocks |
| dispatch_async + stale cx pointer | cx is stack reference, dangling after return |
| Cmd+Y via `[NSApp sendEvent:]` | GPUI doesn't use NSApp event dispatch |
| Cmd+Y via `[contentView keyDown:]` | Focus-dependent; wrong action if not in AgentPanel |
| Manual oneshot channel memory writes | Wrong field offsets, no waker notification |
| `Interceptor::replace` for Rust methods | Requires matching unstable Rust ABI exactly |

---

## 6. The Approach That WORKS

1. **Frida `attach`** on `request_tool_call_authorization` (not `replace`)
2. **`on_enter`**: save `self` pointer (x0)
3. **`on_leave`**: walk `self.entries` Vec using known offsets from disassembly
4. Find last entry with `disc=0x07` (ToolCall) and `status=0x00` (WaitingForConfirmation)
5. Extract `respond_tx` at entry+0x40 (Arc pointer to oneshot Inner)
6. **`dispatch_async_f`** on main queue with the respond_tx pointer
7. In deferred callback: bump Arc refcount, **transmute** pointer into real `oneshot::Sender<Arc<str>>`, call `.send(Arc::from("allow"))`
8. Sender drop automatically: sets complete=true, wakes receiver task

---

## 7. Source Code Locations (Zed v0.226.0)

| File | Line | Description |
|------|------|-------------|
| `crates/acp_thread/src/acp_thread.rs` | 1738-1765 | `request_tool_call_authorization` — creates oneshot, stores tx |
| `crates/acp_thread/src/acp_thread.rs` | 1767-1797 | `authorize_tool_call` — finds entry, sends through tx |
| `crates/acp_thread/src/acp_thread.rs` | 1647-1663 | `tool_call_mut` — finds entry by ToolCallId |
| `crates/acp_thread/src/acp_thread.rs` | 938-958 | `AcpThread` struct (entries at field 3) |
| `crates/acp_thread/src/acp_thread.rs` | 482-502 | `ToolCallStatus` enum |
| `crates/agent_ui/src/agent_ui.rs` | 116-119 | `AllowOnce`, `AllowAlways` action definitions |
| `crates/agent_ui/src/acp/thread_view/active_thread.rs` | 1266-1296 | `allow_once`, `handle_authorize_tool_call` |
| `crates/agent/src/thread.rs` | 799-807 | Permission options (option_id="allow") |
| `crates/agent_servers/src/acp.rs` | 1130-1156 | ACP server `request_permission` handler |
| `crates/agent/src/agent.rs` | 1036-1057 | Internal agent `ToolCallAuthorization` handler |
| `assets/keymaps/default-macos.json` | 301-303 | `cmd-y → AllowOnce` (AgentPanel context only) |
| ACP schema `client.rs` | 599 | `PermissionOptionId(pub Arc<str>)` |
| ACP schema `client.rs` | 613-622 | `PermissionOptionKind` enum |
| ACP schema `tool_call.rs` | 164-177 | `ToolCallUpdate` struct |
