use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone)]
pub(super) struct TraceLog {
    origin: Instant,
    file: Option<Arc<Mutex<BufWriter<File>>>>,
}

impl TraceLog {
    pub(super) fn new() -> Self {
        let file = std::env::var_os("CAPTURE_SLINT_LOG").and_then(|path| {
            let path_display = PathBuf::from(path.clone()).display().to_string();
            match OpenOptions::new().create(true).append(true).open(&path) {
                Ok(file) => Some(Arc::new(Mutex::new(BufWriter::new(file)))),
                Err(error) => {
                    eprintln!(
                        "[capture-slint] logger.open.error path={path_display} error={error}"
                    );
                    None
                }
            }
        });
        let logger = Self {
            origin: Instant::now(),
            file,
        };
        logger.event(
            "logger.ready",
            format!(
                "file={}",
                if std::env::var_os("CAPTURE_SLINT_LOG").is_some() {
                    "enabled"
                } else {
                    "stderr"
                }
            ),
        );
        logger
    }

    pub(super) fn event(&self, stage: &str, detail: impl Display) {
        let line = format!(
            "[{:.3} ms] {stage} {detail}",
            self.origin.elapsed().as_secs_f64() * 1000.0
        );
        eprintln!("{line}");
        if let Some(file) = &self.file {
            if let Ok(mut file) = file.lock() {
                let _ = writeln!(file, "{line}");
            }
        }
    }

    pub(super) fn flush(&self) {
        if let Some(file) = &self.file {
            if let Ok(mut file) = file.lock() {
                let _ = file.flush();
            }
        }
    }

    pub(super) fn duration(&self, stage: &str, started: Instant) {
        self.event(
            stage,
            format!(
                "duration_ms={:.3}",
                started.elapsed().as_secs_f64() * 1000.0
            ),
        );
    }
}
