# Plan: Eliminate tool_authorization Approval Delay

> Date: 2026-03-03
> Scope: Remove dispatch_async_f bottleneck in Hook 2 (ACP tool authorization)
> Target: Zed Preview v0.226.2, macOS aarch64

---

## Context

The `tool_authorization` hook (Hook 2) dispatches the "allow" send via `dispatch_async_f` to the **macOS main dispatch queue**. Log analysis shows this adds **16-41ms delay** in typical cases, and can be **much worse** (seconds to infinite) when the main thread is busy or hung. The `permission_decision` hook (Hook 1) has no delay — it acts synchronously in `on_leave`.

Additionally, each `tool_authorization` invocation emits **~10 info-level log lines** including full memory word dumps, adding file I/O latency in the hot path.

### Measured Dispatch Delay (from logs)

| Invocation | scheduled → deferred_send | Notes |
|-----------|--------------------------|-------|
| #1 (Mar 3) | 17.7ms | Typical |
| #2 (Mar 3) | 40.9ms | Main thread busy |
| #3 (Mar 3) | 0.05ms | Lucky timing |
| #45 (Mar 2) | 16.2ms | ~1 frame at 60fps |
| #2 (Mar 2) | 16.8ms | Typical |
| #4 (Mar 2) | 16.5ms | Typical |

Worst case (main thread hung): the dispatch callback never executes, approval never happens.

---

## Changes

### 1. Send directly in `on_leave` — remove `dispatch_async_f` pattern

**File:** `src/hooks/tool_authorization.rs`

- Remove `DeferredSend` struct and `deferred_send` extern "C" callback (lines 47-108)
- Move the Arc reconstruct + `sender.send(option_id)` logic directly into the `on_leave` method, right after finding `respond_tx`
- Remove `use crate::ffi::dispatch;`
- The re-entrancy concern in the original comment is unfounded: `oneshot::Sender::send()` does an atomic write + receiver wake; the receiver Future is polled later by Zed's async executor, never synchronously re-entering `request_tool_call_authorization`

**Before (simplified):**
```
on_leave → find respond_tx → schedule dispatch_async_f(main_queue, deferred_send)
                                  ↓ (16-41ms+ wait for main runloop)
                              deferred_send → reconstruct Sender → .send("allow")
```

**After:**
```
on_leave → find respond_tx → reconstruct Sender → .send("allow")  [~0ms]
```

### 2. Reduce diagnostic logging

**File:** `src/hooks/tool_authorization.rs`

- Remove the 3-entry memory word dump loop (lines 149-162) — move to debug level or remove entirely
- Downgrade per-entry discriminant/status logging (lines 171-177) to `tracing::debug!`
- Keep only essential events at info: the send result and any failures
- Add a single info log with elapsed time for monitoring: `"tool_authorization #{count}: approved in {elapsed}us"`

### 3. Keep `ffi/dispatch.rs` module (no change)

The dispatch module may be useful for other future hooks. Leave it as-is, just remove the import from `tool_authorization.rs`.

---

## Files Modified

| File | Change |
|------|--------|
| `src/hooks/tool_authorization.rs` | Remove deferred dispatch pattern; send inline; reduce logging |

---

## Verification

1. `cargo build --release` in zed-yolo-hook
2. Restart Zed Preview (with DYLD_INSERT_LIBRARIES)
3. Trigger an AI tool call in Agent Panel
4. Check `~/Library/Logs/Zed/zed-yolo-hook.*.log` — confirm:
   - No more `deferred_send executing` / `deferred_send scheduled` lines
   - Single concise info line per approval with timing
   - `send succeeded!` appears immediately after `on_leave`
5. Compare timestamps: on_leave → send succeeded should be <1ms (vs 16-41ms before)
