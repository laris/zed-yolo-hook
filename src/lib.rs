//! zed-yolo-hook: YOLO mode for Zed.
//!
//! Auto-approves ALL tool call permission dialogs via two hooks:
//!
//! 1. `permission_decision` — hooks `ToolPermissionDecision::from_input`
//!    to always return `Allow` (built-in tools).
//!
//! 2. `tool_authorization` — hooks `AcpThread::request_tool_call_authorization`
//!    to auto-send the matching ACP allow outcome through the oneshot channel
//!    (external ACP agents). Supports both regular tool permissions and
//!    ExitPlanMode prompts with configurable option_ids.
//!
//! Configuration is loaded from `~/.config/dylib-hooks/{app_id}/zed-yolo-hook.json`
//! with environment variable overrides. See `config.rs` for details.

mod config;
mod ffi;
mod hooks;
mod logging;
mod symbols;

pub use config::{PlanOption, ToolOption, YoloConfig, YoloMode};

use ctor::ctor;
use frida_gum::{Gum, Process, interceptor::Interceptor};
use std::sync::OnceLock;

static GUM: OnceLock<Gum> = OnceLock::new();
static INIT_ONCE: std::sync::Once = std::sync::Once::new();

/// Global config, set once during init, readable from hook listeners.
pub(crate) static CONFIG: OnceLock<YoloConfig> = OnceLock::new();

#[ctor]
fn init() {
    INIT_ONCE.call_once(init_inner);
}

fn init_inner() {
    let app_id = config::detect_app_id();
    let cfg = YoloConfig::load(&app_id);

    logging::init(&cfg.log_level);

    let pid = unsafe { libc::getpid() };
    tracing::info!("=== zed-yolo-hook v{} ===", env!("CARGO_PKG_VERSION"));
    tracing::info!("config: mode={:?}, tool_option={:?}, plan_option={:?}, retry_delay_us={}",
        cfg.mode, cfg.tool_option, cfg.plan_option, cfg.retry_delay_us);

    if let Some(path) = config::config_path(&app_id) {
        tracing::info!("config file: {}", path.display());
    }

    if !cfg.is_enabled() {
        tracing::info!("YOLO disabled (pid={pid}).");
        let _ = CONFIG.set(cfg);
        return;
    }

    // Store config for hook listeners
    let mode = cfg.mode;
    let _ = CONFIG.set(cfg);

    let gum = GUM.get_or_init(|| Gum::obtain());
    let process = Process::obtain(gum);
    let main_module = process.main_module();
    let mut interceptor = Interceptor::obtain(gum);

    // -----------------------------------------------------------------------
    // Hook 1: permission_decision (native tool permissions)
    // -----------------------------------------------------------------------
    if matches!(mode, YoloMode::AllowAll) {
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
        tracing::info!("permission_decision: skipped (mode={:?})", mode);
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
    register_in_registry(&app_id, mode);

    tracing::info!("YOLO mode ACTIVE (pid={})", pid);
}

/// Register this hook in the shared dylib-hook-registry.
fn register_in_registry(app_id: &str, mode: YoloMode) {
    use dylib_hook_registry::{HookEntry, HookRegistry};

    let mut registry = HookRegistry::load(app_id).unwrap_or_default();
    registry.app_id = Some(app_id.to_string());

    let dylib_path = format!(
        "{}/target/release/libzed_yolo_hook.dylib",
        env!("CARGO_MANIFEST_DIR")
    );

    let mut entry = HookEntry::new("zed-yolo-hook", &dylib_path)
        .with_version(env!("CARGO_PKG_VERSION"))
        .with_features(&["yolo-mode", "auto-approve-tools"])
        .with_load_order(1);

    // Record which symbols we actually hooked (depends on mode)
    if matches!(mode, YoloMode::AllowAll) {
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

    if let Err(e) = registry.save(app_id) {
        tracing::debug!("Could not save hook registry: {} (non-fatal)", e);
    } else {
        tracing::info!("Registered in hook registry (app_id={})", app_id);
    }
}
