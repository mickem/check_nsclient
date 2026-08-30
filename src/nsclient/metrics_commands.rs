use crate::cli::MetricsCommand;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::Metrics;
use crate::rendering::Rendering;
use indexmap::IndexMap;
use serde_json::Value;

/// Render a JSON value for table/csv output: strings without quotes, everything else as JSON.
fn value_to_plain_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Flatten the metrics map into a sorted key/value list.
fn metrics_to_dict(metrics: &Metrics) -> IndexMap<String, String> {
    let mut keys: Vec<&String> = metrics.keys().collect();
    keys.sort();
    keys.into_iter()
        .map(|key| (key.clone(), value_to_plain_string(&metrics[key])))
        .collect()
}

pub async fn route_metrics_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &MetricsCommand,
) -> anyhow::Result<()> {
    match command {
        MetricsCommand::Show {} => match api.get_metrics().await {
            Ok(metrics) => output.render_single(&metrics, metrics_to_dict),
            Err(e) => anyhow::bail!("Failed to fetch metrics: {:#}", e),
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
    use serde_json::json;
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

    fn sample_metrics() -> Metrics {
        HashMap::from([
            ("system.cpu.total.user".to_string(), json!(12.5)),
            ("system.uptime".to_string(), json!(3600)),
            ("system.hostname".to_string(), json!("host-1")),
            ("system.ok".to_string(), json!(true)),
            ("system.missing".to_string(), json!(null)),
        ])
    }

    #[test]
    fn value_to_plain_string_unquotes_strings() {
        assert_eq!(value_to_plain_string(&json!("text")), "text");
        assert_eq!(value_to_plain_string(&json!(1.5)), "1.5");
        assert_eq!(value_to_plain_string(&json!(true)), "true");
        assert_eq!(value_to_plain_string(&json!(null)), "");
        assert_eq!(value_to_plain_string(&json!([1, 2])), "[1,2]");
    }

    #[tokio::test]
    async fn show_text_renders_sorted_plain_values() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metrics().returning(|| Ok(sample_metrics()));
        let (output, out) = rendering(OutputFormat::Text);

        route_metrics_commands(output, Box::new(api), &MetricsCommand::Show {})
            .await
            .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| system.cpu.total.user | 12.5   |\n\
             | system.hostname       | host-1 |\n\
             | system.missing        |        |\n\
             | system.ok             | true   |\n\
             | system.uptime         | 3600   |\n"
        );
    }

    #[tokio::test]
    async fn show_json_keeps_native_types() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metrics().returning(|| Ok(sample_metrics()));
        let (output, out) = rendering(OutputFormat::Json);

        route_metrics_commands(output, Box::new(api), &MetricsCommand::Show {})
            .await
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed["system.cpu.total.user"], 12.5);
        assert_eq!(parsed["system.hostname"], "host-1");
        assert_eq!(parsed["system.ok"], true);
    }

    #[tokio::test]
    async fn show_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metrics().returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_metrics_commands(output, Box::new(api), &MetricsCommand::Show {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch metrics: boom");
    }
}
