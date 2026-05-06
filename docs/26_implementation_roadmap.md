# Implementation Roadmap — From Today's Dylib to Tomorrow's Patch Train

> 2026-05-06 status: this remains the longer migration roadmap. The active
> 1.1.5 enhanced branch work and source-native project-manager direction are
> consolidated in `27_v1_1_5_enhanced_source_strategy.md`.

> Date: 2026-05-05
> Scope: Concrete milestones, repos, owners, and exit criteria for moving
> `zed-yolo-hook` to the source-patch + staged-build architecture.
> Companions: `23_source_patch_strategy.md`, `24_remote_zed_yolo_strategy.md`,
> `25_cross_compile_macos_strategy.md`.

---

## 0. Executive Summary

We move from one-repo (`zed-yolo-hook`, dylib only) to a four-repo system:

```
zed-yolo-hook            ← keep; becomes the "fallback dylib" path
zed-yolo-patches         ← NEW; the patch train (text patches + applier)
zed-yolo-builder         ← NEW; the staged-build pipeline (Docker + scripts)
zed-yolo-distribution    ← NEW (or piggyback GH Releases of patches repo);
                            hosts the signed Zed.app artifacts and update feed
```

Total estimated effort, sequential, at one engineer's full attention:

| Milestone | Effort | Calendar |
|-----------|--------|----------|
| M1 — Layer 1 patches (claude/codex env-var) | 2 days | Week 1 |
| M2 — Layer 2A patch (Zed handle_request_permission) | 2-3 days | Week 1-2 |
| M3 — Patch train repo + applier | 1-2 days | Week 2 |
| M4 — Linux build container + scripts | 3-4 days | Week 2-3 |
| M5 — rcodesign + notary wiring | 2-3 days | Week 3 |
| M6 — Distribution (auto-update flow) | 3-5 days | Week 3-4 |
| M7 — Documentation + migration guide | 2 days | Week 4 |

So roughly **3-5 weeks of focused work** for a complete pivot. Realistic
calendar (with normal interruptions) is **6-9 weeks**. Each milestone
ships independently and provides immediate value, so the timeline can
tolerate slippage.

---

## 1. Milestone 1 — Layer 1 Patches

### 1.1 Goal

Get a YOLO build of `claude-agent-acp` and `codex-acp` that respects
`ZED_YOLO=1` (and the more granular `ZED_YOLO_APPROVALS` /
`ZED_YOLO_SANDBOX` flags). End state: setting the env var in
`agent_servers.<id>.env` produces a session with no approval dialogs.

### 1.2 Work items

#### claude-agent-acp

- [ ] Fork `gh-agentclientprotocol__claude-agent-acp` to your org.
- [ ] Apply the two patches outlined in `23_source_patch_strategy.md` §2.1.1:
      - `ALLOW_BYPASS` lift in env
      - `permissionMode` default to `bypassPermissions`
- [ ] Add ExitPlanMode short-circuit (`24_remote_zed_yolo_strategy.md` §4.3).
- [ ] Add tests in `src/tests/acp-agent-settings.test.ts`:
      - YOLO env var present + no explicit mode → `bypassPermissions`
      - YOLO env var + explicit mode → user mode wins
      - YOLO env var + ExitPlanMode → auto `acceptEdits`
      - No YOLO env var → original behavior preserved
- [ ] Publish as `@your-org/claude-agent-acp-yolo`.

#### codex-acp

- [ ] Fork `gh-zed-industries__codex-acp`.
- [ ] Apply the patch in `23_source_patch_strategy.md` §2.1.2:
      - `src/codex_agent.rs::build_session_config()` sets
        `approval_policy=Never` and `permission_profile=Disabled` before
        `ThreadManager::start_thread()` when `ZED_YOLO=1` (or the more
        granular flags).
      - `src/thread.rs::handle_set_mode()` remains useful for later
        user-driven `"full-access"` mode changes, but is not the initial
        config patch point.
- [ ] Add tests around the session config and mode path:
      - YOLO env var → initial `current_session_mode_id` / permissions map
        resolve to full-access before the thread starts
      - YOLO env var → no `ExecApprovalRequest` or
        `ApplyPatchApprovalRequest` reaches the ACP permission bridge in a
        smoke turn
      - No YOLO env var → original read-only/default behavior preserved
- [ ] Build the binary for both `aarch64-apple-darwin` and
      `aarch64-unknown-linux-gnu` (and x86_64 variants).
- [ ] Publish via GH Releases of your fork.
- [ ] Optionally publish an npm wrapper that downloads the right binary
      for the user's platform (mirrors upstream `npm/` directory pattern).

### 1.3 Exit criteria

- Set `agent_servers.claude-acp.env.ZED_YOLO=1` and
  `agent_servers.claude-acp.command` pointing at the fork → smoke test
  passes (Topology A test 1 from `24_remote_zed_yolo_strategy.md` §5).
- Same for codex-acp.
- Patches in plain `.patch` form, ready for the train (M3).

---

## 2. Milestone 2 — Layer 2A Patch

### 2.1 Goal

A patched Zed binary that auto-approves any ACP `RequestPermission`
when YOLO is on, regardless of which agent (or even no agent) is asking.

### 2.2 Work items

- [ ] Fork or maintain a working tree of `gh-zed-industries__zed` pinned
      to a specific upstream tag (start with the current Preview).
- [ ] Implement the `handle_request_permission` short-circuit
      (`23_source_patch_strategy.md` §2.2.1).
- [ ] Plumb a real settings field under
      `agent.tool_permissions.external_agents.default`:
      - `crates/settings_content/src/agent.rs` —
        `ToolPermissionsContent::external_agents`
      - `crates/agent_settings/src/agent_settings.rs` —
        compiled enum/field with default `"prompt"`
      - `crates/agent_settings/src/agent_settings.rs` —
        `compile_tool_permissions` reads the new field
      - `crates/agent_servers/src/acp.rs` —
        `handle_request_permission` reads the compiled policy and selects
        an allow option when it is `"always_allow"`
- [ ] Optional helper: when the new external-agent policy is
      `"always_allow"`, auto-add `ZED_YOLO=1` to patched adapter commands
      so Layer 1 and Layer 2 agree. This is a convenience only; explicit
      `agent_servers.<agent>.env` already flows to remote commands through
      Zed's existing spawn path.
- [ ] Add log line when YOLO branch fires (helps users debugging in
      `dev: open acp logs`).
- [ ] Tests:
      - `crates/agent_servers/src/acp.rs` fake ACP server path and
        `crates/agent_servers/src/e2e_tests.rs` — assert auto-approval
        returns a selected allow option without entering UI wait state
      - `crates/agent_settings` compile tests — new settings field defaults
        to prompt and compiles `"always_allow"`
      - Negative test: with YOLO off / prompt policy, the dialog still waits
- [ ] Run `cargo nextest run --workspace` locally and via Zed's CI
      pattern.

### 2.3 Exit criteria

- A debug build of patched Zed locally launched (`cargo run --package zed`)
  shows zero approval dialogs in a Claude/Codex thread when the new
  setting is on.
- Tests pass.
- The patches apply cleanly against at least the current and previous
  Zed Preview tags (forward-compatibility scout).

---

## 3. Milestone 3 — Patch Train Repo

### 3.1 Goal

A versioned, applicable, CI-tested set of patches across all three
upstream repos.

### 3.2 Layout

```
zed-yolo-patches/
├── README.md                      ← user-facing how-to
├── apply.sh                        ← one-shot applier
├── verify.sh                       ← runs a smoke build after apply
├── train.json                      ← which upstream tag each patch series targets
├── claude-agent-acp/
│   ├── 0001-allow-bypass-when-zed-yolo-env.patch
│   ├── 0002-default-permissionmode-bypass-when-yolo.patch
│   └── 0003-exit-plan-mode-yolo-shortcircuit.patch
├── codex-acp/
│   ├── 0001-default-approval-never-when-zed-yolo-env.patch
│   └── 0002-default-permission-profile-disabled-when-yolo.patch
├── zed/
│   ├── 0001-yolo-shortcircuit-handle-request-permission.patch
│   ├── 0002-add-agent-tool-permissions-external-agents-field.patch
│   ├── 0003-optional-inject-zed-yolo-env-into-agent-spawn.patch
│   ├── 0004-acp-debug-log-yolo-shortcircuit.patch
│   └── 0005-gpui-macos-build-rs-target-os-cross-build.patch
└── .github/workflows/
    └── nightly-rebase.yml          ← runs apply.sh against latest upstream tags
```

### 3.3 Work items

- [ ] Generate the patch files via `git format-patch` from M1/M2 branches.
- [ ] Write `apply.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail
TRAIN="$(dirname "$(realpath "$0")")"
config="$TRAIN/train.json"

apply() {
    local repo="$1" dir="$2"
    [[ -d "$dir" ]] || { echo "skip: $dir not found"; return; }
    git -C "$dir" am --3way "$TRAIN/$repo/"*.patch
}

apply zed                  "${ZED_REPO:-../gh-zed-industries__zed}"
apply claude-agent-acp     "${CLAUDE_REPO:-../gh-agentclientprotocol__claude-agent-acp}"
apply codex-acp            "${CODEX_REPO:-../gh-zed-industries__codex-acp}"
```

- [ ] Write `train.json` declaring known-good upstream tags:

```json
{
  "trains": [
    {
      "name": "preview-0.234",
      "zed":              "v0.234.x",
      "claude-agent-acp": "0.32.x",
      "codex-acp":        "0.13.x"
    }
  ]
}
```

- [ ] Add a nightly GHA workflow that:
      - Clones each upstream HEAD
      - Runs `apply.sh`
      - On failure, opens an issue tagged `train-rebase-needed`
      - On success, runs `cargo check --workspace` for Zed and codex-acp,
        and `npm test` for claude-agent-acp

### 3.4 Exit criteria

- `bash apply.sh` against fresh clones of the three upstream repos
  succeeds.
- Nightly CI green for at least 3 consecutive runs.
- A clear "this patch failed to apply against upstream HEAD" failure
  mode exists and produces a useful diff.

---

## 4. Milestone 4 — Linux Build Container

### 4.1 Goal

A reproducible Docker-based build that turns the patch train into a
`Zed.app` for both macOS architectures, on Linux, in under 20 min cold.

### 4.2 Work items

- [ ] Write `zed-yolo-builder/Dockerfile` per `25_cross_compile_macos_strategy.md` §4.
- [ ] Write `script/build-zed-yolo` driver (per `25_cross_compile_macos_strategy.md` §5).
- [ ] Include required build tools in the image: pinned `cargo-zigbuild`,
      `cbindgen`, Zed's forked `cargo-bundle` from the `zed-deploy`
      branch, `sccache`, `zip`, and the `apple-codesign` crate that
      provides the `rcodesign` binary.
- [ ] Apply the `gpui_macos/build.rs` target-OS patch before the first
      Linux macOS build; otherwise runtime shader stitching is skipped on a
      Linux host.
- [ ] Set up sccache backend (R2 or S3 — Backblaze B2 is cheapest).
- [ ] Build & push the image to `ghcr.io/your-org/zed-yolo-builder`.
- [ ] Smoke test: launch the image, run the driver, get a `.app`; compile
      generated licenses first, then compile `zed`/`cli` with
      `gpui_platform/runtime_shaders` and compile `remote_server` in its
      own invocation.
- [ ] Validate: scp the `.app` to a macOS box and launch it; verify it
      runs (it'll be unsigned but should work after `xattr -cr` or
      Gatekeeper prompt-through).

### 4.3 Exit criteria

- Cold build of single arch ≤ 20 min on a 32-core Linux box.
- Warm build (sccache populated) ≤ 5 min.
- Output `.app` launches on macOS and shows the YOLO behavior end-to-end.

---

## 5. Milestone 5 — Code Sign + Notarize on Linux

### 5.1 Goal

Build outputs are signed with our Apple Developer ID and notarized,
producing a Gatekeeper-approved `.app` and `.zip` (or `.dmg`).

### 5.2 Work items

- [ ] Apple Developer enrollment (one-time; ~$99/yr).
- [ ] Create a Developer ID Application certificate; export as PKCS#12.
- [ ] Create a notary API key (`.p8`).
- [ ] Replace Zed Industries team-specific provisioning profiles and
      entitlements with the YOLO fork's team ID / profile set before
      signing.
- [ ] Wire `rcodesign` into `script/build-zed-yolo`:

```bash
rcodesign sign \
    --p12-file /secrets/zed-yolo.p12 \
    --p12-password-file /secrets/zed-yolo.p12.password \
    --code-signature-flags runtime \
    --entitlements-xml-file crates/zed/resources/zed.entitlements \
    "target/$TARGET/release/bundle/osx/Zed.app"

mkdir -p "out/$TARGET"
ROOT="$PWD"
( cd "target/$TARGET/release/bundle/osx" && \
  zip -qry "$ROOT/out/$TARGET/Zed.app.zip" Zed.app )

rcodesign notary-submit --wait \
    --api-key-path /secrets/notary.json \
    "out/$TARGET/Zed.app.zip"

rcodesign staple "target/$TARGET/release/bundle/osx/Zed.app"
```

- [ ] Optional: produce a `.dmg` via libdmg-hfsplus, sign + notarize that
      too. Or skip and ship `.zip`.
- [ ] Validate on a clean macOS Sequoia/Tahoe install: download
      `.app`/`.zip` → drag to Applications → double-click → no Gatekeeper
      block.

### 5.3 Exit criteria

- Output passes `spctl -a -t exec -vv Zed.app` with no errors.
- Output passes `xcrun stapler validate Zed.app`.
- Tested on at least two macOS versions (Sequoia & Tahoe).

---

## 6. Milestone 6 — Distribution + Auto-Update

### 6.1 Goal

Users can install once and receive YOLO Zed updates automatically as
upstream Zed Preview / Stable releases ship.

### 6.2 Architecture options

#### Option A — Hijack Zed's auto-update endpoint

`crates/auto_update/` reaches out to `https://zed.dev/api/releases/...`.
Patch this URL to point at our update server. Pros: zero user
interaction, works exactly like upstream. Cons: requires another patch
in the train, and we're now responsible for serving updates 24/7.

#### Option B — Homebrew tap

Ship a `homebrew-zed-yolo` tap. User runs `brew install zed-yolo` once;
`brew upgrade` pulls new versions. Pros: industry-standard; users already
trust it. Cons: doesn't auto-update silently.

#### Option C — Standalone updater binary

A tiny `zed-yolo-update` CLI that compares the local Zed version with
the GH Releases of the patches repo and downloads + replaces in
`/Applications/`. Mimics what tools like `tldr-pages` do. Pros: simple,
debuggable. Cons: yet another binary to ship.

**Recommendation**: B (Homebrew tap) for v1; C (standalone updater) once
mileage demands it. B is much faster to ship and easier to maintain than
patching Zed's in-app updater on day one.

### 6.3 Work items (Option B path)

- [ ] Set up `homebrew-zed-yolo` GitHub repo with a Cask.
- [ ] Cask formula points at GH Releases of `zed-yolo-patches` (or a
      separate `zed-yolo-distribution` repo).
- [ ] Each release contains:
      - `Zed-YOLO-aarch64-VERSION.zip`
      - `Zed-YOLO-x86_64-VERSION.zip`
      - SHA256 manifest
      - Release notes (auto-generated from upstream Zed changelog +
        patch-train delta)
- [ ] Tag automation: when the train CI succeeds for an upstream tag,
      automatically draft a GH Release and trigger the build pipeline.

### 6.4 Exit criteria

- `brew install --cask zed-yolo` works on a fresh machine.
- `brew upgrade --cask zed-yolo` picks up new versions within 24h of
  upstream Zed.
- Users see the YOLO behavior immediately, no extra config.

---

## 7. Milestone 7 — Documentation + Migration

### 7.1 Goal

A clear story for current users of `zed-yolo-hook` (the dylib) on how to
move to the new system, and clear docs for new users.

### 7.2 Work items

- [ ] Update `zed-yolo-hook/README.md`:
      - Mark dylib path as the "v1 / fallback" approach.
      - Link to new repos for "v2 / preferred" approach.
      - Decision matrix: when to use which.
- [ ] Write `zed-yolo-distribution/README.md` (or wherever the project
      lands its public face): install instructions, FAQ, security notes.
- [ ] Update `docs/01_yolo_background.md` and `02_yolo_quickstart.md`
      with pointers to the v2 quickstart.
- [ ] Migration guide: how to uninstall the dylib (run `cargo patch
      restore`) before installing the new app.
- [ ] Security note (mandatory): YOLO mode disables a real safety
      barrier; document the threat model and who shouldn't use it.

### 7.3 Exit criteria

- A user with no prior knowledge can read the README and have a working
  YOLO Zed in 10 minutes.
- A power user upgrading from the dylib can do so in 2 minutes (uninstall
  dylib → brew install new app).

---

## 8. Open Questions to Resolve Early

These are the items most likely to bite us if not decided up-front.

### 8.1 Trademark / branding

Zed's logo and name are trademarked. Our forked binary cannot ship as
"Zed" — it must be "Zed YOLO" or similar. This affects:

- App bundle name (`Zed YOLO.app`)
- `Info.plist` `CFBundleName`
- Update endpoint URL

Audit `script/bundle-mac` for any hard-coded "Zed" strings that need
search-and-replace and add those edits to the patch train.

### 8.2 Apple Developer Program seat

For signing/notarizing we need an Apple Developer Program enrollment.
$99/year. Use a personal developer ID for personal use, or a corporate
one if this is an org effort.

### 8.3 Upstream contribution feasibility

Some patches in this stack can be shaped as **good citizen** changes that
upstream might accept if they are behind explicit opt-in settings and keep
the current defaults:

- Layer 1 patches (claude/codex env-var YOLO): plausible, but the Claude
  bypass mode is intentionally dangerous, so the upstreamable version
  should be explicit, env-gated, and off by default.
- Layer 2A (Zed `external_agents.default = "always_allow"`): closes
  a documented gap (`docs/src/ai/external-agents.md:275`). Merge
  probability depends on whether maintainers accept forwarding an
  always-allow policy to external ACP permission prompts.

If upstream accepts the patches, **we don't need a fork at all** —
users can just toggle a setting. Recommendation: open a tracking issue
on `zed-industries/zed` and `agentclientprotocol/claude-agent-acp` to
gauge interest before committing to a long-term fork.

### 8.4 Coexistence with Zed's auto-updater

Zed's auto-updater (`crates/auto_update/`) on macOS replaces the binary
in-place. If a user is on the YOLO fork and Zed's updater pulls a stock
binary on top of ours, we lose the patch. Solutions:

1. Patch the updater to point at our update endpoint (simplest;
   reversible since it's in our patch train).
2. Quit Zed before updating; use Homebrew's update flow exclusively.
3. Use a separate app bundle name (`Zed YOLO.app`) so Zed's updater
   never finds itself.

Recommendation: **#3** combined with **#1** as belt-and-suspenders.

---

## 9. Appendix — Cost Model

For someone running this entirely on rented infrastructure (vs. a home
Linux box):

| Item | $/month | Notes |
|------|---------|-------|
| 32-core Linux build server (Hetzner AX102, on-demand only) | $0–80 | Spin up only for builds |
| sccache S3/B2 storage | $1–5 | ~50GB cache, infrequent reads |
| Apple Developer Program | $8.30 ($99/yr) | Required |
| GitHub Actions (private repo, occasional builds) | $0–10 | Free tier likely sufficient |
| GH Releases / Pages for distribution | $0 | Free for public repos |
| **Total** | **$10–100/mo** | Mostly $10/mo if home server is used |

Compare to: a CI run on `namespace-profile-mac-large` is ~$0.50-1.00 per
build; if upstream Zed cuts a release every 2 weeks and we rebuild for
every release, that's only ~$2/mo on macOS-runner-as-a-service. So the
cost benefit only kicks in when iteration is frequent (i.e. while
developing the patch train) — which is exactly when we want it.

---

## 10. References

- `23_source_patch_strategy.md` — what to patch
- `24_remote_zed_yolo_strategy.md` — where to deploy
- `25_cross_compile_macos_strategy.md` — how to build
- `gh-zed-industries__zed/script/bundle-mac` — official build script
- `gh-zed-industries__zed/.github/workflows/release.yml` — CI graph
- [`indygreg/apple-platform-rs`](https://github.com/indygreg/apple-platform-rs)
  — `rcodesign` for Linux signing
- [Homebrew Cask documentation](https://docs.brew.sh/Cask-Cookbook) —
  for distribution
- Existing zed-yolo-hook docs `01-22` for prior-art context
