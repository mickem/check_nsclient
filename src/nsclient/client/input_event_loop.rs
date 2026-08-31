use crate::nsclient::client::events::{UIEvent, send_or_error};
use crossterm::event;
use crossterm::event::Event;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const TICK_RATE: Duration = Duration::from_millis(250);

/// Wait up to one tick for a terminal event.
///
/// `crossterm::event::poll` blocks the calling thread, so this must run on the blocking pool
/// rather than on a tokio worker thread.
fn poll_terminal() -> std::io::Result<Option<Event>> {
    if event::poll(TICK_RATE)? {
        Ok(Some(event::read()?))
    } else {
        Ok(None)
    }
}

pub async fn event_loop(cancellation_token: CancellationToken, ui_sender: mpsc::Sender<UIEvent>) {
    loop {
        if cancellation_token.is_cancelled() {
            return;
        }
        match tokio::task::spawn_blocking(poll_terminal).await {
            Ok(Ok(Some(Event::Key(key)))) => {
                send_or_error(&ui_sender, UIEvent::Key(key)).await;
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                send_or_error(&ui_sender, UIEvent::Error(format!("Error: {e}"))).await;
            }
            Err(e) => {
                send_or_error(
                    &ui_sender,
                    UIEvent::Error(format!("Terminal input task failed: {e}")),
                )
                .await;
            }
        }
    }
}
