# YOLO Hook Upgrade Guide: Zed Updates, Re-Patching, and Offset Drift

> Date: 2026-02-26, updated 2026-03-27
> Target: Zed Preview on macOS aarch64
> Deep binary notes: `docs/11_offset_recalibration_v0.228.0.md`, `docs/12_offset_recalibration_v0.230.0.md`

---

## 1. When Do You Need to Re-Patch?

| Event | Re-patch app bundle? | Re-check binary offsets? |
|-------|-----------------------|--------------------------|
| Zed auto-update | YES | YES |
| Zed Preview minor/major version bump | YES | YES |
| Rebuild hook only, same app binary | NO, if the app already points at the same dylib path | MAYBE |
| Local code changes to hook logic | NO binary rewrite required if already injected at the same path, but restart/relaunch is needed to load the new dylib | MAYBE |
| macOS update only | usually NO | usually NO |

The current patcher workflow is packaged behind `cargo patch`.

---

## 2. Fast Workflow

### Recommended full flow

```bash
cd /path/to/zed-yolo-hook

# Build + patch + sign + relaunch + startup verification
cargo patch --verify
```

### Common variants

```bash
# Preview, build + patch + relaunch
cargo patch

# Stable instead of Preview
cargo patch --stable

# Use an already-built dylib
cargo patch --no-build

# Restore the original binary from zed.original
cargo patch restore
```

The current `xtask` targets:

- Preview: `/Applications/Zed Preview.app`
- Stable: `/Applications/Zed.app`

It does not currently expose the old custom `--zed-app` override mentioned in some older notes.

---

## 3. Current Known-Good Preview 0.230.0 Facts

Installed app under test on 2026-03-27:

- Bundle id: `dev.zed.Zed-Preview`
- app version: `0.230.0`
- build string:
  `0.230.0+preview.205.9437a84390a396d666f04b38db87d89bb07284c1`
- bundle build:
  `20260325.153514`

Current binary-derived ACP constants for this build:

| Item | Value |
|------|-------|
| `AcpThread.entries.ptr` | `self + 0x90` |
| `AcpThread.entries.len` | `self + 0x98` |
| `sizeof(AgentThreadEntry)` | `0x1c0` |
| `AgentThreadEntry::ToolCall` discriminant | `0x02` |
| `ToolCall.status` offset | `0x118` |
| `respond_tx` offset | `0x160` |
| `ToolCall.id.ptr/len` | `0x168 / 0x170` |
| `ToolCallUpdate.id.ptr/len` | `0x128 / 0x130` |
| waiting-state test | `status_head < 0x8000_0000_0000_0002` |

Do not reuse the old `v0.226.x / v0.228.x` assumptions (`ToolCall = 0x07`, `Waiting = 0x00`)
for this build.

---

## 4. How to Tell What Broke

### Case A — patching/injection problem

Symptoms:

- `cargo patch --verify` fails
- `otool -L /Applications/Zed Preview.app/Contents/MacOS/zed` does not show
  `libzed_yolo_hook.dylib`
- no fresh `=== zed-yolo-hook v... ===` log appears after launch

Likely causes:

- app bundle replaced by update
- signing failed
- patch was restored or overwritten

### Case B — dylib loads, but ACP hook logic is stale

Symptoms:

- fresh log shows:
  - `permission_decision: hook installed`
  - `tool_authorization: hook installed`
  - `YOLO mode ACTIVE`
- but ACP requests log:
  - `tool_authorization #N: no WaitingForConfirmation entry found`

Likely cause:

- binary layout drift
- or waiting-state / `ToolCallId` matching logic is stale

This was the exact failure mode observed on 2026-03-27 before the final Preview `0.230.0` fix.

### Case C — hook loads, no ACP warnings, but no success line either

Symptoms:

- startup markers are present
- no fresh `no WaitingForConfirmation` warning
- but also no:
  - `matched v0.230.x entry`
  - `send succeeded`
  - `approved in ...`

Likely cause:

- no external-agent tool permission request has fired since launch

That means patching is probably fine, but the ACP path has not been exercised yet.

---

## 5. Current Recalibration Procedure

When Preview updates and ACP approvals break, use this order:

### Step 1 — confirm the source call path

Read these files in the local Zed checkout:

- `crates/agent_servers/src/acp.rs`
- `crates/acp_thread/src/acp_thread.rs`
- `crates/agent_ui/src/conversation_view.rs`

The important control flow is:

```text
request_permission
  -> request_tool_call_authorization
     -> WaitingForConfirmation { options, respond_tx }

authorize_tool_call
  -> tool_call_mut(id)
  -> respond_tx.send(outcome)
```

This tells you what the hook must mimic.

### Step 2 — inspect the installed app binary

Useful commands:

```bash
nm -nm "/Applications/Zed Preview.app/Contents/MacOS/zed" | \
  grep 'AcpThread31request_tool_call_authorization\|AcpThread19authorize_tool_call\|ToolPermissionDecision10from_input'

xcrun llvm-objdump --disassemble-symbols='__RNvMsn_Cs3xb0dWJrqhb_10acp_threadNtB5_9AcpThread31request_tool_call_authorization' \
  "/Applications/Zed Preview.app/Contents/MacOS/zed"

xcrun llvm-objdump --disassemble-symbols='__RNvMsn_Cs3xb0dWJrqhb_10acp_threadNtB5_9AcpThread19authorize_tool_call' \
  "/Applications/Zed Preview.app/Contents/MacOS/zed"

xcrun llvm-objdump --disassemble-symbols='__RNvMsn_Cs3xb0dWJrqhb_10acp_threadNtB5_9AcpThread22upsert_tool_call_inner' \
  "/Applications/Zed Preview.app/Contents/MacOS/zed"
```

Use those to recover:

- entry size
- `entries.ptr` / `entries.len`
- `ToolCall.status` offset
- `respond_tx` offset
- entry `ToolCallId` offsets
- `ToolCallUpdate.tool_call_id` offsets
- waiting-state encoding

### Step 3 — update the hook code

Current `v0.230.x` strategy in `src/hooks/tool_authorization.rs`:

1. capture `ToolCallId` in `on_enter`
2. walk `self.entries` in `on_leave`
3. match exact `ToolCallId`
4. validate waiting-state via niche-aware status test
5. validate `respond_tx` looks like an `Arc`
6. reconstruct the real `oneshot::Sender<SelectedPermissionOutcome>`
7. send `AllowOnce`

If the ACP payload type changes again, update:

- `Cargo.toml`
- the local shim type / layout assertions in `src/hooks/tool_authorization.rs`

### Step 4 — rebuild and patch

```bash
cargo check
cargo build --release
cargo patch --verify
```

### Step 5 — read the logs in order

```bash
tail -n 80 ~/Library/Logs/Zed/zed-yolo-hook.$(date +%Y-%m-%d).log
```

Interpretation:

- `hook installed` + `YOLO mode ACTIVE`:
  patch and load path good
- `no WaitingForConfirmation entry found`:
  ACP layout still wrong
- `matched v0.230.x entry` + `send succeeded` + `approved in ... via v0.230.x`:
  ACP path confirmed working on the new build

This exact success sequence was observed on 2026-03-27 after the final Preview `0.230.0`
recalibration.

---

## 6. Validation Commands

### Check injection

```bash
otool -L "/Applications/Zed Preview.app/Contents/MacOS/zed" | \
  grep 'libzed_yolo_hook\.dylib'
```

### Check patch registry

```bash
cargo patch status
```

### Check startup markers

```bash
grep -E 'hook installed|YOLO mode ACTIVE' \
  ~/Library/Logs/Zed/zed-yolo-hook.$(date +%Y-%m-%d).log | tail -20
```

### Check ACP approval path

```bash
grep -E 'tool_authorization #|matched v0\.230\.x entry|send succeeded|approved in|no WaitingForConfirmation' \
  ~/Library/Logs/Zed/zed-yolo-hook.$(date +%Y-%m-%d).log | tail -40
```

---

## 7. Notes

1. The local source checkout can lag the installed Preview build. When they diverge, trust the
   binary for offsets and enum layout.
2. Startup verification is necessary but not sufficient. It proves the dylib loads, not that ACP
   approval succeeded.
3. Keep `docs/12_offset_recalibration_v0.230.0.md` as the authoritative note for the current
   Preview generation.
