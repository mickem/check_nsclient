use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::prelude::{Line, Style};
use ratatui::style::Color;
use ratatui::widgets::{Block, Borders, List, ListItem};
use std::cell::RefCell;

#[derive(Debug, PartialEq, Eq)]
pub enum LogLevel {
    Output,
    Debug,
    Info,
    Warning,
    Error,
}

impl LogLevel {
    /// Map a level name as reported by NSClient++ (any case) to a log level.
    /// Unknown levels (e.g. "critical") are treated as errors so they are not missed.
    pub(crate) fn parse(level: &str) -> Self {
        match level.trim().to_ascii_lowercase().as_str() {
            "debug" | "trace" => LogLevel::Debug,
            "info" => LogLevel::Info,
            "warning" | "warn" => LogLevel::Warning,
            "error" => LogLevel::Error,
            _ => LogLevel::Error,
        }
    }
}
#[derive(Debug)]
pub struct LogRecord {
    pub level: LogLevel,
    pub message: String,
}

impl LogRecord {
    pub(crate) fn from_output(message: &str) -> Self {
        Self {
            level: LogLevel::Output,
            message: message.to_string(),
        }
    }
    pub(crate) fn from_error(message: &str) -> LogRecord {
        Self {
            level: LogLevel::Error,
            message: message.to_string(),
        }
    }
    pub(crate) fn from_status(message: &str) -> LogRecord {
        Self {
            level: LogLevel::Info,
            message: message.to_string(),
        }
    }
}

pub struct LogWidget {
    logs: RefCell<Vec<LogRecord>>,
}
impl LogWidget {
    pub fn new() -> Self {
        Self {
            logs: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn add(&self, log: LogRecord) {
        self.logs.borrow_mut().push(log);
    }

    pub fn render(&self, frame: &mut Frame, rect: Rect) {
        let log_block = Block::default()
            .title(Line::raw("Log").centered())
            .borders(Borders::ALL)
            .style(Style::default());

        let height = rect.height.saturating_sub(2) as usize;
        let logs = self.logs.borrow();
        let start = logs.len().saturating_sub(height);
        let visible_logs = &logs[start..];

        let items: Vec<ListItem> = visible_logs
            .iter()
            .map(|l| {
                ListItem::new(Line::styled(
                    l.message.to_string(),
                    color_from_level(&l.level),
                ))
            })
            .collect();

        let log = List::new(items).block(log_block);

        frame.render_widget(log, rect);
    }
}

fn color_from_level(level: &LogLevel) -> Color {
    match level {
        LogLevel::Output => Color::White,
        LogLevel::Info => Color::Green,
        LogLevel::Warning => Color::Yellow,
        LogLevel::Error => Color::Red,
        LogLevel::Debug => Color::DarkGray,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_is_case_insensitive() {
        assert_eq!(LogLevel::parse("debug"), LogLevel::Debug);
        assert_eq!(LogLevel::parse("DEBUG"), LogLevel::Debug);
        assert_eq!(LogLevel::parse("trace"), LogLevel::Debug);
        assert_eq!(LogLevel::parse("Info"), LogLevel::Info);
        assert_eq!(LogLevel::parse(" INFO "), LogLevel::Info);
        assert_eq!(LogLevel::parse("WARNING"), LogLevel::Warning);
        assert_eq!(LogLevel::parse("warn"), LogLevel::Warning);
        assert_eq!(LogLevel::parse("Error"), LogLevel::Error);
    }

    #[test]
    fn parse_treats_unknown_levels_as_errors() {
        assert_eq!(LogLevel::parse("critical"), LogLevel::Error);
        assert_eq!(LogLevel::parse(""), LogLevel::Error);
    }

    #[test]
    fn record_constructors_set_level() {
        assert_eq!(LogRecord::from_output("x").level, LogLevel::Output);
        assert_eq!(LogRecord::from_error("x").level, LogLevel::Error);
        assert_eq!(LogRecord::from_status("x").level, LogLevel::Info);
    }
}
