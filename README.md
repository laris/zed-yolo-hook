# zed-yolo-hook

YOLO mode for Zed on macOS (arm64): auto-approves ACP tool-call permission dialogs so external agents (Claude, Codex, Gemini, etc.) can run without manual confirmation clicks.

This is implemented as a Rust `cdylib` injected into Zed's `zed` binary and two Frida Gum hooks:

- `AcpThread::request_tool_call_authorization` (ACP agents)
- `ToolPermissionDecision::from_input` (native tool permissions; enabled in default allow-all mode)

## Related Repositories

- `dylib-kit`: https://github.com/laris/dylib-kit
- `zed-project-workspace`: https://github.com/laris/zed-project-workspace

## Configuration

Config is stored in `~/.config/dylib-hooks/{app_id}/zed-yolo-hook.json` (works with Finder/Dock launches — no shell env needed):

```json
{
  "mode": "allow_all",
  "tool_option": "allow",
  "plan_option": "acceptEdits",
  "log_level": "info",
  "retry_delay_us": 1500
}
```

| Field | Default | Values | Effect |
|-------|---------|--------|--------|
| `mode` | `allow_all` | `allow_all`, `allow_safe`, `disabled` | Which hooks to install |
| `tool_option` | `allow` | `allow`, `allow_always` | Option for regular tool permissions |
| `plan_option` | `acceptEdits` | `acceptEdits`, `bypassPermissions`, `default`, `plan` | Option for "Ready to code?" prompt |
| `log_level` | `info` | `trace`, `debug`, `info`, `warn`, `error` | Log verbosity |
| `retry_delay_us` | `1500` | 0–10000 | Retry delay on miss (µs) |

Manage via CLI:

```bash
cargo patch config                    # Show current config
cargo patch config set plan_option bypassPermissions
cargo patch config set tool_option allow_always
cargo patch config reset              # Reset to defaults
```

Environment variables (`ZED_YOLO_MODE`, `ZED_YOLO_TOOL_OPTION`, `ZED_YOLO_PLAN_OPTION`, `ZED_YOLO_LOG`) override config file values when set (useful for terminal testing).

## Quickstart

```bash
# Patch Zed Preview (default)
cargo patch

# Create config (optional — defaults work out of the box)
cargo patch config reset

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
- `docs/14_exitplanmode_gap_analysis.md` — ExitPlanMode bug fix, complete ACP permission matrix, config reference
