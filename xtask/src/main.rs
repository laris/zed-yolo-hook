//! xtask — patch/restore/verify workflow for zed-yolo-hook.
//!
//! Uses `dylib-patcher` SDK for all injection and verification mechanics.
//!
//! Usage:
//!   cargo patch                     Build + inject into Zed Preview
//!   cargo patch --verify            Build + inject + launch + verify hooks
//!   cargo patch verify              Launch app + check hook health
//!   cargo patch status              Show injected hooks + registry
//!   cargo patch remove              Remove this hook only
//!   cargo patch restore             Restore original binary (all hooks)

use dylib_hook_registry::{HealthCheck, HookEntry};
use dylib_patcher::{HookProject, Patcher, TargetApp};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let project = HookProject::new("zed-yolo-hook", "libzed_yolo_hook.dylib")
        .with_crate_name("zed-yolo-hook")
        .with_registry_entry(
            HookEntry::new("zed-yolo-hook", "")
                .with_version(env!("CARGO_PKG_VERSION"))
                .with_features(&["yolo-mode", "auto-approve-tools"])
                .with_symbol(
                    "ToolPermissionDecision::from_input",
                    "attach",
                    "Auto-approve built-in tool calls",
                )
                .with_symbol(
                    "AcpThread::request_tool_call_authorization",
                    "attach",
                    "Auto-approve ACP agent tool calls",
                )
                .with_load_order(1)
                .with_log_path("~/Library/Logs/Zed/zed-yolo-hook.*.log")
                .with_health_check(
                    HealthCheck::new("~/Library/Logs/Zed/zed-yolo-hook.*.log")
                        .with_success("=== zed-yolo-hook v")
                        .with_success("YOLO mode ACTIVE")
                        .with_failure("Cannot find")
                        .with_failure("attach failed")
                        .with_timeout(15),
                ),
        );

    let target = TargetApp::from_args(&args);
    let project_root = project_root();
    let patcher = Patcher::new(project, target, project_root);

    dylib_patcher::cli::run(patcher)
}

fn project_root() -> std::path::PathBuf {
    let output = std::process::Command::new("cargo")
        .args(["locate-project", "--workspace", "--message-format=plain"])
        .output()
        .expect("failed to run cargo locate-project");
    let path = String::from_utf8(output.stdout).expect("invalid utf8");
    std::path::PathBuf::from(path.trim())
        .parent()
        .expect("no parent")
        .to_path_buf()
}
