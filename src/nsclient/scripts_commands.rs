use crate::cli::ScriptsCommand;
use crate::nsclient::api::ApiClientApi;
use crate::rendering::Rendering;
use serde::Serialize;
use tabled::Tabled;

/// One script name, so the table gets a meaningful column header.
#[derive(Tabled, Serialize)]
struct ScriptRow {
    script: String,
}

pub async fn route_script_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &ScriptsCommand,
) -> anyhow::Result<()> {
    match command {
        ScriptsCommand::ListRuntimes {} => match api.list_script_runtimes().await {
            Ok(runtimes) => output.render_rows(&runtimes, &false, &[]),
            Err(e) => anyhow::bail!("Failed to fetch script runtimes: {:#}", e),
        },
        ScriptsCommand::List { runtime } => match api.list_scripts(runtime).await {
            Ok(scripts) => output.render_list(
                &scripts,
                |script| ScriptRow {
                    script: script.clone(),
                },
                &false,
                &[],
            ),
            Err(e) => anyhow::bail!("Failed to fetch scripts for runtime {runtime}: {:#}", e),
        },
        // The script endpoints answer with plain text (a definition, or a short
        // confirmation), so the body is printed as-is in every output format.
        ScriptsCommand::Show { runtime, script } => match api.get_script(runtime, script).await {
            Ok(body) => {
                output.print(body.trim_end());
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to fetch script {script} ({runtime}): {:#}", e),
        },
        ScriptsCommand::Add {
            runtime,
            script,
            file,
        } => {
            let content = std::fs::read_to_string(file)
                .map_err(|e| anyhow::anyhow!("Failed to read {file}: {e}"))?;
            match api.add_script(runtime, script, content).await {
                Ok(body) => {
                    output.print(body.trim_end());
                    Ok(())
                }
                Err(e) => anyhow::bail!("Failed to add script {script} ({runtime}): {:#}", e),
            }
        }
        ScriptsCommand::Delete { runtime, script } => {
            match api.delete_script(runtime, script).await {
                Ok(body) => {
                    output.print(body.trim_end());
                    Ok(())
                }
                Err(e) => anyhow::bail!("Failed to delete script {script} ({runtime}): {:#}", e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::nsclient::messages::ScriptRuntimes;
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

    #[tokio::test]
    async fn list_runtimes_text() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_script_runtimes().returning(|| {
            Ok(vec![ScriptRuntimes {
                module: "PythonScript".into(),
                name: "python".into(),
                title: "Python".into(),
            }])
        });
        let (output, out) = rendering(OutputFormat::Text);

        route_script_commands(output, Box::new(api), &ScriptsCommand::ListRuntimes {})
            .await
            .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| module       | name   | title  |\n\
             |--------------|--------|--------|\n\
             | PythonScript | python | Python |\n"
        );
    }

    #[tokio::test]
    async fn list_runtimes_json() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_script_runtimes().returning(|| {
            Ok(vec![ScriptRuntimes {
                module: "PythonScript".into(),
                name: "python".into(),
                title: "Python".into(),
            }])
        });
        let (output, out) = rendering(OutputFormat::Json);

        route_script_commands(output, Box::new(api), &ScriptsCommand::ListRuntimes {})
            .await
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["name"], "python");
    }

    #[tokio::test]
    async fn list_scripts_text() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_scripts()
            .withf(|runtime| runtime == "python")
            .returning(|_| Ok(vec!["check_a.py".into(), "check_b.py".into()]));
        let (output, out) = rendering(OutputFormat::Text);

        route_script_commands(
            output,
            Box::new(api),
            &ScriptsCommand::List {
                runtime: "python".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| script     |\n\
             |------------|\n\
             | check_a.py |\n\
             | check_b.py |\n"
        );
    }

    #[tokio::test]
    async fn list_scripts_yaml() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_scripts()
            .returning(|_| Ok(vec!["check_a.py".into()]));
        let (output, out) = rendering(OutputFormat::Yaml);

        route_script_commands(
            output,
            Box::new(api),
            &ScriptsCommand::List {
                runtime: "python".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(out.borrow().as_str(), "- check_a.py\n\n");
    }

    #[tokio::test]
    async fn show_prints_the_definition_verbatim() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_script()
            .withf(|runtime, script| runtime == "ext" && script == "check_probe")
            .returning(|_, _| Ok("scripts/check_probe.sh --arg  \n".to_string()));
        let (output, out) = rendering(OutputFormat::Text);

        route_script_commands(
            output,
            Box::new(api),
            &ScriptsCommand::Show {
                runtime: "ext".into(),
                script: "check_probe".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(out.borrow().as_str(), "scripts/check_probe.sh --arg\n");
    }

    #[tokio::test]
    async fn add_uploads_the_file_contents() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("check_probe.sh");
        std::fs::write(&file, "echo OK\n").unwrap();

        let mut api = MockApiClientApiImpl::new();
        api.expect_add_script()
            .withf(|runtime, script, content| {
                runtime == "ext" && script == "check_probe" && content == "echo OK\n"
            })
            .times(1)
            .returning(|_, _, _| Ok("Added check_probe".to_string()));
        let (output, out) = rendering(OutputFormat::Text);

        route_script_commands(
            output,
            Box::new(api),
            &ScriptsCommand::Add {
                runtime: "ext".into(),
                script: "check_probe".into(),
                file: file.to_string_lossy().into_owned(),
            },
        )
        .await
        .unwrap();

        assert_eq!(out.borrow().as_str(), "Added check_probe\n");
    }

    #[tokio::test]
    async fn add_reports_a_missing_file_without_calling_the_api() {
        let api = MockApiClientApiImpl::new();
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_script_commands(
            output,
            Box::new(api),
            &ScriptsCommand::Add {
                runtime: "ext".into(),
                script: "check_probe".into(),
                file: "definitely/not/here.sh".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Failed to read definitely/not/here.sh"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn delete_prints_the_confirmation() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_delete_script()
            .withf(|runtime, script| runtime == "ext" && script == "check_probe")
            .times(1)
            .returning(|_, _| Ok("Script file was removed".to_string()));
        let (output, out) = rendering(OutputFormat::Text);

        route_script_commands(
            output,
            Box::new(api),
            &ScriptsCommand::Delete {
                runtime: "ext".into(),
                script: "check_probe".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(out.borrow().as_str(), "Script file was removed\n");
    }

    #[tokio::test]
    async fn script_errors_name_the_script_and_runtime() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_script()
            .returning(|_, _| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);
        let err = route_script_commands(
            output,
            Box::new(api),
            &ScriptsCommand::Show {
                runtime: "lua".into(),
                script: "mock".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch script mock (lua): boom");
    }

    #[tokio::test]
    async fn errors_are_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_script_runtimes()
            .returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);
        let err = route_script_commands(output, Box::new(api), &ScriptsCommand::ListRuntimes {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch script runtimes: boom");
    }
}
