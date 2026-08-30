use crate::cli::QueriesCommand;
use crate::nsclient::api::ApiClientApi;
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
                if output.is_flat() {
                    output.render_flat_list(&queries, long, &["description"])?;
                } else {
                    output.render_nested_list(&queries)?;
                }
                Ok(0)
            }
            Err(e) => anyhow::bail!("Failed to fetch queries: {:#}", e),
        },
        &QueriesCommand::Show { id } => match api.get_query(id).await {
            Ok(query) => {
                if output.is_flat() {
                    output.render_flat_single(&query.to_dict())?;
                } else {
                    output.render_nested_single(&query)?;
                }
                Ok(0)
            }
            Err(e) => anyhow::bail!("Failed to fetch query {id}: {:#}", e),
        },
        &QueriesCommand::Execute { id, args } => match api.execute_query(id, args).await {
            Ok(result) => {
                if output.is_flat() {
                    output.render_flat_single(&result.to_dict())?;
                } else {
                    output.render_nested_single(&result)?;
                }
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
                    } else if output.is_flat() {
                        output.render_flat_single(&result.to_dict())?;
                    } else {
                        output.render_nested_single(&result)?;
                    }
                    Ok(result.get_exit_code())
                }
                Err(e) => anyhow::bail!("Failed to execute query {id}: {:#}", e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::nsclient::messages::{
        ExecuteLine, ExecuteNagiosLine, ExecuteNagiosResult, ExecuteResult, ListQueriesResult,
        PerfData, QueryResult,
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
            r#"╭───────────┬───────────┬─────────────╮
│ name      │ title     │ plugin      │
├───────────┼───────────┼─────────────┤
│ check_cpu │ Check CPU │ CheckSystem │
╰───────────┴───────────┴─────────────╯
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
