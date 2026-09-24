use crate::cli::FactsCommand;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::FactsResponse;
use crate::rendering::Rendering;
use anyhow::Context;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use tabled::Tabled;

#[derive(Serialize, Tabled)]
struct FactRow {
    path: String,
    value: String,
    gathered: String,
    status: String,
    error: String,
}

impl FactsResponse {
    fn row(&self, path: &str, value: Option<&Value>) -> FactRow {
        let set = path.split('.').next().unwrap_or_default();
        let error = self.errors.get(set).cloned().unwrap_or_default();
        FactRow {
            path: path.to_string(),
            value: match value {
                Some(Value::String(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            },
            gathered: if value.is_some() {
                self.gathered
                    .get(set)
                    .filter(|when| !when.is_empty())
                    .unwrap_or(&self.collected)
                    .clone()
            } else {
                String::new()
            },
            status: match (value.is_some(), self.errors.contains_key(set)) {
                (true, true) => "stale",
                (true, false) => "collected",
                (false, _) => "not collected",
            }
            .to_string(),
            error,
        }
    }

    fn flatten(&self, path: &str, value: &Value, rows: &mut Vec<FactRow>) {
        match value {
            Value::Object(object) if !object.is_empty() => {
                for (key, child) in object {
                    self.flatten(&format!("{path}.{key}"), child, rows);
                }
            }
            // Keep lists together: record ids and the order of string lists
            // survive, and table paths remain valid dotted API paths.
            _ => rows.push(self.row(path, Some(value))),
        }
    }

    fn to_rows(&self) -> Vec<FactRow> {
        let mut rows = Vec::new();
        if !self.path.is_empty() {
            if self.found {
                self.flatten(&self.path, &self.facts, &mut rows);
            } else {
                rows.push(self.row(&self.path, None));
            }
        } else {
            // Include enabled sets that have never produced a value, and
            // errors even when there is no retained inventory to display.
            let mut sets: BTreeSet<&str> = self.enabled.iter().map(String::as_str).collect();
            sets.extend(self.errors.keys().map(String::as_str));
            if let Some(facts) = self.facts.as_object() {
                sets.extend(facts.keys().map(String::as_str));
            }
            for set in sets {
                match self.facts.get(set) {
                    Some(value) => self.flatten(set, value, &mut rows),
                    None => rows.push(self.row(set, None)),
                }
            }
        }
        rows
    }
}

pub async fn route_facts_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &FactsCommand,
) -> anyhow::Result<()> {
    let facts = match command {
        FactsCommand::Show { path } => api
            .get_facts(path.as_deref().unwrap_or_default())
            .await
            .context("Failed to fetch facts")?,
        FactsCommand::Refresh {} => api
            .refresh_facts()
            .await
            .context("Failed to refresh facts")?,
    };
    if !output.is_flat() {
        return output.render_nested_single(&facts);
    }
    let rows = facts.to_rows();
    if output.is_text() {
        output.print(&format!("Revision: {}", facts.revision));
        if !facts.collected.is_empty() {
            output.print(&format!("Last collection round: {}", facts.collected));
        }
        if rows.is_empty() {
            output.print("No facts collected");
            return Ok(());
        }
    }
    output.render_rows(&rows, &false, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::rendering::StringRender;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn sample() -> Value {
        json!({
            "revision": 7, "collected": "2026-09-23T10:00:00Z", "path": "", "found": true,
            "enabled": ["storage", "os", "hardware", "network"],
            "errors": {"hardware": "WMI query timed out", "network": "Not available"},
            "gathered": {"os": "2026-09-23T06:12:41Z", "hardware": "2026-09-22T06:12:41Z"},
            "facts": {
                "os": {"family": "windows", "secure_boot": false, "features": ["a", "b"]},
                "hardware": {"cpu_cores": 20, "memory_gb": 32.5},
                "storage": {"volumes": [{"id": "c:", "size": 42}]}
            },
            "future_metadata": {"source": "agent"}
        })
    }

    fn rendering(format: OutputFormat) -> (Rendering, Rc<RefCell<String>>) {
        let sink = Box::new(StringRender::new());
        let out = sink.string.clone();
        (
            Rendering::new(format, OutputStyle::Markdown, false, sink),
            out,
        )
    }

    async fn show(body: Value, format: OutputFormat, path: Option<&str>) -> String {
        let mut api = MockApiClientApiImpl::new();
        let expected_path = path.unwrap_or_default().to_owned();
        api.expect_get_facts()
            .withf(move |path| path == expected_path)
            .times(1)
            .returning(move |_| Ok(serde_json::from_value(body.clone()).unwrap()));
        let (output, out) = rendering(format);
        route_facts_commands(
            output,
            Box::new(api),
            &FactsCommand::Show {
                path: path.map(str::to_owned),
            },
        )
        .await
        .unwrap();
        out.borrow().clone()
    }

    #[tokio::test]
    async fn text_shows_gathered_times_retained_values_and_collection_errors() {
        let rendered = show(sample(), OutputFormat::Text, None).await;
        assert!(rendered.contains("Revision: 7"), "{rendered}");
        assert!(
            rendered.contains("Last collection round: 2026-09-23T10:00:00Z"),
            "{rendered}"
        );
        let cpu = rendered
            .lines()
            .find(|line| line.contains("hardware.cpu_cores"))
            .unwrap();
        assert!(
            cpu.contains("20") && cpu.contains("stale") && cpu.contains("WMI query timed out"),
            "{cpu}"
        );
        assert!(cpu.contains("2026-09-22T06:12:41Z"), "{cpu}");
        assert!(!cpu.contains("2026-09-23T10:00:00Z"), "{cpu}");
        let os = rendered
            .lines()
            .find(|line| line.contains("os.family"))
            .unwrap();
        assert!(
            os.contains("windows") && os.contains("2026-09-23T06:12:41Z"),
            "{os}"
        );
        let network = rendered
            .lines()
            .find(|line| line.contains("network"))
            .unwrap();
        assert!(
            network.contains("not collected") && network.contains("Not available"),
            "{network}"
        );
        let volumes = rendered
            .lines()
            .find(|line| line.contains("storage.volumes"))
            .unwrap();
        assert!(
            volumes.contains("2026-09-23T10:00:00Z"),
            "fallback to round time: {volumes}"
        );
        assert!(rendered.find("hardware.cpu_cores") < rendered.find("os.family"));
    }

    #[tokio::test]
    async fn nested_outputs_preserve_the_envelope_and_json_types() {
        for format in [OutputFormat::Json, OutputFormat::Yaml] {
            let is_json = matches!(format, OutputFormat::Json);
            let rendered = show(sample(), format, None).await;
            let parsed: Value = if is_json {
                serde_json::from_str(&rendered).unwrap()
            } else {
                serde_yaml_ng::from_str(&rendered).unwrap()
            };
            assert_eq!(parsed, sample());
        }
    }

    #[tokio::test]
    async fn csv_is_parseable_and_retains_lists_booleans_and_numbers() {
        let rendered = show(sample(), OutputFormat::Csv, None).await;
        let mut reader = csv::Reader::from_reader(rendered.as_bytes());
        assert_eq!(
            reader.headers().unwrap().iter().collect::<Vec<_>>(),
            ["path", "value", "gathered", "status", "error"]
        );
        let records: Vec<_> = reader.records().map(Result::unwrap).collect();
        let field = |path: &str| records.iter().find(|row| &row[0] == path).unwrap();
        assert_eq!(&field("hardware.memory_gb")[1], "32.5");
        assert_eq!(&field("hardware.memory_gb")[3], "stale");
        assert_eq!(&field("os.secure_boot")[1], "false");
        assert_eq!(
            serde_json::from_str::<Value>(&field("os.features")[1]).unwrap(),
            json!(["a", "b"])
        );
        assert_eq!(
            serde_json::from_str::<Value>(&field("storage.volumes")[1]).unwrap(),
            json!([{"id": "c:", "size": 42}])
        );
        assert_eq!(&field("network")[3], "not collected");
    }

    #[tokio::test]
    async fn subtree_objects_scalars_and_empty_lists_render_at_the_requested_path() {
        for (path, value, expected_path, expected_value) in [
            ("os", json!({"family": "windows"}), "os.family", "windows"),
            ("os.family", json!("windows"), "os.family", "windows"),
            ("os.secure_boot", json!(false), "os.secure_boot", "false"),
            ("storage.volumes", json!([]), "storage.volumes", "[]"),
            ("os", json!({}), "os", "{}"),
        ] {
            let mut body = sample();
            body["path"] = json!(path);
            body["facts"] = value;
            let rendered = show(body.clone(), OutputFormat::Csv, Some(path)).await;
            let mut reader = csv::Reader::from_reader(rendered.as_bytes());
            let rows: Vec<_> = reader.records().map(Result::unwrap).collect();
            assert_eq!(rows.len(), 1);
            assert_eq!(&rows[0][0], expected_path);
            assert_eq!(&rows[0][1], expected_value);
            let nested = show(body.clone(), OutputFormat::Json, Some(path)).await;
            assert_eq!(serde_json::from_str::<Value>(&nested).unwrap(), body);
        }
    }

    #[tokio::test]
    async fn missing_paths_and_empty_inventory_are_successful_results() {
        let mut body = sample();
        body["path"] = json!("storage.volumes");
        body["found"] = json!(false);
        body["facts"] = json!({});
        let rendered = show(body.clone(), OutputFormat::Text, Some("storage.volumes")).await;
        assert!(
            rendered.contains("storage.volumes") && rendered.contains("not collected"),
            "{rendered}"
        );
        let rendered = show(body.clone(), OutputFormat::Json, Some("storage.volumes")).await;
        assert_eq!(serde_json::from_str::<Value>(&rendered).unwrap(), body);

        body = json!({"revision": 0, "collected": "", "path": "", "found": false,
            "enabled": [], "errors": {}, "gathered": {}, "facts": {}});
        let rendered = show(body.clone(), OutputFormat::Text, None).await;
        assert!(rendered.contains("No facts collected"), "{rendered}");
        let rendered = show(body.clone(), OutputFormat::Json, None).await;
        assert_eq!(serde_json::from_str::<Value>(&rendered).unwrap(), body);
        assert!(show(body, OutputFormat::Csv, None).await.trim().is_empty());
    }

    #[tokio::test]
    async fn empty_inventory_still_reports_failed_or_pending_sets() {
        let mut body = sample();
        body["found"] = json!(false);
        body["facts"] = json!({});
        body["enabled"] = json!(["os"]);
        let rendered = show(body, OutputFormat::Text, None).await;
        assert!(!rendered.contains("No facts collected"), "{rendered}");
        assert!(rendered.contains("WMI query timed out"), "{rendered}");
        assert!(
            rendered
                .lines()
                .any(|row| row.contains("os") && row.contains("not collected")),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn refresh_renders_the_returned_snapshot_without_followup_get() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_refresh_facts()
            .times(1)
            .returning(|| Ok(serde_json::from_value(sample()).unwrap()));
        let (output, out) = rendering(OutputFormat::Json);
        route_facts_commands(output, Box::new(api), &FactsCommand::Refresh {})
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&out.borrow()).unwrap(),
            sample()
        );
    }

    #[tokio::test]
    async fn transport_errors_keep_the_operation_and_cause() {
        for command in [FactsCommand::Show { path: None }, FactsCommand::Refresh {}] {
            let mut api = MockApiClientApiImpl::new();
            let operation = match command {
                FactsCommand::Show { .. } => {
                    api.expect_get_facts()
                        .returning(|_| anyhow::bail!("HTTP 403"));
                    "fetch"
                }
                FactsCommand::Refresh {} => {
                    api.expect_refresh_facts()
                        .returning(|| anyhow::bail!("HTTP 403"));
                    "refresh"
                }
            };
            let (output, out) = rendering(OutputFormat::Json);
            let error = route_facts_commands(output, Box::new(api), &command)
                .await
                .unwrap_err();
            assert_eq!(
                format!("{error:#}"),
                format!("Failed to {operation} facts: HTTP 403")
            );
            assert!(out.borrow().is_empty());
        }
    }
}
