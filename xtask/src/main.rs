//! xtask — patch/restore/verify/config workflow for zed-yolo-hook.
//!
//! Uses `dylib-patcher` SDK for all injection, verification, and config mechanics.
//!
//! Usage:
//!   cargo patch                          Build + inject into Zed Preview
//!   cargo patch --verify                 Build + inject + launch + verify hooks
//!   cargo patch verify                   Launch app + check hook health
//!   cargo patch status                   Show injected hooks + registry
//!   cargo patch remove                   Remove this hook only
//!   cargo patch restore                  Restore original binary (all hooks)
//!   cargo patch config                   Show current YOLO config + available options
//!   cargo patch config set KEY VALUE     Set a config field
//!   cargo patch config reset             Reset config to defaults
//!   cargo patch config path              Print config file path

use dylib_hook_registry::{HealthCheck, HookEntry};
use dylib_patcher::{ConfigField, HookConfigMeta, HookProject, Patcher, TargetApp};

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    let config_meta = HookConfigMeta::new(
        "zed-yolo-hook.json",
        r#"{"mode":"allow_all","tool_option":"allow","plan_option":"acceptEdits","log_level":"info","retry_delay_us":1500}"#,
    )
    .with_field(
        ConfigField::new("mode", "Which hooks to install")
            .with_option("allow_all", "Both ACP + native hooks (auto-approve everything)")
            .with_option("allow_safe", "ACP hook only (native permissions follow Zed settings)")
            .with_option("disabled", "Dylib loads but installs no hooks")
            .with_default("allow_all"),
    )
    .with_field(
        ConfigField::new("tool_option", "What to send for regular tool permission dialogs")
            .with_option("allow", "AllowOnce — one-time approval per tool call")
            .with_option("allow_always", "AllowAlways — persistent rule, stops asking for this tool type")
            .with_default("allow"),
    )
    .with_field(
        ConfigField::new("plan_option", "What to send for ExitPlanMode / \"Ready to code?\" dialog")
            .with_option("acceptEdits", "Auto-accept file edits, prompt for commands")
            .with_option("bypassPermissions", "Bypass all checks (has hallucination bug zed#48992)")
            .with_option("default", "Manual approval for each edit")
            .with_option("plan", "Stay in plan mode (reject ExitPlanMode)")
            .with_default("acceptEdits"),
    )
    .with_field(
        ConfigField::new("log_level", "Tracing filter level")
            .with_options(&["trace", "debug", "info", "warn", "error"])
            .with_default("info"),
    )
    .with_field(
        ConfigField::new("retry_delay_us", "Microseconds to wait before retry on miss (0-10000)")
            .with_default("1500"),
    );

    let project = HookProject::new("zed-yolo-hook", "libzed_yolo_hook.dylib")
        .with_crate_name("zed-yolo-hook")
        .with_config(config_meta)
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
