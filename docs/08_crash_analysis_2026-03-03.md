# Crash & Hang Analysis: 2026-03-03

> Date: 2026-03-03
> Incident: Zed Preview v0.226.2 (PID 54762) hung at 20:24:02 CST
> Scope: Cross-repo analysis of zed-yolo-hook, zed-prj-workspace-hook, and dylib-kit
> Logs analyzed: hung10.txt, Zed.log, zed-yolo-hook.2026-03-03.log, zed-prj-workspace-hook.2026-03-03.log

---

## Table of Contents

1. [Incident Summary](#1-incident-summary)
2. [Hang Report Analysis (hung10.txt)](#2-hang-report-analysis)
3. [Bug 1 — CRITICAL: Unsafe Vec Access Race in tool_authorization](#3-bug-1--critical-unsafe-vec-access-race-in-tool_authorization)
4. [Bug 2 — CRITICAL: Arc Pointer Reconstruction via Transmute](#4-bug-2--critical-arc-pointer-reconstruction-via-transmute)
5. [Bug 3 — CRITICAL: No Subprocess Guard in Hook Initialization](#5-bug-3--critical-no-subprocess-guard-in-hook-initialization)
6. [Bug 4 — HIGH: Blind 32-byte Zeroing in permission_decision](#6-bug-4--high-blind-32-byte-zeroing-in-permission_decision)
7. [Bug 5 — MEDIUM: Socket Server EINVAL Race](#7-bug-5--medium-socket-server-einval-race)
8. [Bug 6 — MEDIUM: Workspace Write Log Spam](#8-bug-6--medium-workspace-write-log-spam)
9. [Bug 7 — LOW: WaitingForConfirmation Not Found](#9-bug-7--low-waitingforconfirmation-not-found)
10. [Root Cause Assessment](#10-root-cause-assessment)
11. [Recommended Fixes](#11-recommended-fixes)

---

## 1. Incident Summary

Zed Preview (PID 54762) experienced a system-reported **1.42s hang** at 20:24:02. The crash dump (`09991d92-e057-4a48-beea-e76b55a3c026.json`) confirms no Rust panic — the crash was a **kernel-level thread suspension**. The main thread had not run for 67.593 seconds before the hang was captured, with all 279 threads suspended.

Both injected dylibs (`zed-yolo-hook` and `zed-prj-workspace-hook`) were active. The hooks initialized in **10+ subprocess PIDs** within seconds of Zed launch. Three `WaitingForConfirmation` lookup failures occurred in the yolo-hook, and two `EINVAL` socket errors in the workspace-hook.

### Log Evidence Summary

| Source | Key Observations |
|--------|-----------------|
| `hung10.txt` | Main thread stuck in `_sigtramp`; all threads suspended in kernel |
| `zed-yolo-hook.log` | 3x `no WaitingForConfirmation entry found` warnings; hooks installed in 10+ PIDs |
| `zed-prj-workspace-hook.log` | 2x `Invalid argument (os error 22)` in socket_server; 175 "Workspace write detected" messages |
| `Zed.log` | codanna context server timeout; model refresh timeout; LSP broken pipe |

---

## 2. Hang Report Analysis

From `hung10.txt`:

```
Event:            hang
Duration:         1.42s
Steps:            15 (100ms sampling interval)
Time Since Fork:  74s
Num threads:      279

Thread 0x10d74ed1    Thread name "main"    15 samples (1-15)
  15  _sigtramp + 0 (libsystem_platform.dylib + 14124) [0x18f68172c]
   *15  ??? (kernel.release.t8122 + 753352) [0xfffffe00088d3ec8] (suspended)
```

The main thread is trapped in a signal handler (`_sigtramp`) and was suspended by the kernel. This is consistent with memory corruption triggering a signal (SIGBUS/SIGSEGV) that escalated to a full process suspension. The "last ran 67.593s ago" note means the main thread had been non-functional for over a minute.

---

## 3. Bug 1 — CRITICAL: Unsafe Vec Access Race in tool_authorization

**File:** `src/hooks/tool_authorization.rs:131-137`

```rust
let (entries_ptr, entries_len) = unsafe {
    let p = self_ptr as *const u64;
    let ptr = *p.byte_add(ENTRIES_PTR_OFFSET);   // AcpThread + 0x60
    let len = *p.byte_add(ENTRIES_LEN_OFFSET);   // AcpThread + 0x68
    (ptr, len)
};
```

**Problem:** The hook reads `entries.ptr` and `entries.len` from `AcpThread` without any synchronization. Zed's own threads can concurrently:

- **Reallocate the Vec** (changing `ptr`, making the old `ptr` dangling)
- **Push/pop entries** (changing `len`, causing out-of-bounds iteration)
- **Drop entries** (invalidating memory at previously-valid offsets)

This is a classic **TOCTOU (time-of-check-time-of-use)** race. Between reading `ptr`/`len` at lines 134-135 and iterating at line 166, the underlying data can change.

**Downstream impact:** The iteration at lines 166-195 reads entry discriminants and status values from potentially invalid memory:

```rust
for i in (0..entries_len).rev() {
    let entry = entries_ptr + (i * ENTRY_SIZE as u64);
    let discriminant = unsafe { *(entry as *const u64).byte_add(ENTRY_DISCRIMINANT_OFFSET) };
    let status = unsafe { *(entry as *const u64).byte_add(ENTRY_STATUS_OFFSET) };
```

If `entries_ptr` is stale (Vec was reallocated), this reads freed memory → undefined behavior → crash or hang.

**Evidence from logs:** The 3 `no WaitingForConfirmation entry found` warnings (at 12:57:12, 12:57:21, 12:57:22) suggest the hook occasionally can't find the expected entry, which could be a symptom of reading stale data after a Vec reallocation.

---

## 4. Bug 2 — CRITICAL: Arc Pointer Reconstruction via Transmute

**File:** `src/hooks/tool_authorization.rs:62-108` (`deferred_send`)

```rust
unsafe {
    let strong = sender_arc_ptr as *const std::sync::atomic::AtomicUsize;
    (*strong).fetch_add(1, Ordering::Relaxed);          // Bump refcount

    let sender: futures_channel::oneshot::Sender<Arc<str>> =
        std::mem::transmute(sender_arc_ptr);            // Reconstruct Sender
```

**Problems:**

1. **Unvalidated pointer:** `sender_arc_ptr` is read from entry memory at offset 0x40 with only a `> 0x1_0000_0000` check (line 186). This threshold is insufficient to distinguish valid heap pointers from corrupted values on aarch64 where user-space addresses can span a wide range.

2. **Use-after-free window:** Between capturing `respond_tx` in `on_leave` and executing `deferred_send` (dispatched via `dispatch_async_f`), the original `AcpThread` entry could be dropped by Zed. When `deferred_send` runs, it may be operating on freed memory.

3. **Double-free risk:** The `fetch_add(1, Relaxed)` bumps the Arc strong count to account for the reconstructed Sender. But if the original entry is dropped between the bump and the send, the refcount arithmetic becomes incorrect, leading to a double-free when both the entry's Sender and the reconstructed Sender are dropped.

4. **Transmute assumptions:** `std::mem::transmute(sender_arc_ptr)` assumes `Sender<T>` has the same layout as a raw pointer. This is true for current `futures-channel` but is not guaranteed by any stability promise.

---

## 5. Bug 3 — CRITICAL: No Subprocess Guard in Hook Initialization

**Files:**
- `src/lib.rs:26-29` (zed-yolo-hook)
- `zed-prj-workspace-hook/src/lib.rs:31-33`

```rust
#[ctor]
fn init() {
    INIT_ONCE.call_once(init_inner);
}
```

Both hooks use `#[ctor]` which executes for every process that loads the dylib. Since the dylib is injected via `LC_LOAD_WEAK_DYLIB` in the Zed binary, every child process (language servers, MCP servers, CLI subprocesses, scratch processes) also loads and initializes the hooks.

**Observed impact:** Today's logs show hooks initializing in PIDs: 54762, 54845, 55024, 55448, 55482, 55487, 55488, 56117, 56124, 56128, 56183, 56218, 56241, 56263, 56264, 56265, 56267, 56279, 56285, 56290 — **20+ processes**.

Each subprocess independently:
- Scans the Zed binary symbol table (`symbols::find_by_pattern`) — expensive I/O
- Installs Frida-Gum interceptors on the same functions
- Creates socket servers (workspace-hook)
- Runs sync/discovery threads (workspace-hook)
- Registers in the hook registry

**Why it matters for crashes:** Multiple processes intercepting the same function addresses with Frida creates potential for instruction cache conflicts and race conditions during hook installation.

**Note:** The `dylib-patcher` crate in `dylib-kit` already has `is_running_inside_target()` which walks the parent process ancestry — but neither hook uses this logic.

---

## 6. Bug 4 — HIGH: Blind 32-byte Zeroing in permission_decision

**File:** `src/hooks/permission_decision.rs:23-26`

```rust
if x8 != 0 && (x8 >> 32) < 2 {
    unsafe {
        std::ptr::write_bytes(x8 as *mut u8, 0, 32);
    }
```

**Problems:**

1. **Weak validation:** The check `(x8 >> 32) < 2` only verifies the upper 32 bits are 0 or 1, which includes most valid aarch64 user-space pointers *and* many invalid ones. It does not verify alignment, heap validity, or that x8 actually points to the return buffer.

2. **Size assumption:** Zeroing exactly 32 bytes assumes the `ToolPermissionDecision` return struct is >= 32 bytes. If a Zed update changes this struct's layout, the write could overwrite adjacent stack or heap data.

3. **No bounds checking:** Unlike `tool_authorization` which at least checks pointer ranges, this hook has no way to verify the target memory region is writable and belongs to the expected struct.

**Mitigation:** This hook has been stable in practice because `x8` (the sret pointer) is reliably set by the ARM64 calling convention for indirect return values. However, any compiler optimization that changes the return convention would silently break this.

---

## 7. Bug 5 — MEDIUM: Socket Server EINVAL Race

**File:** `zed-prj-workspace-hook/src/socket_server.rs:82-83`

```rust
fn handle_connection(stream: UnixStream, channel: &str, pid: u32) -> std::io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
```

**File:** `zed-project-workspace/src/hook_client.rs` (client side)

```rust
stream.write_all(msg.as_bytes())?;
stream.flush()?;
stream.shutdown(std::net::Shutdown::Write)?;
```

**Race condition:** If the client connects, writes, and calls `shutdown(Write)` before the server thread reaches `set_read_timeout()`, the `set_read_timeout` call returns `EINVAL` (os error 22) because the stream is partially closed.

**Today's evidence:** 2 occurrences at 12:22:53.809 and 12:28:36.587.

**Impact:** Low — the commands themselves still succeed (the log shows `reuse_folders` processed successfully). The error is cosmetic but confusing.

**Additional issue:** Thread spawn errors are silently swallowed at line 71:

```rust
.spawn(move || { ... })
.ok();  // Errors silently discarded
```

---

## 8. Bug 6 — MEDIUM: Workspace Write Log Spam

**File:** `zed-prj-workspace-hook/src/sync.rs:36`

```rust
pub fn on_workspace_write_detected(_sql: &str, state: u8) {
    tracing::info!("Workspace write detected (state={})", state);   // UNCONDITIONAL
    if SYNC_PENDING.compare_exchange(...).is_ok() { ... }
```

**Problem:** The `tracing::info!` fires on *every* intercepted SQL write to the `workspaces` table, even when sync is already pending and the event will be coalesced. Today produced **175 messages** from this single line.

The actual sync thread spawning is properly debounced via `SYNC_PENDING` atomic, but the log output is not.

**State values:**
- `state=2` (`TARGET_PENDING`): Discovery not yet attempted
- `state=1` (`TARGET_SET`): Target known, ready to sync

---

## 9. Bug 7 — LOW: WaitingForConfirmation Not Found

**File:** `src/hooks/tool_authorization.rs:197-199`

```rust
if respond_tx == 0 {
    tracing::warn!("tool_authorization #{}: no WaitingForConfirmation entry found", count);
    return;
}
```

**Today's occurrences:** 3 warnings at 12:57:12, 12:57:21, 12:57:22.

**Possible causes:**
1. **Timing:** The hook fires in `on_leave` but the entry hasn't been fully written yet
2. **Offset mismatch:** `WAITING_VARIANT = 0x00` (niche-optimized) may not match all Zed builds
3. **Vec race (Bug 1):** Reading stale `entries_len` causing the scan to miss entries

This is a symptom rather than a root cause — likely triggered by Bug 1 or a benign timing window.

---

## 10. Root Cause Assessment

The most probable cause chain for the hang:

```
1. zed-yolo-hook's tool_authorization hook fires during an AI tool call
2. on_leave reads AcpThread.entries Vec WITHOUT synchronization (Bug 1)
3. A concurrent Zed thread reallocates or modifies the entries Vec
4. The hook reads stale/invalid pointers from freed memory
5. Memory corruption propagates → triggers SIGBUS/SIGSEGV
6. Signal handler (_sigtramp) entered → kernel suspends all threads
7. Main thread becomes unresponsive for 67+ seconds → macOS reports hang
```

**Contributing factors:**
- 20+ subprocess hook installations amplify the probability of races (Bug 3)
- The `deferred_send` callback may try to use a freed Sender (Bug 2)
- Each subprocess runs independent sync threads creating DB contention (Bug 6)

---

## 11. Recommended Fixes

### Priority 1: Address Memory Safety (Bugs 1, 2)

**Option A — Read entries in `on_enter` instead of `on_leave`:**
In `on_enter`, Zed hasn't yet created the `respond_tx` — but if we can capture the entries Vec state before the original function modifies it, we reduce the race window.

**Option B — Add a brief sleep + re-validate:**
After reading `entries_ptr`/`entries_len`, re-read and confirm they haven't changed. This doesn't eliminate the race but reduces the window.

**Option C — Use Frida's `replace` instead of `attach`:**
Replace the entire function to gain full control over execution flow, avoiding the need to read internal state concurrently.

**Option D — Validate respond_tx more thoroughly:**
Before capturing `respond_tx`, check that the memory at that offset looks like a valid Arc (verify strong count > 0, weak count > 0, alignment is correct).

### Priority 2: Add Subprocess Guard (Bug 3)

```rust
fn init_inner() {
    // Skip if we're not the main Zed UI process
    let exe = std::env::current_exe().unwrap_or_default();
    let exe_name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if exe_name != "zed" {
        return; // CLI, language server, or other subprocess
    }
    // ... rest of init ...
}
```

Or use process ancestry checking from `dylib-patcher::is_running_inside_target()`.

### Priority 3: Fix Logging and Socket Issues (Bugs 5, 6)

**Workspace write log spam** — move the info log inside the `compare_exchange` success branch:

```rust
pub fn on_workspace_write_detected(_sql: &str, state: u8) {
    if SYNC_PENDING.compare_exchange(false, true, ...).is_ok() {
        tracing::info!("Workspace write detected (state={}), spawning sync", state);
        // ...
    } else {
        tracing::debug!("Workspace write coalesced (state={})", state);
    }
}
```

**Socket EINVAL** — catch and downgrade the error:

```rust
fn handle_connection(stream: UnixStream, ...) -> std::io::Result<()> {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
    // ...
}
```

---

## Files Referenced

| File | Repo | Relevance |
|------|------|-----------|
| `src/hooks/tool_authorization.rs` | zed-yolo-hook | Bug 1, 2, 7 |
| `src/hooks/permission_decision.rs` | zed-yolo-hook | Bug 4 |
| `src/lib.rs` | zed-yolo-hook | Bug 3 |
| `zed-prj-workspace-hook/src/socket_server.rs` | zed-project-workspace | Bug 5 |
| `zed-prj-workspace-hook/src/sync.rs` | zed-project-workspace | Bug 6 |
| `zed-prj-workspace-hook/src/lib.rs` | zed-project-workspace | Bug 3 |
| `logs/hung10.txt` | zed-project-workspace | Hang report |
| `~/Library/Logs/Zed/zed-yolo-hook.2026-03-03.log` | (runtime) | Bug 7 evidence |
| `~/Library/Logs/Zed/zed-prj-workspace-hook.2026-03-03.log` | (runtime) | Bug 5, 6 evidence |
