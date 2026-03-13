# Offset Recalibration: Zed Preview v0.228.0

> Date: 2026-03-13
> Build: 0.228.0+preview.201.7d41a5350a09e4e0ef25194b99e5a3f279bcc230
> Scope: zed-yolo-hook offset update after Zed version bump
> Cross-hook: zed-prj-workspace-hook confirmed compatible (no changes needed)

---

## 1. Summary

Zed Preview v0.228.0 shifted the `entries` Vec within `AcpThread` by +0x18 (24 bytes).
Only `ENTRIES_PTR_OFFSET` and `ENTRIES_LEN_OFFSET` changed; all entry-internal offsets remain the same.

| Constant | v0.227.0 | v0.228.0 | Changed? |
|----------|----------|----------|----------|
| `ENTRIES_PTR_OFFSET` | 0x78 | **0x90** | YES (+0x18) |
| `ENTRIES_LEN_OFFSET` | 0x80 | **0x98** | YES (+0x18) |
| `ENTRY_SIZE` | 0x1b0 | 0x1b0 | no |
| `ENTRY_DISCRIMINANT_OFFSET` | 0x00 | 0x00 | no |
| `ENTRY_STATUS_OFFSET` | 0x48 | 0x48 | no |
| `ENTRY_RESPOND_TX_OFFSET` | 0x68 | 0x68 | no |
| `TOOLCALL_VARIANT` | 0x07 | 0x07 | no |
| `WAITING_VARIANT` | 0x00 | 0x00 | no |

---

## 2. Root Cause: AcpThread Struct Changes

Three fields were added/moved before `entries`:

```diff
 pub struct AcpThread {
+    session_id: acp::SessionId,          // MOVED from after `connection` (was at offset ~0x60+)
+    cwd: Option<PathBuf>,                // NEW
     parent_session_id: Option<acp::SessionId>,
     title: SharedString,
+    provisional_title: Option<SharedString>,  // NEW
     entries: Vec<AgentThreadEntry>,       // <-- shifted by +0x18
     plan: Plan,
     project: Entity<Project>,
     action_log: Entity<ActionLog>,
     ...
-    session_id: acp::SessionId,          // REMOVED from here (moved up)
     ...
 }
```

### Field size accounting

- `session_id: acp::SessionId` — moved before `parent_session_id` (no net size change, but reordering shifts `entries`)
- `cwd: Option<PathBuf>` — NEW, `Option<PathBuf>` = 32 bytes (ptr + len + cap + discriminant, aligned)
- `provisional_title: Option<SharedString>` — NEW, `Option<SharedString>` = 32 bytes

The compiler (`repr(Rust)`) reorders for alignment. The net result is `entries` shifted from offset 0x78 to 0x90 (+0x18 = 24 bytes).

### Relevant commits (v0.227.0 → v0.228.0)

| Commit | Description | Impact |
|--------|-------------|--------|
| `5db8d6d1bc` | agent: Only use AgentSessionInfo in history (#50933) | Moves `session_id` field position, adds `cwd` |
| `a0ba509838` | Fix provisional thread title (#50905) | Adds `provisional_title` field |
| `8d4913168c` | acp: Update to `0.10.2` (#51280) | ACP protocol update (no struct layout impact) |

---

## 3. Disassembly Evidence

Symbol: `__RNvMsj_CsheIy8I6v8Qz_10acp_threadNtB5_9AcpThread19authorize_tool_call`

```asm
; authorize_tool_call (v0.228.0 aarch64)
00000001000731d0  sub   sp, sp, #0x1b0
...
00000001000731fc  ldr   x8, [x0, #0x98]       ; entries.len  (was 0x80)
0000000100073200  cbz   x8, 0x100073444       ; early return if empty
...
0000000100073218  ldr   x9, [x0, #0x90]       ; entries.ptr  (was 0x78)
000000010007321c  mov   w10, #0x1b0           ; entry size = 432 (unchanged)
...
000000010007324c  cmp   x8, #0x7              ; ToolCall variant = 7 (unchanged)
0000000100073250  b.ne  0x100073238
...
0000000100073288  ldur  q0, [x25, #0x48]      ; status offset (unchanged)
...
0000000100073294  ldr   x9, [x25, #0x68]      ; respond_tx offset (unchanged)
...
000000010007329c  str   x8, [x25, #0x48]      ; write new status
```

Key observations:
- `ldr x8, [x0, #0x98]` → `ENTRIES_LEN_OFFSET = 0x98` (was 0x80)
- `ldr x9, [x0, #0x90]` → `ENTRIES_PTR_OFFSET = 0x90` (was 0x78)
- `mov w10, #0x1b0` → `ENTRY_SIZE = 0x1b0` (unchanged)
- `cmp x8, #0x7` → `TOOLCALL_VARIANT = 0x07` (unchanged)
- `ldur q0, [x25, #0x48]` → `ENTRY_STATUS_OFFSET = 0x48` (unchanged)
- `ldr x9, [x25, #0x68]` → `ENTRY_RESPOND_TX_OFFSET = 0x68` (unchanged)

---

## 4. zed-prj-workspace-hook Compatibility

The workspace hook depends on `OpenFolderEntry`, not `AcpThread`. Verified unchanged at v0.228.0:

```rust
// crates/recent_projects/src/recent_projects.rs (identical across v0.226–v0.228)
struct OpenFolderEntry {
    worktree_id: WorktreeId,
    name: SharedString,      // offset 0, size 24
    path: PathBuf,
    branch: Option<SharedString>,
    is_active: bool,
}
```

- Entry size 0x58 (88 bytes): unchanged
- `name` at offset 0: unchanged
- `insertion_sort_shift_left<OpenFolderEntry>` symbol: present at `0x106d785bc`
- sqlite3 hooks: stable system API, unaffected

**No changes needed for zed-prj-workspace-hook.**

---

## 5. Version Offset History

| Version | entries.ptr | entries.len | entry_size | status | respond_tx |
|---------|------------|------------|------------|--------|------------|
| v0.226.0 | 0x60 | 0x68 | 0x1b0 | 0x20 | 0x40 |
| v0.227.0 | 0x78 | 0x80 | 0x1b0 | 0x48 | 0x68 |
| v0.228.0 | 0x90 | 0x98 | 0x1b0 | 0x48 | 0x68 |

---

## 6. Related Documents

- [06_yolo_upgrade_guide.md](06_yolo_upgrade_guide.md) — Full recalibration procedure
- [10_hang_analysis_v0.227.0_2026-03-05.md](10_hang_analysis_v0.227.0_2026-03-05.md) — Previous offset recalibration (v0.226→v0.227)
