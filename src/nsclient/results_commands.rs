use crate::cli::{FeedTargetArgs, ResultFilterArgs, ResultsCommand};
use crate::nagios;
use crate::nagios::PassiveResult;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::{
    CACHED_RESULT_LONG_COLUMNS, CachedResult, ResultFilter, ResultsRemoved,
};
use crate::rendering::Rendering;
use serde::Serialize;
use std::path::Path;

impl From<&ResultFilterArgs> for ResultFilter {
    fn from(args: &ResultFilterArgs) -> Self {
        ResultFilter {
            channel: args.channel.clone(),
            host: args.host.clone(),
            command: args.command.clone(),
            alias: args.alias.clone(),
            status: args.status.clone(),
        }
    }
}

/// Route a `results` sub command.
///
/// Returns the process exit code: `feed` behaves as a Nagios plugin (0-3, with
/// its diagnosis on stdout), everything else returns 0 on success.
pub async fn route_results_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &ResultsCommand,
) -> anyhow::Result<i32> {
    match command {
        ResultsCommand::List { filter, long } => {
            match api.list_results(&ResultFilter::from(filter)).await {
                Ok(results) => {
                    output.render_list(
                        &results,
                        CachedResult::to_flat,
                        long,
                        CACHED_RESULT_LONG_COLUMNS,
                    )?;
                    Ok(0)
                }
                Err(e) => anyhow::bail!("Failed to fetch results: {:#}", e),
            }
        }
        ResultsCommand::Show { key } => match api.get_result(key).await {
            Ok(result) => {
                output.render_single(&result, CachedResult::to_dict)?;
                Ok(0)
            }
            Err(e) => anyhow::bail!("Failed to fetch result {key}: {:#}", e),
        },
        ResultsCommand::Delete { key } => match api.delete_result(key).await {
            Ok(removed) => {
                output.render_single(&removed, ResultsRemoved::to_dict)?;
                Ok(0)
            }
            Err(e) => anyhow::bail!("Failed to delete result {key}: {:#}", e),
        },
        ResultsCommand::Clear {} => match api.clear_results().await {
            Ok(removed) => {
                output.render_single(&removed, ResultsRemoved::to_dict)?;
                Ok(0)
            }
            Err(e) => anyhow::bail!("Failed to clear results: {:#}", e),
        },
        ResultsCommand::Feed {
            target,
            filter,
            nagios_host,
            service,
            max_age,
            worst,
        } => {
            let options = FeedOptions {
                target,
                filter: ResultFilter::from(filter),
                nagios_host: nagios_host.clone(),
                service_template: service.clone(),
                max_age: *max_age,
                worst: *worst,
            };
            Ok(feed(&output, api.as_ref(), &options).await)
        }
    }
}

struct FeedOptions<'a> {
    target: &'a FeedTargetArgs,
    filter: ResultFilter,
    nagios_host: Option<String>,
    service_template: String,
    max_age: i64,
    worst: bool,
}

/// One result as it was handed to Nagios.
#[derive(Debug, Serialize)]
struct FedResult {
    key: String,
    host: String,
    service: String,
    status: String,
    output: String,
    stale: bool,
}

/// What `feed` did, rendered as the plugin output (text) or as a document.
#[derive(Debug, Serialize)]
struct FeedSummary {
    status: String,
    exit_code: i32,
    message: String,
    fed: usize,
    ok: usize,
    warning: usize,
    critical: usize,
    unknown: usize,
    stale: usize,
    skipped: Vec<String>,
    results: Vec<FedResult>,
}

/// Poll the cache and hand every result to Nagios.
///
/// Everything that goes wrong is reported the way a Nagios plugin reports it:
/// an `UNKNOWN:` line on stdout and exit code 3, never an `Err`.
async fn feed(output: &Rendering, api: &dyn ApiClientApi, options: &FeedOptions<'_>) -> i32 {
    match feed_inner(output, api, options).await {
        Ok(summary) => {
            if output.is_flat() {
                // The dry-run command lines were already printed by feed_inner.
                output.print(&summary.message);
            } else if let Err(e) = output.render_nested_single(&summary) {
                output.print(&format!("UNKNOWN: Failed to render summary: {e:#}"));
                return 3;
            }
            summary.exit_code
        }
        Err(e) => {
            output.print(&format!("UNKNOWN: {e:#}"));
            3
        }
    }
}

async fn feed_inner(
    output: &Rendering,
    api: &dyn ApiClientApi,
    options: &FeedOptions<'_>,
) -> anyhow::Result<FeedSummary> {
    // Validate everything that can fail locally *before* polling: with the
    // server's default `clear on poll = true` a poll consumes the results, so
    // failing afterwards would lose them.
    validate_template(&options.service_template)?;
    if let Some(file) = &options.target.command_file {
        nagios::check_command_file(Path::new(file))?;
    }
    if let Some(dir) = &options.target.spool_dir {
        nagios::check_spool_dir(Path::new(dir))?;
    }

    let cached = api
        .list_results(&options.filter)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to fetch results: {e:#}"))?;

    let now = nagios::now();
    let mut passive = Vec::with_capacity(cached.len());
    let mut fed = Vec::with_capacity(cached.len());
    let mut skipped = Vec::new();
    let mut counts = [0usize; 4];
    let mut stale = 0usize;
    for result in &cached {
        let host = options
            .nagios_host
            .clone()
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| {
                if result.host.is_empty() {
                    result.source.clone()
                } else {
                    result.host.clone()
                }
            });
        if host.is_empty() {
            skipped.push(format!("{}: no host name (pass --nagios-host)", result.key));
            continue;
        }
        let service = expand_template(&options.service_template, result)?;
        if service.trim().is_empty() {
            skipped.push(format!("{}: empty service description", result.key));
            continue;
        }
        let is_stale = options.max_age > 0 && result.age > options.max_age;
        let (status, output_text, timestamp) = if is_stale {
            stale += 1;
            (
                3,
                format!(
                    "UNKNOWN: stale result, last reported {}s ago ({}): {}",
                    result.age,
                    result.last_seen_date,
                    result.nagios_output()
                ),
                now,
            )
        } else {
            (
                result.status,
                result.nagios_output(),
                result_timestamp(result, now),
            )
        };
        let status = if (0..=3).contains(&status) { status } else { 3 };
        counts[status as usize] += 1;
        let entry = PassiveResult {
            host,
            service,
            status,
            output: output_text,
            timestamp,
        };
        fed.push(FedResult {
            key: result.key.clone(),
            host: entry.host.clone(),
            service: entry.service.clone(),
            status: status_name(status),
            output: entry.output.clone(),
            stale: is_stale,
        });
        passive.push(entry);
    }

    if options.target.dry_run {
        if output.is_flat() {
            for entry in &passive {
                output.print(&nagios::command_line(entry));
            }
        }
    } else if let Some(file) = &options.target.command_file {
        nagios::write_command_file(Path::new(file), &passive)?;
    } else if let Some(dir) = &options.target.spool_dir {
        nagios::write_spool_file(Path::new(dir), &passive, now)?;
    }

    let worst_status = passive.iter().map(|r| r.status).max().unwrap_or(0);
    let exit_code = if options.worst { worst_status } else { 0 };
    let mut message = format!(
        "{}: Fed {} result(s) to Nagios: {} ok, {} warning, {} critical, {} unknown",
        status_name(exit_code),
        passive.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3]
    );
    if stale > 0 {
        message.push_str(&format!(" ({stale} stale)"));
    }
    if !skipped.is_empty() {
        message.push_str(&format!(
            ", skipped {}: {}",
            skipped.len(),
            skipped.join("; ")
        ));
    }
    if options.target.dry_run {
        message.push_str(" [dry run]");
    }
    message.push_str(&format!(
        "|fed={} ok={} warning={} critical={} unknown={} stale={}",
        passive.len(),
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        stale
    ));

    Ok(FeedSummary {
        status: status_name(exit_code),
        exit_code,
        message,
        fed: passive.len(),
        ok: counts[0],
        warning: counts[1],
        critical: counts[2],
        unknown: counts[3],
        stale,
        skipped,
        results: fed,
    })
}

/// When the served result was produced, falling back to when the key last
/// reported and finally to now.
fn result_timestamp(result: &CachedResult, now: i64) -> i64 {
    if result.result_seen > 0 {
        result.result_seen
    } else if result.last_seen > 0 {
        result.last_seen
    } else {
        now
    }
}

fn status_name(status: i32) -> String {
    match status {
        0 => "OK",
        1 => "WARNING",
        2 => "CRITICAL",
        _ => "UNKNOWN",
    }
    .to_string()
}

const TEMPLATE_VARIABLES: &[&str] = &[
    "alias-or-command",
    "alias",
    "command",
    "host",
    "source",
    "channel",
    "key",
];

/// Fail early on a template naming a variable that does not exist.
fn validate_template(template: &str) -> anyhow::Result<()> {
    expand_template(template, &CachedResult::default()).map(|_| ())
}

/// Replace every `${variable}` in `template` with the matching field of `result`.
fn expand_template(template: &str, result: &CachedResult) -> anyhow::Result<String> {
    let mut expanded = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            anyhow::bail!("Unterminated variable in service template '{template}'");
        };
        let name = &after[..end];
        let value = match name {
            "alias-or-command" => result.alias_or_command(),
            "alias" => &result.alias,
            "command" => &result.command,
            "host" => &result.host,
            "source" => &result.source,
            "channel" => &result.channel,
            "key" => &result.key,
            _ => anyhow::bail!(
                "Unknown variable ${{{name}}} in service template '{template}' (expected one of: {})",
                TEMPLATE_VARIABLES
                    .iter()
                    .map(|v| format!("${{{v}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        expanded.push_str(value);
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    Ok(expanded)
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

    fn cached(key: &str, alias: &str, status: i32, message: &str, perf: &str) -> CachedResult {
        CachedResult {
            key: key.into(),
            channel: "WEB".into(),
            host: "srv1".into(),
            source: "srv1".into(),
            command: "check_drivesize".into(),
            alias: alias.into(),
            status,
            result: status_name(status),
            message: message.into(),
            perf: perf.into(),
            count: 3,
            first_seen: 1757145000,
            last_seen: 1757145900,
            result_seen: 1757145900,
            first_seen_date: "2026-09-06 10:30:00".into(),
            last_seen_date: "2026-09-06 10:45:00".into(),
            result_seen_date: "2026-09-06 10:45:00".into(),
            age: 12,
            ..Default::default()
        }
    }

    fn samples() -> Vec<CachedResult> {
        vec![
            cached(
                "srv1/Disk C",
                "Disk C",
                1,
                "WARNING: 85% used",
                "'C:'=85%;80;90",
            ),
            cached("srv1/check_drivesize", "", 0, "OK: all fine", ""),
        ]
    }

    fn feed_command(target: FeedTargetArgs) -> ResultsCommand {
        ResultsCommand::Feed {
            target,
            filter: ResultFilterArgs::default(),
            nagios_host: None,
            service: "${alias-or-command}".into(),
            max_age: 0,
            worst: false,
        }
    }

    fn dry_run() -> FeedTargetArgs {
        FeedTargetArgs {
            dry_run: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn list_text_shows_the_short_columns_and_passes_the_filter() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results()
            .withf(|filter| {
                *filter
                    == ResultFilter {
                        status: Some("warning,critical".into()),
                        ..Default::default()
                    }
            })
            .returning(|_| Ok(samples()));
        let (output, out) = rendering(OutputFormat::Text);

        let code = route_results_commands(
            output,
            Box::new(api),
            &ResultsCommand::List {
                filter: ResultFilterArgs {
                    status: Some("warning,critical".into()),
                    ..Default::default()
                },
                long: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(code, 0);
        assert_eq!(
            out.borrow().as_str(),
            "| key                  | result  | age | message           |\n\
             |----------------------|---------|-----|-------------------|\n\
             | srv1/Disk C          | WARNING | 12  | WARNING: 85% used |\n\
             | srv1/check_drivesize | OK      | 12  | OK: all fine      |\n"
        );
    }

    #[tokio::test]
    async fn list_long_shows_every_column() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results().returning(|_| Ok(samples()));
        let (output, out) = rendering(OutputFormat::Text);

        route_results_commands(
            output,
            Box::new(api),
            &ResultsCommand::List {
                filter: ResultFilterArgs::default(),
                long: true,
            },
        )
        .await
        .unwrap();

        let rendered = out.borrow();
        assert!(rendered.contains("| perf "), "{rendered}");
        assert!(rendered.contains("'C:'=85%;80;90"), "{rendered}");
        assert!(rendered.contains("2026-09-06 10:45:00"), "{rendered}");
    }

    #[tokio::test]
    async fn list_json_keeps_the_server_document() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results().returning(|_| Ok(samples()));
        let (output, out) = rendering(OutputFormat::Json);

        route_results_commands(
            output,
            Box::new(api),
            &ResultsCommand::List {
                filter: ResultFilterArgs::default(),
                long: false,
            },
        )
        .await
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["key"], "srv1/Disk C");
        assert_eq!(parsed[0]["status"], 1);
        assert_eq!(parsed[0]["result_seen"], 1757145900);
    }

    #[tokio::test]
    async fn show_renders_a_key_value_table() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_result()
            .withf(|key| key == "srv1/Disk C")
            .returning(|_| Ok(samples().remove(0)));
        let (output, out) = rendering(OutputFormat::Text);

        route_results_commands(
            output,
            Box::new(api),
            &ResultsCommand::Show {
                key: "srv1/Disk C".into(),
            },
        )
        .await
        .unwrap();

        let rendered = out.borrow();
        assert!(
            rendered.contains("| key         | srv1/Disk C"),
            "{rendered}"
        );
        assert!(rendered.contains("| result      | WARNING"), "{rendered}");
        assert!(
            rendered.contains("| last seen   | 2026-09-06 10:45:00"),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn delete_and_clear_report_the_removed_count() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_delete_result()
            .withf(|key| key == "srv1/Disk C")
            .returning(|_| Ok(ResultsRemoved { removed: 1 }));
        let (output, out) = rendering(OutputFormat::Text);
        route_results_commands(
            output,
            Box::new(api),
            &ResultsCommand::Delete {
                key: "srv1/Disk C".into(),
            },
        )
        .await
        .unwrap();
        assert!(out.borrow().contains("| removed | 1 |"), "{}", out.borrow());

        let mut api = MockApiClientApiImpl::new();
        api.expect_clear_results()
            .returning(|| Ok(ResultsRemoved { removed: 12 }));
        let (output, out) = rendering(OutputFormat::Json);
        route_results_commands(output, Box::new(api), &ResultsCommand::Clear {})
            .await
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed["removed"], 12);
    }

    #[tokio::test]
    async fn errors_are_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results()
            .returning(|_| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);
        let err = route_results_commands(
            output,
            Box::new(api),
            &ResultsCommand::List {
                filter: ResultFilterArgs::default(),
                long: false,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch results: boom");
    }

    #[tokio::test]
    async fn feed_dry_run_prints_the_commands_and_a_summary() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results().returning(|_| Ok(samples()));
        let (output, out) = rendering(OutputFormat::Text);

        let code = route_results_commands(output, Box::new(api), &feed_command(dry_run()))
            .await
            .unwrap();

        assert_eq!(code, 0);
        assert_eq!(
            out.borrow().as_str(),
            "[1757145900] PROCESS_SERVICE_CHECK_RESULT;srv1;Disk C;1;WARNING: 85% used|'C:'=85%;80;90\n\
             [1757145900] PROCESS_SERVICE_CHECK_RESULT;srv1;check_drivesize;0;OK: all fine\n\
             OK: Fed 2 result(s) to Nagios: 1 ok, 1 warning, 0 critical, 0 unknown [dry run]|fed=2 ok=1 warning=1 critical=0 unknown=0 stale=0\n"
        );
    }

    #[tokio::test]
    async fn feed_writes_to_the_command_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nagios.cmd");
        std::fs::write(&file, "").unwrap();
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results().returning(|_| Ok(samples()));
        let (output, out) = rendering(OutputFormat::Text);

        let code = route_results_commands(
            output,
            Box::new(api),
            &ResultsCommand::Feed {
                target: FeedTargetArgs {
                    command_file: Some(file.to_string_lossy().into_owned()),
                    ..Default::default()
                },
                filter: ResultFilterArgs::default(),
                nagios_host: Some("nagios-name".into()),
                service: "NSClient ${alias-or-command}".into(),
                max_age: 0,
                worst: true,
            },
        )
        .await
        .unwrap();

        assert_eq!(code, 1, "--worst returns the worst fed state");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "[1757145900] PROCESS_SERVICE_CHECK_RESULT;nagios-name;NSClient Disk C;1;WARNING: 85% used|'C:'=85%;80;90\n\
             [1757145900] PROCESS_SERVICE_CHECK_RESULT;nagios-name;NSClient check_drivesize;0;OK: all fine\n"
        );
        assert!(
            out.borrow()
                .starts_with("WARNING: Fed 2 result(s) to Nagios: 1 ok, 1 warning"),
            "{}",
            out.borrow()
        );
    }

    #[tokio::test]
    async fn feed_writes_a_spool_file_with_marker() {
        let dir = tempfile::tempdir().unwrap();
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results().returning(|_| Ok(samples()));
        let (output, out) = rendering(OutputFormat::Json);

        let code = route_results_commands(
            output,
            Box::new(api),
            &feed_command(FeedTargetArgs {
                spool_dir: Some(dir.path().to_string_lossy().into_owned()),
                ..Default::default()
            }),
        )
        .await
        .unwrap();

        assert_eq!(code, 0);
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names.len(), 2, "{names:?}");
        assert_eq!(names[1], format!("{}.ok", names[0]));
        let content = std::fs::read_to_string(dir.path().join(&names[0])).unwrap();
        assert!(
            content.contains("service_description=Disk C\n"),
            "{content}"
        );
        assert!(content.contains("return_code=1\noutput=WARNING: 85% used|'C:'=85%;80;90\n"));

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed["status"], "OK");
        assert_eq!(parsed["fed"], 2);
        assert_eq!(parsed["results"][0]["service"], "Disk C");
    }

    #[tokio::test]
    async fn feed_marks_old_results_as_stale() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results().returning(|_| {
            let mut old = samples().remove(1);
            old.age = 7200;
            Ok(vec![old])
        });
        let (output, out) = rendering(OutputFormat::Text);

        let code = route_results_commands(
            output,
            Box::new(api),
            &ResultsCommand::Feed {
                target: dry_run(),
                filter: ResultFilterArgs::default(),
                nagios_host: None,
                service: "${alias-or-command}".into(),
                max_age: 3600,
                worst: true,
            },
        )
        .await
        .unwrap();

        assert_eq!(code, 3);
        let rendered = out.borrow();
        assert!(
            rendered.contains(
                "PROCESS_SERVICE_CHECK_RESULT;srv1;check_drivesize;3;UNKNOWN: stale result, last reported 7200s ago (2026-09-06 10:45:00): OK: all fine"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains("0 ok, 0 warning, 0 critical, 1 unknown (1 stale)"),
            "{rendered}"
        );
    }

    #[tokio::test]
    async fn feed_validates_before_polling_so_nothing_is_drained() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results().times(0);
        let (output, out) = rendering(OutputFormat::Text);

        let code = route_results_commands(
            output,
            Box::new(api),
            &ResultsCommand::Feed {
                target: dry_run(),
                filter: ResultFilterArgs::default(),
                nagios_host: None,
                service: "${nope}".into(),
                max_age: 0,
                worst: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(code, 3);
        assert!(
            out.borrow()
                .starts_with("UNKNOWN: Unknown variable ${nope}"),
            "{}",
            out.borrow()
        );

        let dir = tempfile::tempdir().unwrap();
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results().times(0);
        let (output, out) = rendering(OutputFormat::Text);
        let code = route_results_commands(
            output,
            Box::new(api),
            &feed_command(FeedTargetArgs {
                command_file: Some(
                    dir.path()
                        .join("missing.cmd")
                        .to_string_lossy()
                        .into_owned(),
                ),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        assert_eq!(code, 3);
        assert!(
            out.borrow()
                .starts_with("UNKNOWN: Cannot access command file"),
            "{}",
            out.borrow()
        );
    }

    #[tokio::test]
    async fn feed_reports_api_errors_as_unknown() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results()
            .returning(|_| Err(anyhow!("503 Service Unavailable: cache disabled")));
        let (output, out) = rendering(OutputFormat::Text);

        let code = route_results_commands(output, Box::new(api), &feed_command(dry_run()))
            .await
            .unwrap();

        assert_eq!(code, 3);
        assert_eq!(
            out.borrow().as_str(),
            "UNKNOWN: Failed to fetch results: 503 Service Unavailable: cache disabled\n"
        );
    }

    #[tokio::test]
    async fn feed_skips_results_without_a_host_and_says_so() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_results().returning(|_| {
            let mut nameless = samples().remove(1);
            nameless.host.clear();
            nameless.source.clear();
            Ok(vec![nameless])
        });
        let (output, out) = rendering(OutputFormat::Text);

        let code = route_results_commands(output, Box::new(api), &feed_command(dry_run()))
            .await
            .unwrap();

        assert_eq!(code, 0);
        assert_eq!(
            out.borrow().as_str(),
            "OK: Fed 0 result(s) to Nagios: 0 ok, 0 warning, 0 critical, 0 unknown, skipped 1: srv1/check_drivesize: no host name (pass --nagios-host) [dry run]|fed=0 ok=0 warning=0 critical=0 unknown=0 stale=0\n"
        );
    }

    #[test]
    fn template_expands_every_variable() {
        let result = samples().remove(0);
        assert_eq!(
            expand_template(
                "${host}|${source}|${channel}|${command}|${alias}|${alias-or-command}|${key}|x",
                &result
            )
            .unwrap(),
            "srv1|srv1|WEB|check_drivesize|Disk C|Disk C|srv1/Disk C|x"
        );
        assert_eq!(expand_template("plain", &result).unwrap(), "plain");
        assert!(expand_template("${open", &result).is_err());
        assert!(validate_template("${bogus}").is_err());
        assert!(validate_template("${alias-or-command}").is_ok());
    }
}
