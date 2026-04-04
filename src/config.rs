//! YOLO mode configuration.
//!
//! Configuration is loaded from a JSON file with environment variable overrides.
//!
//! ## Config file location
//!
//! `~/.config/dylib-hooks/{app_id}/zed-yolo-hook.json`
//!
//! Co-located with the hook registry. The `app_id` is detected from the running
//! executable (e.g., `zed-preview`, `zed-stable`).
//!
//! ## Precedence (highest wins)
//!
//! 1. Environment variable (for terminal testing)
//! 2. Config file
//! 3. Built-in defaults
//!
//! ## Example config file
//!
//! ```json
//! {
//!   "mode": "allow_all",
//!   "tool_option": "allow",
//!   "plan_option": "acceptEdits",
//!   "log_level": "info",
//!   "retry_delay_us": 1500
//! }
//! ```

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Config structs
// ---------------------------------------------------------------------------

/// Top-level YOLO hook configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct YoloConfig {
    /// Which hooks to install.
    pub mode: YoloMode,
    /// What option_id to send for regular tool permissions.
    pub tool_option: ToolOption,
    /// What option_id to send for ExitPlanMode / "Ready to code?" prompts.
    pub plan_option: PlanOption,
    /// Tracing filter level.
    pub log_level: String,
    /// Microseconds to wait before single retry on miss.
    pub retry_delay_us: u64,
}

/// Controls which hooks are installed.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum YoloMode {
    /// Both ACP and native tool hooks enabled.
    AllowAll,
    /// Only ACP hook enabled (native tool permissions follow Zed settings).
    AllowSafe,
    /// Dylib loads but installs no hooks.
    Disabled,
}

/// What to send for regular tool permission dialogs (Scenario C).
///
/// Maps directly to Claude Code's expected option_ids:
/// - `"allow"` (AllowOnce) — one-time approval
/// - `"allow_always"` (AllowAlways) — persistent rule, won't ask again for this tool type
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOption {
    /// Send option_id="allow", kind=AllowOnce. One-time approval.
    Allow,
    /// Send option_id="allow_always", kind=AllowAlways. Creates persistent session rule.
    AllowAlways,
}

/// What to send for ExitPlanMode / "Ready to code?" prompts (Scenario A).
///
/// Maps directly to Claude Code session modes. The option_id IS the mode name.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanOption {
    /// Send option_id="acceptEdits", kind=AllowAlways. Auto-accept file edits.
    AcceptEdits,
    /// Send option_id="bypassPermissions", kind=AllowAlways. Bypass all checks.
    /// Warning: has known hallucination bug (zed-industries/zed#48992).
    BypassPermissions,
    /// Send option_id="default", kind=AllowOnce. Manual approval for each edit.
    Default,
    /// Send option_id="plan", kind=RejectOnce. Stay in plan mode (reject ExitPlanMode).
    Plan,
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

impl Default for YoloConfig {
    fn default() -> Self {
        Self {
            mode: YoloMode::AllowAll,
            tool_option: ToolOption::Allow,
            plan_option: PlanOption::AcceptEdits,
            log_level: "info".to_string(),
            retry_delay_us: 1500,
        }
    }
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

impl YoloConfig {
    /// Load config: env vars override config file, which overrides defaults.
    pub fn load(app_id: &str) -> Self {
        let mut config = Self::load_from_file(app_id).unwrap_or_default();

        // Env var overrides (for terminal testing / cargo patch --verify)
        if let Ok(val) = std::env::var("ZED_YOLO_MODE") {
            if let Some(mode) = parse_yolo_mode(&val) {
                config.mode = mode;
            }
        }
        if let Ok(val) = std::env::var("ZED_YOLO_TOOL_OPTION") {
            if let Some(opt) = parse_tool_option(&val) {
                config.tool_option = opt;
            }
        }
        if let Ok(val) = std::env::var("ZED_YOLO_PLAN_OPTION") {
            if let Some(opt) = parse_plan_option(&val) {
                config.plan_option = opt;
            }
        }
        if let Ok(val) = std::env::var("ZED_YOLO_LOG") {
            if !val.is_empty() {
                config.log_level = val;
            }
        }
        if let Ok(val) = std::env::var("ZED_YOLO_RETRY_DELAY_US") {
            if let Ok(us) = val.parse::<u64>() {
                config.retry_delay_us = us.min(10_000);
            }
        }

        config
    }

    fn load_from_file(app_id: &str) -> Option<Self> {
        let path = config_path(app_id)?;
        let content = std::fs::read_to_string(&path).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Save current config to file.
    pub fn save(&self, app_id: &str) -> std::io::Result<()> {
        let path = config_path(app_id)
            .ok_or_else(|| std::io::Error::other("cannot determine config path"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.mode != YoloMode::Disabled
    }
}

/// Config file path: `~/.config/dylib-hooks/{app_id}/zed-yolo-hook.json`
pub fn config_path(app_id: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".config")
            .join("dylib-hooks")
            .join(app_id)
            .join("zed-yolo-hook.json"),
    )
}

/// Detect app_id from executable path.
pub fn detect_app_id() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| {
            let s = exe.to_string_lossy().to_string();
            if s.contains("Zed Preview") {
                Some("zed-preview".to_string())
            } else if s.contains("Zed.app") {
                Some("zed-stable".to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "zed".to_string())
}

// ---------------------------------------------------------------------------
// Env var parsers (lenient, case-insensitive)
// ---------------------------------------------------------------------------

fn parse_yolo_mode(val: &str) -> Option<YoloMode> {
    match val.trim().to_lowercase().as_str() {
        "0" | "off" | "disabled" => Some(YoloMode::Disabled),
        "allow_safe" | "safe" => Some(YoloMode::AllowSafe),
        "allow_all" | "1" | "on" | "" => Some(YoloMode::AllowAll),
        _ => Some(YoloMode::AllowAll), // unknown → default
    }
}

fn parse_tool_option(val: &str) -> Option<ToolOption> {
    match val.trim().to_lowercase().as_str() {
        "allow" => Some(ToolOption::Allow),
        "allow_always" => Some(ToolOption::AllowAlways),
        _ => None,
    }
}

fn parse_plan_option(val: &str) -> Option<PlanOption> {
    match val.trim().to_lowercase().as_str() {
        "acceptedits" | "accept_edits" => Some(PlanOption::AcceptEdits),
        "bypasspermissions" | "bypass_permissions" | "bypass" => Some(PlanOption::BypassPermissions),
        "default" => Some(PlanOption::Default),
        "plan" => Some(PlanOption::Plan),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = YoloConfig::default();
        assert_eq!(config.mode, YoloMode::AllowAll);
        assert_eq!(config.tool_option, ToolOption::Allow);
        assert_eq!(config.plan_option, PlanOption::AcceptEdits);
        assert_eq!(config.log_level, "info");
        assert_eq!(config.retry_delay_us, 1500);
    }

    #[test]
    fn test_roundtrip_json() {
        let config = YoloConfig {
            mode: YoloMode::AllowSafe,
            tool_option: ToolOption::AllowAlways,
            plan_option: PlanOption::BypassPermissions,
            log_level: "debug".to_string(),
            retry_delay_us: 2000,
        };
        let json = serde_json::to_string_pretty(&config).unwrap();
        let loaded: YoloConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.mode, YoloMode::AllowSafe);
        assert_eq!(loaded.tool_option, ToolOption::AllowAlways);
        assert_eq!(loaded.plan_option, PlanOption::BypassPermissions);
        assert_eq!(loaded.log_level, "debug");
        assert_eq!(loaded.retry_delay_us, 2000);
    }

    #[test]
    fn test_partial_json() {
        // Only some fields specified — rest should use defaults
        let json = r#"{ "plan_option": "bypassPermissions" }"#;
        let config: YoloConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.mode, YoloMode::AllowAll);
        assert_eq!(config.plan_option, PlanOption::BypassPermissions);
        assert_eq!(config.tool_option, ToolOption::Allow);
    }

    #[test]
    fn test_empty_json() {
        let config: YoloConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.mode, YoloMode::AllowAll);
        assert_eq!(config.plan_option, PlanOption::AcceptEdits);
    }

    #[test]
    fn test_parse_yolo_mode() {
        assert_eq!(parse_yolo_mode("0"), Some(YoloMode::Disabled));
        assert_eq!(parse_yolo_mode("off"), Some(YoloMode::Disabled));
        assert_eq!(parse_yolo_mode("disabled"), Some(YoloMode::Disabled));
        assert_eq!(parse_yolo_mode("allow_safe"), Some(YoloMode::AllowSafe));
        assert_eq!(parse_yolo_mode("safe"), Some(YoloMode::AllowSafe));
        assert_eq!(parse_yolo_mode("allow_all"), Some(YoloMode::AllowAll));
        assert_eq!(parse_yolo_mode(""), Some(YoloMode::AllowAll));
    }

    #[test]
    fn test_parse_tool_option() {
        assert_eq!(parse_tool_option("allow"), Some(ToolOption::Allow));
        assert_eq!(parse_tool_option("allow_always"), Some(ToolOption::AllowAlways));
        assert_eq!(parse_tool_option("unknown"), None);
    }

    #[test]
    fn test_parse_plan_option() {
        assert_eq!(parse_plan_option("acceptEdits"), Some(PlanOption::AcceptEdits));
        assert_eq!(parse_plan_option("accept_edits"), Some(PlanOption::AcceptEdits));
        assert_eq!(parse_plan_option("bypass"), Some(PlanOption::BypassPermissions));
        assert_eq!(parse_plan_option("default"), Some(PlanOption::Default));
        assert_eq!(parse_plan_option("plan"), Some(PlanOption::Plan));
        assert_eq!(parse_plan_option("unknown"), None);
    }

    #[test]
    fn test_serde_plan_option_camel_case() {
        // PlanOption uses camelCase for JSON (matching ACP protocol option_ids)
        let json = serde_json::to_string(&PlanOption::AcceptEdits).unwrap();
        assert_eq!(json, r#""acceptEdits""#);
        let json = serde_json::to_string(&PlanOption::BypassPermissions).unwrap();
        assert_eq!(json, r#""bypassPermissions""#);
    }

    #[test]
    fn test_serde_tool_option_snake_case() {
        let json = serde_json::to_string(&ToolOption::AllowAlways).unwrap();
        assert_eq!(json, r#""allow_always""#);
    }
}
