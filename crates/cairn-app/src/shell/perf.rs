// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Opt-in timing traces used to choose loading thresholds.
//!
//! Set `CAIRN_PERF_LOG` to a file path to record process start, probe, query and render timings.
//! Nothing is written, and no timer is read, unless that variable is set.

use std::io::Write;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

static START: LazyLock<Instant> = LazyLock::new(Instant::now);
static LOG: LazyLock<Option<Mutex<std::fs::File>>> = LazyLock::new(|| {
    let path = std::env::var_os("CAIRN_PERF_LOG")?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()?;
    Some(Mutex::new(file))
});

pub fn enabled() -> bool {
    LOG.is_some()
}

pub fn mark(label: &str) {
    let Some(log) = LOG.as_ref() else {
        return;
    };
    let elapsed = START.elapsed().as_secs_f64() * 1000.0;
    if let Ok(mut file) = log.lock() {
        let _ = writeln!(file, "{elapsed:>9.3}ms {label}");
    }
}

/// Logs the time its scope lived, so a function can be timed with one guard.
pub struct Scope {
    label: &'static str,
    started: Instant,
}

impl Drop for Scope {
    fn drop(&mut self) {
        if enabled() {
            mark(&format!(
                "{} {:.3}ms",
                self.label,
                self.started.elapsed().as_secs_f64() * 1000.0
            ));
        }
    }
}

pub fn scope(label: &'static str) -> Option<Scope> {
    enabled().then(|| Scope {
        label,
        started: Instant::now(),
    })
}
