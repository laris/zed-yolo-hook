# Compatibility verification: Zed Preview v0.233.3

> Date: 2026-04-21
> Installed app: `/Applications/Zed Preview.app`
> App version: `Zed Preview 0.233.3`, build `0.233.3+preview.233.3ae52299d4af96a6a2caf663d02516fc469a15cb`
> Zed source commit: `3ae52299d4af96a6a2caf663d02516fc469a15cb`
> Previous verified version: `v0.233.0` (docs/19)

---

## 1. Summary

**Status: ✅ FULLY COMPATIBLE — no recalibration needed.**

`zed-yolo-hook` runs unmodified on Zed Preview v0.233.3. All 5 hooked
symbols still resolve via the existing pattern matchers; the
`AcpThread.entries` Vec offset (recalibrated to `0xb0/0xb8` in v0.233.0,
docs/19) is unchanged in v0.233.3.

| Constant              | v0.233.0 (docs/19) | v0.233.3 (this verification) |
|-----------------------|--------------------|------------------------------|
| `ENTRIES_PTR_OFFSET`  | `0xb0`             | `0xb0` (unchanged)           |
| `ENTRIES_LEN_OFFSET`  | `0xb8`             | `0xb8` (unchanged)           |
| `ENTRY_SIZE`          | `0x1c0`            | `0x1c0` (unchanged)          |
| `STATUS_OFFSET`       | `0x118`            | `0x118` (unchanged)          |
| `RESPOND_TX_OFFSET`   | `0x160`            | `0x160` (unchanged)          |
| `ID_PTR_OFFSET`       | `0x168`            | `0x168` (unchanged)          |
| `ID_LEN_OFFSET`       | `0x170`            | `0x170` (unchanged)          |

---

## 2. Symbol resolution

All 5 exported symbol patterns resolved on the v0.233.3 binary:

| Pattern                                           | Resolved symbol prefix                                                            |
|---------------------------------------------------|-----------------------------------------------------------------------------------|
| `ToolPermissionDecision::from_input`              | `_RNvMNtCsd6cD1FhxaON_5agent16tool_permissionsNtB2_22ToolPermissionDecision10from_input` |
| `AcpThread::request_tool_call_authorization`      | `_RNvMsp_Cscb5D3lviN1O_10acp_threadNtB5_9AcpThread31request_tool_call_authorization`     |
| `AcpThread::upsert_tool_call_inner`               | `_RNvMsp_Cscb5D3lviN1O_10acp_threadNtB5_9AcpThread22upsert_tool_call_inner`              |
| `AcpThread::handle_session_update`                | `_RNvMsp_Cscb5D3lviN1O_10acp_threadNtB5_9AcpThread21handle_session_update`               |
| `AcpThread::push_entry`                           | `_RNvMsp_Cscb5D3lviN1O_10acp_threadNtB5_9AcpThread10push_entry`                          |

(The crate-disambiguator hash component `Cscb5D3lviN1O` differs from the
v0.233.0 hash `CseS5viS5XUxq` — but the pattern matcher uses
substring matching so this is transparent.)

---

## 3. Live runtime verification

Patched `/Applications/Zed Preview.app` with the existing
`libzed_yolo_hook.dylib` (v0.1.0, build commit `1eb50f0`). Init log shows
all 5 hooks installed cleanly:

```
2026-04-21T13:24:03.853914+08:00 INFO zed_yolo_hook: permission_decision: hook installed
2026-04-21T13:24:03.86248+08:00  INFO zed_yolo_hook: tool_authorization: hook installed
2026-04-21T13:24:03.871254+08:00 INFO zed_yolo_hook: upsert_hook: hook installed (approach 1)
2026-04-21T13:24:03.880642+08:00 INFO zed_yolo_hook: session_update_hook: hook installed (approach 2)
2026-04-21T13:24:03.889726+08:00 INFO zed_yolo_hook: push_entry_hook: hook installed (catch-all registration)
2026-04-21T13:24:03.889757+08:00 INFO zed_yolo_hook::hooks::stale_scanner: stale_scanner: started (interval=2000ms)
```

30+ tool authorizations approved at microsecond latency in the first 4 minutes
of runtime — no misses, no warnings, no offset-related diagnostics:

```
tool_authorization #22 [s:ca80]: approved in 68us via v0.230.x call_id="toolu_017zxp"
tool_authorization #23 [s:ca80]: approved in 71us via v0.230.x call_id="toolu_01S3dr"
... (similar lines through #30+)
```

`cargo patch verify` reports `[verify] PASS zed-yolo-hook — all markers found`.

---

## 4. Conclusion

No code changes required. The v0.233.0 recalibration (docs/19) carries
forward intact. Re-verify on the next minor bump (v0.234.x) — Zed's
`AcpThread` struct layout has shifted twice in 2026 (v0.228.x → v0.230.x,
v0.232.x → v0.233.0), so future drift is expected.
