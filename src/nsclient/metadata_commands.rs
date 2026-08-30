use crate::cli::MetadataCommand;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::MetadataChannel;
use crate::rendering::Rendering;
use serde::Serialize;
use serde_json::Value;
use tabled::Tabled;

/// A performance counter as reported by `CheckSystem pdh --list --json`.
///
/// The payload is forwarded verbatim by the server, so it is kept as raw JSON
/// for json/yaml output and only projected onto these two columns for tables.
#[derive(Tabled, Serialize)]
struct CounterRow {
    name: String,
    #[tabled(rename = "type")]
    counter_type: String,
}

fn counter_to_row(counter: &Value) -> CounterRow {
    // NSClient++ 0.18.0 answers with a flat array of counter paths, while the
    // API documentation describes objects with `name` and `type`. Accept both
    // so the table is useful either way.
    if let Value::String(name) = counter {
        return CounterRow {
            name: name.clone(),
            counter_type: String::new(),
        };
    }
    let field = |key: &str| match &counter[key] {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    CounterRow {
        name: field("name"),
        counter_type: field("type"),
    }
}

pub async fn route_metadata_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &MetadataCommand,
) -> anyhow::Result<()> {
    match command {
        MetadataCommand::List {} => match api.list_metadata().await {
            Ok(resources) => output.render_rows(&resources, &false, &[]),
            Err(e) => anyhow::bail!("Failed to fetch metadata: {:#}", e),
        },
        MetadataCommand::Counters {} => match api.get_metadata_counters().await {
            Ok(counters) => output.render_list(&counters, counter_to_row, &false, &[]),
            Err(e) => anyhow::bail!("Failed to fetch performance counters: {:#}", e),
        },
        MetadataCommand::Channels {} => match api.get_metadata_channels().await {
            Ok(channels) => output.render_list(&channels, MetadataChannel::to_flat, &false, &[]),
            Err(e) => anyhow::bail!("Failed to fetch channels: {:#}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::nsclient::messages::MetadataResource;
    use crate::rendering::StringRender;
    use anyhow::anyhow;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn rendering(format: OutputFormat) -> (Rendering, Rc<RefCell<String>>) {
        let sink = Box::new(StringRender::new());
        let out = sink.string.clone();
        (
            Rendering::new(format, OutputStyle::Markdown, false, sink),
            out,
        )
    }

    #[tokio::test]
    async fn list_shows_the_available_resources() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_metadata().returning(|| {
            Ok(vec![MetadataResource {
                name: "counters".into(),
                title: "Performance counters".into(),
                url: "https://localhost:8443/api/v2/metadata/counters".into(),
            }])
        });
        let (output, out) = rendering(OutputFormat::Text);

        route_metadata_commands(output, Box::new(api), &MetadataCommand::List {})
            .await
            .unwrap();

        let rendered = out.borrow();
        assert!(rendered.contains("| name"), "{rendered}");
        assert!(rendered.contains("counters"), "{rendered}");
        assert!(rendered.contains("Performance counters"), "{rendered}");
    }

    #[tokio::test]
    async fn counters_are_projected_onto_name_and_type() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metadata_counters().returning(|| {
            Ok(vec![
                json!({"name": "\\Memory\\Available Bytes", "type": "large"}),
                json!({"name": "\\Processor(_Total)\\% Processor Time"}),
            ])
        });
        let (output, out) = rendering(OutputFormat::Text);

        route_metadata_commands(output, Box::new(api), &MetadataCommand::Counters {})
            .await
            .unwrap();

        let rendered = out.borrow();
        assert!(rendered.contains("| name"), "{rendered}");
        assert!(rendered.contains("| type"), "{rendered}");
        assert!(rendered.contains("Available Bytes"), "{rendered}");
        // A counter without a type renders an empty cell rather than "null".
        assert!(!rendered.contains("null"), "{rendered}");
    }

    #[tokio::test]
    async fn counters_accept_a_flat_list_of_names() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metadata_counters()
            .returning(|| Ok(vec![json!("\\Memory\\Available Bytes")]));
        let (output, out) = rendering(OutputFormat::Text);

        route_metadata_commands(output, Box::new(api), &MetadataCommand::Counters {})
            .await
            .unwrap();

        let rendered = out.borrow();
        assert!(rendered.contains("Available Bytes"), "{rendered}");
        assert!(!rendered.contains('"'), "not json quoted: {rendered}");
    }

    #[tokio::test]
    async fn counters_json_is_forwarded_verbatim() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metadata_counters()
            .returning(|| Ok(vec![json!({"name": "c", "type": "double", "extra": 1})]));
        let (output, out) = rendering(OutputFormat::Json);

        route_metadata_commands(output, Box::new(api), &MetadataCommand::Counters {})
            .await
            .unwrap();

        let parsed: Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["extra"], 1, "unknown fields must survive");
    }

    #[tokio::test]
    async fn channels_join_their_plugins() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metadata_channels().returning(|| {
            Ok(vec![MetadataChannel {
                name: "submit".into(),
                plugins: vec!["Op5Client".into(), "GraphiteClient".into()],
            }])
        });
        let (output, out) = rendering(OutputFormat::Text);

        route_metadata_commands(output, Box::new(api), &MetadataCommand::Channels {})
            .await
            .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| name   | plugins                   |\n\
             |--------|---------------------------|\n\
             | submit | Op5Client, GraphiteClient |\n"
        );
    }

    #[tokio::test]
    async fn errors_name_the_resource() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metadata_counters()
            .returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);
        let err = route_metadata_commands(output, Box::new(api), &MetadataCommand::Counters {})
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Failed to fetch performance counters: boom"
        );

        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metadata_channels()
            .returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);
        let err = route_metadata_commands(output, Box::new(api), &MetadataCommand::Channels {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch channels: boom");
    }
}
