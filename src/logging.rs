//! Logs go to the console (what `docker logs` shows) and to a file per day
//! in `data/logs`, so a problem can still be traced after a restart.

use std::path::PathBuf;

use tracing_appender::{
    non_blocking::WorkerGuard,
    rolling::{RollingFileAppender, Rotation},
};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Days of log files kept; older ones are deleted as new days start.
const KEEP_DAYS: usize = 14;
const DEFAULT_FILTER: &str = "info,librespot_playback=warn,songbird=warn";

/// Starts logging. Keep the guard alive: dropping it flushes the file.
pub fn init() -> Option<WorkerGuard> {
    // The config isn't loaded yet, so the data folder comes from the
    // environment directly (with the same default).
    let dir = std::env::var("DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data"))
        .join("logs");
    let file = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix("loski")
        .filename_suffix("log")
        .max_log_files(KEEP_DAYS)
        .build(&dir);
    let (file_layer, guard, problem) = match file {
        Ok(appender) => {
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let layer = fmt::layer()
                .with_target(false)
                .with_ansi(false)
                .with_writer(writer);
            (Some(layer), Some(guard), None)
        }
        Err(e) => (None, None, Some(e)),
    };

    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(false))
        .with(file_layer)
        .init();
    if let Some(e) = problem {
        tracing::warn!("Logs won't be saved to {}: {e}", dir.display());
    }
    // A panic in a background task would otherwise vanish; keep it in the log.
    std::panic::set_hook(Box::new(|info| tracing::error!("Loski hit a bug: {info}")));
    guard
}
