# Hang Analysis: Zed Preview v0.227.0 — 2026-03-05

> Date: 2026-03-05
> Incident: Zed Preview v0.227.0 (PID 66444) hung at 19:39:44 CST
> Build: 0.227.0+preview.196.170485782a1d0ffb3a67f3382681b50deea10b6e
> Log: `logs/hung-2026.03.05-19.40.txt`
> Scope: zed-yolo-hook offset staleness after Zed version bump

---

## 1. Incident Summary

| Field | Value |
|-------|-------|
| Event | hang |
| Duration | 44.93s (sampled 1.10s after 44s unresponsive) |
| Time Since Fork | 140s (hook worked ~95s before hang) |
| Num threads | 38 |
| Main thread | Stuck in `libzed_yolo_hook.dylib + 0x8c8bc` → `_sigtramp` → kernel (suspended) |
| Deadlock | MxNotify [1578] self-deadlock (unrelated system process) |

The process ran normally for ~95 seconds, then the main thread became unresponsive for 44.93 seconds. All 38 threads were suspended by the kernel at the time of the stackshot.

---

## 2. Main Thread Backtrace

```
Thread 0x510e50    "main"    11 samples (1-11)
  11  ??? [0x200854000100]                                     ← JIT/coroutine frame
    11  ??? [0x11cb1800c]                                      ← unresolvable (patched)
      11  ??? [0x1182b01ec]                                    ← unresolvable (patched)
        11  (libzed_yolo_hook.dylib + 575676) [0x1184548bc]   ← dylib code at 0x8c8bc
          11  <patched truncated backtrace>                    ← Frida-patched frames
            11  _sigtramp + 0                                  ← signal handler entered
             *11  kernel (suspended)
```

The `<patched truncated backtrace>` indicates the stackshot resampler could not unwind through Frida-Gum's interceptor trampolines. The `_sigtramp` suggests a signal (SIGBUS/SIGSEGV) was delivered while executing inside the hook's code path.

### Disassembly at crash address (dylib + 0x8c8bc)

```asm
8c864: ldr  w8, [x21, #0x8]      ; loop: load count/length
8c868: cmp  w22, w8               ; compare iteration index
8c86c: b.eq 0x8c8c4              ; exit loop if done
8c870: ldr  x8, [x21]            ; load base pointer
8c874: ldr  x8, [x8, w22, uxtw #3]  ; load element at index
8c878: cbz  x8, 0x8c8bc          ; if null → skip (branch to 8c8bc)
8c87c: cbz  w23, 0x8c888         ; check another flag
8c880: ldr  w9, [x8, #0x18]      ; load field from element
8c884: cbz  w9, 0x8c8bc          ; if zero → skip
...
8c8bc: add  w22, w22, #0x1       ; ← CRASH ADDRESS: increment index
8c8c0: b    0x8c864              ; back to loop top
```

This is a loop iterating over elements with stride, consistent with the entry-walking code in `tool_authorization.rs:on_leave`. The hang occurred while iterating entries with stale size/offset values.

---

## 3. Root Cause: Stale Memory Layout Offsets

The yolo hook hardcodes offsets calibrated for **v0.226.0**. Three struct changes in v0.227.0 invalidate them:

### 3.1 AcpThread — 2 new fields

```diff
  pub struct AcpThread {
      parent_session_id: Option<acp::SessionId>,
      title: SharedString,
      entries: Vec<AgentThreadEntry>,         ← offset 0x60 may have shifted
      plan: Plan,
      ...
      had_error: bool,
+     draft_prompt: Option<Vec<acp::ContentBlock>>,      // NEW (commit 42ba961075)
+     ui_scroll_position: Option<gpui::ListOffset>,       // NEW (commit 832782f6b3)
  }
```

Since Rust uses `repr(Rust)` (not `repr(C)`), the compiler may reorder fields for optimal alignment. Two new fields can change the offset of `entries` from its previous `0x60`.

**Impact:** `ENTRIES_PTR_OFFSET` and `ENTRIES_LEN_OFFSET` may be wrong.

### 3.2 ToolCall — field type grew

```diff
  pub struct ToolCall {
      ...
-     pub subagent_session_id: Option<acp::SessionId>,
+     pub subagent_session_info: Option<SubagentSessionInfo>,
  }
```

`SubagentSessionInfo` is a 3-field struct (vs bare `SessionId`):

```rust
pub struct SubagentSessionInfo {
    pub session_id: acp::SessionId,
    pub message_start_index: usize,
    pub message_end_index: Option<usize>,
}
```

This increases `sizeof(ToolCall)` → changes `sizeof(AgentThreadEntry)`.

**Impact:** `ENTRY_SIZE = 0x1b0` (432 bytes) is wrong.

### 3.3 AssistantMessage — new field

```diff
  pub struct AssistantMessage {
      pub chunks: Vec<AssistantMessageChunk>,
      pub indented: bool,
+     pub is_subagent_output: bool,    // NEW
  }
```

Since `AssistantMessage` is a variant of `AgentThreadEntry`, this also affects the enum's overall alignment and size.

**Impact:** Compounds the `ENTRY_SIZE` change.

### 3.4 AcpThreadEvent::Stopped — now takes a parameter

```diff
- Stopped,
+ Stopped(acp::StopReason),
```

This doesn't affect the hook directly (it operates on `AcpThread`, not `AcpThreadEvent`), but indicates broader refactoring.

### 3.5 ToolCallStatus — UNCHANGED

```rust
pub enum ToolCallStatus {
    Pending,
    WaitingForConfirmation { options, respond_tx },
    InProgress,
    Completed,
    Failed,
    Rejected,
    Canceled,
}
```

The enum itself is identical between versions. The `WAITING_VARIANT` discriminant and `respond_tx` offset within the status variant are likely still correct — but this is moot if `ENTRY_STATUS_OFFSET` within the entry is wrong due to changed entry size.

---

## 4. Failure Mechanism

1. Hook worked fine for ~95s (offsets close enough for simple entries)
2. A tool call arrived (possibly a subagent spawn with `SubagentSessionInfo`)
3. `on_leave` read entries with **wrong `ENTRY_SIZE`** (0x1b0 instead of actual new size)
4. Entry walker landed on misaligned memory → read garbage discriminants/status
5. Either: found a false-positive "WaitingForConfirmation" → transmuted garbage as `Sender` → memory corruption
6. Or: infinite loop / SIGSEGV from reading unmapped memory
7. `_sigtramp` entered → kernel suspended all threads → 44s hang

The 95s delay before the hang supports this: earlier tool calls may have had fewer entries or simpler layouts that happened to work despite wrong stride.

---

## 5. Other Observations

### tracing-appender thread in yolo hook

```
Thread 0x510e7a    "tracing-appender"    last ran 131.331s ago
  11  (libzed_yolo_hook.dylib + 445136) → dispatch_semaphore_wait_slow
```

The tracing appender thread is idle waiting on a semaphore — normal behavior, not related to the hang.

### crash_handler thread

```
Thread 0x510ef0    last ran 46.563s ago
  11  crash_handler::mac::state::attach → mach_msg_overwrite → mach_msg2_trap
```

Zed's crash handler was listening for Mach exceptions — it did not trigger, consistent with a hang (not a crash/panic).

---

## 6. Constants Requiring Recalibration

| Constant | v0.226.0 Value | Status for v0.227.0 |
|----------|---------------|---------------------|
| `ENTRIES_PTR_OFFSET` | `0x60` | **NEEDS CHECK** — new AcpThread fields may shift it |
| `ENTRIES_LEN_OFFSET` | `0x68` | **NEEDS CHECK** — same reason |
| `ENTRY_SIZE` | `0x1b0` (432) | **WRONG** — ToolCall and AssistantMessage grew |
| `ENTRY_DISCRIMINANT_OFFSET` | `0x00` | Likely OK (discriminant is always first) |
| `ENTRY_STATUS_OFFSET` | `0x20` | **NEEDS CHECK** — ToolCall fields before status may have shifted |
| `ENTRY_RESPOND_TX_OFFSET` | `0x40` | **NEEDS CHECK** — respond_tx within WaitingForConfirmation may have shifted |
| `TOOLCALL_VARIANT` | `0x07` | **NEEDS CHECK** — AgentThreadEntry has 3 variants, unlikely to change but verify |
| `WAITING_VARIANT` | `0x00` | Likely OK — WaitingForConfirmation is variant 1 (niche-optimized to 0) |

---

## 7. Recalibration Steps

Follow the upgrade guide (doc 06) against the v0.227.0 binary:

```bash
# 1. Find the hooked symbol
nm "/Applications/Zed Preview.app/Contents/MacOS/zed" | \
  grep 'request_tool_call_authorization' | grep -v drop | grep -v closure | grep -v spawn

# 2. Disassemble to extract new offsets
otool -tv -p <SYMBOL_NAME> "/Applications/Zed Preview.app/Contents/MacOS/zed" | head -80

# 3. Look for:
#    ldr xN, [x0, #IMM]     → entries.ptr / entries.len offsets
#    mov wN, #SIZE           → entry size
#    cmp xN, #DISC           → variant discriminants
#    ldr xN, [xM, #OFFSET]  → status and respond_tx offsets within entry
```

---

## 8. Related Documents

- [06_yolo_upgrade_guide.md](06_yolo_upgrade_guide.md) — Full recalibration procedure
- [08_crash_analysis_2026-03-03.md](08_crash_analysis_2026-03-03.md) — Previous hang on v0.226.2 (different root cause: Vec race)
- [09_eliminate_approval_delay.md](09_eliminate_approval_delay.md) — Inline send design (still valid)

---

## 9. Zed Source Changes (v0.226.4 → v0.227.0)

Key commits affecting hooked code:

| Commit | Description | Impact |
|--------|-------------|--------|
| `42ba961075` | Persist unsent draft prompt across Zed restarts | Adds `draft_prompt` field to AcpThread |
| `832782f6b3` | Persist token count and scroll position across agent restarts | Adds `ui_scroll_position` to AcpThread |
| `5e9ee9ea4a` | agent: More subagent fixes | Changes `subagent_session_id` → `subagent_session_info` (larger struct) |
| `ef60143e7a` | agent: Show full subagent output if no concurrent tool calls | Adds `is_subagent_output` to AssistantMessage |

Total diff: 71 files changed, 11898 insertions(+), 4374 deletions(-)
