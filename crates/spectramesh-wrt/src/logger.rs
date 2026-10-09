//! Logs to stderr. On OpenWRT, procd passes it on to the system log (`logread`).

use log::{Level, LevelFilter, Log, Metadata, Record};

struct StderrLogger;

impl Log for StderrLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= log::max_level()
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let level = match record.level() {
                Level::Error => "error",
                Level::Warn => "warning",
                Level::Info => "info",
                Level::Debug => "debug",
                Level::Trace => "trace",
            };
            eprintln!("{level}: {}", record.args());
        }
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

pub fn init(verbose: bool) {
    // Only fails if a logger is already set, which would be a bug in main.
    log::set_logger(&LOGGER).expect("logger already set");
    log::set_max_level(if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    });
}
