use crate::cli::QueriesCommand;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::{ExecuteNagiosResult, ExecuteResult, QueryHelp, QueryResult};
use crate::rendering::Rendering;

/// Route a `queries` sub command.
///
/// Returns the process exit code: `execute-nagios` maps the check result to the Nagios
/// exit code (0-3), everything else returns 0 on success.
pub async fn route_query_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &QueriesCommand,
) -> anyhow::Result<i32> {
    match &command {
        QueriesCommand::List { all, long } => match api.list_queries(all).await {
            Ok(queries) => {
                output.render_rows(&queries, long, &["description"])?;
                Ok(0)
            }
            Err(e) => anyhow::bail!("Failed to fetch queries: {:#}", e),
        },
        &QueriesCommand::Show { id } => match api.get_query(id).await {
            Ok(query) => {
                output.render_single(&query, QueryResult::to_dict)?;
                Ok(0)
            }
            Err(e) => anyhow::bail!("Failed to fetch query {id}: {:#}", e),
        },
        &QueriesCommand::Describe { id, long } => match api.get_query_help(id).await {
            Ok(Some(help)) => {
                render_help(&output, &help, long)?;
                Ok(0)
            }
            // The agent answered, and what it said was "nothing here". It
            // cannot tell an unknown query from an agent without the endpoint,
            // but `queries show` answers on both.
            Ok(None) => anyhow::bail!(
                concat!(
                    "Nothing to describe for query {id}: either no such query, ",
                    "or an agent without the endpoint. `queries show {id}` ",
                    "works on both."
                ),
                id = id
            ),
            // A 404 here is ambiguous: the query may not exist, or the agent
            // may predate the endpoint. `queries show` answers on both.
            Err(e) => anyhow::bail!("Failed to fetch help for query {id}: {:#}", e),
        },
        &QueriesCommand::Execute { id, args } => match api.execute_query(id, args).await {
            Ok(result) => {
                output.render_single(&result, ExecuteResult::to_dict)?;
                Ok(0)
            }
            Err(e) => anyhow::bail!("Failed to execute query {id}: {:#}", e),
        },
        &QueriesCommand::ExecuteNagios { id, args } => {
            match api.execute_query_nagios(id, args).await {
                Ok(result) => {
                    if output.is_text() {
                        for line in &result.lines {
                            output.print(&line.render_nagios());
                        }
                    } else {
                        output.render_single(&result, ExecuteNagiosResult::to_dict)?;
                    }
                    Ok(result.get_exit_code())
                }
                Err(e) => anyhow::bail!("Failed to execute query {id}: {:#}", e),
            }
        }
    }
}

/// Render what a check accepts.
///
/// The answer has two halves that do not share a shape -- the options a check
/// takes and the filter keywords it offers -- so the flat formats get a table
/// each rather than one table with half its columns empty. json and yaml get
/// the document as the agent sent it, halves intact.
///
/// `long` reveals the full descriptions, which for an option run to several
/// lines; the table otherwise shows the summary line the interactive prompt
/// shows.
fn render_help(output: &Rendering, help: &QueryHelp, long: &bool) -> anyhow::Result<()> {
    if !output.is_flat() {
        return output.render_nested_single(help);
    }
    // Two tables with different columns cannot share one csv: the second set of
    // headers would arrive mid-file and every reader would choke on it. Refused
    // rather than written out wrong, the way the nested renderers refuse text.
    if !output.is_text() {
        anyhow::bail!(
            "csv cannot hold both halves of this answer (the options and the filter keywords              are different shapes); use --output json or --output yaml"
        );
    }
    output.print(&format!("Options for {}:", help.name));
    output.render_rows(&help.parameters, long, &["details"])?;
    // An alias declares no keywords of its own -- the list belongs to the
    // command it stands for, and saying so is the difference between "this
    // check has none" and "look at that one instead".
    let source = if help.keyword_source.is_empty() || help.keyword_source == help.name {
        String::new()
    } else {
        format!(" (from {})", help.keyword_source)
    };
    output.print(&format!(
        "
Filter keywords{source}:"
    ));
    if help.fields.is_empty() {
        output.print("  none: this check is not filter based.");
        return Ok(());
    }
    output.render_rows(&help.fields, long, &["details"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::nsclient::messages::{
        ExecuteLine, ExecuteNagiosLine, ListQueriesResult, PerfData, QueryField, QueryHelp,
        QueryParameter,
    };
    use crate::rendering::StringRender;
    use anyhow::anyhow;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    fn rendering(format: OutputFormat) -> (Rendering, Rc<RefCell<String>>) {
        let output_sink = Box::new(StringRender::new());
        let output_ref = output_sink.string.clone();
        (
            Rendering::new(format, OutputStyle::Rounded, false, output_sink),
            output_ref,
        )
    }

    fn sample_query() -> ListQueriesResult {
        ListQueriesResult {
            name: "check_cpu".into(),
            title: "Check CPU".into(),
            description: "Checks the CPU".into(),
            plugin: "CheckSystem".into(),
            experimental: false,
        }
    }

    #[tokio::test]
    async fn list_text_shows_name_by_default_and_hides_description() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_queries()
            .withf(|all| !*all)
            .returning(|_| Ok(vec![sample_query()]));
        let (output, output_ref) = rendering(OutputFormat::Text);

        route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::List {
                all: false,
                long: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            output_ref.borrow().as_str(),
            r#"╭───────────┬───────────┬─────────────┬──────────────╮
│ name      │ title     │ plugin      │ experimental │
├───────────┼───────────┼─────────────┼──────────────┤
│ check_cpu │ Check CPU │ CheckSystem │              │
╰───────────┴───────────┴─────────────┴──────────────╯
"#
        );
    }

    #[tokio::test]
    async fn list_long_shows_description() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_queries()
            .withf(|all| *all)
            .returning(|_| Ok(vec![sample_query()]));
        let (output, output_ref) = rendering(OutputFormat::Text);

        route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::List {
                all: true,
                long: true,
            },
        )
        .await
        .unwrap();

        assert!(output_ref.borrow().contains("Checks the CPU"));
    }

    #[tokio::test]
    async fn show_yaml_includes_metadata() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_query()
            .withf(|id| id == "check_cpu")
            .returning(|_| {
                Ok(QueryResult {
                    name: "check_cpu".into(),
                    title: "Check CPU".into(),
                    description: "Checks the CPU".into(),
                    plugin: "CheckSystem".into(),
                    experimental: true,
                    metadata: HashMap::from([("k".to_string(), "v".to_string())]),
                })
            });
        let (output, output_ref) = rendering(OutputFormat::Yaml);

        route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Show {
                id: "check_cpu".into(),
            },
        )
        .await
        .unwrap();

        let rendered = output_ref.borrow();
        assert!(rendered.contains("name: check_cpu"), "{rendered}");
        assert!(rendered.contains("  k: v"), "{rendered}");
    }

    fn sample_help(name: &str, keyword_source: &str, fields: Vec<QueryField>) -> QueryHelp {
        QueryHelp {
            name: name.into(),
            keyword_source: keyword_source.into(),
            parameters: vec![QueryParameter {
                name: "show-all".into(),
                default_value: String::new(),
                required: false,
                repeatable: false,
                content_type: "bool".into(),
                short_description: "Show all items.".into(),
                long_description: "Show all items.
Even the boring ones."
                    .into(),
            }],
            fields,
        }
    }

    fn free_space() -> Vec<QueryField> {
        vec![QueryField {
            name: "free".into(),
            short_description: "Free space".into(),
            long_description: "Free disk space on the drive".into(),
        }]
    }

    #[tokio::test]
    async fn describe_text_lists_options_then_keywords() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_query_help()
            .withf(|id| id == "check_drivesize")
            .returning(|_| {
                Ok(Some(sample_help(
                    "check_drivesize",
                    "check_drivesize",
                    free_space(),
                )))
            });
        let (output, out) = rendering(OutputFormat::Text);

        route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Describe {
                id: "check_drivesize".into(),
                long: false,
            },
        )
        .await
        .unwrap();

        let rendered = out.borrow();
        assert!(
            rendered.contains("Options for check_drivesize:"),
            "{rendered}"
        );
        assert!(rendered.contains("show-all"), "{rendered}");
        assert!(rendered.contains("Filter keywords:"), "{rendered}");
        assert!(rendered.contains("free"), "{rendered}");
        // The long description is behind --long.
        assert!(!rendered.contains("Even the boring ones."), "{rendered}");
    }

    #[tokio::test]
    async fn describe_long_reveals_the_full_descriptions() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_query_help().returning(|_| {
            Ok(Some(sample_help(
                "check_drivesize",
                "check_drivesize",
                free_space(),
            )))
        });
        let (output, out) = rendering(OutputFormat::Text);

        route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Describe {
                id: "check_drivesize".into(),
                long: true,
            },
        )
        .await
        .unwrap();

        assert!(
            out.borrow().contains("Even the boring ones."),
            "{}",
            out.borrow()
        );
    }

    #[tokio::test]
    async fn describe_names_the_command_an_alias_borrows_its_keywords_from() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_query_help().returning(|_| {
            Ok(Some(sample_help(
                "alias_disk",
                "check_drivesize",
                free_space(),
            )))
        });
        let (output, out) = rendering(OutputFormat::Text);

        route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Describe {
                id: "alias_disk".into(),
                long: false,
            },
        )
        .await
        .unwrap();

        assert!(
            out.borrow()
                .contains("Filter keywords (from check_drivesize):"),
            "{}",
            out.borrow()
        );
    }

    #[tokio::test]
    async fn describe_says_so_when_a_check_is_not_filter_based() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_query_help()
            .returning(|_| Ok(Some(sample_help("check_ok", "check_ok", vec![]))));
        let (output, out) = rendering(OutputFormat::Text);

        route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Describe {
                id: "check_ok".into(),
                long: false,
            },
        )
        .await
        .unwrap();

        assert!(
            out.borrow()
                .contains("none: this check is not filter based."),
            "{}",
            out.borrow()
        );
    }

    #[tokio::test]
    async fn describe_json_keeps_the_two_halves_apart() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_query_help().returning(|_| {
            Ok(Some(sample_help(
                "check_drivesize",
                "check_drivesize",
                free_space(),
            )))
        });
        let (output, out) = rendering(OutputFormat::Json);

        route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Describe {
                id: "check_drivesize".into(),
                long: false,
            },
        )
        .await
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed["parameters"][0]["content_type"], "bool");
        assert_eq!(parsed["fields"][0]["name"], "free");
        assert_eq!(parsed["keyword_source"], "check_drivesize");
    }

    #[tokio::test]
    async fn describe_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_query_help()
            .returning(|_| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Describe {
                id: "check_cpu".into(),
                long: false,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Failed to fetch help for query check_cpu: boom"
        );
    }

    #[tokio::test]
    async fn describe_reports_a_query_the_agent_has_nothing_to_say_about() {
        let mut api = MockApiClientApiImpl::new();
        // `Ok(None)` is the agent's 404, which is an answer rather than a
        // failure and reads differently from one.
        api.expect_get_query_help().returning(|_| Ok(None));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Describe {
                id: "check_cpu".into(),
                long: false,
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Nothing to describe for query check_cpu"),
            "{err}"
        );
        assert!(err.to_string().contains("queries show check_cpu"), "{err}");
    }

    #[tokio::test]
    async fn describe_refuses_csv_rather_than_writing_two_headers_into_one_file() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_query_help()
            .returning(|_| Ok(Some(sample_help("check_cpu", "check_cpu", free_space()))));
        let (output, _) = rendering(OutputFormat::Csv);

        let err = route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Describe {
                id: "check_cpu".into(),
                long: false,
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("csv cannot hold both halves"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn execute_text_renders_output_and_perf() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_execute_query()
            .withf(|id, args| {
                id == "check_cpu" && args == [("warning".to_string(), "80".to_string())]
            })
            .returning(|_, _| {
                Ok(ExecuteResult {
                    command: "check_cpu".into(),
                    lines: vec![ExecuteLine {
                        message: "OK: CPU load is ok.".into(),
                        perf: HashMap::from([(
                            "total 5m".to_string(),
                            PerfData {
                                value: Some(3.0),
                                unit: Some("%".into()),
                                warning: Some(80.0),
                                critical: Some(90.0),
                                minimum: None,
                                maximum: None,
                            },
                        )]),
                    }],
                    result: 0,
                })
            });
        let (output, output_ref) = rendering(OutputFormat::Text);

        route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Execute {
                id: "check_cpu".into(),
                args: vec![("warning".into(), "80".into())],
            },
        )
        .await
        .unwrap();

        let rendered = output_ref.borrow();
        assert!(rendered.contains("│ command  │ check_cpu"), "{rendered}");
        assert!(
            rendered.contains("│ output   │ OK: CPU load is ok."),
            "{rendered}"
        );
        assert!(
            rendered.contains("│ total 5m │ 3%, warning: 80, critical: 90"),
            "{rendered}"
        );
        assert!(rendered.contains("│ result   │ OK"), "{rendered}");
    }

    fn nagios_result(status: &str) -> ExecuteNagiosResult {
        ExecuteNagiosResult {
            command: "check_cpu".into(),
            lines: vec![
                ExecuteNagiosLine {
                    message: "WARNING: CPU load is high.".into(),
                    perf: "'total 5m'=85%;80;90".into(),
                },
                ExecuteNagiosLine {
                    message: "second line".into(),
                    perf: String::new(),
                },
            ],
            result: status.into(),
        }
    }

    #[tokio::test]
    async fn execute_nagios_text_prints_nagios_format_and_returns_exit_code() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_execute_query_nagios()
            .withf(|id, args| id == "check_cpu" && args.is_empty())
            .returning(|_, _| Ok(nagios_result("WARNING")));
        let (output, output_ref) = rendering(OutputFormat::Text);

        let code = route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::ExecuteNagios {
                id: "check_cpu".into(),
                args: vec![],
            },
        )
        .await
        .unwrap();

        assert_eq!(code, 1);
        assert_eq!(
            output_ref.borrow().as_str(),
            "WARNING: CPU load is high.|'total 5m'=85%;80;90\nsecond line\n"
        );
    }

    #[tokio::test]
    async fn execute_nagios_json_returns_exit_code() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_execute_query_nagios()
            .returning(|_, _| Ok(nagios_result("CRITICAL")));
        let (output, output_ref) = rendering(OutputFormat::Json);

        let code = route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::ExecuteNagios {
                id: "check_cpu".into(),
                args: vec![],
            },
        )
        .await
        .unwrap();

        assert_eq!(code, 2);
        let parsed: serde_json::Value = serde_json::from_str(&output_ref.borrow()).unwrap();
        assert_eq!(parsed["result"], "CRITICAL");
        assert_eq!(parsed["lines"][0]["perf"], "'total 5m'=85%;80;90");
    }

    #[tokio::test]
    async fn successful_commands_return_zero() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_queries().returning(|_| Ok(vec![]));
        let (output, _) = rendering(OutputFormat::Json);
        let code = route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::List {
                all: false,
                long: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn execute_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_execute_query()
            .returning(|_, _| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_query_commands(
            output,
            Box::new(api),
            &QueriesCommand::Execute {
                id: "check_cpu".into(),
                args: vec![],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Failed to execute query check_cpu: boom");
    }
}
