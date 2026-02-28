//! YOLO mode configuration.
//!
//! Controls whether the hook auto-approves tool calls and at what level.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum YoloMode {
    Disabled,
    AllowAll,
    AllowSafe,
}

impl YoloMode {
    pub fn from_env() -> Self {
        Self::parse(std::env::var("ZED_YOLO_MODE").ok().as_deref())
    }

    pub fn parse(val: Option<&str>) -> Self {
        match val {
            Some("0") | Some("off") | Some("disabled") => YoloMode::Disabled,
            Some("allow_safe") | Some("safe") => YoloMode::AllowSafe,
            _ => YoloMode::AllowAll,
        }
    }

    pub fn is_enabled(self) -> bool {
        self != YoloMode::Disabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_allow_all() {
        assert_eq!(YoloMode::parse(None), YoloMode::AllowAll);
    }

    #[test]
    fn test_disabled_variants() {
        assert_eq!(YoloMode::parse(Some("0")), YoloMode::Disabled);
        assert_eq!(YoloMode::parse(Some("off")), YoloMode::Disabled);
        assert_eq!(YoloMode::parse(Some("disabled")), YoloMode::Disabled);
    }

    #[test]
    fn test_allow_safe() {
        assert_eq!(YoloMode::parse(Some("allow_safe")), YoloMode::AllowSafe);
        assert_eq!(YoloMode::parse(Some("safe")), YoloMode::AllowSafe);
    }

    #[test]
    fn test_unknown_defaults_to_allow_all() {
        assert_eq!(YoloMode::parse(Some("anything")), YoloMode::AllowAll);
        assert_eq!(YoloMode::parse(Some("")), YoloMode::AllowAll);
    }
}
