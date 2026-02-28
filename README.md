# zed-yolo-hook

YOLO mode for Zed on macOS (arm64): auto-approves ACP tool-call permission dialogs so external agents (Claude, Codex, Gemini, etc.) can run without manual confirmation clicks.

This is implemented as a Rust `cdylib` injected into Zed's `zed` binary and two Frida Gum hooks:

- `AcpThread::request_tool_call_authorization` (ACP agents)
- `ToolPermissionDecision::from_input` (native tool permissions; enabled in default allow-all mode)

## Related Repositories

- `dylib-kit`: https://github.com/laris/dylib-kit
- `zed-project-workspace`: https://github.com/laris/zed-project-workspace

## Modes

`ZED_YOLO_MODE` controls which hooks are installed:

- Unset: enable both hooks (ACP + native)
- `allow_safe`/`safe`: enable ACP-only; native permissions stay governed by Zed settings
- `0`/`off`/`disabled`: disable hooks (dylib loads but does nothing)

## Quickstart

```bash
# Patch Zed Preview (default)
cargo patch

# Tail logs
tail -f ~/Library/Logs/Zed/zed-yolo-hook.*.log

# Restore original
cargo patch restore
```

For the full workflow (stable builds, custom app paths, dry-run inject), see `docs/02_yolo_quickstart.md`.

## How This Repo Uses dylib-kit

This repo uses `dylib-kit` in `xtask`:

1. `dylib-patcher` provides patch/restore/codesign/verify command flow.
2. `dylib-hook-registry` tracks hook metadata and supports multi-hook coexistence.
3. `cargo patch` command behavior is delegated to the shared SDK CLI.

This keeps `zed-yolo-hook` focused on hook behavior instead of maintaining separate patch scripts.

## Safety

This intentionally removes an important safety barrier.

- Any agent with tool access can execute actions without interactive confirmation.
- Keep rollback handy (`cargo patch restore`).
- Prefer testing in a separate Zed install or a dedicated machine.

## Docs

- `docs/01_yolo_background.md`
- `docs/02_yolo_quickstart.md`
- `docs/03_yolo_research.md`
- `docs/04_yolo_design.md`
- `docs/05_yolo_implementation_log.md`
- `docs/06_yolo_upgrade_guide.md`
