# Offset Recalibration: Zed Preview v0.231.1

> Date: 2026-04-03
> Installed app: `/Applications/Zed Preview.app`
> App version: `0.231.1`
> App build: `0.231.1+preview.211.c8a91564e8bc30251fc9a15901c33af3c3ddf7e0`

---

## 1. Scope

This note records the 2026-04-03 verification pass for Zed Preview 0.231.1 against the
existing `zed-yolo-hook` code (written for v0.230.0).

**Result: no code changes required.** All offsets, discriminants, ACP types, and symbol
names are identical to v0.230.0.

---

## 2. Symbol Availability

Both hook targets are still exported in the v0.231.1 binary:

```text
__RNvMsn_CseS5viS5XUxq_10acp_threadNtB5_9AcpThread31request_tool_call_authorization
__RNvMNtCslQbgH0e8eCG_5agent16tool_permissionsNtB2_22ToolPermissionDecision10from_input
```

Note: the crate hashes in the mangled names changed from v0.230.0 (`Cs3xb0dWJrqhb` ->
`CseS5viS5XUxq` for `acp_thread`, `Cs8PSDMm7WXSm` -> `CslQbgH0e8eCG` for `agent`).
This does not affect the hook — symbols are resolved by pattern matching on the
demangled name fragments, not by exact mangled string.

---

## 3. Binary Verification — ACP Entry Layout

Disassembly of `upsert_tool_call_inner` in the v0.231.1 binary confirms the entry
iteration loop uses the same constants:

```asm
; entry size = 0x1c0
sub  x21, x21, #0x1c0
add  x22, x22, #0x1c0
cmp  x20, x22

; ToolCall discriminant = 0x2
cmp  x9, #0x2

; tool_call_id.len at entry + 0x170 (= entry_end - 0x50)
ldur x8, [x8, #-0x50]

; tool_call_id.ptr at entry + 0x168 (= entry_end - 0x58)
ldur x8, [x8, #-0x58]
```

### Offset comparison table

| Item | v0.230.0 | v0.231.1 | Status |
|------|----------|----------|--------|
| `AcpThread.entries.ptr` | `self + 0x90` | `self + 0x90` | **unchanged** |
| `AcpThread.entries.len` | `self + 0x98` | `self + 0x98` | **unchanged** |
| `sizeof(AgentThreadEntry)` | `0x1c0` | `0x1c0` | **unchanged** |
| `AgentThreadEntry::ToolCall` discriminant | `0x02` | `0x02` | **unchanged** |
| `ToolCall.status` offset | `0x118` | `0x118` | **unchanged** |
| `respond_tx` offset | `0x160` | `0x160` | **unchanged** |
| `ToolCall.id.ptr` | `0x168` | `0x168` | **unchanged** |
| `ToolCall.id.len` | `0x170` | `0x170` | **unchanged** |
| `ToolCallUpdate.id.ptr` | `0x128` | `0x128` | **unchanged** |
| `ToolCallUpdate.id.len` | `0x130` | `0x130` | **unchanged** |
| waiting-state test | `< 0x8000_0000_0000_0002` | `< 0x8000_0000_0000_0002` | **unchanged** |

---

## 4. ACP Protocol Version

The binary embeds the same crate versions:

- `agent-client-protocol 0.10.2` — matches the hook's `Cargo.toml` pin
- `agent-client-protocol-schema 0.11.2`

The `SelectedPermissionOutcome` layout (`sizeof = 0x30`, field offsets `0x00 / 0x18 / 0x28`)
remains valid.

---

## 5. Action Required

**None.** The existing hook code built for v0.230.0 is binary-compatible with v0.231.1.

To re-patch after the Zed auto-update:

```bash
cd /path/to/zed-yolo-hook
cargo patch --verify
```

---

## 6. Methodology

Verification was performed via:

1. `nm -gU` to confirm exported symbol names
2. `otool -tv` disassembly of `request_tool_call_authorization` and `upsert_tool_call_inner`
   to read entry size, discriminant, and field offsets from the instruction operands
3. `strings` to confirm embedded ACP crate version strings
