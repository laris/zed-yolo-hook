# zed-yolo-hook: Quickstart

> Date: 2026-02-27
> Target: Zed Preview + Zed Stable (macOS aarch64)

---

## Prerequisites

- macOS on Apple Silicon (aarch64)
- Rust toolchain per `rust-toolchain.toml` (nightly)
- Xcode Command Line Tools (for `codesign`, `otool`)
- Zed installed at one of:
  - `/Applications/Zed Preview.app`
  - `/Applications/Zed.app`

---

## 1. Build + Inject

This repo includes an `xtask` patcher exposed via a Cargo alias.

Patch Zed Preview (default):

```bash
cd /path/to/zed-yolo-hook
cargo patch
```

Recommended full workflow with startup verification:

```bash
cd /path/to/zed-yolo-hook
cargo patch --verify
```

Patch Zed Stable:

```bash
cargo patch --stable
```

The current `xtask` targets `/Applications/Zed Preview.app` by default.

What it does:

1. Builds `libzed_yolo_hook.dylib` (unless you pass `--dylib`)
2. Installs `insert-dylib` into `target/tools` (first run only)
3. Backs up `.../Contents/MacOS/zed` to `.../Contents/MacOS/zed.original`
4. Injects the dylib load command
5. Re-signs the app bundle (ad-hoc)
6. Verifies injection via `otool -L`

Verified on 2026-03-27 for:

- Zed Preview `0.230.0`
- build `0.230.0+preview.205.9437a84390a396d666f04b38db87d89bb07284c1`
- bundle build `20260325.153514`

---

## 2. Verify

Watch the hook log:

```bash
tail -f ~/Library/Logs/Zed/zed-yolo-hook.*.log
```

You should see something like:

- `=== zed-yolo-hook vX.Y.Z ===`
- `permission_decision: hook installed` (only in allow-all mode)
- `tool_authorization: hook installed`
- `YOLO mode ACTIVE`

For a full ACP-path confirmation, a fresh external-agent tool permission request should then
produce lines like:

- `matched v0.230.x entry`
- `send succeeded`
- `approved in ... via v0.230.x`

This exact sequence was observed in the 2026-03-27 verification log after the final
Preview `0.230.0` patch.

Example: agent tool call proceeds without the approval dialog:

![After patch: tool call runs without approval UI](zed-after-patched-run-shell-date.png)

For reference, the dialog this project bypasses:

![ACP approval dialog](zed-yolo-always-allow-auto-run.png)

---

## 3. Disable Without Restoring

Launch Zed with YOLO disabled:

```bash
ZED_YOLO_MODE=0 open "/Applications/Zed Preview.app"
```

Modes:

- `ZED_YOLO_MODE` unset: enable hooks (default)
- `ZED_YOLO_MODE=allow_safe`: only auto-approve ACP dialogs; native permissions remain governed by Zed settings
- `ZED_YOLO_MODE=0|off|disabled`: dylib loads but installs no hooks

---

## 4. Restore

Restore the original binary from the `.original` backup and re-sign:

```bash
cargo patch restore           # Zed Preview
cargo patch restore --stable  # Zed Stable
```

---

## Troubleshooting

### "Permission" or write failures

If patching fails due to filesystem permissions on `/Applications`, patch from a shell session that has
write access to the app bundle. The current `xtask` does not expose a custom `--zed-app` override.

### Keychain prompts / access issues

After patching and re-signing, macOS may show additional prompts the first time Zed (or plugins) access protected resources.

![After patch: example keychain access prompt](zed-after-patched-require-read-keychains.png)
