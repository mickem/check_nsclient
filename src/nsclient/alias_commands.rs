use crate::cli::AliasesCommand;
use crate::nsclient::api::ApiClientApi;
use crate::rendering::Rendering;

pub async fn route_alias_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &AliasesCommand,
) -> anyhow::Result<()> {
    match command {
        AliasesCommand::List { all, long } => match api.list_aliases(all).await {
            Ok(aliases) => output.render_rows(&aliases, long, &["description"]),
            Err(e) => anyhow::bail!("Failed to fetch aliases: {:#}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::nsclient::messages::AliasResult;
    use crate::rendering::StringRender;
    use anyhow::anyhow;
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

    fn sample() -> AliasResult {
        AliasResult {
            name: "alias_cpu".into(),
            title: "alias_cpu".into(),
            description: "Alias for: check_cpu".into(),
            plugin: "CheckExternalScripts".into(),
            query_url: "https://localhost:8443/api/v2/queries/alias_cpu/".into(),
            metadata: HashMap::from([("k".to_string(), "v".to_string())]),
        }
    }

    #[tokio::test]
    async fn list_text_hides_the_description_by_default() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_aliases()
            .withf(|all| !*all)
            .returning(|_| Ok(vec![sample()]));
        let (output, out) = rendering(OutputFormat::Text);

        route_alias_commands(
            output,
            Box::new(api),
            &AliasesCommand::List {
                all: false,
                long: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| name      | title     | plugin               |\n\
             |-----------|-----------|----------------------|\n\
             | alias_cpu | alias_cpu | CheckExternalScripts |\n"
        );
    }

    #[tokio::test]
    async fn list_long_shows_the_description_and_passes_all() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_aliases()
            .withf(|all| *all)
            .returning(|_| Ok(vec![sample()]));
        let (output, out) = rendering(OutputFormat::Text);

        route_alias_commands(
            output,
            Box::new(api),
            &AliasesCommand::List {
                all: true,
                long: true,
            },
        )
        .await
        .unwrap();

        assert!(
            out.borrow().contains("Alias for: check_cpu"),
            "{}",
            out.borrow()
        );
    }

    #[tokio::test]
    async fn list_json_keeps_the_query_url_and_metadata() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_aliases().returning(|_| Ok(vec![sample()]));
        let (output, out) = rendering(OutputFormat::Json);

        route_alias_commands(
            output,
            Box::new(api),
            &AliasesCommand::List {
                all: false,
                long: false,
            },
        )
        .await
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["name"], "alias_cpu");
        assert_eq!(
            parsed[0]["query_url"],
            "https://localhost:8443/api/v2/queries/alias_cpu/"
        );
        assert_eq!(parsed[0]["metadata"]["k"], "v");
    }

    #[tokio::test]
    async fn list_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_aliases()
            .returning(|_| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_alias_commands(
            output,
            Box::new(api),
            &AliasesCommand::List {
                all: false,
                long: false,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch aliases: boom");
    }
}
