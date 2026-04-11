#!/usr/bin/env bash
# ci-verify.sh — Download Zed Preview, build hook, inject, launch, verify.
#
# Usage:
#   ./scripts/ci-verify.sh [ZED_TAG]
#
# If ZED_TAG is omitted, fetches the latest pre-release from zed-industries/zed.
# Exits 0 on success, 1 on verification failure.

set -euo pipefail

# ---- Configuration ----
ZED_REPO="zed-industries/zed"
ZED_APP_NAME="Zed Preview"
ZED_APP_PATH="/Applications/Zed Preview.app"
ZED_BINARY="$ZED_APP_PATH/Contents/MacOS/zed"
LOG_DIR="$HOME/Library/Logs/Zed"
LOG_PATTERN="$LOG_DIR/zed-yolo-hook.*.log"
VERIFY_TIMEOUT=30  # seconds to wait for hook init
DMG_NAME="Zed-aarch64.dmg"
MOUNT_POINT="/Volumes/Zed Preview"

# Success/failure markers (must match xtask health check)
SUCCESS_MARKERS=(
    "=== zed-yolo-hook v"
    "YOLO mode ACTIVE"
)
FAILURE_MARKERS=(
    "Cannot find"
    "attach failed"
)

# ---- Helpers ----
info()  { echo "::group::$1"; }
endg()  { echo "::endgroup::"; }
fail()  { echo "::error::$1"; exit 1; }
warn()  { echo "::warning::$1"; }

# ---- Step 1: Resolve Zed Version ----
info "Resolve Zed Preview version"

ZED_TAG="${1:-}"
if [ -z "$ZED_TAG" ]; then
    echo "No tag specified, finding latest pre-release..."
    ZED_TAG=$(gh release list -R "$ZED_REPO" --limit 20 \
        | grep -i "pre-release" \
        | head -1 \
        | awk '{print $1}')
    if [ -z "$ZED_TAG" ]; then
        fail "Could not find a pre-release tag"
    fi
fi

echo "Zed Preview tag: $ZED_TAG"

# Extract version from tag (v0.232.0-pre → 0.232.0)
ZED_VERSION="${ZED_TAG#v}"
ZED_VERSION="${ZED_VERSION%-pre}"
echo "Zed Preview version: $ZED_VERSION"
endg

# ---- Step 2: Download & Install Zed Preview ----
info "Download Zed Preview $ZED_TAG"

WORK_DIR=$(mktemp -d)
DMG_PATH="$WORK_DIR/$DMG_NAME"

echo "Downloading $DMG_NAME from release $ZED_TAG..."
gh release download "$ZED_TAG" -R "$ZED_REPO" -p "$DMG_NAME" -D "$WORK_DIR"

if [ ! -f "$DMG_PATH" ]; then
    fail "DMG not found at $DMG_PATH"
fi
echo "Downloaded: $(du -h "$DMG_PATH" | cut -f1)"
endg

info "Install Zed Preview"

# Unmount if already mounted
if [ -d "$MOUNT_POINT" ]; then
    hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null || true
fi

# Remove existing installation
if [ -d "$ZED_APP_PATH" ]; then
    echo "Removing existing Zed Preview..."
    rm -rf "$ZED_APP_PATH"
fi

echo "Mounting DMG..."
hdiutil attach "$DMG_PATH" -nobrowse -quiet

if [ ! -d "$MOUNT_POINT" ]; then
    # Try alternate mount point names
    MOUNT_POINT=$(find /Volumes -maxdepth 1 -name "Zed*" -type d 2>/dev/null | head -1)
    if [ -z "$MOUNT_POINT" ]; then
        fail "DMG mounted but could not find volume"
    fi
fi

echo "Copying to /Applications..."
cp -R "$MOUNT_POINT/$ZED_APP_NAME.app" "$ZED_APP_PATH"

echo "Detaching DMG..."
hdiutil detach "$MOUNT_POINT" -quiet 2>/dev/null || true

# Verify binary
if [ ! -f "$ZED_BINARY" ]; then
    fail "Zed binary not found at $ZED_BINARY"
fi

ARCH=$(file "$ZED_BINARY" | grep -o "arm64" || true)
if [ "$ARCH" != "arm64" ]; then
    warn "Binary may not be ARM64: $(file "$ZED_BINARY")"
fi
echo "Installed: $ZED_BINARY ($ARCH)"
endg

# ---- Step 3: Build Hook ----
info "Build zed-yolo-hook"

echo "Building dylib (release)..."
cargo build --release -p zed-yolo-hook 2>&1

DYLIB_PATH="target/release/libzed_yolo_hook.dylib"
if [ ! -f "$DYLIB_PATH" ]; then
    fail "Dylib not found at $DYLIB_PATH"
fi

DYLIB_ARCH=$(lipo -archs "$DYLIB_PATH" 2>/dev/null || file "$DYLIB_PATH")
echo "Built: $DYLIB_PATH ($DYLIB_ARCH)"
endg

# ---- Step 4: Inject Dylib ----
info "Inject dylib into Zed"

# Build insert-dylib tool if needed
TOOLS_DIR="target/tools/bin"
INSERT_DYLIB="$TOOLS_DIR/insert-dylib"
if [ ! -f "$INSERT_DYLIB" ]; then
    echo "Building insert-dylib..."
    mkdir -p "$TOOLS_DIR"
    cargo install insert-dylib --root "target/tools" 2>&1
fi

# Backup original
cp "$ZED_BINARY" "$ZED_BINARY.original"

# Get absolute dylib path
DYLIB_ABS=$(cd "$(dirname "$DYLIB_PATH")" && pwd)/$(basename "$DYLIB_PATH")

echo "Injecting $DYLIB_ABS → $ZED_BINARY"
"$INSERT_DYLIB" --weak --strip-codesig --all-yes \
    "$DYLIB_ABS" "$ZED_BINARY" "$ZED_BINARY"

# Re-sign
echo "Ad-hoc codesigning..."
codesign -fs - --deep "$ZED_APP_PATH"

# Verify injection
if otool -L "$ZED_BINARY" | grep -q "libzed_yolo_hook"; then
    echo "Injection verified via otool"
else
    fail "Dylib not found in binary after injection"
fi
endg

# ---- Step 5: Launch & Verify ----
info "Launch Zed and verify hooks"

# Clear old logs
rm -f $LOG_PATTERN 2>/dev/null || true
mkdir -p "$LOG_DIR"

# Set hook config for CI
export ZED_YOLO_MODE="allow_all"
export ZED_YOLO_LOG="debug"

echo "Launching Zed Preview..."
open "$ZED_APP_PATH"

# Wait for log file to appear
echo "Waiting for hook log (up to ${VERIFY_TIMEOUT}s)..."
ELAPSED=0
LOG_FILE=""
while [ $ELAPSED -lt $VERIFY_TIMEOUT ]; do
    LOG_FILE=$(ls -t $LOG_PATTERN 2>/dev/null | head -1 || true)
    if [ -n "$LOG_FILE" ] && [ -f "$LOG_FILE" ]; then
        break
    fi
    sleep 1
    ELAPSED=$((ELAPSED + 1))
done

if [ -z "$LOG_FILE" ] || [ ! -f "$LOG_FILE" ]; then
    # Check for crash
    CRASH=$(find "$HOME/Library/Logs/DiagnosticReports" -name "zed*" -newer "$ZED_BINARY.original" 2>/dev/null | head -1 || true)
    if [ -n "$CRASH" ]; then
        echo "::error::Zed crashed on launch. Crash report: $CRASH"
        cat "$CRASH" | head -50
    fi
    fail "No hook log file appeared within ${VERIFY_TIMEOUT}s"
fi

echo "Log file: $LOG_FILE"

# Poll for success markers
PASS=true
for marker in "${SUCCESS_MARKERS[@]}"; do
    FOUND=false
    for _ in $(seq 1 $VERIFY_TIMEOUT); do
        if grep -q "$marker" "$LOG_FILE" 2>/dev/null; then
            FOUND=true
            break
        fi
        sleep 1
    done
    if $FOUND; then
        echo "  PASS: found \"$marker\""
    else
        echo "  FAIL: missing \"$marker\""
        PASS=false
    fi
done

# Check failure markers
for marker in "${FAILURE_MARKERS[@]}"; do
    if grep -q "$marker" "$LOG_FILE" 2>/dev/null; then
        echo "  FAIL: found failure marker \"$marker\""
        PASS=false
    fi
done

# Kill Zed
echo "Stopping Zed..."
pkill -f "Zed Preview" 2>/dev/null || true
sleep 2

endg

# ---- Step 6: Report ----
info "Results"

echo "--- Hook Log (last 40 lines) ---"
tail -40 "$LOG_FILE" 2>/dev/null || echo "(no log)"
echo "--- End Log ---"

# Clean up
rm -rf "$WORK_DIR"

if $PASS; then
    echo ""
    echo "VERIFICATION PASSED: zed-yolo-hook works with Zed Preview $ZED_VERSION ($ZED_TAG)"
    echo ""
    endg
    exit 0
else
    echo ""
    echo "VERIFICATION FAILED: zed-yolo-hook does NOT work with Zed Preview $ZED_VERSION ($ZED_TAG)"
    echo ""
    echo "Full log:"
    cat "$LOG_FILE" 2>/dev/null || true
    endg
    exit 1
fi
