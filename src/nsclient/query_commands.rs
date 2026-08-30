use crate::cli::QueriesCommand;
use crate::nsclient::api::ApiClientApi;
use crate::rendering::Rendering;

pub async fn route_query_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &QueriesCommand,
) -> anyhow::Result<()> {
    match &command {
        QueriesCommand::List { all, long } => match api.list_queries(all).await {
            Ok(queries) => {
                if output.is_flat() {
                    output.render_flat_list(&queries, long, &["description"])
                } else {
                    output.render_nested_list(&queries)
                }
            }
            Err(e) => anyhow::bail!("Failed to fetch queries: {:#}", e),
        },
        &QueriesCommand::Show { id } => match api.get_query(id).await {
            Ok(query) => {
                if output.is_flat() {
                    output.render_flat_single(&query.to_dict())
                } else {
                    output.render_nested_single(&query)
                }
            }
            Err(e) => anyhow::bail!("Failed to fetch query {id}: {:#}", e),
        },
        &QueriesCommand::Execute { id, args } => match api.execute_query(id, args).await {
            Ok(result) => {
                if output.is_flat() {
                    output.render_flat_single(&result.to_dict())
                } else {
                    output.render_nested_single(&result)
                }
            }
            Err(e) => anyhow::bail!("Failed to execute query {id}: {:#}", e),
        },
        &QueriesCommand::ExecuteNagios { id, args } => {
            match api.execute_query_nagios(id, args).await {
                Ok(result) => {
                    let exit = result.get_exit_code();
                    if output.is_text() {
                        for line in result.lines {
                            if line.perf.is_empty() {
                                println!("{}", line.message);
                            } else {
                                println!("{}|{}", line.message, line.perf);
                            }
                        }
                        std::process::exit(exit);
                    }
                    if let Err(e) = if output.is_flat() {
                        output.render_flat_single(&result.to_dict())
                    } else {
                        output.render_nested_single(&result)
                    } {
                        eprintln!("Failed to render output: {:#}", e);
                        std::process::exit(4);
                    }
                    std::process::exit(exit);
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
        ExecuteLine, ExecuteResult, ListQueriesResult, PerfData, QueryResult,
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
