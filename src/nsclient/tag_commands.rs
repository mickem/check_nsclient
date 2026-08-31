use crate::cli::TagsCommand;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::Tags;
use crate::rendering::Rendering;
use indexmap::IndexMap;

/// Flatten the tag map into a sorted key/value list.
fn tags_to_dict(tags: &Tags) -> IndexMap<String, String> {
    let mut keys: Vec<&String> = tags.keys().collect();
    keys.sort();
    keys.into_iter()
        .map(|key| (key.clone(), tags[key].clone()))
        .collect()
}

pub async fn route_tag_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &TagsCommand,
) -> anyhow::Result<()> {
    match command {
        TagsCommand::Show {} => match api.get_tags().await {
            Ok(tags) => {
                if output.is_flat() && tags.is_empty() {
                    output.print("No tags set");
                    return Ok(());
                }
                output.render_single(&tags, tags_to_dict)
            }
            Err(e) => anyhow::bail!("Failed to fetch tags: {:#}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
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

    fn sample() -> Tags {
        HashMap::from([
            ("site".to_string(), "stockholm".to_string()),
            ("env".to_string(), "prod".to_string()),
        ])
    }

    #[tokio::test]
    async fn show_text_is_sorted_by_tag_name() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_tags().returning(|| Ok(sample()));
        let (output, out) = rendering(OutputFormat::Text);

        route_tag_commands(output, Box::new(api), &TagsCommand::Show {})
            .await
            .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| env  | prod      |\n| site | stockholm |\n"
        );
    }

    #[tokio::test]
    async fn show_json_keeps_the_map() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_tags().returning(|| Ok(sample()));
        let (output, out) = rendering(OutputFormat::Json);

        route_tag_commands(output, Box::new(api), &TagsCommand::Show {})
            .await
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed["env"], "prod");
        assert_eq!(parsed["site"], "stockholm");
    }

    #[tokio::test]
    async fn show_reports_an_empty_tag_set_in_text_but_not_in_json() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_tags().returning(|| Ok(Tags::new()));
        let (output, out) = rendering(OutputFormat::Text);
        route_tag_commands(output, Box::new(api), &TagsCommand::Show {})
            .await
            .unwrap();
        assert_eq!(out.borrow().as_str(), "No tags set\n");

        let mut api = MockApiClientApiImpl::new();
        api.expect_get_tags().returning(|| Ok(Tags::new()));
        let (output, out) = rendering(OutputFormat::Json);
        route_tag_commands(output, Box::new(api), &TagsCommand::Show {})
            .await
            .unwrap();
        assert_eq!(out.borrow().trim(), "{}");
    }

    #[tokio::test]
    async fn show_error_is_reported() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_tags().returning(|| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);
        let err = route_tag_commands(output, Box::new(api), &TagsCommand::Show {})
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch tags: boom");
    }
}
