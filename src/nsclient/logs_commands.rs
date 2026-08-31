use crate::cli::LogsCommand;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::{LogStatus, NewLogRecord};
use crate::rendering::Rendering;

pub async fn route_log_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &LogsCommand,
) -> anyhow::Result<()> {
    match command {
        LogsCommand::List {
            page,
            size,
            level,
            long,
        } => match api.get_logs(*page, *size, level.clone()).await {
            Ok(page_response) => {
                if output.is_flat() {
                    output.render_flat_list(&page_response.content, long, &["file", "line"])
                } else {
                    output.render_nested_single(&page_response)
                }
            }
            Err(e) => anyhow::bail!("Failed to fetch logs: {:#}", e),
        },
        LogsCommand::Status {} => match api.get_log_status().await {
            Ok(status) => output.render_single(&status, LogStatus::to_dict),
            Err(e) => anyhow::bail!("Failed to obtain log status: {:#}", e),
        },
        LogsCommand::Clear {} => match api.clear_logs().await {
            Ok(result) => {
                output.print(&format!("Cleared {} log record(s)", result.count));
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to clear logs: {:#}", e),
        },
        LogsCommand::Add {
            message,
            level,
            file,
            line,
        } => {
            let record = NewLogRecord {
                level: level.clone(),
                message: message.clone(),
                file: file.clone(),
                line: *line,
            };
            match api.add_log(&record).await {
                Ok(()) => {
                    output.print("Log record added");
                    Ok(())
                }
                Err(e) => anyhow::bail!("Failed to add log record: {:#}", e),
            }
        }
        LogsCommand::Reset {} => match api.reset_log_status().await {
            Ok(()) => {
                output.print("Successfully reset log status");
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to reset log status: {:#}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::nsclient::messages::{LogClearResult, LogRecord, PaginatedResponse};
    use crate::rendering::StringRender;
    use anyhow::anyhow;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn rendering(format: OutputFormat, long: bool) -> (Rendering, Rc<RefCell<String>>) {
        let output_sink = Box::new(StringRender::new());
        let output_ref = output_sink.string.clone();
        (
            Rendering::new(format, OutputStyle::Rounded, long, output_sink),
            output_ref,
        )
    }

    fn sample_page() -> PaginatedResponse<Vec<LogRecord>> {
        PaginatedResponse {
            content: vec![LogRecord {
                level: "level".to_string(),
                date: "date".to_string(),
                file: "file".to_string(),
                line: 123,
                message: "message".to_string(),
            }],
            page: 0,
            pages: 5,
            limit: 10,
            count: 3,
        }
    }

    #[tokio::test]
    async fn list_text_hides_file_and_line_by_default() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_logs()
            .withf(|page, size, level| *page == 5 && *size == 100 && level.is_none())
            .returning(|_, _, _| Ok(sample_page()));
        let (output, output_ref) = rendering(OutputFormat::Text, false);

        let result = route_log_commands(
            output,
            Box::new(api),
            &LogsCommand::List {
                page: 5,
                size: 100,
                level: None,
                long: false,
            },
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(
            output_ref.borrow().as_str(),
            r#"╭───────┬──────┬─────────╮
│ level │ date │ message │
├───────┼──────┼─────────┤
│ level │ date │ message │
╰───────┴──────┴─────────╯
"#
        );
    }

    #[tokio::test]
    async fn list_text_long_shows_file_and_line() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_logs()
            .withf(|_, _, level| level.as_deref() == Some("error"))
            .returning(|_, _, _| Ok(sample_page()));
        let (output, output_ref) = rendering(OutputFormat::Text, false);

        let result = route_log_commands(
            output,
            Box::new(api),
            &LogsCommand::List {
                page: 1,
                size: 10,
                level: Some("error".into()),
                long: true,
            },
        )
        .await;

        assert!(result.is_ok());
        let rendered = output_ref.borrow();
        assert!(rendered.contains("│ file │"), "{rendered}");
        assert!(rendered.contains("│ 123 "), "{rendered}");
    }

    #[tokio::test]
    async fn list_json_includes_pagination() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_logs().returning(|_, _, _| Ok(sample_page()));
        let (output, output_ref) = rendering(OutputFormat::Json, false);

        let result = route_log_commands(
            output,
            Box::new(api),
            &LogsCommand::List {
                page: 1,
                size: 10,
                level: None,
                long: false,
            },
        )
        .await;

        assert!(result.is_ok());
        let parsed: serde_json::Value = serde_json::from_str(&output_ref.borrow()).unwrap();
        assert_eq!(parsed["pages"], 5);
        assert_eq!(parsed["content"][0]["message"], "message");
    }

    #[tokio::test]
    async fn list_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_logs()
            .returning(|_, _, _| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text, false);

        let err = route_log_commands(
            output,
            Box::new(api),
            &LogsCommand::List {
                page: 1,
                size: 10,
                level: None,
                long: false,
            },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("Failed to fetch logs"));
    }

    #[tokio::test]
    async fn status_text_renders_key_value_table() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_log_status().returning(|| {
            Ok(LogStatus {
                errors: 2,
                last_error: Some("disk full".into()),
            })
        });
        let (output, output_ref) = rendering(OutputFormat::Text, false);

        let result = route_log_commands(output, Box::new(api), &LogsCommand::Status {}).await;

        assert!(result.is_ok(), "{:?}", result.unwrap_err());
        assert_eq!(
            output_ref.borrow().as_str(),
            r#"╭────────────┬───────────╮
│ errors     │ 2         │
│ last_error │ disk full │
╰────────────┴───────────╯
"#
        );
    }

    #[tokio::test]
    async fn status_json_keeps_null_last_error() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_log_status().returning(|| {
            Ok(LogStatus {
                errors: 0,
                last_error: None,
            })
        });
        let (output, output_ref) = rendering(OutputFormat::Json, false);

        let result = route_log_commands(output, Box::new(api), &LogsCommand::Status {}).await;

        assert!(result.is_ok());
        assert_eq!(
            output_ref.borrow().as_str(),
            r#"{
  "errors": 0,
  "last_error": null
}
"#
        );
    }

    #[tokio::test]
    async fn reset_prints_confirmation() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_reset_log_status().returning(|| Ok(()));
        let (output, output_ref) = rendering(OutputFormat::Text, false);

        let result = route_log_commands(output, Box::new(api), &LogsCommand::Reset {}).await;

        assert!(result.is_ok());
        assert_eq!(
            output_ref.borrow().as_str(),
            "Successfully reset log status\n"
        );
    }

    #[tokio::test]
    async fn clear_reports_how_many_records_were_dropped() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_clear_logs()
            .times(1)
            .returning(|| Ok(LogClearResult { count: 12 }));
        let (output, out) = rendering(OutputFormat::Text, false);

        route_log_commands(output, Box::new(api), &LogsCommand::Clear {})
            .await
            .unwrap();

        assert_eq!(out.borrow().as_str(), "Cleared 12 log record(s)\n");
    }

    #[tokio::test]
    async fn add_sends_the_record_with_defaults_applied_by_the_cli() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_add_log()
            .withf(|record| {
                record.level == "warning"
                    && record.message == "from the cli"
                    && record.file == "check_nsclient"
                    && record.line == 7
            })
            .times(1)
            .returning(|_| Ok(()));
        let (output, out) = rendering(OutputFormat::Text, false);

        route_log_commands(
            output,
            Box::new(api),
            &LogsCommand::Add {
                message: "from the cli".into(),
                level: "warning".into(),
                file: "check_nsclient".into(),
                line: 7,
            },
        )
        .await
        .unwrap();

        assert_eq!(out.borrow().as_str(), "Log record added\n");
    }

    #[tokio::test]
    async fn clear_and_add_errors_are_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_clear_logs().returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text, false);
        let err = route_log_commands(output, Box::new(api), &LogsCommand::Clear {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Failed to clear logs: boom");

        let mut api = MockApiClientApiImpl::new();
        api.expect_add_log().returning(|_| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text, false);
        let err = route_log_commands(
            output,
            Box::new(api),
            &LogsCommand::Add {
                message: "m".into(),
                level: "info".into(),
                file: "f".into(),
                line: 0,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Failed to add log record: boom");
    }

    #[tokio::test]
    async fn reset_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_reset_log_status()
            .returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text, false);

        let err = route_log_commands(output, Box::new(api), &LogsCommand::Reset {})
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Failed to reset log status"));
    }
}
