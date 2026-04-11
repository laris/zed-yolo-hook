# Free macOS CI/CD Environments for Post-Release Verification

Research date: 2026-04-11
Status: **IMPLEMENTED AND VERIFIED** (GitHub Actions, first successful run 2026-04-11)

## Goal

Find a free macOS ARM64 environment to automatically verify that `zed-yolo-hook`
works with each new Zed Preview release. The workflow needs to:

1. Build the Rust `cdylib` targeting `aarch64-apple-darwin`
2. Download the latest Zed Preview `.dmg` from GitHub Releases
3. Inject the dylib via `insert_dylib`
4. Launch Zed briefly and verify hooks initialized (check log markers)
5. Report pass/fail

## Evaluation Criteria

| Criterion | Required | Nice-to-have |
|-----------|----------|-------------|
| ARM64 (Apple Silicon) | Yes (hook targets aarch64) | |
| macOS 14+ | Yes (Zed requires it) | macOS 15/26 |
| Free for public repos | Yes | |
| Can launch `.app` bundles | Yes (`open -a`) | |
| Rust toolchain available | Yes (or installable) | Pre-installed |
| GUI session (Aqua) | Partial (see notes) | Full Aqua |
| API/CLI triggerable | Yes | Webhook on release |

## Candidates Evaluated

### 1. GitHub Actions (macOS hosted runners) -- SELECTED

| Feature | Detail |
|---------|--------|
| **Free tier** | Unlimited minutes for **public repositories** |
| **Runner labels** | `macos-14`, `macos-15`, `macos-26` (all ARM64/M1) |
| **Hardware** | 3-core M1, 7 GB RAM, 14 GB SSD |
| **GUI session** | Auto-login enabled. GitHub's runner images provision auto-login via `configure-autologin.sh` and `setAutoLogin.sh` (found in `actions/runner-images` repo: `images/macos/scripts/build/configure-autologin.sh` and `images/macos/assets/bootstrap-provisioner/setAutoLogin.sh`). WindowServer runs with a logged-in user. `open -a` successfully launches `.app` bundles. |
| **Rust** | Pre-installed on macOS runners via `rustup` (stable toolchain) |
| **Codesigning** | `codesign -fs - --deep` works for ad-hoc signing |
| **Trigger** | `workflow_dispatch`, `schedule`, `push`, `repository_dispatch` |
| **Nested virt** | Not supported (Apple Virtualization Framework limitation) -- not needed |
| **Storage** | 500 MB artifacts, 10 GB cache (Free plan) |
| **Multiplier** | macOS costs $0.062/min (~10x Linux at $0.006/min) for private repos, but **completely free for public repos** |

**Verdict: Best option.** Free, ARM64, confirmed working.

Key insight discovered during implementation: we don't need a full interactive
Aqua session. The hook initializes via `#[ctor]` during dylib load, before any
GUI rendering. The Zed process starts via `open`, the dylib constructor fires,
Frida attaches hooks, writes to log. All within ~200ms of process launch.

**Confirmed working:** First successful CI run on 2026-04-11 (run ID 24283318357).
Zed Preview v0.232.0-pre launched on `macos-15` runner, all 6 hooks installed,
`YOLO mode ACTIVE` logged, total job time 1m30s.

**Limitations:**
- ARM64 runners have no nested virtualization (Apple limitation)
- No static UDID (irrelevant -- no iOS signing needed)
- 14 GB SSD is tight; use GitHub Actions cache for cargo registry/target
- Zed spawns multiple processes (3 pids observed in CI log), all get hooked
- The `open` command returns immediately; must poll log file for readiness

### 2. Codemagic

| Feature | Detail |
|---------|--------|
| **Free tier** | 500 free macOS M2 minutes/month, refilled monthly |
| **Hardware** | Mac mini M2 |
| **Max build** | 120 minutes per build |
| **Cache** | 3 GB per build |
| **Parallel** | 1 concurrent build on free tier |
| **Non-mobile** | Supported via `codemagic.yaml` (not just Flutter/mobile) |
| **Signup** | Requires account creation |

**Verdict: Viable backup.** 500 min/month is generous for periodic verification.
However, Codemagic is primarily mobile-focused and the ecosystem tooling assumes
Flutter/iOS/Android. Setting up a pure Rust build is possible but less ergonomic
than GitHub Actions. The `codemagic.yaml` supports arbitrary shell scripts.

**Not selected** because GitHub Actions is simpler, has better integration with
our GitHub-hosted repo, and has no monthly minute cap for public repos.

### 3. CircleCI

| Feature | Detail |
|---------|--------|
| **Free tier** | 6,000 credits/month |
| **macOS** | M1-based VMs available |
| **ARM64** | Supported |
| **macOS credit cost** | ~50 credits/min vs 10/min for Linux |
| **Effective minutes** | ~120 macOS minutes/month on free tier |

**Verdict: Possible but credit-hungry.** The 6,000 credits translate to only
~120 macOS minutes. Our workflow takes ~1.5 minutes, so ~80 runs/month. Usable
but tight for frequent verification runs, and no headroom.

### 4. Bitrise

| Feature | Detail |
|---------|--------|
| **Free tier** | Limited free trial, then paid |
| **Hardware** | M1, M1 Max, M4 Pro |
| **Focus** | Mobile CI/CD |

**Verdict: Not suitable.** Free tier is a trial, not ongoing. Mobile-focused.

### 5. Semaphore CI

| Feature | Detail |
|---------|--------|
| **Free tier** | Free for open-source projects |
| **macOS** | Available |
| **ARM64** | Undocumented for free tier |

**Verdict: Possible.** Less documentation on macOS ARM64 specifics. Open-source
projects get free parallel CI. Would need investigation.

### 6. Self-hosted Runner (own Mac)

| Feature | Detail |
|---------|--------|
| **Cost** | Free (own hardware) |
| **Full control** | Yes -- full Aqua session, any macOS version |
| **Maintenance** | Manual -- must keep runner online |
| **GUI** | Full interactive session when configured as LaunchAgent |

**Verdict: Good for development, not for automated CI.** Requires always-on Mac.

## Decision Matrix

| Platform | Free | ARM64 | GUI | Rust | Trigger | Effort | Score |
|----------|------|-------|-----|------|---------|--------|-------|
| **GitHub Actions** | Unlimited (public) | M1 | Auto-login | Pre-installed | Native | Low | **Best** |
| Codemagic | 500 min/mo | M2 | Full | Installable | Webhook | Medium | Good |
| CircleCI | ~120 min/mo | M1 | Partial | Installable | Webhook | Medium | OK |
| Semaphore | OSS free | TBD | TBD | TBD | Webhook | High | Unknown |
| Self-hosted | Free (HW) | Any | Full | Any | Manual | High | Dev only |

## Implementation Details

### Repository Requirements

- **Public repo required** for free macOS minutes (`laris/zed-yolo-hook` is public)
- **Dependencies must be publicly accessible:**
  - `laris/dylib-kit` (git dependency, public)
  - `insert-dylib` v0.1.1 (crates.io)
  - `frida-gum` (git dependency, public, auto-downloads devkit)
  - `agent-client-protocol` =0.10.2 (crates.io)

### Runner Environment (Verified 2026-04-11)

The `macos-15` runner provides:
- **Architecture:** `arm64` (Apple M1)
- **macOS version:** 15.x (Sequoia)
- **User:** `runner` (home: `/Users/runner`)
- **Cargo home:** `/Users/runner/.cargo`
- **Git:** v2.53.0 (via Homebrew)
- **auto-login:** Enabled (confirmed by `actions/runner-images` provisioning scripts)
- **Rust stable:** Pre-installed via `rustup`

### Dependency Resolution on CI

Path dependencies were converted to git dependencies to work in CI:

```toml
# Before (local development only):
dylib-hook-registry = { path = "/Users/lqiao/dev/codes/dylib-kit/crates/dylib-hook-registry" }

# After (works everywhere):
dylib-hook-registry = { git = "https://github.com/laris/dylib-kit" }
```

Cargo resolves workspace member crates by name from git dependencies. Since
`dylib-kit` is a workspace with `dylib-hook-registry` and `dylib-patcher` as
members, both resolve automatically.

### Pitfalls Discovered During Implementation

1. **`nicokoch/insert-dylib` repo is dead.** The original GitHub repo no longer
   exists. Must use `cargo install insert-dylib` from crates.io (v0.1.1).

2. **`insert-dylib` in-place writing.** The two-argument form
   `insert-dylib <dylib> <binary> <binary>` fails with "Failed to create" on
   GitHub Actions because it tries to create a new file alongside the original.
   Use `--inplace` flag instead: `insert-dylib --inplace <dylib> <binary>`.

3. **Cargo `git` + `path` is not allowed.** You cannot combine `git = "..."` and
   `path = "crates/..."` in a single dependency spec. Cargo resolves workspace
   members by crate name automatically from git repos.

## Verification Levels

The CI workflow currently implements **Level 1** verification:

| Level | What it verifies | Status |
|-------|-----------------|--------|
| **1. Hook install** | Dylib loads, symbols found, hooks attached | **Implemented** |
| 2. Static offset check | Struct layouts match (parse Zed source) | Planned |
| 3. Live tool call | End-to-end approval (needs Anthropic API key) | Planned |

Level 1 catches the most common breakage mode: **symbol renames** after Zed
updates. It confirms all 6 hook points are found and attached. It does NOT
exercise the memory layout offsets (entry_size, status_offset, respond_tx_offset)
because no ACP agent is connected to trigger actual tool calls.

## References

- [GitHub-hosted runners reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
- [GitHub Actions billing](https://docs.github.com/en/billing/managing-billing-for-github-actions/about-billing-for-github-actions)
- [GitHub Actions runner pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing)
- [macOS-26 GA announcement](https://github.blog/changelog/2026-02-26-macos-26-is-now-generally-available-for-github-hosted-runners/)
- [ARM64 runners for private repos](https://github.blog/changelog/2026-01-29-arm64-standard-runners-are-now-available-in-private-repositories/)
- [Runner images auto-login provisioning](https://github.com/actions/runner-images) — `images/macos/scripts/build/configure-autologin.sh`
- [Accessing macOS GUI in automation](https://aahlenst.dev/blog/accessing-the-macos-gui-in-automation-contexts/)
- [Codemagic pricing](https://codemagic.io/pricing/)
- [CircleCI pricing](https://circleci.com/pricing/)
- [free-for.dev](https://free-for.dev/)
- [First successful CI run](https://github.com/laris/zed-yolo-hook/actions/runs/24283318357)
