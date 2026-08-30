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
