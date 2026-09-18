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
    /// What a query accepts, for completing an argument line.
    QueryHelp(String, QueryHelpAnswer),
}

/// What came back when the client asked what a query accepts.
pub enum QueryHelpAnswer {
    /// The agent described it.
    Described(Box<QueryHelp>),
    /// The agent has nothing to describe: an unknown query, or one on an agent
    /// from before the endpoint existed. A fact about the query, so it is
    /// remembered and not asked again.
    Nothing,
    /// The asking failed for a reason that may not repeat -- a timeout, a token
    /// that had to be refreshed. Forgotten rather than remembered, so the next
    /// keystroke past the command name tries again: caching it would leave
    /// completion dead for the session with nothing on screen to explain why.
    Failed,
}

pub enum UICommand {
    Command(Command),
    /// Fetch what this query accepts. Sent once per command name, the first
    /// time the user starts typing arguments for it.
    DescribeQuery(String),
}
