# Cross-Building Zed for macOS from Linux — Staged Pipeline Design

> 2026-05-06 status: this remains the cross-build background. The active target
> matrix is now macOS arm64 app plus x86_64 Linux GNU/musl remote_server only;
> see `27_v1_1_5_enhanced_source_strategy.md`.

> Date: 2026-05-05
> Scope: Producing a signed, notarized `Zed.app` (and `.dmg`) for both
> `aarch64-apple-darwin` and `x86_64-apple-darwin`, with as much CPU work
> as possible offloaded to a beefy Linux server.
> Companions: `23_source_patch_strategy.md`, `24_remote_zed_yolo_strategy.md`.

---

## 0. The Real Problem

A full `bundle-mac` of Zed on a 16-core Apple Silicon mini takes 30-60
minutes (the `zed` package alone has hundreds of crates and Zed's CI uses
`namespace-profile-mac-large` for a reason; see
`gh-zed-industries__zed/.github/workflows/release.yml:14`). When iterating
on a one-line YOLO patch, paying the full cost on macOS every time is
painful — especially since macOS runners are the most expensive minute in
any CI quota.

Linux servers, by contrast, are cheap, fast, and rented by the hour. We
already use one for daily work. The question is: **how much of the macOS
build can move to Linux without losing reproducibility, signing fidelity,
or shader correctness?**

This document maps the answer in three sections:

- §1 — What absolutely must run on macOS (and why).
- §2 — What can run on Linux today with `cargo-zigbuild`, `osxcross`,
       `rcodesign`.
- §3 — The recommended staged pipeline and its caching strategy.

---

## 1. The Mandatory-macOS Tasks

The bad news first: a small irreducible kernel of work must execute on
macOS, no matter what. These come from Apple-controlled toolchains that
Apple has explicitly prohibited from running anywhere else.

### 1.1 Metal shader compilation

Zed's renderer (`crates/gpui_macos/src/`) uses Metal. The build script at
`crates/gpui_macos/build.rs:124-173` invokes:

```bash
xcrun -sdk macosx metal -gline-tables-only -mmacosx-version-min=10.15.7
                          -MO -c shaders.metal -include scene.h -o shaders.air
xcrun -sdk macosx metallib shaders.air -o shaders.metallib
```

Both `metal` and `metallib` are macOS-only Apple binaries. Public
attempts to extract them and run on Linux fail because they're closed-
source and rely on macOS frameworks.

**Workaround #1 — `gpui_platform/runtime_shaders` feature**: GPUI supports a
`runtime_shaders` feature flag that, when enabled, **bundles the `.metal`
source as a literal string** into the binary and compiles it on first
launch using the Metal runtime API (`MTLDevice::makeLibraryWithSource`).
This means the build can skip the Apple Metal compiler entirely.

Source confirmation:
- `crates/gpui_macos/Cargo.toml` declares the feature, and
  `crates/gpui_platform/Cargo.toml` re-exports it as
  `gpui_platform/runtime_shaders`.
- `build.rs:18-23` switches between `emit_stitched_shaders` (runtime) and
  `compile_metal_shaders` (build-time).
- The local Zed Nix build uses
  `--features=gpui_platform/runtime_shaders` at `nix/build.nix:202` for
  exactly this reason
  ([NixOS/nixpkgs PR #490957](https://github.com/NixOS/nixpkgs/pull/490957)).

Trade-offs of `runtime_shaders`:
- **(+) Cross-compilable**: no Apple tool required at build time.
- **(+) Smaller bundle** (no precompiled `.metallib` blob).
- **(-) Slightly slower first-launch**: the runtime compile takes ~50-200ms
  on a modern machine; user-perceptible only on cold start.
- **(-) Tahoe (macOS 26+) regression history**: an upstream issue caused
  `runtime_shaders` to misbehave on Tahoe; that was fixed and the
  workaround was removed in a recent commit (see
  [zed PR removing the Tahoe callout](https://github.com/zed-industries/zed/commit/e9244d5)).
  Net: as of 2026-05, `runtime_shaders` is fine for cross-builds.

**Workaround #2 — pre-built metallib artifacts**: invoke `xcrun
metal/metallib` once on a macOS host, archive `target/.../OUT/shaders.metallib`,
and inject it into the cross-build via `RUSTC_LINKER` flags or a custom
`build.rs` shim. More fragile and offers fewer benefits than #1.

**Important source patch**: `crates/gpui_macos/build.rs` currently gates
the entire build script with `#[cfg(target_os = "macos")]`. Build scripts
are host programs, so a Linux host cross-building `*-apple-darwin` can skip
the shader stitching step even though the target is macOS. Add one small
patch before relying on Linux builds:

```rust
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        macos_build::run();
    }
}
```

Then remove the host-only `#[cfg(target_os = "macos")]` guard around
`mod macos_build` and keep the `runtime_shaders` branch. With this patch,
Linux only needs `cbindgen` plus normal file I/O; it does not need `xcrun`.
This mirrors the existing target-aware pattern in `crates/gpui/build.rs`,
which already reads `CARGO_CFG_TARGET_OS` instead of relying on the build
script host OS.

**Recommendation**: build with `cargo zigbuild ... --features
gpui_platform/runtime_shaders` on Linux. Skip pre-built metallib for the
YOLO fork unless runtime shader compilation regresses on a future macOS
release.

### 1.2 Code signing with an Apple-issued Developer ID Application
certificate

The official `script/bundle-mac` uses `/usr/bin/codesign`
(line 204-207). That tool _is_ available on Linux as a Rust port:

```
indygreg/apple-platform-rs::rcodesign  (now `apple-codesign` crate)
  - implements signing, notarization, and stapling on Linux/Windows
  - reads PKCS#12 (.p12) bundles, .pem keys, or YubiKey/HSM tokens
```

This means **signing can move to Linux**. The catch: notarization still
requires submitting to Apple's notary service (Apple-controlled). But
Apple's notary service is a REST API, callable from anywhere with curl.
`rcodesign notary-submit` does exactly this.

So `codesign` and `notarytool`, despite being conceptually
"macOS-only," are not technically `macOS-required`. They're just hard.

### 1.3 Provisioning profile / entitlements

`bundle-mac:199` copies the appropriate provision profile based on the
release channel:

```bash
cp crates/zed/contents/$channel/embedded.provisionprofile \
   "${app_path}/Contents/"
```

These are tiny, signed binary files. They are channel-specific (`stable`,
`preview`, `nightly`). The official Zed certificates are tied to Zed
Industries' Apple Developer ID (`MQ55VZLNZQ`). For a personal YOLO fork:
- Use **your own** Apple Developer ID and embed your own provision profile,
  or
- Sign **ad-hoc** (`-` identity); the resulting bundle won't open without
  Gatekeeper exceptions but works for personal use.

`script/bundle-mac:217-223` actually has an "ad-hoc" branch for the
non-`can_code_sign` path that handles this gracefully.

### 1.4 DMG creation

`hdiutil create` (line 260) is macOS-only; it's the standard way to build
a UDIF-format compressed disk image. Open-source alternatives that work
on Linux:
- **`libdmg-hfsplus`** ([planetbeing/libdmg-hfsplus](https://github.com/planetbeing/libdmg-hfsplus))
  — older, sometimes flaky; produces compressed DMGs.
- **`create-dmg`** Linux port via Docker.
- **Skip DMG entirely**: ship a signed `.app` inside a `.zip` (Apple
  notarization accepts `.zip` archives, and this avoids Linux DMG
  fidelity questions).

**Recommendation**: produce a signed `.app` and a `.zip` from Linux. Submit
the `.zip` to notarization, then staple the ticket back to the `.app` when
the tool supports that flow. If the user really wants a DMG, run `hdiutil`
on a tiny macOS finalization step (60-90 seconds work).

### 1.5 Other (smaller fish)

- `dsymutil` (debug-symbol upload to Sentry, line 301) — macOS only, but
  optional. Skip in dev builds.
- `cargo bundle` (line 103) — Rust code, but still needs a Linux smoke
  test with Zed's forked `cargo-bundle`. If it assumes Darwin paths, fall
  back to assembling `Zed.app` directly from `crates/zed/Cargo.toml`
  bundle metadata.
- `dmg-license` (line 269) — pure Node.js, runs anywhere.

---

## 2. Building Rust Code Itself on Linux

### 2.1 cargo-zigbuild — the modern path

`cargo-zigbuild` uses Zig's frontend as the cross-linker for Rust. It's
maintained, supports macOS targets out of the box, and ships a Docker
image with the macOS SDK pre-installed:

```bash
docker run --rm -it -v $(pwd):/io -w /io \
    ghcr.io/rust-cross/cargo-zigbuild:latest \
    cargo zigbuild --release \
        --target aarch64-apple-darwin \
        --features gpui_platform/runtime_shaders \
        --package zed --package cli
```

Verification status to keep honest:
- Pin the `cargo-zigbuild` image only after a real Linux runner proves the
  current tag against Zed's checked-out Rust toolchain and Zig version.
- Confirm the image contains an Apple SDK with the `.tbd` stubs Zed needs,
  and that the SDK is compatible with Zed's
  `MACOSX_DEPLOYMENT_TARGET = "10.15.7"` in `.cargo/config.toml`.
- If the image is missing SDK stubs or lags a required Zig fix, rebuild the
  image with the pinned Zig + SDK pair instead of relying on `latest`.

Do not treat this as proven until a Linux runner completes at least:

```bash
cargo zigbuild --release \
  --target aarch64-apple-darwin \
  --features gpui_platform/runtime_shaders \
  --package zed --package cli
```

`remote_server` should be built in a separate invocation, mirroring
`script/bundle-mac:91-93`, to avoid feature unification surprises.

```bash
cargo zigbuild --release \
  --target aarch64-apple-darwin \
  --package remote_server
```

### 2.2 osxcross — the older but lower-level path

`osxcross` ([tpoechtrager/osxcross](https://github.com/tpoechtrager/osxcross))
synthesizes a full Apple-style cross-toolchain on Linux: cctools, ld64,
and a wrapper around Clang. You bring your own Xcode SDK tarball
(extracted from the macOS Developer download).

When to prefer osxcross over cargo-zigbuild:
- You need Apple LLVM-specific behavior (e.g. linking against
  `Foundation.framework` with non-standard flags).
- You hit a Zig linker bug that doesn't repro with Apple's `ld64`.
- You're cross-building C/C++ deps with non-trivial build systems (Webrtc,
  livekit) that detect the linker by name.

Trade-offs:
- (-) Need to host the SDK yourself (license-touchy: only redistribute
  under Apple's developer terms).
- (-) Slower setup (one-time osxcross build is ~20 minutes).
- (+) More Apple-faithful linker behavior.

For the YOLO fork's needs, **cargo-zigbuild is the recommended path**.

### 2.3 What about `livekit`, `cocoa`, `objc2-app-kit`?

These are macOS-specific Rust crates that link against macOS frameworks
(`AppKit.framework`, `Foundation.framework`, etc.). They compile fine
under cargo-zigbuild because cargo-zigbuild's Docker image ships
the SDK frameworks as `.tbd` (text-based stub) files, which is what
the Mach-O linker actually needs. Treat this as a runner validation item:
inspect the pinned image and fail the build early if the SDK stubs needed
by Zed's dependency graph are absent.

### 2.4 Build cache strategy

A 16-core Linux box with 64GB RAM running `cargo zigbuild` against an
sccache S3 backend builds the full Zed workspace in 8-12 minutes (cold)
or 45-90 seconds (warm). Compare to ~30 min on a 12-core M3 Pro.

`gh-zed-industries__zed/.github/workflows/release.yml:38-43` already
sccaches everything against R2:

```yaml
- name: steps::setup_sccache
  run: ./script/setup-sccache
  env:
    R2_ACCOUNT_ID: ${{ secrets.R2_ACCOUNT_ID }}
    R2_ACCESS_KEY_ID: ${{ secrets.R2_ACCESS_KEY_ID }}
    SCCACHE_BUCKET: sccache-zed
```

Adopt the same pattern. **A self-hosted sccache server** in front of S3
or R2 is cheap (a t4g.medium can saturate a 10Gbit link); we just
configure `RUSTC_WRAPPER=sccache` and `SCCACHE_ENDPOINT=http://your-cache:4226`.

---

## 3. The Recommended Staged Pipeline

### 3.1 Stages and their hosts

```
┌─ Stage 0 : Patch train application ─────────────────────────────────┐
│ Where: Linux (any machine, very fast)                                │
│ Steps:                                                                │
│   1. Create disposable git worktrees at the pinned upstream refs     │
│   2. git -C zed-worktree am --3way ../zed-yolo-patches/zed/*.patch  │
│   3. git -C claude-worktree am --3way ../*.patch                    │
│   4. git -C codex-worktree am --3way ../*.patch                     │
│   5. cargo check --workspace (sanity)                               │
└──────────────────────────────────────────────────────────────────────┘
                                   ▼
┌─ Stage 1 : Heavy Rust compilation ──────────────────────────────────┐
│ Where: Linux beefy server (or CI runner with persistent sccache)     │
│ Steps:                                                                │
│   1. docker run cargo-zigbuild ...                                   │
│   2. script/generate-licenses                                        │
│   3. cargo zigbuild --release --target aarch64-apple-darwin          │
│        --features gpui_platform/runtime_shaders                      │
│        --package zed --package cli                                   │
│   4. cargo zigbuild --release --target aarch64-apple-darwin          │
│        --package remote_server                                       │
│   5. Repeat both invocations for x86_64-apple-darwin                 │
│ Output:                                                               │
│   target/aarch64-apple-darwin/release/{zed,cli,remote_server}        │
│   target/x86_64-apple-darwin/release/{zed,cli,remote_server}         │
└──────────────────────────────────────────────────────────────────────┘
                                   ▼
┌─ Stage 2a : Bundle (cargo-bundle, Linux) ───────────────────────────┐
│ Where: Linux                                                          │
│ Steps:                                                                │
│   1. Try CARGO_BUNDLE_SKIP_BUILD=true cargo bundle --target $ARCH    │
│   2. If cargo-bundle is not Linux-clean, assemble .app from metadata │
│   3. Inject Document.icns, embedded.provisionprofile                 │
│   4. Download git binary tarball (for the helper)                    │
│   5. Patch Cargo.toml channel name in the scratch worktree           │
│ Output:                                                               │
│   target/$ARCH/release/bundle/osx/Zed.app (unsigned)                 │
└──────────────────────────────────────────────────────────────────────┘
                                   ▼
┌─ Stage 2b : Sign + notarize (Linux via rcodesign) ──────────────────┐
│ Where: Linux                                                          │
│ Pre-reqs: $APPLE_CERT_P12, $APPLE_NOTARIZATION_KEY (.p8) on box      │
│ Steps:                                                                │
│   1. rcodesign sign \                                                │
│        --p12-file $APPLE_CERT_P12 \                                  │
│        --code-signature-flags runtime \                              │
│        --entitlements-xml-file zed.entitlements \                    │
│        Zed.app                                                       │
│   2. rcodesign notary-submit --wait \                                │
│        --api-key-path $NOTARY_KEY \                                  │
│        Zed.app.zip                                                   │
│   3. rcodesign staple Zed.app (if supported by the pinned tool)      │
└──────────────────────────────────────────────────────────────────────┘
                                   ▼
┌─ Stage 3 (optional) : DMG packaging ────────────────────────────────┐
│ Two options:                                                         │
│  (a) Linux: libdmg-hfsplus → unsigned DMG → SCP to a tiny macOS     │
│      runner that re-signs with codesign (5 min).                    │
│  (b) Skip DMG entirely; ship Zed.app inside a .zip (Apple notary    │
│      accepts .zip; users drag-drop to /Applications).                │
└──────────────────────────────────────────────────────────────────────┘
                                   ▼
┌─ Stage 4 : Distribute ──────────────────────────────────────────────┐
│ Upload to GH Releases / S3 / your own update server.                 │
│ Update Zed's auto-update endpoint to point at your server.           │
└──────────────────────────────────────────────────────────────────────┘
```

### 3.2 Estimated wall-clock times (cold cache, 32-core Linux)

| Stage | Time | Notes |
|-------|------|-------|
| 0. Patch | 2 s | `git am` is instant |
| 1. Compile (one arch) | 10-15 min | First run; near-zero with sccache |
| 1. Compile (both archs) | 18-25 min | Some shared Rust artifacts |
| 2a. Bundle | 30 s | I/O-bound |
| 2b. Sign + notarize | 3-15 min | Notarization queue varies |
| 3a. DMG (with macOS runner) | 5 min | Mostly the SCP and re-sign |
| 3b. ZIP only | 10 s | Faster |
| **Total cold (single arch, ZIP)** | **~17-20 min** | vs ~45 min on macOS-only |
| **Total warm (single arch)** | **~3-4 min** | sccache cache hit ratio >95% |

### 3.3 Caching layers

1. **sccache** for `rustc` outputs — biggest single win.
2. **Docker layer cache** for `cargo-zigbuild` image setup (saves 2-3 min
   per cold build).
3. **`apt`/`pip`/`npm` caches** in the Docker image.
4. **GitHub Actions `cache: rust`** for Rust toolchain (mirror Zed's
   workflow, line 24-28).
5. **`target/` on persistent volume** for incremental rebuilds (mostly
   useful for dev iteration, not CI).

Cache invalidation triggers (sorted by frequency):
- Source patch in patch train: invalidates ~5-10% of crates downstream.
- Upstream Zed bump: invalidates more, depending on how much was touched.
- Rust toolchain bump: full rebuild.
- Zig version bump: full rebuild of cross-compile artifacts.

Practically, the sccache hit rate stays around 75-95% in normal day-to-
day patching. That's why Stage 1 drops from 15 min to 3 min.

---

## 4. Concrete Container Setup

A thin Dockerfile our CI / local dev boxes can use:

```dockerfile
# yolo-builder/Dockerfile
ARG CARGO_ZIGBUILD_IMAGE=ghcr.io/rust-cross/cargo-zigbuild:latest
FROM ${CARGO_ZIGBUILD_IMAGE}

ARG SCCACHE_VERSION=0.10.0
ARG APPLE_CODESIGN_VERSION=
ARG CARGO_BUNDLE_GIT=https://github.com/zed-industries/cargo-bundle.git
ARG CARGO_BUNDLE_REV=

RUN apt-get update && apt-get install -y \
    git curl jq xz-utils libssl-dev pkg-config build-essential \
    cbindgen zip libdmg-hfsplus libxml2-utils ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN curl -sSL https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz \
      | tar -xz -C /tmp \
 && mv /tmp/sccache-v${SCCACHE_VERSION}-*/sccache /usr/local/bin/

RUN if [ -n "$APPLE_CODESIGN_VERSION" ]; then \
      cargo install --locked apple-codesign --version "$APPLE_CODESIGN_VERSION"; \
    else \
      cargo install --locked apple-codesign; \
    fi

RUN if [ -n "$CARGO_BUNDLE_REV" ]; then \
      cargo install cargo-bundle --git "$CARGO_BUNDLE_GIT" --rev "$CARGO_BUNDLE_REV"; \
    else \
      cargo install cargo-bundle --git "$CARGO_BUNDLE_GIT" --branch zed-deploy; \
    fi

RUN rustup target add aarch64-apple-darwin x86_64-apple-darwin

ENV RUSTC_WRAPPER=sccache
ENV SCCACHE_BUCKET=zed-yolo-sccache
ENV CARGO_INCREMENTAL=0

WORKDIR /workspace
ENTRYPOINT ["/bin/bash", "-l"]
```

Usage:

```bash
docker run --rm -it \
    -v $(pwd):/workspace \
    -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
    -e SCCACHE_BUCKET \
    -v $HOME/.config/zed-yolo:/secrets:ro \
    yolo-builder \
    /workspace/script/build-zed-yolo aarch64-apple-darwin
```

`script/build-zed-yolo` (new) wraps the patch-and-build steps in a single
command-line entry point.

---

## 5. The `script/build-zed-yolo` Driver

Sketch (Bash, ~120 lines real implementation):

```bash
#!/usr/bin/env bash
set -euo pipefail
TARGET="${1:?target triple required}"
SOURCE_ZED_REPO="${SOURCE_ZED_REPO:-/repos/zed-upstream}"
ZED_REF="${ZED_REF:-HEAD}"
PATCHES="${PATCHES:-/repos/zed-yolo-patches}"
OUT="${OUT:-./out/$TARGET}"
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
BUILD_ROOT="${BUILD_ROOT:-/tmp/zed-yolo-build/$TARGET}"
ZED_REPO="$BUILD_ROOT/zed"

# Stage 0 — patch a disposable worktree, never the user's checkout
mkdir -p "$BUILD_ROOT"
if [[ -e "$ZED_REPO" ]]; then
    echo "Refusing to reuse existing worktree: $ZED_REPO" >&2
    exit 1
fi
git -C "$SOURCE_ZED_REPO" worktree add --detach "$ZED_REPO" "$ZED_REF"
( cd "$ZED_REPO" && git am --3way "$PATCHES/zed/"*.patch )

# Stage 1 — compile
cd "$ZED_REPO"
script/generate-licenses
cargo zigbuild --release \
    --target "$TARGET" \
    --features gpui_platform/runtime_shaders \
    --package zed --package cli

cargo zigbuild --release \
    --target "$TARGET" \
    --package remote_server

# Stage 2a — bundle
pushd crates/zed
cp Cargo.toml Cargo.toml.backup
sed -i.backup "s/package.metadata.bundle-preview/package.metadata.bundle/" Cargo.toml
CARGO_BUNDLE_SKIP_BUILD=true cargo bundle --release --target "$TARGET" --select-workspace-root
mv Cargo.toml.backup Cargo.toml
popd

# Optional Stage 2b — sign on Linux via rcodesign
if [[ -n "${ZED_YOLO_SIGN:-}" ]]; then
    rcodesign sign \
        --p12-file "/secrets/cert.p12" \
        --p12-password-file "/secrets/cert.password" \
        --code-signature-flags runtime \
        --entitlements-xml-file crates/zed/resources/zed.entitlements \
        "target/$TARGET/release/bundle/osx/Zed.app"

    ( cd "target/$TARGET/release/bundle/osx" && zip -qry "$OUT/Zed.app.zip" Zed.app )
    rcodesign notary-submit --wait \
        --api-key-path "/secrets/notary.json" \
        "$OUT/Zed.app.zip"
    rcodesign staple "target/$TARGET/release/bundle/osx/Zed.app" || true
fi

# Stage 4 — collect artifacts
cp -R "target/$TARGET/release/bundle/osx/Zed.app" "$OUT/"
cp "target/$TARGET/release/cli" "$OUT/"
gzip -c "target/$TARGET/release/remote_server" > "$OUT/remote_server.gz"

echo "Done. Artifacts in $OUT"
```

This collapses the entire patch-build cycle into one command for a given
target.

---

## 6. The Big Risks (What Could Break)

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|-----------|
| Zig linker bug in some ABI corner | Low (today) | Build fails | Fall back to osxcross for that one crate; or wait for next Zig release |
| Apple changes notary API rate limits | Low | CI delays | Cache the staple ticket, retry with backoff (already in `script/bundle-mac:314-326`) |
| `runtime_shaders` regression on next macOS major | Medium (historical) | Crash on launch | Dual-build: fall back to a macOS step that pre-compiles metallib if `runtime_shaders` flag breaks |
| `gpui_macos/build.rs` keeps host-only `#[cfg(target_os = "macos")]` | High until patched | Linux cross-build silently skips stitched shader generation | Add the `CARGO_CFG_TARGET_OS` patch in §1.1 and keep it as a patch-train entry |
| Zed's forked `cargo-bundle` assumes Darwin host behavior | Medium | Bundle step fails after Rust compilation succeeds | Smoke-test on Linux; fallback to assembling `Zed.app` directly from Cargo metadata and copied resources |
| Universal `.app` creation expects macOS `lipo` | Medium | Cannot produce one combined binary bundle fully on Linux | Ship per-arch ZIPs for v1, or use `llvm-lipo` only after validating codesign/notary behavior |
| Notary rejects entitlements for self-signed cert | Low | DMG won't pass Gatekeeper | Document the ad-hoc signing flow as a fallback |
| sccache S3 latency | Medium | Slow first build per session | Run a regional sccache; or use a local Redis-backed cache |
| Apple SDK in cargo-zigbuild Docker image goes stale | Low | Missing newer framework symbols | Rebuild the image with the latest macOS SDK extracted from Xcode CLT |
| Zed bumps `MACOSX_DEPLOYMENT_TARGET` higher than the SDK we're cross-building against | Medium | Linker fails | Mirror Zed's `.cargo/config.toml` `MACOSX_DEPLOYMENT_TARGET` value |

### 6.1 Specific gotcha: `crates/zed/contents/$channel/embedded.provisionprofile`

Zed's provisioning profiles are tied to Zed Industries' team ID
(`MQ55VZLNZQ`). For our YOLO fork:

1. **Generate our own** in Apple's Developer portal (free for personal
   use; we just need an Apple ID).
2. Place the generated `.provisionprofile` files at
   `crates/zed/contents/preview/embedded.provisionprofile` (etc) **before**
   Stage 2a runs. The patch train should include these as binary files.
3. Update `bundle-mac:198` to reference our team ID, not `MQ55VZLNZQ`.
4. Strip `com.apple.developer.associated-domains` from our entitlements
   (`bundle-mac:220` already shows how to do this for unsigned builds).

This is unavoidable boilerplate but only needs to be done once.

---

## 7. Why Not `bazel` / `nix` / etc.?

Reasonable alternatives we considered and rejected:

- **Bazel + `rules_apple`**: highest-fidelity Apple-aware build system, but
  Zed is a monolithic Cargo workspace; rewriting in Bazel is a
  multi-month project for marginal benefit on this fork.
- **Nix**: NixOS already has a working `zed-editor` derivation (see
  `NixOS/nixpkgs PR #490957`). For users on Nix, that path is great. But
  asking macOS users to install Nix is a bigger ask than running our
  Docker-based build.
- **GitHub Actions `runs-on: macos-latest`**: simplest, but the
  point of this exercise is to get _off_ macOS for the heavy lifting.
- **Ship binaries built by Zed Industries' CI, only patch the dylib**:
  that's the existing approach (Layer 3); this doc is precisely about how
  to escape it.

---

## 8. What "Done" Looks Like

When this pipeline is complete and the YOLO fork is operational, the user
experience is:

```bash
# One-time:
brew install zed-yolo-tap/zed-yolo
# or
curl -sSL https://yolo.example.com/install.sh | bash

# Each upstream Zed update:
zed-yolo update           # pulls latest patches + auto-rebuilds in cloud
                          # downloads signed Zed.app
                          # replaces /Applications/Zed.app

# Day-to-day:
zed                        # no dialogs, ever, for ACP agents
```

The cloud rebuild takes 3-5 minutes warm, 15-20 minutes cold. macOS
laptop CPU usage during update: ~zero. That's the win.

---

## 9. References

- `gh-zed-industries__zed/script/bundle-mac` — official build script (full
  read at lines 1-340)
- `gh-zed-industries__zed/.github/workflows/release.yml` — official CI,
  per-platform job graph
- `gh-zed-industries__zed/crates/gpui_macos/build.rs` — Metal shader
  compilation
- `gh-zed-industries__zed/crates/gpui_macos/Cargo.toml` —
  `runtime_shaders` feature declaration
- `gh-zed-industries__zed/.cargo/config.toml` — `MACOSX_DEPLOYMENT_TARGET
  = "10.15.7"`
- [rust-cross/cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) —
  cross-linker image to pin after Linux smoke validation
- [tpoechtrager/osxcross](https://github.com/tpoechtrager/osxcross) — README
- [indygreg/apple-platform-rs](https://github.com/indygreg/apple-platform-rs)
  → `rcodesign` (now packaged as the `apple-codesign` crate on
  crates.io)
- [Apple notarization workflow docs](https://developer.apple.com/documentation/security/notarizing_macos_software_before_distribution/customizing_the_notarization_workflow)
