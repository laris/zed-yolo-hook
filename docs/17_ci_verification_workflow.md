# CI Verification Workflow Design

Status: **IMPLEMENTED AND VERIFIED** (2026-04-11, run ID 24283318357)
First pass: Zed Preview v0.232.0-pre on `macos-15` ARM64, all 6 hooks verified.

## Overview

Automated post-release verification that `zed-yolo-hook` works with each new
Zed Preview version. Runs on GitHub Actions `macos-15` ARM64 runner. Downloads
Zed Preview from GitHub Releases, builds the hook from source, injects it,
launches Zed, and checks log markers.

Everything runs online -- no local uploads, no secrets required for Level 1.

## Files

```
.github/
  workflows/
    verify-hook.yml          # Main verification workflow (GitHub Actions)
scripts/
  ci-verify.sh               # Standalone verification script (local or CI)
```

## Trigger Strategy

| Trigger | Use Case | Auto? |
|---------|----------|-------|
| `workflow_dispatch` | Manual run (test a specific Zed version) | No |
| `schedule: cron` | Weekly on Monday 08:00 UTC | Yes |
| `push` to main | Verify after hook code changes (`src/**`, `Cargo.toml`, `Cargo.lock`) | Yes |

Manual dispatch accepts an optional `zed_version` input (e.g., `v0.232.0-pre`).
If omitted, fetches the latest pre-release from `zed-industries/zed`.

Note: changes to `.github/workflows/` alone do NOT trigger the push path filter.
Use `gh workflow run verify-hook.yml -R laris/zed-yolo-hook` for manual dispatch.

## Workflow Steps (Detailed)

```
┌────────────────────────────────────────────────────┐
│  1. Setup Environment                   (~10s)     │
│     - Runner: macos-15 (ARM64 M1, 3-core, 7GB)    │
│     - Rust: stable via dtolnay/rust-toolchain      │
│     - Cache: ~/.cargo/registry + ~/.cargo/git +    │
│              target/ (keyed on Cargo.lock hash)     │
│     - User: runner (/Users/runner)                 │
└─────────────┬──────────────────────────────────────┘
              │
┌─────────────▼──────────────────────────────────────┐
│  2. Resolve Zed Preview Version          (~2s)     │
│     - gh release list -R zed-industries/zed        │
│     - Grep for "Pre-release" tag                   │
│     - Extract: tag (v0.232.0-pre) and version      │
│       (0.232.0) via shell string manipulation      │
│     - Outputs: steps.zed.outputs.{tag,version}     │
└─────────────┬──────────────────────────────────────┘
              │
┌─────────────▼──────────────────────────────────────┐
│  3. Download & Install Zed Preview       (~8s)     │
│     - gh release download → Zed-aarch64.dmg        │
│     - hdiutil attach -nobrowse -quiet              │
│     - find /Volumes -name "Zed*" (volume name      │
│       may vary between releases)                   │
│     - cp -R "Zed Preview.app" /Applications/       │
│     - hdiutil detach                               │
│     - Verify: file → Mach-O arm64 executable       │
└─────────────┬──────────────────────────────────────┘
              │
┌─────────────▼──────────────────────────────────────┐
│  4. Build Hook                           (~30s)    │
│     - cargo build --release -p zed-yolo-hook       │
│     - Dependencies fetched from:                   │
│       - crates.io: agent-client-protocol, ctor,    │
│         futures-channel, serde, etc.               │
│       - git: frida-gum (frida/frida-rust),         │
│         dylib-hook-registry (laris/dylib-kit)      │
│     - Output: target/release/libzed_yolo_hook.dylib│
│     - Note: -p flag required to skip xtask build   │
│       (xtask has extra deps not needed for CI)     │
└─────────────┬──────────────────────────────────────┘
              │
┌─────────────▼──────────────────────────────────────┐
│  5. Install insert-dylib                 (~10s)    │
│     - cargo install insert-dylib --root target/    │
│       tools (from crates.io v0.1.1)               │
│     - IMPORTANT: nicokoch/insert-dylib git repo    │
│       no longer exists. Must use crates.io.        │
│     - Cached via GitHub Actions cache              │
└─────────────┬──────────────────────────────────────┘
              │
┌─────────────▼──────────────────────────────────────┐
│  6. Inject Dylib                         (~2s)     │
│     - Backup: cp binary → binary.original          │
│     - insert-dylib --weak --strip-codesig          │
│       --all-yes --inplace <dylib> <binary>         │
│     - IMPORTANT: Must use --inplace flag.          │
│       The 3-arg form (in==out) fails on CI with    │
│       "Failed to create" (permission issue).       │
│     - codesign -fs - --deep (ad-hoc re-signing)    │
│     - Verify: otool -L | grep libzed_yolo_hook     │
└─────────────┬──────────────────────────────────────┘
              │
┌─────────────▼──────────────────────────────────────┐
│  7. Launch & Verify                      (~5s)     │
│     - Clear old log files                          │
│     - Set env: ZED_YOLO_MODE=allow_all,            │
│       ZED_YOLO_LOG=debug                           │
│     - open "/Applications/Zed Preview.app"         │
│     - Poll log for up to 30s:                      │
│       ✓ "=== zed-yolo-hook v" (dylib loaded)      │
│       ✓ "YOLO mode ACTIVE"   (all hooks OK)       │
│       ✗ "Cannot find"        (symbol missing)      │
│       ✗ "attach failed"      (hook failed)         │
│     - In practice, markers appear within ~1s       │
│     - pkill "Zed Preview"                          │
│     - Output: steps.verify.outputs.result          │
└─────────────┬──────────────────────────────────────┘
              │
┌─────────────▼──────────────────────────────────────┐
│  8. Collect Artifacts                    (~3s)     │
│     - Upload hook log as artifact                  │
│       (retention: 14 days)                         │
│     - Write job summary (markdown table)           │
│     - Total workflow time: ~1m30s                  │
└────────────────────────────────────────────────────┘
```

## Key Design Decisions

### Why not `cargo patch --verify`?

`cargo patch --verify` is designed for local use:
- Detects if running inside the target app process (walks process tree)
- Manages quit/restart cycles via AppleScript `osascript`
- Uses `dylib-patcher` SDK which is not needed in CI

In CI, we bypass all of that:
1. Build the dylib directly (`cargo build --release -p zed-yolo-hook`)
2. Use raw `insert-dylib` CLI for injection (from crates.io)
3. Launch Zed with `open` and check logs ourselves

This is simpler, more transparent in CI logs, and avoids the xtask's
process-tree detection which doesn't work on a fresh CI runner.

### GUI Session on GitHub-hosted runners

**Confirmed working.** GitHub-hosted macOS runners have auto-login enabled.
The `actions/runner-images` repo provisions this via:
- `images/macos/scripts/build/configure-autologin.sh`
- `images/macos/assets/bootstrap-provisioner/setAutoLogin.sh`
  (encodes password via XOR cipher → `/etc/kcpassword`, sets
  `com.apple.loginwindow` defaults, disables screen lock)

This means WindowServer runs with a logged-in user session. The `open -a`
command successfully launches `.app` bundles, and our `#[ctor]` hook fires
during dylib load.

**What we verified on 2026-04-11:**
- `open "/Applications/Zed Preview.app"` returns immediately (~200ms)
- Zed spawns multiple processes (pids 6845, 6851 observed in CI log)
- Hook `#[ctor]` fires in all processes, writes to log within ~100ms
- All 6 hook points found and attached on first try
- `stale_scanner` background thread starts correctly

**What is NOT available:**
- Screen Recording permission (TCC restriction, GitHub issue #8951)
- Full interactive Aqua session (no screenshots or UI automation)
- Neither is needed for our verification workflow

### Dependency Management

All dependencies are fetched online, nothing from localhost:

| Dependency | Source | Notes |
|-----------|--------|-------|
| Zed Preview DMG | `gh release download` from `zed-industries/zed` | Always latest |
| Hook source | `actions/checkout` from `laris/zed-yolo-hook` | Current commit |
| `dylib-hook-registry` | Cargo git dep from `laris/dylib-kit` | Workspace member |
| `dylib-patcher` | Cargo git dep from `laris/dylib-kit` | Workspace member |
| `frida-gum` | Cargo git dep from `frida/frida-rust` | Auto-downloads devkit |
| `agent-client-protocol` | crates.io (=0.10.2) | Pinned exact version |
| `insert-dylib` | crates.io (v0.1.1) | `cargo install` |

**Important:** The `nicokoch/insert-dylib` GitHub repo no longer exists.
The crates.io package still works. This was discovered on the first CI run
(run ID 24283228452, failed with "failed to authenticate when downloading
repository").

### Version Resolution

Zed Preview releases use the tag pattern `v{VERSION}-pre` on GitHub:
```
$ gh release list -R zed-industries/zed --limit 5
v0.231.2    Latest     v0.231.2    2026-04-10
v0.232.0-pre Pre-release v0.232.0-pre 2026-04-08
v0.231.1             v0.231.1    2026-04-08
```

Assets follow the naming convention:
```
Zed-aarch64.dmg    (macOS ARM64 Preview)
Zed-x86_64.dmg     (macOS Intel Preview)
```

The workflow greps for "Pre-release" to find the latest preview tag.

### Caching Strategy

Cache the Cargo registry and build artifacts:
- Key: `cargo-macos-arm64-${{ hashFiles('Cargo.lock') }}`
- Fallback key: `cargo-macos-arm64-`
- Paths: `~/.cargo/registry`, `~/.cargo/git`, `target/`
- Cache size observed: ~216 MB compressed

The Zed DMG is NOT cached (always download latest to verify against it).
The `insert-dylib` binary is cached inside `target/tools/` (part of target cache).

### insert-dylib Flags

```
insert-dylib --weak --strip-codesig --all-yes --inplace <dylib> <binary>
```

| Flag | Purpose |
|------|---------|
| `--weak` | Use `LC_LOAD_WEAK_DYLIB` (app doesn't crash if dylib missing) |
| `--strip-codesig` | Strip existing code signature before modifying |
| `--all-yes` | Answer "yes" to all prompts (non-interactive) |
| `--inplace` | Modify binary in place (required on CI; see pitfalls) |

## Success Criteria

The verification **passes** when the log file contains ALL of:
1. `=== zed-yolo-hook v` — dylib loaded, version banner printed
2. `YOLO mode ACTIVE` — all hooks installed successfully

And does NOT contain ANY of:
1. `Cannot find` — symbol lookup failure
2. `attach failed` — Frida `Interceptor::attach` failed

These markers match the `HealthCheck` definition in `xtask/src/main.rs`.

## What Each Log Line Means

From the successful CI run (2026-04-11):

```
=== zed-yolo-hook v0.1.0 ===                          # Dylib loaded, version banner
config: mode=AllowAll, tool_option=Allow, ...          # Config parsed (defaults or file)
config file: /Users/runner/.config/dylib-hooks/...     # Config file path (created on first run)
Searching for symbol matching ["tool_permissions", ...] # Symbol scan started
permission_decision: Found _RNvMNt...from_input at ... # Hook 1: native tool permissions
permission_decision: hook installed                     # Hook 1: attached
Searching for symbol matching ["acp_thread", ...]       # Symbol scan for ACP
tool_authorization: Found _RNvMs...authorization at ... # Hook 2: ACP tool authorization
tool_authorization: hook installed                      # Hook 2: attached
upsert_hook: Found _RNvMs...upsert_tool_call_inner ...  # Hook 3: tool call upsert
upsert_hook: hook installed (approach 1)                # Hook 3: attached
session_update_hook: Found ...handle_session_update ... # Hook 4: session updates
session_update_hook: hook installed (approach 2)        # Hook 4: attached
push_entry_hook: Found ...push_entry at ...             # Hook 5: catch-all entry push
push_entry_hook: hook installed (catch-all registration)# Hook 5: attached
stale_scanner: started (interval=2000ms)                # Hook 6: background scanner
Registered in hook registry (app_id=zed-preview)        # Registry updated
YOLO mode ACTIVE (pid=6845)                             # SUCCESS: all hooks active
```

## Failure Modes and Troubleshooting

| Failure | Meaning | Action |
|---------|---------|--------|
| Build fails | Dependency version mismatch (e.g., ACP protocol) | Update `Cargo.toml` |
| `insert-dylib` can't clone | Git repo doesn't exist | Use crates.io (`cargo install insert-dylib`) |
| `insert-dylib` "Failed to create" | In-place write permission issue | Use `--inplace` flag |
| Injection succeeds but `otool -L` doesn't show dylib | insert-dylib silently failed | Check exit code, try `--overwrite` |
| Codesign fails | Entitlements changed or binary too large | Try `--deep` flag, check disk space |
| Log shows "Cannot find" | Symbol names changed in new Zed version | Re-scan symbols, update patterns |
| Log shows "attach failed" | Frida can't attach (function changed) | Investigate with disasm |
| No log output at all | Dylib didn't load or Zed crashed | Check crash reports in `~/Library/Logs/DiagnosticReports/` |
| Offsets wrong (silent failure) | Entry layout changed | This is NOT caught by Level 1 CI. Recalibrate offsets manually. |

## CI Run History

| Run | Date | Zed Version | Result | Issue |
|-----|------|-------------|--------|-------|
| #1 (24283228452) | 2026-04-11 | v0.232.0-pre | FAIL | `nicokoch/insert-dylib` repo dead → 401 auth error |
| #2 (24283282605) | 2026-04-11 | v0.232.0-pre | FAIL | `insert-dylib` "Failed to create" → need `--inplace` |
| #3 (24283318357) | 2026-04-11 | v0.232.0-pre | **PASS** | All 6 hooks verified, 1m30s total |

## Verification Levels

| Level | Verifies | Needs | Status |
|-------|----------|-------|--------|
| **1. Hook install** | Symbols found, Frida attach succeeds | Just launch Zed | **Implemented** |
| 2. Static offset check | Struct layouts match Zed source | Clone Zed source at matching commit, parse structs | Planned |
| 3. Live tool call | End-to-end auto-approval works | Anthropic API key as GitHub secret, Claude Code in Zed | Planned |

Level 1 catches **symbol renames** (most common breakage after Zed updates).
Level 2 would catch **struct layout changes** (field additions/removals/reordering).
Level 3 would catch **semantic breakage** (correct struct but wrong behavior).

## Environment Variables

| Variable | Purpose | Default | CI Value |
|----------|---------|---------|----------|
| `ZED_YOLO_MODE` | Hook mode | `allow_all` | `allow_all` |
| `ZED_YOLO_LOG` | Log level | `info` | `debug` |
| `ZED_YOLO_TOOL_OPTION` | Tool approval option | `allow` | (default) |
| `ZED_YOLO_PLAN_OPTION` | Plan mode option | `acceptEdits` | (default) |
| `ZED_YOLO_RETRY_DELAY_US` | Retry delay | `1500` | (default) |
| `GH_TOKEN` | GitHub API auth | — | `${{ github.token }}` |

## Future Enhancements

1. **Offset auto-detection**: If verification fails, automatically run
   `llvm-objdump` on the Zed binary and extract new offsets. Compare
   against `ENTRY_LAYOUTS` in `src/hooks/tool_authorization.rs`.

2. **PR comment**: Post verification results as a comment on PRs that
   change `src/hooks/tool_authorization.rs`.

3. **Matrix build**: Test against multiple Zed versions simultaneously
   (current preview + last 2 releases).

4. **Notification**: GitHub Actions can send email on failure. Could also
   use Slack webhook or issue auto-creation.

5. **Level 2 verification**: Clone `zed-industries/zed` at the matching
   commit, parse `AgentThreadEntry`, `ToolCall`, `ToolCallStatus` structs,
   compute expected field counts and sizes, compare against our offsets.

6. **Level 3 verification**: Store `ANTHROPIC_API_KEY` as repo secret,
   install Claude Code extension, send a test prompt that triggers a
   tool call, verify `"send succeeded"` appears in the hook log.

7. **Zed release webhook**: Use `repository_dispatch` triggered by a
   GitHub Action watching `zed-industries/zed` releases, so verification
   runs automatically on each new Zed Preview release (not just weekly).
