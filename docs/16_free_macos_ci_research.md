# Free macOS CI/CD Environments for Post-Release Verification

Research date: 2026-04-11

## Goal

Find a free macOS ARM64 environment to automatically verify that `zed-yolo-hook`
works with each new Zed Preview release. The workflow needs to:

1. Build the Rust `cdylib` targeting `aarch64-apple-darwin`
2. Download the latest Zed Preview `.dmg` from GitHub Releases
3. Inject the dylib via `insert_dylib` / `dylib-patcher`
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

### 1. GitHub Actions (macOS hosted runners) -- RECOMMENDED

| Feature | Detail |
|---------|--------|
| **Free tier** | Unlimited minutes for **public repositories** |
| **Runner labels** | `macos-14`, `macos-15`, `macos-26` (all ARM64/M1) |
| **Hardware** | 3-core M1, 7 GB RAM, 14 GB SSD |
| **GUI session** | Partial -- `open` command works to launch `.app` bundles; process starts and dylib loads. No full Aqua interactive session, but sufficient for our use case (hook init writes to log immediately via `#[ctor]`). |
| **Rust** | Pre-installed on macOS runners via `rustup` |
| **Codesigning** | `codesign --force --deep -s -` works (ad-hoc) |
| **Trigger** | `workflow_dispatch`, `schedule`, `repository_dispatch` |
| **Nested virt** | Not supported (Apple limitation) -- not needed |
| **Storage** | 500 MB artifacts, 10 GB cache (Free plan) |
| **Multiplier** | 10x Linux minutes for private repos, but **free for public** |

**Verdict: Best option.** Free, ARM64, sufficient for our verification workflow.
The key insight: we don't need a full GUI session. The hook initializes via
`#[ctor]` during dylib load, before any GUI rendering. We just need the Zed
process to start, wait for log markers, then kill it.

**Limitations:**
- ARM64 runners have no nested virtualization
- No static UDID (irrelevant for us -- no iOS signing needed)
- 14 GB SSD is tight if caching many builds; use GitHub Actions cache

### 2. Codemagic

| Feature | Detail |
|---------|--------|
| **Free tier** | 500 free macOS M2 minutes/month, refilled monthly |
| **Hardware** | Mac mini M2 |
| **Max build** | 120 minutes |
| **Cache** | 3 GB per build |
| **Parallel** | 1 concurrent build on free tier |
| **Non-mobile** | Supported via `codemagic.yaml` (not just Flutter/mobile) |

**Verdict: Viable backup.** 500 min/month is generous for periodic verification.
However, Codemagic is primarily mobile-focused and the ecosystem tooling assumes
Flutter/iOS/Android. Setting up a pure Rust build is possible but less ergonomic
than GitHub Actions. The `codemagic.yaml` supports arbitrary shell scripts.

**Limitation:** Requires signup, less community documentation for non-mobile.

### 3. CircleCI

| Feature | Detail |
|---------|--------|
| **Free tier** | 6,000 credits/month (macOS consumes credits faster) |
| **macOS** | M1-based VMs available |
| **ARM64** | Supported |

**Verdict: Possible but credit-hungry.** macOS builds consume ~50 credits/min
vs 10/min for Linux. 6,000 credits = ~120 macOS minutes. Usable but tight
for frequent verification runs.

### 4. Bitrise

| Feature | Detail |
|---------|--------|
| **Free tier** | Limited free trial, then paid |
| **Hardware** | M1, M1 Max, M4 Pro |

**Verdict: Not suitable.** Free tier is a trial, not ongoing. Mobile-focused.

### 5. Semaphore CI

| Feature | Detail |
|---------|--------|
| **Free tier** | Free for open-source projects |
| **macOS** | Available |

**Verdict: Possible.** Less documentation on macOS ARM64 specifics. Would need
investigation into whether M1/M2 runners are available on free tier.

### 6. Self-hosted Runner (own Mac)

| Feature | Detail |
|---------|--------|
| **Cost** | Free (own hardware) |
| **Full control** | Yes -- full Aqua session, any macOS version |
| **Maintenance** | Manual -- must keep runner online |

**Verdict: Good for development, not for automated CI.** Requires always-on Mac.

## Decision Matrix

| Platform | Free | ARM64 | GUI | Rust | Trigger | Effort | Score |
|----------|------|-------|-----|------|---------|--------|-------|
| **GitHub Actions** | Unlimited (public) | M1 | Partial | Pre-installed | Native | Low | **Best** |
| Codemagic | 500 min/mo | M2 | Full | Installable | Webhook | Medium | Good |
| CircleCI | ~120 min/mo | M1 | Partial | Installable | Webhook | Medium | OK |
| Semaphore | OSS free | TBD | TBD | TBD | Webhook | High | Unknown |
| Self-hosted | Free (HW) | Any | Full | Any | Manual | High | Dev only |

## Recommendation

**Use GitHub Actions with `macos-15` runner.** Reasons:

1. **Free and unlimited** for public repositories
2. **ARM64 M1** matches our target architecture
3. **Rust pre-installed** -- no setup overhead
4. **Native GitHub integration** -- can trigger on Zed releases via `repository_dispatch` or scheduled cron
5. **Minimal setup** -- single workflow YAML file
6. **Community standard** -- most Rust projects use it

The only consideration is that the repo must be **public** for free macOS minutes.
For a private repo, macOS minutes cost 10x Linux (but the Free plan includes
2,000 Linux-equivalent minutes = 200 macOS minutes/month).

## References

- [GitHub-hosted runners reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
- [GitHub Actions billing](https://docs.github.com/en/billing/managing-billing-for-github-actions/about-billing-for-github-actions)
- [GitHub Actions runner pricing](https://docs.github.com/en/billing/reference/actions-runner-pricing)
- [macOS-26 GA announcement](https://github.blog/changelog/2026-02-26-macos-26-is-now-generally-available-for-github-hosted-runners/)
- [ARM64 runners for private repos](https://github.blog/changelog/2026-01-29-arm64-standard-runners-are-now-available-in-private-repositories/)
- [Codemagic pricing](https://codemagic.io/pricing/)
- [CircleCI pricing](https://circleci.com/pricing/)
- [free-for.dev](https://free-for.dev/)
- [Accessing macOS GUI in automation](https://aahlenst.dev/blog/accessing-the-macos-gui-in-automation-contexts/)
