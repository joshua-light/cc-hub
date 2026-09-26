use cc_hub_lib::platform;
use log::LevelFilter;
use simplelog::WriteLogger;
use std::fs::File;
use std::path::PathBuf;

/// Start the debug-level file logger and return the log's path. Logging is
/// best-effort: an unwritable cache dir leaves the logger uninstalled.
pub(crate) fn init_logging() -> PathBuf {
    let log_dir = platform::paths::cache_dir();
    std::fs::create_dir_all(&log_dir).ok();

    let log_path = log_dir.join(format!(
        "cc-hub_{}.log",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    ));

    if let Ok(file) = File::create(&log_path) {
        // Millisecond timestamps: input-latency forensics correlate a key's
        // arrival with the frame that follows — second granularity can't
        // show a 200ms stall. (Times are UTC; simplelog's local offset
        // needs an extra feature + unsound-on-unix caveats.)
        let config = simplelog::ConfigBuilder::new()
            .set_time_format_custom(time::macros::format_description!(
                "[hour]:[minute]:[second].[subsecond digits:3]"
            ))
            .build();
        WriteLogger::init(LevelFilter::Debug, config, file).ok();
    }

    log_path
}

/// Log the 1/5/15-minute load averages. Input-lag forensics repeatedly dead-end
/// at "the app was fast, the delay was outside the process" — this line ties
/// each felt incident to how loaded the whole machine was at that moment.
#[cfg(unix)]
pub(crate) fn log_loadavg() {
    let mut la = [0f64; 3];
    // SAFETY: `la` is a valid buffer of 3 doubles and getloadavg writes at
    // most the 3 requested entries.
    let n = unsafe { libc::getloadavg(la.as_mut_ptr(), 3) };
    if n == 3 {
        log::debug!("sys: loadavg={:.2} {:.2} {:.2}", la[0], la[1], la[2]);
    }
}

#[cfg(not(unix))]
pub(crate) fn log_loadavg() {}
