# Offset Recalibration: Zed Preview v0.230.0

> Date: 2026-03-27
> Installed app: `/Applications/Zed Preview.app`
> App version: `0.230.0`
> App build: `0.230.0+preview.205.9437a84390a396d666f04b38db87d89bb07284c1`
> Bundle build: `20260325.153514`
> Local Zed source checkout: `6bc34ff44f9931a77e5e82cff87dc2aa266a41a4`

---

## 1. Scope

This note records the 2026-03-27 reverse-engineering pass for the current Preview app and
the corresponding `zed-yolo-hook` changes.

The important context is that the installed Preview app advertises commit
`9437a84390a396d666f04b38db87d89bb07284c1`, but the local source checkout under
`/Users/lqiao/codes-repos/gh-zed-industries__zed` is at
`6bc34ff44f9931a77e5e82cff87dc2aa266a41a4`.

That mismatch meant the source tree was useful for understanding call flow, but the final offsets
and enum layout had to come from binary analysis of the installed app.

---

## 2. Source-Level Call Paths

### Native tool permission path

The built-in tool path still goes through `ToolPermissionDecision::from_input`:

```text
agent::tool_permissions::decide_permission_from_settings
  -> ToolPermissionDecision::from_input
```

Relevant source:

- `crates/agent/src/tool_permissions.rs`

This remains the code path used by the `permission_decision` hook in
`src/hooks/permission_decision.rs`.

### ACP / external-agent permission path

The ACP path for Claude/Codex/Gemini still looks like:

```text
agent_servers::acp::ClientDelegate::request_permission
  -> AcpThread::request_tool_call_authorization
     -> upsert_tool_call_inner
     -> ToolCallStatus::WaitingForConfirmation { options, respond_tx }
     -> return Task<RequestPermissionOutcome> that awaits rx
```

Relevant source:

- `crates/agent_servers/src/acp.rs`
- `crates/acp_thread/src/acp_thread.rs`

### UI approval path

When the user clicks Allow/Reject in the UI, the path is:

```text
agent_ui::Conversation::authorize_tool_call
  -> AcpThread::authorize_tool_call
     -> tool_call_mut(id)
     -> mem::replace(call.status, InProgress/Rejected)
     -> respond_tx.send(outcome)
```

Relevant source:

- `crates/agent_ui/src/conversation_view.rs`
- `crates/acp_thread/src/acp_thread.rs`

This is the crucial clue: the UI path finds the entry by `ToolCallId`, then consumes the
`respond_tx` stored in `WaitingForConfirmation`.

---

## 3. Binary / IDA Findings

The shipped app exports the two ACP symbols we care about:

```text
__RNvMsn_Cs3xb0dWJrqhb_10acp_threadNtB5_9AcpThread31request_tool_call_authorization
__RNvMsn_Cs3xb0dWJrqhb_10acp_threadNtB5_9AcpThread19authorize_tool_call
```

It also still exports the native-tool hook target:

```text
__RNvMNtCs8PSDMm7WXSm_5agent16tool_permissionsNtB2_22ToolPermissionDecision10from_input
```

### Ground-truth constants from the Preview 0.230.0 binary

| Item | Value | Notes |
|------|-------|-------|
| `AcpThread.entries.ptr` | `self + 0x90` | unchanged from v0.228.x |
| `AcpThread.entries.len` | `self + 0x98` | unchanged from v0.228.x |
| `sizeof(AgentThreadEntry)` | `0x1c0` | grew from `0x1b0` |
| `AgentThreadEntry::ToolCall` discriminant | `0x02` | old `0x07` assumption was wrong for this build |
| `ToolCall.status` offset | `0x118` | moved from the old front-of-entry location |
| `ToolCall.respond_tx` offset | `0x160` | `status + 0x48` |
| `ToolCall.id.ptr` offset | `0x168` | used by `tool_call_mut` / `authorize_tool_call` |
| `ToolCall.id.len` offset | `0x170` | used by `tool_call_mut` / `authorize_tool_call` |
| `ToolCallUpdate.id.ptr` offset | `0x128` | used when `request_tool_call_authorization` clones the update |
| `ToolCallUpdate.id.len` offset | `0x130` | same |
| Waiting-state encoding | `status_head < 0x8000_0000_0000_0002` | niche-encoded, not `status == 0` |
| `SelectedPermissionOutcome` size | `0x30` | new send payload |
| `SelectedPermissionOutcome.params` | `+0x00` | optional params |
| `SelectedPermissionOutcome.option_id` | `+0x18` | ACP option id |
| `SelectedPermissionOutcome.option_kind` | `+0x28` | `AllowOnce`, `RejectOnce`, etc. |

### What the first 0.230 port got wrong

The first port correctly identified:

- `ENTRY_SIZE = 0x1c0`
- `ENTRY_STATUS_OFFSET = 0x118`
- `ENTRY_RESPOND_TX_OFFSET = 0x160`
- the need to send `SelectedPermissionOutcome` instead of a bare `"allow"` string

But it still used two stale assumptions from the v0.228 notes:

1. `ToolCall` entry discriminant was still treated as `0x07`
2. `WaitingForConfirmation` was still treated as an exact discriminant value

Those assumptions were the reason the hook still logged repeated
`no WaitingForConfirmation entry found` warnings even after the new offsets and payload type
were added.

### Why matching by `ToolCallId` is the correct fix

Disassembly of `AcpThread::authorize_tool_call` and `upsert_tool_call_inner` showed that the
real runtime logic is:

1. iterate entries in reverse
2. find the `ToolCall` entry by `ToolCallId`
3. inspect `call.status`
4. if the old status is `WaitingForConfirmation`, send through `respond_tx`

That is why the final hook now:

- captures `ToolCallUpdate.tool_call_id` on `on_enter`
- walks `self.entries` on `on_leave`
- matches the exact entry by `ToolCallId`
- treats waiting as a niche-encoded payload state
- then reconstructs the real `oneshot::Sender<SelectedPermissionOutcome>` and calls `.send()`

---

## 4. Repo Changes for v0.230.0

### `Cargo.toml`

Added an exact ACP dependency:

```toml
agent-client-protocol = "=0.10.2"
```

This lets the hook construct the current ACP `PermissionOptionId` and
`PermissionOptionKind` values directly.

### `src/hooks/tool_authorization.rs`

The final 0.230 update changed the ACP hook in four important ways:

1. kept the correct `0x1c0 / 0x118 / 0x160` entry layout
2. captured `ToolCallId` from `ToolCallUpdate + 0x128 / 0x130`
3. matched the exact `ToolCall` entry by `ToolCallId` using entry offsets `0x168 / 0x170`
4. treated `WaitingForConfirmation` as a niche-encoded payload state instead of `status == 0`

The send payload also changed from the old legacy string-style approval to:

```rust
SelectedPermissionOutcome {
    option_id: acp::PermissionOptionId::new("allow"),
    option_kind: acp::PermissionOptionKind::AllowOnce,
    params: None,
}
```

Legacy `v0.228.x` support was retained as a fallback layout.

### `src/lib.rs`

The top-level crate comment was updated to reflect the current ACP behavior:

- native hook: `ToolPermissionDecision::from_input`
- ACP hook: `AcpThread::request_tool_call_authorization`
- ACP send path: current allow outcome through the oneshot channel

---

## 5. Logs and What They Mean

### Non-working log window

Before the final fix, the hook was loading but not finding the waiting entry. The log showed
repeated failures such as:

```text
2026-03-27T11:36:10Z  tool_authorization #1: no WaitingForConfirmation entry found
...
2026-03-27T11:57:53Z  tool_authorization #25: no WaitingForConfirmation entry found
```

Interpretation:

- the symbol hook was attached
- `request_tool_call_authorization` was definitely being intercepted
- the entry walk logic was stale for Preview `0.230.0`

### Patch / startup verification after the final fix

After the final code update, the app was patched with:

```bash
cargo patch --verify
```

The patcher output ended with:

```text
[verify] PASS zed-yolo-hook — all markers found
[verify] ALL HOOKS VERIFIED
```

Fresh startup logs after that patch showed:

```text
permission_decision: hook installed
tool_authorization: hook installed
YOLO mode ACTIVE
```

Binary-level verification also confirmed the dylib was injected:

```text
otool -L /Applications/Zed Preview.app/Contents/MacOS/zed
  -> /Users/lqiao/codes/zed-yolo-hook/target/release/libzed_yolo_hook.dylib
```

### Later working log window

A later ACP authorization request on the same day produced the full success path:

```text
2026-03-27T12:03:12Z  tool_authorization #1: matched v0.230.x entry[5] by ToolCallId, respond_tx=0x9582aa700
2026-03-27T12:03:12Z  tool_authorization #1: send succeeded
2026-03-27T12:03:12Z  tool_authorization #1: approved in 54us via v0.230.x
```

So the documentation can now claim:

- patching works
- the dylib loads
- both hooks install
- the old repeated `no WaitingForConfirmation entry found` failures were specific to the stale
  first 0.230 port
- the final 0.230 hook matched the exact `ToolCallId`, sent the ACP outcome, and completed the
  approval path successfully

---

## 6. Takeaways

1. The Preview `0.230.0` update did not just move offsets; it changed how the `ToolCall` entry
   must be identified.
2. The most reliable mental model is now the source-level one:
   `request_tool_call_authorization` inserts/updates a `ToolCall` entry, and
   `authorize_tool_call` later finds it by `ToolCallId`.
3. Treat `ToolCallStatus::WaitingForConfirmation` as a payload-bearing niche case, not as a
   simple exact discriminant.
4. Keep the source tree open for call flow, but trust the shipped app binary for offsets and
   enum layout.
