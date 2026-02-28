//! Logging setup for zed-yolo-hook.
//!
//! Writes to ~/Library/Logs/Zed/zed-yolo-hook.*.log (Zed's standard log directory on macOS).

use std::path::PathBuf;

/// Initialize tracing with a rolling file appender in Zed's log directory.
///
/// Reads `ZED_YOLO_LOG` env var for log level (default: "info").
pub fn init() {
    let log_dir = std::env::var("HOME")
        .map(|h| PathBuf::from(h).join("Library/Logs/Zed"))
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    let _ = std::fs::create_dir_all(&log_dir);

    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("zed-yolo-hook")
        .filename_suffix("log")
        .build(&log_dir)
        .expect("failed to create log file appender");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    std::mem::forget(guard);

    let log_level = std::env::var("ZED_YOLO_LOG").unwrap_or_else(|_| "info".to_string());

    let _ = tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(log_level.parse().unwrap_or(tracing::Level::INFO.into())),
        )
        .try_init();

    tracing::info!(
        "Logs: {}/zed-yolo-hook.*.log",
        log_dir.display()
    );
}
