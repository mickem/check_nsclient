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
        // `--meta` asks the agent for a different document -- the readings
        // and what they mean, from one snapshot -- so it is rendered
        // differently too: a row per metric for the flat formats, and the
        // document as the agent sent it for json/yaml, where the two halves
        // are more useful kept apart.
        MetricsCommand::Show { meta: true, long } => match api.get_metrics_described().await {
            Ok(described) => {
                if output.is_flat() {
                    output.render_rows(&described.to_rows(), long, &["labels", "help"])
                } else {
                    output.render_nested_single(&described)
                }
            }
            // An agent that predates `?meta=1` ignores the parameter and
            // answers with the flat map, which does not fit this shape, so the
            // failure reads as a decoding error rather than as "too old".
            Err(e) => anyhow::bail!(
                concat!(
                    "Failed to fetch described metrics: {:#} ",
                    "(an agent without ?meta=1 answers with the plain metric map; ",
                    "drop --meta to read it)"
                ),
                e
            ),
        },
        MetricsCommand::Show { meta: false, .. } => match api.get_metrics().await {
            Ok(metrics) => output.render_single(&metrics, metrics_to_dict),
            Err(e) => anyhow::bail!("Failed to fetch metrics: {:#}", e),
        },
        // The exposition format is line based and meant to be piped straight
        // into a scraper, so it is printed verbatim in every output format.
        MetricsCommand::Openmetrics {} => match api.get_openmetrics().await {
            Ok(body) => {
                output.print(&body);
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to fetch openmetrics: {:#}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::nsclient::messages::{DescribedMetrics, MetricDescription};
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

        route_metrics_commands(
            output,
            Box::new(api),
            &MetricsCommand::Show {
                meta: false,
                long: false,
            },
        )
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

        route_metrics_commands(
            output,
            Box::new(api),
            &MetricsCommand::Show {
                meta: false,
                long: false,
            },
        )
        .await
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed["system.cpu.total.user"], 12.5);
        assert_eq!(parsed["system.hostname"], "host-1");
        assert_eq!(parsed["system.ok"], true);
    }

    fn described() -> DescribedMetrics {
        DescribedMetrics {
            metrics: HashMap::from([
                ("system.mem.physical.used".to_string(), json!(5123456789u64)),
                ("workers.jobs".to_string(), json!(1847)),
            ]),
            metadata: HashMap::from([
                (
                    "system.mem.physical.used".to_string(),
                    MetricDescription {
                        metric_type: "gauge".into(),
                        help: Some("Physical memory in use".into()),
                        unit: Some("bytes".into()),
                        labels: None,
                    },
                ),
                (
                    "workers.jobs".to_string(),
                    MetricDescription {
                        metric_type: "counter".into(),
                        help: None,
                        unit: None,
                        labels: None,
                    },
                ),
            ]),
        }
    }

    #[tokio::test]
    async fn meta_text_puts_the_unit_next_to_the_value() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metrics_described()
            .returning(|| Ok(described()));
        let (output, out) = rendering(OutputFormat::Text);

        route_metrics_commands(
            output,
            Box::new(api),
            &MetricsCommand::Show {
                meta: true,
                long: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            r#"| metric                   | value      | unit  | type    |
|--------------------------|------------|-------|---------|
| system.mem.physical.used | 5123456789 | bytes | gauge   |
| workers.jobs             | 1847       |       | counter |
"#
        );
    }

    #[tokio::test]
    async fn meta_long_adds_the_labels_and_help() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metrics_described()
            .returning(|| Ok(described()));
        let (output, out) = rendering(OutputFormat::Text);

        route_metrics_commands(
            output,
            Box::new(api),
            &MetricsCommand::Show {
                meta: true,
                long: true,
            },
        )
        .await
        .unwrap();

        let rendered = out.borrow();
        assert!(rendered.contains("labels"), "{rendered}");
        assert!(rendered.contains("Physical memory in use"), "{rendered}");
    }

    #[tokio::test]
    async fn meta_json_keeps_the_document_the_agent_sent() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metrics_described()
            .returning(|| Ok(described()));
        let (output, out) = rendering(OutputFormat::Json);

        route_metrics_commands(
            output,
            Box::new(api),
            &MetricsCommand::Show {
                meta: true,
                long: false,
            },
        )
        .await
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        // The two halves stay apart, and the value keeps its native type.
        assert_eq!(parsed["metrics"]["workers.jobs"], 1847);
        assert_eq!(parsed["metadata"]["workers.jobs"]["type"], "counter");
        // A field the producer never declared is not invented.
        assert!(parsed["metadata"]["workers.jobs"]["unit"].is_null());
    }

    #[tokio::test]
    async fn meta_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metrics_described()
            .returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_metrics_commands(
            output,
            Box::new(api),
            &MetricsCommand::Show {
                meta: true,
                long: false,
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Failed to fetch described metrics: boom"),
            "{err}"
        );
        assert!(err.to_string().contains("drop --meta"), "{err}");
    }

    #[tokio::test]
    async fn openmetrics_is_printed_verbatim() {
        for format in [OutputFormat::Text, OutputFormat::Json] {
            let mut api = MockApiClientApiImpl::new();
            api.expect_get_openmetrics()
                .returning(|| Ok("cpu_total 12\nmem_used 42\n".to_string()));
            let (output, out) = rendering(format);

            route_metrics_commands(output, Box::new(api), &MetricsCommand::Openmetrics {})
                .await
                .unwrap();

            assert_eq!(out.borrow().as_str(), "cpu_total 12\nmem_used 42\n\n");
        }
    }

    #[tokio::test]
    async fn openmetrics_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_openmetrics()
            .returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_metrics_commands(output, Box::new(api), &MetricsCommand::Openmetrics {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch openmetrics: boom");
    }

    #[tokio::test]
    async fn show_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_metrics().returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_metrics_commands(
            output,
            Box::new(api),
            &MetricsCommand::Show {
                meta: false,
                long: false,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch metrics: boom");
    }
}
