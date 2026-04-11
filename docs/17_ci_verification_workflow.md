# CI Verification Workflow Design

## Overview

Automated post-release verification that `zed-yolo-hook` works with each new
Zed Preview version. Runs on GitHub Actions macOS ARM64 runners.

## Trigger Strategy

| Trigger | Use Case |
|---------|----------|
| `workflow_dispatch` | Manual run (test a specific Zed version) |
| `schedule: cron` | Weekly check (catch new Zed releases) |
| `push` to main | Verify after hook code changes |

Manual dispatch accepts an optional `zed_version` input (e.g., `v0.232.0-pre`).
If omitted, fetches the latest pre-release from `zed-industries/zed`.

## Workflow Steps

```
┌─────────────────────────────────────────────┐
│  1. Setup Environment                        │
│     - macos-15 runner (ARM64 M1)            │
│     - Rust toolchain (stable, aarch64)      │
│     - Cache: cargo registry + target dir    │
└─────────────┬───────────────────────────────┘
              │
┌─────────────▼───────────────────────────────┐
│  2. Resolve Zed Preview Version              │
│     - gh release list -R zed-industries/zed │
│     - Pick latest *-pre tag                 │
│     - Extract version + commit hash         │
└─────────────┬───────────────────────────────┘
              │
┌─────────────▼───────────────────────────────┐
│  3. Download & Install Zed Preview           │
│     - gh release download → Zed-aarch64.dmg │
│     - hdiutil attach → mount DMG            │
│     - cp -R "Zed Preview.app" /Applications │
│     - hdiutil detach                        │
│     - Verify binary: file ... → arm64       │
└─────────────┬───────────────────────────────┘
              │
┌─────────────▼───────────────────────────────┐
│  4. Build Hook                               │
│     - cargo build --release -p zed-yolo-hook│
│     - Verify: file libzed_yolo_hook.dylib   │
│     - Verify: lipo -archs → arm64           │
└─────────────┬───────────────────────────────┘
              │
┌─────────────▼───────────────────────────────┐
│  5. Inject Dylib                             │
│     - Backup original binary                │
│     - insert_dylib (via dylib-patcher)      │
│     - Ad-hoc codesign                       │
│     - Verify: otool -L shows dylib          │
└─────────────┬───────────────────────────────┘
              │
┌─────────────▼───────────────────────────────┐
│  6. Launch & Verify                          │
│     - Clear old log files                   │
│     - open "/Applications/Zed Preview.app"  │
│     - Poll log file for up to 30s:          │
│       ✓ "=== zed-yolo-hook v"               │
│       ✓ "YOLO mode ACTIVE"                  │
│       ✗ "Cannot find"                       │
│       ✗ "attach failed"                     │
│     - Kill Zed process                      │
│     - Report result                         │
└─────────────┬───────────────────────────────┘
              │
┌─────────────▼───────────────────────────────┐
│  7. Collect Artifacts                        │
│     - Upload log file as artifact           │
│     - Upload dylib as artifact              │
│     - Set job summary with version info     │
└─────────────────────────────────────────────┘
```

## Key Design Decisions

### Why not `cargo patch --verify`?

`cargo patch --verify` is designed for local use — it detects if it's running
inside the target app process and manages quit/restart cycles. In CI, we don't
run inside Zed, so we can use a simpler approach:

1. Build the dylib directly (`cargo build --release`)
2. Use `dylib-patcher` CLI or raw `insert_dylib` for injection
3. Launch Zed with `open` and check logs ourselves

This avoids the complexity of the xtask's process-tree detection and gives us
more control over timeouts and error reporting.

### GUI Session on GitHub-hosted runners

GitHub-hosted macOS runners run as an auto-login user with a WindowServer
process. The `open -a` command successfully launches `.app` bundles. Our hook
initializes in `#[ctor]` (constructor function) during dylib load — this
happens before any GUI rendering. The hook writes to the log file immediately,
so we don't need an interactive Aqua session.

If the process crashes on launch (e.g., due to offset changes), that's also
a valid signal — we check for crash logs too.

### Version Resolution

Zed Preview releases use the tag pattern `v{VERSION}-pre` on GitHub:
```
gh release list -R zed-industries/zed --limit 10
```
Assets follow the naming convention:
```
Zed-aarch64.dmg    (macOS ARM64 Preview)
Zed-x86_64.dmg     (macOS Intel Preview)
```

Download URL pattern:
```
https://github.com/zed-industries/zed/releases/download/v{VERSION}-pre/Zed-aarch64.dmg
```

### Caching Strategy

Cache the Cargo registry and build artifacts to speed up subsequent runs:
- Key: `cargo-${{ runner.os }}-${{ hashFiles('Cargo.lock') }}`
- Paths: `~/.cargo/registry`, `~/.cargo/git`, `target/`

The Zed DMG is NOT cached (always download latest to verify against it).

### Failure Modes

| Failure | Meaning | Action |
|---------|---------|--------|
| Build fails | Dependency version mismatch (e.g., ACP protocol) | Update `Cargo.toml` |
| Injection fails | Binary format changed | Update `dylib-patcher` |
| Codesign fails | Entitlements changed | Update signing flags |
| Log shows "Cannot find" | Symbol names changed | Re-scan symbols |
| Log shows "attach failed" | Function signature changed | Investigate with disasm |
| No log output | Dylib didn't load / crash | Check crash logs |
| Offsets wrong (silent) | Entry layout changed | Recalibrate offsets |

### Success Criteria

The verification **passes** when the log file contains both:
1. `=== zed-yolo-hook v` (dylib loaded, version banner)
2. `YOLO mode ACTIVE` (all hooks installed successfully)

And does NOT contain:
1. `Cannot find` (symbol lookup failure)
2. `attach failed` (hook installation failure)

## File Structure

```
.github/
  workflows/
    verify-hook.yml          # Main verification workflow
scripts/
  ci-verify.sh               # Standalone verification script (optional)
```

## Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `ZED_VERSION` | Override Zed version to test | Latest pre-release |
| `ZED_YOLO_MODE` | Hook mode | `allow_all` |
| `ZED_YOLO_LOG` | Log level | `debug` (verbose in CI) |

## Future Enhancements

1. **Offset auto-detection**: If verification fails, automatically run
   `llvm-objdump` on the Zed binary and extract new offsets
2. **PR comment**: Post verification results as a comment on PRs
3. **Matrix build**: Test against multiple Zed versions simultaneously
4. **Notification**: Slack/email alert when a new Zed version breaks the hook
