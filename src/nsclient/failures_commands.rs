use crate::cli::FailuresCommand;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::{
    DiagnosticsSection, FAILED_CHECK_LONG_COLUMNS, FailedCheck, FailureContext, FailuresCleared,
    FailuresFilter, FailuresLog,
};
use crate::rendering::Rendering;
use serde_json::{Value, json};

/// The part of `failures context` that `--section metrics` names.
const METRICS_SECTION: &str = "metrics";

/// Columns of the metrics table that are only shown with `--long`.
const METRIC_LONG_COLUMNS: &[&str] = &["labels", "help"];

/// The listing answered `404`: the route does not exist on this agent.
const NO_FAILURES_API: &str = concat!(
    "this agent has no failed-check log ",
    "(GET /api/v2/failures answered 404; it needs a newer NSClient++)"
);

pub async fn route_failures_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &FailuresCommand,
) -> anyhow::Result<()> {
    match command {
        FailuresCommand::List {
            status,
            command,
            since,
            limit,
            long,
        } => {
            let filter = FailuresFilter {
                status: status.clone(),
                command: command.clone(),
                since: *since,
                limit: *limit,
            };
            let log = fetch_log(api.as_ref(), &filter).await?;
            render_list(&output, &log, long)
        }
        FailuresCommand::Status {} => {
            // The envelope comes with every listing; one entry is the
            // cheapest listing that still is one.
            let filter = FailuresFilter {
                limit: Some(1),
                ..FailuresFilter::default()
            };
            let log = fetch_log(api.as_ref(), &filter).await?;
            if output.is_flat() {
                output.render_flat_single(&log.to_dict())
            } else {
                output.render_nested_single(&configuration_only(&log)?)
            }
        }
        FailuresCommand::Context {
            index,
            section,
            long,
        } => match api.get_failure_context(*index).await {
            Ok(Some(context)) => render_context(&output, &context, section.as_deref(), long),
            Ok(None) => anyhow::bail!(
                "Failed check {index} not found: the log no longer holds it (never recorded, evicted by max entries, or cleared)"
            ),
            Err(e) => anyhow::bail!("Failed to fetch the context of failed check {index}: {e:#}"),
        },
        FailuresCommand::Clear {} => match api.clear_failures().await {
            Ok(cleared) => output.render_single(&cleared, FailuresCleared::to_dict),
            Err(e) => anyhow::bail!("Failed to clear the failed-check log: {e:#}"),
        },
    }
}

async fn fetch_log(api: &dyn ApiClientApi, filter: &FailuresFilter) -> anyhow::Result<FailuresLog> {
    match api.list_failures(filter).await {
        Ok(Some(log)) => Ok(log),
        Ok(None) => anyhow::bail!("Failed to fetch failures: {NO_FAILURES_API}"),
        Err(e) => anyhow::bail!("Failed to fetch failures: {e:#}"),
    }
}

/// The envelope without its entries: what `failures status` reports.
fn configuration_only(log: &FailuresLog) -> anyhow::Result<Value> {
    let mut value = serde_json::to_value(log)?;
    if let Some(object) = value.as_object_mut() {
        object.remove("entries");
        object.remove("count");
    }
    Ok(value)
}

fn render_list(output: &Rendering, log: &FailuresLog, long: &bool) -> anyhow::Result<()> {
    // json/yaml keep the envelope: a poller needs `last_index` to pass back
    // as --since, and `enabled` to tell an idle log from a switched off one.
    if !output.is_flat() {
        return output.render_nested_single(log);
    }
    if log.entries.is_empty() && output.is_text() {
        output.print(if log.enabled {
            "No failed checks"
        } else {
            "The failed-check log is disabled on the agent (enabled under [/settings/core/failed checks])"
        });
        return Ok(());
    }
    output.render_list(
        &log.entries,
        FailedCheck::to_flat,
        long,
        FAILED_CHECK_LONG_COLUMNS,
    )
}

fn render_context(
    output: &Rendering,
    context: &FailureContext,
    section: Option<&str>,
    long: &bool,
) -> anyhow::Result<()> {
    match section {
        Some(METRICS_SECTION) => render_metrics(output, context, long),
        Some(name) => {
            let found = find_section(context, name)?;
            if output.is_flat() {
                render_section(output, name, found, false)
            } else {
                output.render_nested_single(found)
            }
        }
        None if !output.is_flat() => output.render_nested_single(context),
        // csv holds a single table, so it gets the one every entry with a
        // snapshot has.
        None if !output.is_text() => render_metrics(output, context, long),
        None => {
            output.render_flat_single(&context.to_dict())?;
            output.print("");
            output.print("Metrics at the time of the failure:");
            render_metrics(output, context, long)?;
            if let Some(sections) = &context.diagnostics.sections {
                for (name, found) in sections {
                    output.print("");
                    render_section(output, name, found, true)?;
                }
            }
            Ok(())
        }
    }
}

fn render_metrics(output: &Rendering, context: &FailureContext, long: &bool) -> anyhow::Result<()> {
    if !output.is_flat() {
        // The same shape as `metrics show --meta`, so one parser reads both.
        return output.render_nested_single(&json!({
            "metrics": context.metrics.clone().unwrap_or_default(),
            "metadata": context.metadata.clone().unwrap_or_default(),
        }));
    }
    let rows = context.metric_rows();
    if rows.is_empty() && output.is_text() {
        output.print(&format!(
            "No metrics captured ({})",
            context.reference.summary()
        ));
        return Ok(());
    }
    output.render_rows(&rows, long, METRIC_LONG_COLUMNS)
}

fn find_section<'a>(
    context: &'a FailureContext,
    name: &str,
) -> anyhow::Result<&'a DiagnosticsSection> {
    let Some(sections) = &context.diagnostics.sections else {
        anyhow::bail!(
            "Failed check {} has no diagnostics ({})",
            context.index,
            context.diagnostics.reference.summary()
        );
    };
    match sections.get(name) {
        Some(found) => Ok(found),
        None => {
            let mut known: Vec<&str> = vec![METRICS_SECTION];
            known.extend(sections.keys().map(String::as_str));
            anyhow::bail!(
                "Failed check {} has no section '{name}' (available: {})",
                context.index,
                known.join(", ")
            )
        }
    }
}

/// One diagnostics table; `heading` puts its name and provenance above it,
/// for the text view that shows every table one after another.
fn render_section(
    output: &Rendering,
    name: &str,
    section: &DiagnosticsSection,
    heading: bool,
) -> anyhow::Result<()> {
    if heading {
        let mut title = format!("Diagnostics: {name}");
        let mut details: Vec<String> = Vec::new();
        if !section.source.is_empty() {
            details.push(section.source.clone());
        }
        if !section.captured_at.is_empty() {
            details.push(format!("captured {}", section.captured_at));
        }
        if section.truncated {
            details.push("truncated".to_string());
        }
        if !details.is_empty() {
            title.push_str(&format!(" ({})", details.join(", ")));
        }
        output.print(&title);
        if let Some(description) = section.description.as_deref().filter(|d| !d.is_empty()) {
            output.print(description);
        }
    }
    if let Some(error) = section.error.as_deref().filter(|e| !e.is_empty())
        && output.is_text()
    {
        output.print(&format!("Error: {error}"));
    }
    if section.rows.is_empty() && output.is_text() {
        output.print("(no rows)");
        return Ok(());
    }
    output.render_dynamic_table(&section.headers(), &section.cells())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::rendering::StringRender;
    use anyhow::anyhow;
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

    /// The listing the REST reference documents, with one entry.
    fn sample_log() -> FailuresLog {
        serde_json::from_value(json!({
            "enabled": true,
            "max_entries": 50,
            "arguments_recorded": true,
            "arguments_mode": "safe",
            "statuses": "warning,critical,unknown",
            "last_index": 12,
            "dropped": 0,
            "capture_metrics": true,
            "context_budget": 4194304,
            "max_context_size": 1048576,
            "context_bytes_used": 72650,
            "capture_diagnostics": true,
            "diagnostics_rows": 10,
            "count": 1,
            "entries": [{
                "index": 12,
                "timestamp": 1793426271,
                "date": "2026-Oct-31 06:57:51",
                "age": 42,
                "subject": "NRPEServer",
                "depth": 0,
                "command": "check_drivesize",
                "alias": "",
                "status": 2,
                "result": "CRITICAL",
                "message": "CRITICAL C: 92% used",
                "lines": [{
                    "message": "CRITICAL C: 92% used",
                    "perf": {"C: used %": {"value": 92, "unit": "%", "warning": 80, "critical": 90}}
                }],
                "arguments": ["drive=C:", "warning=used > 80%"],
                "context": {"state": "captured", "snapshot_id": 15, "captured_at": 1793426266, "age_at_failure": 5, "size": 72650},
                "diagnostics": {"state": "disabled"}
            }]
        }))
        .unwrap()
    }

    fn sample_context() -> FailureContext {
        serde_json::from_value(json!({
            "state": "captured",
            "snapshot_id": 15,
            "captured_at": 1793426266,
            "age_at_failure": 5,
            "size": 72650,
            "index": 12,
            "timestamp": 1793426271,
            "metrics": {"system.cpu.total.idle": 3, "workers.jobs": 214},
            "metadata": {
                "system.cpu.total.idle": {"type": "gauge", "help": "Idle CPU", "unit": "percent"},
                "workers.jobs": {"type": "counter"}
            },
            "diagnostics": {
                "state": "captured",
                "snapshot_id": 15,
                "captured_at": 1793426266,
                "age_at_failure": 5,
                "size": 2310,
                "sections": {
                    "processes.top_cpu": {
                        "source": "CheckSystem",
                        "captured_at": "2026-10-31T06:57:45Z",
                        "truncated": true,
                        "columns": [
                            {"name": "name", "type": "string"},
                            {"name": "cpu", "type": "float", "unit": "percent"}
                        ],
                        "rows": [{"name": "acme-indexer.exe", "cpu": 87.5}, {"name": "idle"}]
                    }
                }
            }
        }))
        .unwrap()
    }

    fn list() -> FailuresCommand {
        FailuresCommand::List {
            status: None,
            command: None,
            since: None,
            limit: None,
            long: false,
        }
    }

    fn context(section: Option<&str>) -> FailuresCommand {
        FailuresCommand::Context {
            index: 12,
            section: section.map(str::to_string),
            long: false,
        }
    }

    async fn run(
        api: MockApiClientApiImpl,
        format: OutputFormat,
        command: &FailuresCommand,
    ) -> (anyhow::Result<()>, String) {
        let (output, out) = rendering(format);
        let result = route_failures_commands(output, Box::new(api), command).await;
        let text = out.borrow().clone();
        (result, text)
    }

    #[tokio::test]
    async fn list_passes_the_filter_to_the_agent() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_failures()
            .withf(|filter| {
                filter.to_query()
                    == vec![
                        ("status".to_string(), "critical".to_string()),
                        ("command".to_string(), "check_cpu".to_string()),
                        ("since".to_string(), "7".to_string()),
                        ("limit".to_string(), "3".to_string()),
                    ]
            })
            .returning(|_| Ok(Some(sample_log())));
        let command = FailuresCommand::List {
            status: Some("critical".into()),
            command: Some("check_cpu".into()),
            since: Some(7),
            limit: Some(3),
            long: false,
        };
        let (result, _) = run(api, OutputFormat::Text, &command).await;
        result.unwrap();
    }

    #[tokio::test]
    async fn list_text_shows_one_row_per_failure() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_failures()
            .returning(|_| Ok(Some(sample_log())));
        let (result, out) = run(api, OutputFormat::Text, &list()).await;
        result.unwrap();
        assert_eq!(
            out,
            "| index | date                 | result   | command         | message              |\n\
             |-------|----------------------|----------|-----------------|----------------------|\n\
             | 12    | 2026-Oct-31 06:57:51 | CRITICAL | check_drivesize | CRITICAL C: 92% used |\n"
        );
    }

    #[tokio::test]
    async fn list_long_shows_the_arguments_perf_and_snapshots() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_failures()
            .returning(|_| Ok(Some(sample_log())));
        let command = FailuresCommand::List {
            status: None,
            command: None,
            since: None,
            limit: None,
            long: true,
        };
        let (result, out) = run(api, OutputFormat::Text, &command).await;
        result.unwrap();
        assert!(out.contains("NRPEServer"), "{out}");
        assert!(out.contains("drive=C: warning=used > 80%"), "{out}");
        assert!(
            out.contains("C: used %=92%, warning: 80, critical: 90"),
            "{out}"
        );
        assert!(out.contains("captured (5s before)"), "{out}");
        assert!(out.contains("disabled"), "{out}");
    }

    #[tokio::test]
    async fn list_json_keeps_the_envelope() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_failures()
            .returning(|_| Ok(Some(sample_log())));
        let (result, out) = run(api, OutputFormat::Json, &list()).await;
        result.unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["last_index"], 12);
        assert_eq!(parsed["entries"][0]["index"], 12);
        assert_eq!(
            parsed["entries"][0]["lines"][0]["perf"]["C: used %"]["value"],
            92
        );
        // Fields this client does not model still make it through.
        assert_eq!(parsed["entries"][0]["diagnostics"]["state"], "disabled");
        assert!(
            parsed["entries"][0]["context"].get("snapshot_id").is_some(),
            "{parsed}"
        );
    }

    #[tokio::test]
    async fn arguments_not_recorded_are_left_out_not_null() {
        let mut log = sample_log();
        log.entries[0].arguments = None;
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_failures()
            .returning(move |_| Ok(Some(log.clone())));
        let (result, out) = run(api, OutputFormat::Json, &list()).await;
        result.unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert!(parsed["entries"][0].get("arguments").is_none(), "{parsed}");
    }

    #[tokio::test]
    async fn unknown_fields_survive_a_round_trip() {
        let mut value = serde_json::to_value(sample_log()).unwrap();
        value["new_envelope_field"] = json!("x");
        value["entries"][0]["new_entry_field"] = json!(1);
        let log: FailuresLog = serde_json::from_value(value).unwrap();
        let back = serde_json::to_value(&log).unwrap();
        assert_eq!(back["new_envelope_field"], "x");
        assert_eq!(back["entries"][0]["new_entry_field"], 1);
    }

    #[tokio::test]
    async fn empty_list_says_whether_the_log_is_on() {
        for (enabled, expected) in [
            (true, "No failed checks\n"),
            (
                false,
                "The failed-check log is disabled on the agent (enabled under [/settings/core/failed checks])\n",
            ),
        ] {
            let mut api = MockApiClientApiImpl::new();
            api.expect_list_failures().returning(move |_| {
                Ok(Some(FailuresLog {
                    enabled,
                    ..FailuresLog::default()
                }))
            });
            let (result, out) = run(api, OutputFormat::Text, &list()).await;
            result.unwrap();
            assert_eq!(out, expected);
        }
    }

    #[tokio::test]
    async fn an_agent_without_the_log_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_failures().returning(|_| Ok(None));
        let (result, _) = run(api, OutputFormat::Text, &list()).await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no failed-check log"), "{err}");
    }

    #[tokio::test]
    async fn list_errors_are_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_failures()
            .returning(|_| Err(anyhow!("boom")));
        let (result, _) = run(api, OutputFormat::Text, &list()).await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "Failed to fetch failures: boom"
        );
    }

    #[tokio::test]
    async fn status_asks_for_one_entry_and_shows_the_configuration() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_failures()
            .withf(|filter| filter.limit == Some(1) && filter.status.is_none())
            .returning(|_| Ok(Some(sample_log())));
        let (result, out) = run(api, OutputFormat::Text, &FailuresCommand::Status {}).await;
        result.unwrap();
        assert!(out.contains("| record arguments    | safe"), "{out}");
        assert!(
            out.contains("| context bytes used  | 72650 of 4194304"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn status_json_leaves_out_the_entries() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_failures()
            .returning(|_| Ok(Some(sample_log())));
        let (result, out) = run(api, OutputFormat::Json, &FailuresCommand::Status {}).await;
        result.unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["enabled"], true);
        assert!(parsed.get("entries").is_none(), "{parsed}");
    }

    #[tokio::test]
    async fn context_text_shows_every_part() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_failure_context()
            .withf(|index| *index == 12)
            .returning(|_| Ok(Some(sample_context())));
        let (result, out) = run(api, OutputFormat::Text, &context(None)).await;
        result.unwrap();
        assert!(
            out.contains("| metrics              | captured (5s before)"),
            "{out}"
        );
        assert!(
            out.contains("| diagnostics sections | processes.top_cpu"),
            "{out}"
        );
        assert!(
            out.contains("| system.cpu.total.idle | 3     | percent | gauge   |"),
            "{out}"
        );
        assert!(
            out.contains(
                "Diagnostics: processes.top_cpu (CheckSystem, captured 2026-10-31T06:57:45Z, truncated)"
            ),
            "{out}"
        );
        assert!(
            out.contains("| name             | cpu (percent) |"),
            "{out}"
        );
        assert!(
            out.contains("| acme-indexer.exe | 87.5          |"),
            "{out}"
        );
        // A cell the row lacks is empty, not "null".
        assert!(
            out.contains("| idle             |               |"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn context_without_a_snapshot_says_why() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_failure_context().returning(|_| {
            Ok(Some(
                serde_json::from_value(json!({
                    "state": "evicted", "snapshot_id": 3, "captured_at": 10, "age_at_failure": 1, "size": 9,
                    "index": 12, "timestamp": 11,
                    "diagnostics": {"state": "disabled"}
                }))
                .unwrap(),
            ))
        });
        let (result, out) = run(api, OutputFormat::Text, &context(None)).await;
        result.unwrap();
        assert!(out.contains("No metrics captured (evicted)"), "{out}");
        assert!(!out.contains("Diagnostics:"), "{out}");
    }

    #[tokio::test]
    async fn context_section_selects_one_diagnostics_table() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_failure_context()
            .returning(|_| Ok(Some(sample_context())));
        let (result, out) = run(api, OutputFormat::Csv, &context(Some("processes.top_cpu"))).await;
        result.unwrap();
        assert_eq!(out, "name,cpu (percent)\nacme-indexer.exe,87.5\nidle,\n\n");
    }

    #[tokio::test]
    async fn context_csv_defaults_to_the_metrics() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_failure_context()
            .returning(|_| Ok(Some(sample_context())));
        let (result, out) = run(api, OutputFormat::Csv, &context(None)).await;
        result.unwrap();
        assert!(
            out.starts_with("metric,value,unit,metric_type,labels,help\n"),
            "{out}"
        );
        assert!(
            out.contains("system.cpu.total.idle,3,percent,gauge,,Idle CPU\n"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn context_json_echoes_the_document() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_failure_context()
            .returning(|_| Ok(Some(sample_context())));
        let (result, out) = run(api, OutputFormat::Json, &context(None)).await;
        result.unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["state"], "captured");
        assert_eq!(parsed["metrics"]["workers.jobs"], 214);
        assert_eq!(parsed["metadata"]["workers.jobs"]["type"], "counter");
        assert_eq!(
            parsed["diagnostics"]["sections"]["processes.top_cpu"]["rows"][0]["cpu"],
            87.5
        );
    }

    #[tokio::test]
    async fn context_json_metrics_section_matches_metrics_show_meta() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_failure_context()
            .returning(|_| Ok(Some(sample_context())));
        let (result, out) = run(api, OutputFormat::Json, &context(Some("metrics"))).await;
        result.unwrap();
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["metrics"]["system.cpu.total.idle"], 3);
        assert_eq!(
            parsed["metadata"]["system.cpu.total.idle"]["unit"],
            "percent"
        );
        assert!(parsed.get("diagnostics").is_none(), "{parsed}");
    }

    #[tokio::test]
    async fn an_unknown_section_lists_the_available_ones() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_failure_context()
            .returning(|_| Ok(Some(sample_context())));
        let (result, _) = run(api, OutputFormat::Text, &context(Some("nope"))).await;
        let err = result.unwrap_err().to_string();
        assert_eq!(
            err,
            "Failed check 12 has no section 'nope' (available: metrics, processes.top_cpu)"
        );
    }

    #[tokio::test]
    async fn a_section_without_diagnostics_says_why() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_failure_context().returning(|_| {
            Ok(Some(
                serde_json::from_value(
                    json!({"state": "none", "index": 12, "diagnostics": {"state": "disabled"}}),
                )
                .unwrap(),
            ))
        });
        let (result, _) = run(api, OutputFormat::Text, &context(Some("x"))).await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "Failed check 12 has no diagnostics (disabled)"
        );
    }

    #[tokio::test]
    async fn a_missing_entry_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_failure_context().returning(|_| Ok(None));
        let (result, _) = run(api, OutputFormat::Text, &context(None)).await;
        let err = result.unwrap_err().to_string();
        assert!(err.starts_with("Failed check 12 not found"), "{err}");
    }

    #[tokio::test]
    async fn clear_reports_what_went() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_clear_failures().returning(|| {
            Ok(
                serde_json::from_value(json!({"enabled": true, "last_index": 12, "removed": 4}))
                    .unwrap(),
            )
        });
        let (result, out) = run(api, OutputFormat::Text, &FailuresCommand::Clear {}).await;
        result.unwrap();
        assert!(out.contains("| removed    | 4  |"), "{out}");
        assert!(out.contains("| last index | 12 |"), "{out}");
    }

    #[tokio::test]
    async fn clear_errors_are_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_clear_failures()
            .returning(|| Err(anyhow!("403")));
        let (result, _) = run(api, OutputFormat::Text, &FailuresCommand::Clear {}).await;
        assert_eq!(
            result.unwrap_err().to_string(),
            "Failed to clear the failed-check log: 403"
        );
    }
}
