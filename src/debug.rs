//! Process-wide debug verbosity (set from `--debug`/`-d`, repeatable).
//!
//! Debug output always goes to stderr so it never corrupts machine readable
//! (json/yaml/csv) output on stdout.

use std::sync::atomic::{AtomicU8, Ordering};

static LEVEL: AtomicU8 = AtomicU8::new(0);

pub fn set_level(level: u8) {
    LEVEL.store(level, Ordering::Relaxed);
}

pub fn level() -> u8 {
    LEVEL.load(Ordering::Relaxed)
}

/// Emit `message` on stderr when the configured debug level is at least `min_level`.
pub fn log(min_level: u8, message: impl AsRef<str>) {
    if level() >= min_level {
        eprintln!("[debug] {}", message.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial(debug)]
    fn level_round_trips() {
        set_level(0);
        assert_eq!(level(), 0);
        set_level(2);
        assert_eq!(level(), 2);
        set_level(0);
    }
}
