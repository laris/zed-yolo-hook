# zed-yolo-hook: Background

> Date: 2026-02-27
> Scope: Zed (macOS arm64) binaries, no source rebuild

---

## What Problem This Solves

External Agent Client Protocol (ACP) agents (Claude, Codex, Gemini, etc.) running inside Zed can trigger an approval UI for *every* tool call. The friction is high enough that it effectively disables autonomous workflows.

This project implements a "YOLO mode" for Zed: automatically approve tool-call permission prompts so the agent can proceed without manual clicks.

---

## What This Project Is

- A Rust `cdylib` (`libzed_yolo_hook.dylib`) injected into Zed's `zed` binary.
- No Zed source code required: it targets symbols in the shipped binary.
- The injection workflow is packaged as an `xtask` (exposed as `cargo patch`).

---

## How It Works (High Level)

The dylib initializes via `#[ctor]` when loaded, then uses Frida Gum interception to install hooks.

Two independent code paths are hooked:

1. Built-in tool permissions (`ToolPermissionDecision::from_input`).
   This affects Zed's native tool permission decisions.

2. ACP agent tool authorization (`AcpThread::request_tool_call_authorization`).
   This is the external-agent dialog path. The hook finds the stored oneshot `respond_tx` and sends the option id `"allow"` asynchronously.

All activity is logged to `~/Library/Logs/Zed/zed-yolo-hook.*.log`.

---

## Compatibility Notes

- Target platform: macOS aarch64 (Apple Silicon). The current offsets and ABI assumptions are specific to that target.
- Zed updates can change internal layouts and symbol names. Use the upgrade guide to re-calibrate when needed.

---

## Safety / Disclaimer

This project intentionally removes an important safety barrier.

- Treat the patched Zed binary as untrusted: any agent with tool access can execute actions without interactive confirmation.
- Keep a rollback path handy (`cargo patch restore`).
- Prefer using a dedicated test machine or a separate Zed install when experimenting.

---

## Docs Map

- `02_yolo_quickstart.md` - build, inject, restore, verify
- `03_yolo_research.md` - why this hook target is required
- `04_yolo_design.md` - architecture and design notes
- `05_yolo_implementation_log.md` - version history and methodology
- `06_yolo_upgrade_guide.md` - re-hooking after Zed updates
