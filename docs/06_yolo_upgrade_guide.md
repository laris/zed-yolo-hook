# YOLO Hook Upgrade Guide: Handling Zed Updates & API Changes

> Date: 2026-02-26
> For: zed-yolo-hook v0.17.1+
> Target: Zed Preview macOS aarch64

---

## 1. When Do You Need to Re-hook?

| Event | Re-inject needed? | Re-calibrate offsets? |
|-------|-------------------|----------------------|
| Zed auto-update | YES — binary replaced | MAYBE — if AcpThread struct changed |
| Zed major version (0.227+) | YES | LIKELY |
| macOS update | NO (unless SIP re-enabled) | NO |
| Rebuild dylib only | NO — same path | NO |
| Rust toolchain update for Zed | YES (if binary changes) | LIKELY — field reordering may change |

---

## 2. Quick Re-inject After Zed Update

Zed auto-updates replace the binary, removing `LC_LOAD_WEAK_DYLIB`. Re-inject:

```bash
# 1. Quit Zed
osascript -e 'quit app "Zed Preview"' 2>/dev/null; sleep 1
osascript -e 'quit app "Zed"' 2>/dev/null; sleep 1

# 2. Re-inject
cd /path/to/zed-yolo-hook

# Zed Preview:
cargo patch

# Zed Stable:
cargo patch --stable

# If you want to skip the build step, pass a pre-built dylib:
#   cargo patch --dylib target/release/libzed_yolo_hook.dylib

# 3. Relaunch
open "/Applications/Zed Preview.app"  # or Zed.app

# 4. Verify
tail -5 ~/Library/Logs/Zed/zed-yolo-hook.$(date +%Y-%m-%d).log
# Should show "=== zed-yolo-hook vX.Y.Z ===" and "YOLO mode ACTIVE"
```

### Supported Apps

The same `libzed_yolo_hook.dylib` works for both apps — no rebuild needed:

| App | Path | Bundle ID | Verified versions |
|-----|------|-----------|-------------------|
| Zed Preview | `/Applications/Zed Preview.app` | `dev.zed.Zed-Preview` | v0.226.0 |
| Zed Stable | `/Applications/Zed.app` | `dev.zed.Zed` | v0.225.9 |

Both use identical struct layout offsets (verified via disassembly).

---

## 3. How to Detect if Offsets Changed

If after re-inject the hook logs `no WaitingForConfirmation entry found` or crashes, the struct offsets have changed. Diagnosis:

### Step 1: Check symbol exists

```bash
nm "/Applications/Zed Preview.app/Contents/MacOS/zed" | \
  grep 'request_tool_call_authorization' | grep -v drop | grep -v closure | grep -v spawn
```

If the symbol name changed, update the `SYMBOL_INCLUDE` patterns in `src/hooks/tool_authorization.rs`.

### Step 2: Disassemble authorize_tool_call

```bash
# Find the entries offset
otool -tv -p __RNvMsk_Cs..._10acp_threadNtB5_9AcpThread19authorize_tool_call \
  "/Applications/Zed Preview.app/Contents/MacOS/zed" | head -30
```

Look for:
- `ldr x9, [x0, #0xNN]` — entries.ptr offset (currently 0x60)
- `ldr x8, [x0, #0xMM]` — entries.len offset (currently 0x68)
- `mov wN, #0xSIZE` — entry size (currently 0x1b0)
- `cmp x8, #0xVAR` — ToolCall variant discriminant (currently 0x7)

### Step 3: Disassemble the matching loop

Look for the `memcmp` call pattern:
- `ldr x8, [x25, #OFFSET_A]` then `cmp x8, x23` — status/id length comparison
- `ldr x8, [x25, #OFFSET_B]` then `add x0, x8, #0x10` — ArcInner ptr + 16 = string data

The offsets used here are:
- `OFFSET_A` = where ToolCallId.len is in the entry (currently 0x128)
- `OFFSET_B` = where ToolCallId.ptr is in the entry (currently 0x120)

### Step 4: Find status and respond_tx offsets

After the memcmp match, look for:
- `ldp q0, q1, [x25, #STATUS_OFF]` — old status load (currently 0x20)
- `str x8, [x25, #STATUS_OFF]` — new status write
- `ldr x23, [sp, #0x40]` after `ldr x9, [x25, #TX_OFF]` — respond_tx (currently 0x40)

### Step 5: Find WaitingForConfirmation discriminant

Look for:
- `cmp x22, #VALUE` with `b.hi` — values <= this are WaitingForConfirmation
- Currently `cmp x22, #0x1` meaning values 0 and 1 are valid

### Step 6: Update constants

Edit `src/hooks/tool_authorization.rs` and update:

```rust
const ENTRIES_PTR_OFFSET: usize = 0x60;  // from Step 2
const ENTRIES_LEN_OFFSET: usize = 0x68;  // from Step 2
const ENTRY_SIZE: usize = 0x1b0;         // from Step 2
const ENTRY_STATUS_OFFSET: usize = 0x20; // from Step 4
const ENTRY_RESPOND_TX_OFFSET: usize = 0x40; // from Step 4
const TOOLCALL_VARIANT: u64 = 0x07;      // from Step 2
const WAITING_VARIANT: u64 = 0x00;       // from Step 5
```

---

## 4. What If the Code Architecture Changes?

### Scenario: `request_tool_call_authorization` renamed or removed

Search for similar symbols:

```bash
nm "/Applications/Zed Preview.app/Contents/MacOS/zed" | grep -i 'authorization\|permission\|tool_call' | grep -v drop | head -20
```

The function must:
1. Create a `oneshot::channel()`
2. Store `respond_tx` in a `WaitingForConfirmation`-like status
3. Return a future wrapping `rx`

### Scenario: oneshot channel replaced with different async primitive

If `futures::channel::oneshot` is replaced (e.g., with tokio's oneshot), the `transmute` into `Sender` won't work. You'd need to:
1. Identify the new channel type
2. Add the correct crate dependency
3. Adjust the transmute target type

### Scenario: PermissionOptions format changes

The `option_id: "allow"` string might change. Check:

```bash
grep -r 'PermissionOptionId.*new.*"allow"' /path/to/zed/source/crates/agent/
```

### Scenario: `ToolCallStatus` enum variants change

Re-disassemble `authorize_tool_call` and look for the discriminant comparison after entry matching. The `cmp` instruction reveals the new WaitingForConfirmation value.

---

## 5. Automated Offset Calibration (Future Improvement)

Instead of hardcoded offsets, the hook could self-calibrate:

### Strategy 1: Disassemble at runtime

Use Frida's `Instruction` API to disassemble `authorize_tool_call` at load time, find the `ldr` patterns, and extract offsets automatically.

```rust
// Pseudocode
let instructions = frida_gum::disassemble(authorize_fn_ptr, 100);
for insn in instructions {
    if insn.is_ldr() && insn.base_reg() == "x0" {
        // Found entries offset
    }
}
```

### Strategy 2: Pattern scanning

Scan for specific byte patterns in the function prologue:

```rust
// Pattern: ldr x9, [x0, #IMM]  followed by  ldr x8, [x0, #IMM+8]
// This gives us entries.ptr and entries.len offsets
```

### Strategy 3: Signature-based entry detection

Instead of hardcoded `TOOLCALL_VARIANT` and `WAITING_VARIANT`, scan entries for heuristics:
- Entry has a heap pointer at known-ish offset (respond_tx is always a heap ptr)
- Entry has a `WaitingForConfirmation` look (contains valid Arc pointer at respond_tx offset)
- The last entry is likely the one just created by `request_tool_call_authorization`

### Strategy 4: Hook oneshot::channel instead

Instead of walking entries, hook `futures::channel::oneshot::channel::<PermissionOptionId>()` to intercept `tx` at creation time:

```
channel() → returns (Sender, Receiver)
```

Save the Sender, then in `on_leave` of `request_tool_call_authorization`, use the saved Sender to send directly. This avoids ALL struct offset dependencies.

Challenge: `channel()` is generic and may be inlined.

---

## 6. Build & Deploy Checklist

```bash
# 1. Quit Zed
osascript -e 'quit app "Zed Preview"' 2>/dev/null; sleep 1
osascript -e 'quit app "Zed"' 2>/dev/null; sleep 1

# 2. Build + inject
cd /path/to/zed-yolo-hook
cargo patch   # or: cargo patch --stable

# 3. Launch
open "/Applications/Zed Preview.app"  # or Zed.app

# 4. Verify (in another terminal)
sleep 3 && tail -20 ~/Library/Logs/Zed/zed-yolo-hook.$(date +%Y-%m-%d).log
```

---

## 7. Debugging Tips

### Check if hook loaded
```bash
grep "YOLO mode ACTIVE" ~/Library/Logs/Zed/zed-yolo-hook.*
```

### Check if tool_authorization hook fired
```bash
grep "tool_authorization #" ~/Library/Logs/Zed/zed-yolo-hook.* | tail -10
```

### Check send status
```bash
grep "send" ~/Library/Logs/Zed/zed-yolo-hook.* | tail -10
# "send succeeded!" = working
# "send failed" = receiver already dropped (race condition)
# No "send" line = entry not found (offset mismatch)
```

### Memory dump analysis

If offsets seem wrong, the entry dump lines show raw words:
```
entry[N] words: [disc, field1, field2, field3, status_at_0x20, field5, field6, field7, respond_tx_at_0x40]
```

Map word indices to byte offsets: `word[i]` = offset `i * 8` from entry start.

### Restore original binary
```bash
cp "/Applications/Zed Preview.app/Contents/MacOS/zed.original" \
   "/Applications/Zed Preview.app/Contents/MacOS/zed"
codesign -fs - --deep "/Applications/Zed Preview.app"
```
