use crate::cli::EventsCommand;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::EventRecord;
use crate::rendering::Rendering;

pub async fn route_event_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &EventsCommand,
) -> anyhow::Result<()> {
    match command {
        EventsCommand::List {} => match api.list_events().await {
            Ok(events) => output.render_list(&events, EventRecord::to_flat, &false, &[]),
            Err(e) => anyhow::bail!("Failed to fetch events: {:#}", e),
        },
        // The server drains the store: the events it returns are gone from it,
        // so they are rendered rather than dropped on the floor.
        EventsCommand::Clear {} => match api.clear_events().await {
            Ok(events) => output.render_list(&events, EventRecord::to_flat, &false, &[]),
            Err(e) => anyhow::bail!("Failed to clear events: {:#}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::rendering::StringRender;
    use anyhow::anyhow;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    fn rendering(format: OutputFormat) -> (Rendering, Rc<RefCell<String>>) {
        let sink = Box::new(StringRender::new());
        let out = sink.string.clone();
        (
            Rendering::new(format, OutputStyle::Markdown, false, sink),
            out,
        )
    }

    fn sample() -> EventRecord {
        EventRecord {
            index: 7,
            event: "eventlog".into(),
            date: "2026-08-30 12:00:00".into(),
            data: HashMap::from([
                ("source".to_string(), "kernel".to_string()),
                ("id".to_string(), "42".to_string()),
            ]),
        }
    }

    #[tokio::test]
    async fn list_text_flattens_the_data_map() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_events().returning(|| Ok(vec![sample()]));
        let (output, out) = rendering(OutputFormat::Text);

        route_event_commands(output, Box::new(api), &EventsCommand::List {})
            .await
            .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| index | date                | event    | data                 |\n\
             |-------|---------------------|----------|----------------------|\n\
             | 7     | 2026-08-30 12:00:00 | eventlog | id=42, source=kernel |\n"
        );
    }

    #[tokio::test]
    async fn list_json_keeps_the_data_map() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_events().returning(|| Ok(vec![sample()]));
        let (output, out) = rendering(OutputFormat::Json);

        route_event_commands(output, Box::new(api), &EventsCommand::List {})
            .await
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["index"], 7);
        assert_eq!(parsed[0]["data"]["source"], "kernel");
    }

    #[tokio::test]
    async fn clear_renders_the_drained_events() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_clear_events()
            .times(1)
            .returning(|| Ok(vec![sample()]));
        let (output, out) = rendering(OutputFormat::Json);

        route_event_commands(output, Box::new(api), &EventsCommand::Clear {})
            .await
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["index"], 7, "drained events must not be lost");
    }

    #[tokio::test]
    async fn errors_are_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_events().returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);
        let err = route_event_commands(output, Box::new(api), &EventsCommand::List {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch events: boom");

        let mut api = MockApiClientApiImpl::new();
        api.expect_clear_events().returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);
        let err = route_event_commands(output, Box::new(api), &EventsCommand::Clear {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Failed to clear events: boom");
    }
}
