# 27 - Zed 1.1.5 Enhanced Source Strategy

> Date: 2026-05-06
> Scope: `lary.me/zed-yolo` on `v1.1.5-pre-enhanced`
> Supersedes the build target assumptions in docs 23-26 for the active 1.1.5
> line. The older docs remain useful background for approval layers, remote
> topology, cross-build constraints, and the old dylib fallback.

## 1. Current Repository State

- Upstream base is `zed-industries/zed` tag `v1.1.5-pre`
  (`0ba798cfbe4b8bfae0c5eae60a7399eb9c8780bc`).
- Active fork branch is `v1.1.5-pre-enhanced`.
- The old `v1.1.4-pre-enhanced` line was removed from local and CNB fork state.
- `https://cnb.cool/lary.me/zed-upstream` is not a bit-for-bit mirror branch:
  it intentionally carries one `.cnb.yml` commit on top of GitHub `main` so CNB
  can keep pulling from GitHub server-side with `tencentcom/git-sync`.
- 2026-05-06 21:xx CST mirror check: GitHub `main` was
  `65107c90b10d2719b4739277ed5c06612d3180a4`; CNB `zed-upstream/main` was
  advanced to `39e5313889aa3adfbfc207e6a2a217212010689d`, a merge commit that
  preserves CNB's `.cnb.yml` bootstrap commit while incorporating that GitHub
  head. Tag `v1.1.5-pre` remains
  `0ba798cfbe4b8bfae0c5eae60a7399eb9c8780bc`.
- `https://cnb.cool/lary.me/zed-yolo` is the enhanced fork. The branch should be
  treated as the single integration branch for local macOS builds and CNB
  workspace builds.
- 2026-05-06 21:xx CST fork check: local
  `v1.1.5-pre-enhanced` and CNB `v1.1.5-pre-enhanced` both point to
  `3fac5316630b0d2dd9ffdb3f32d687234d6253fc`. No local or CNB
  `v1.1.4-pre-enhanced` branch remains after pruning.

Natural squash groups if the branch is rewritten later:

1. CNB workspace build infrastructure: `.cnb.yml`, Dockerfile, live log wrapper,
   target matrix, attachment publishing.
2. Darwin cross-build fixes: `gpui_macos`, `media`, Mach-O duplicate dylib
   checker.
3. Enhanced runtime policy: config-backed YOLO, About title marker.
4. Bundled remote-server distribution: release asset naming, app resource copy,
   auto-update override.

Do not rewrite the published branch while a CNB build is running unless we are
ready to restart that build from the new head.

## 2. YOLO Policy Is Now a Zed Setting

The active fork should not require `launchctl setenv` or a shell prefix to enable
YOLO. The policy is now represented under `agent.enhanced_yolo`:

```json
{
  "agent": {
    "enhanced_yolo": {
      "enabled": true,
      "auto_approve_acp": true,
      "inject_agent_env": true,
      "disable_agent_sandbox": false
    }
  }
}
```

Behavior:

- `auto_approve_acp=true` makes Zed's ACP `RequestPermission` handler choose an
  allow option without opening the UI dialog.
- `inject_agent_env=true` adds `ZED_YOLO=1` and `ZED_YOLO_APPROVALS=1` to every
  ACP agent launch. This applies to local projects and SSH remote projects,
  because Zed builds the remote command from the same `AgentServerCommand.env`.
- `disable_agent_sandbox=false` is deliberately conservative. When set true, Zed
  also injects `ZED_YOLO_SANDBOX=1` for patched adapters that support disabling
  their execution sandbox.
- Explicit process env still wins for emergency overrides:
  `ZED_YOLO=0` or `ZED_YOLO_APPROVALS=0` disables the ACP auto-approval branch.

This replaces the env-only default used in the first 1.1.5 builds while keeping
the env knobs as an operational fallback.

## 3. Remote Server Must Be Patched By Distribution, Not Just Source

For SSH remote development, the UI process is local macOS Zed, but the project
backend is a Linux `remote_server`. Upstream Zed downloads `zed-remote-server`
through `AutoUpdater::get_remote_server_release_url` /
`download_remote_server_release`, which resolves official assets from Zed cloud.

That means a patched macOS app can silently pair with an official remote server
unless we override remote-server asset selection.

Active enhanced policy:

- The app first searches bundled resources:
  `Zed Preview.app/Contents/Resources/remote_servers/`.
- It also supports `ZED_ENHANCED_REMOTE_SERVER_DIR` for local testing and
  `~/Library/Application Support/Zed/remote_servers/bundled` as a user-level
  drop-in cache.
- Linux x86_64 lookup prefers musl by default:
  `zed-remote-server-linux-x86_64-musl.gz`.
- Set `ZED_ENHANCED_REMOTE_SERVER_LIBC=gnu` to prefer
  `zed-remote-server-linux-x86_64-gnu.gz`.
- Set `ZED_ENHANCED_REMOTE_SERVER_REQUIRED=1` to fail instead of falling back to
  official remote-server downloads when no enhanced asset exists.

The CNB release job now emits both:

- archive form: `zed-yolo-v<VERSION>-x86_64-unknown-linux-<libc>-<DATE>-g<SHA>.tar.zst`
- direct app-resource form: `zed-remote-server-linux-x86_64-<libc>.gz`

The official `script/bundle-mac` exists and is the right base for app packaging.
The enhanced fork now copies any `dist/zed-remote-server-linux-*.gz` or
`target/zed-remote-server-linux-*.gz` files into the app resources before code
signing. For quick local tests, manual `/private/tmp/.../Zed Preview.app`
assembly is still acceptable, but the target state is to use `script/bundle-mac`
or a thin wrapper around it.

## 4. Current Build Goal

Default build matrix for this phase:

- macOS desktop app: `aarch64-apple-darwin`, packages `zed` and `cli`.
- Linux remote server: `x86_64-unknown-linux-gnu`, package `remote_server`.
- Linux remote server: `x86_64-unknown-linux-musl`, package `remote_server`.

Disabled by default:

- `x86_64-apple-darwin` app.
- Linux `zed` / `cli` app packages.
- Linux `aarch64` remote_server until there is a real deployment target.

Reason: the immediate workflow is local Apple Silicon desktop Zed plus x86_64
Linux remote hosts. Building fewer packages makes the CNB workspace cycle
shorter and avoids spending workspace CPU on unused artifacts.

## 5. Source-Native Project Manager Direction

`zed-project-workspace` solved three practical problems with a dylib + MCP layer:

1. stable project identity through `project_name`;
2. `.code-workspace` file sync for multi-folder projects;
3. primary-root pinning so Zed's UI title and folder order remain stable.

Upstream Zed 1.1.5 has moved part of this problem forward. The workspace DB now
stores both `paths` and `identity_paths`, and recent-project grouping dedupes by
`identity_paths`. This means the source-native version should not port the old
SQLite detour literally. It should be built around upstream's persistence model.

Recommended source patch shape:

- Add a `workspace.project_manager` settings object with explicit toggles:
  - `enabled`
  - `code_workspace_sync`
  - `auto_create_code_workspace`
  - `pin_primary_root`
  - `reconcile_external_file_edits`
  - `migrate_on_primary_rename`
- Treat `.code-workspace` as a persistent project manifest, not a Zed runtime
  cache.
- Store the chosen manifest path in `identity_paths` or a dedicated DB column
  rather than overloading folder names.
- Keep the old hook's decision matrix: generate file, normal sync, partial open,
  explicit removal, superset expand, divergence sidecar, rename/demote primary.
- Implement the watcher and DB writes inside Zed's own workspace/persistence
  code instead of using `sqlite3_prepare_v2` interception.

Probable source touchpoints:

- `crates/workspace/src/persistence.rs`: read/write `identity_paths`, recent
  grouping, DB schema migration if a manifest path is added.
- `crates/workspace/src/workspace.rs`: save serialized workspace with the
  project identity hint.
- `crates/settings_content/src/workspace.rs` and
  `crates/workspace/src/workspace_settings.rs`: settings schema and compiled
  settings.
- `crates/workspace/src/welcome.rs` and title/sidebar code only if the project
  display name needs a first-class field.

This should be a separate patch series from the YOLO and remote-server work.
It has higher persistence risk and needs dedicated tests against the workspace
DB.

Implementation checkpoint:

- `3fac531663` adds the settings/schema scaffold in the Zed fork:
  `project_manager.enabled`, `code_workspace_sync`,
  `auto_create_code_workspace`, `pin_primary_root`,
  `reconcile_external_file_edits`, `write_sidecars_on_divergence`,
  `auto_expand_partial_open`, and `migrate_on_primary_rename`.
- The master switch defaults to `false`; the commit is intentionally inert and
  does not yet read or write Zed's workspace DB or `.code-workspace` files.
- This gives the later source-native project-manager patch a stable config
  surface without mixing persistence behavior into the current YOLO /
  remote-server build.

## 6. Verification Checklist

Local macOS:

1. `cargo fmt`
2. `cargo check --package agent_servers`
3. `cargo check --package auto_update`
4. `ZED_BUNDLE=true ZED_RELEASE_CHANNEL=preview cargo build --release --package zed --package cli --target aarch64-apple-darwin --features gpui_platform/runtime_shaders`
5. Build or refresh `/private/tmp/zed-yolo-app-test/Zed Preview.app`.
6. Confirm About title shows `Zed Preview 1.1.5 Enhanced`.
7. Confirm an ACP permission request is auto-approved with default settings.

2026-05-06 local verification at commit `3fac531663`:

- `cargo fmt` passed.
- `cargo check --package workspace --package settings_content` passed.
- `ZED_BUNDLE=true ZED_RELEASE_CHANNEL=preview cargo build --release --package zed --package cli --target aarch64-apple-darwin --features gpui_platform/runtime_shaders`
  passed on local macOS.
- Test app:
  `/private/tmp/zed-yolo-app-test-3fac531-202413/Zed Preview.app`.
- `file` reports arm64 Mach-O for `zed` and `cli`.
- `cli --version` reports `Zed 1.1.5`.
- `script/check-macho-dylibs.py` reports no duplicate dylib load commands for
  both `zed` and `cli`.

Remote-server:

1. CNB builds `zed-remote-server-linux-x86_64-gnu.gz`.
2. CNB builds `zed-remote-server-linux-x86_64-musl.gz`.
3. Copy both into `Contents/Resources/remote_servers`.
4. Set `ZED_ENHANCED_REMOTE_SERVER_REQUIRED=1` for a negative test.
5. Open an SSH remote project and verify logs say the bundled enhanced remote
   server was used.

2026-05-06 CNB verification in progress:

- Release tag `zed-yolo-v1.1.5-pre-enhanced` exists on commit `3fac531663`.
- Release id: `2052006239594655744`.
- Workspace-CPU pipeline `cnb-j6o-1jnul23ub-001` uses 16 CPU / 32 GiB.
- `build-aarch64-apple-darwin-zed-cli` and
  `package-aarch64-apple-darwin-zed-cli` succeeded.
- Current remaining checks are GNU remote_server, musl remote_server, release
  attachment upload, local app resource copy, app re-sign, and SSH
  remote-server selection verification.
