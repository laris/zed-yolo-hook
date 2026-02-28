//! zed-yolo-hook: YOLO mode for Zed.
//!
//! Auto-approves ALL tool call permission dialogs via two hooks:
//!
//! 1. `permission_decision` — hooks `ToolPermissionDecision::from_input`
//!    to always return `Allow` (built-in tools).
//!
//! 2. `tool_authorization` — hooks `AcpThread::request_tool_call_authorization`
//!    to auto-send "allow" through the oneshot channel (external ACP agents).
//!
//! In `ZED_YOLO_MODE=allow_safe`, only the ACP hook is enabled.

mod config;
mod ffi;
mod hooks;
mod logging;
mod symbols;

use ctor::ctor;
use frida_gum::{Gum, Process, interceptor::Interceptor};
use std::sync::OnceLock;

static GUM: OnceLock<Gum> = OnceLock::new();
static INIT_ONCE: std::sync::Once = std::sync::Once::new();

#[ctor]
fn init() {
    INIT_ONCE.call_once(init_inner);
}

fn init_inner() {
    let mode = config::YoloMode::from_env();

    logging::init();

    let pid = unsafe { libc::getpid() };
    tracing::info!("=== zed-yolo-hook v{} ===", env!("CARGO_PKG_VERSION"));
    tracing::info!("YOLO mode: {:?}, pid: {}", mode, pid);

    if !mode.is_enabled() {
        tracing::info!("YOLO disabled.");
        return;
    }

    let gum = GUM.get_or_init(|| Gum::obtain());
    let process = Process::obtain(gum);
    let main_module = process.main_module();
    let mut interceptor = Interceptor::obtain(gum);

    // -----------------------------------------------------------------------
    // Hook 1: permission_decision (native tool permissions)
    // -----------------------------------------------------------------------
    if matches!(mode, config::YoloMode::AllowAll) {
        if let Some((name, ptr)) = symbols::find_by_pattern(
            &main_module,
            hooks::permission_decision::SYMBOL_INCLUDE,
            hooks::permission_decision::SYMBOL_EXCLUDE,
        ) {
            tracing::info!("permission_decision: Found {} at {:?}", name, ptr);
            let mut listener = hooks::permission_decision::Listener;
            match interceptor.attach(ptr, &mut listener) {
                Ok(_) => {
                    std::mem::forget(listener);
                    tracing::info!("permission_decision: hook installed");
                }
                Err(e) => tracing::error!("permission_decision: attach failed: {:?}", e),
            }
        } else {
            tracing::warn!("permission_decision: from_input symbol not found");
        }
    } else {
        tracing::info!("permission_decision: skipped (ZED_YOLO_MODE=allow_safe)");
    }

    // -----------------------------------------------------------------------
    // Hook 2: tool_authorization (ACP agents)
    // -----------------------------------------------------------------------
    if let Some((name, ptr)) = symbols::find_by_pattern(
        &main_module,
        hooks::tool_authorization::SYMBOL_INCLUDE,
        hooks::tool_authorization::SYMBOL_EXCLUDE,
    ) {
        tracing::info!("tool_authorization: Found {} at {:?}", name, ptr);
        let mut listener = hooks::tool_authorization::Listener;
        match interceptor.attach(ptr, &mut listener) {
            Ok(_) => {
                std::mem::forget(listener);
                tracing::info!("tool_authorization: hook installed");
            }
            Err(e) => tracing::error!("tool_authorization: attach failed: {:?}", e),
        }
    } else {
        tracing::warn!("tool_authorization: request_tool_call_authorization symbol not found");
    }

    // Register in shared hook registry
    register_in_registry(&mode);

    tracing::info!("YOLO mode ACTIVE (pid={})", pid);
}

/// Register this hook in the shared dylib-hook-registry.
fn register_in_registry(mode: &config::YoloMode) {
    use dylib_hook_registry::{HookEntry, HookRegistry};

    // Detect Zed channel from executable path
    let app_id = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            let s = exe.to_string_lossy().to_string();
            if s.contains("Zed Preview") { Some("zed-preview".to_string()) }
            else if s.contains("Zed.app") { Some("zed-stable".to_string()) }
            else { None }
        })
        .unwrap_or_else(|| "zed".to_string());

    let mut registry = HookRegistry::load(&app_id).unwrap_or_default();
    registry.app_id = Some(app_id.clone());

    let dylib_path = format!(
        "{}/target/release/libzed_yolo_hook.dylib",
        env!("CARGO_MANIFEST_DIR")
    );

    let mut entry = HookEntry::new("zed-yolo-hook", &dylib_path)
        .with_version(env!("CARGO_PKG_VERSION"))
        .with_features(&["yolo-mode", "auto-approve-tools"])
        .with_load_order(1);

    // Record which symbols we actually hooked (depends on mode)
    if matches!(mode, config::YoloMode::AllowAll) {
        entry = entry.with_symbol(
            "ToolPermissionDecision::from_input",
            "attach",
            "Auto-approve built-in tool calls",
        );
    }
    entry = entry.with_symbol(
        "AcpThread::request_tool_call_authorization",
        "attach",
        "Auto-approve ACP agent tool calls",
    );

    registry.register(entry);

    if let Err(e) = registry.save(&app_id) {
        tracing::debug!("Could not save hook registry: {} (non-fatal)", e);
    } else {
        tracing::info!("Registered in hook registry (app_id={})", app_id);
    }
}
