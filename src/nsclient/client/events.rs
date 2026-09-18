use crate::nsclient::client::command_input::Command;
use crate::nsclient::client::log_widget::LogRecord;
use crate::nsclient::messages::QueryHelp;
use crossterm::event::KeyEvent;
use tokio::sync::mpsc;

pub async fn send_or_error<T>(sender: &mpsc::Sender<T>, event: T) {
    if let Err(e) = sender.send(event).await {
        eprintln!("Error sending event: {}", e);
    }
}
pub enum UIEvent {
    Key(KeyEvent),
    Status(String),
    Output(String),
    Error(String),
    Performance(f64, f64, f64),
    Log(LogRecord),
    Commands(Vec<String>),
    /// What a query accepts, for completing an argument line. `None` means the
    /// agent could not say -- an unknown query, or one too old to be asked --
    /// which is cached just the same so it is not asked again.
    QueryHelp(String, Box<Option<QueryHelp>>),
}

pub enum UICommand {
    Command(Command),
    /// Fetch what this query accepts. Sent once per command name, the first
    /// time the user starts typing arguments for it.
    DescribeQuery(String),
}
