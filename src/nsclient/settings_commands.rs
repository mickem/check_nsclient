use crate::cli::{SettingsCommand, SettingsCommandActionCli};
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::{SettingsCommandAction, SettingsEntry};
use crate::rendering::Rendering;

fn map_action(action: &SettingsCommandActionCli) -> SettingsCommandAction {
    match action {
        SettingsCommandActionCli::Load => SettingsCommandAction::Load,
        SettingsCommandActionCli::Save => SettingsCommandAction::Save,
        SettingsCommandActionCli::Reload => SettingsCommandAction::Reload,
    }
}

pub async fn route_settings_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &SettingsCommand,
) -> anyhow::Result<()> {
    match command {
        SettingsCommand::Status {} => match api.get_settings_status().await {
            Ok(status) => {
                if output.is_flat() {
                    output.render_flat_single(&status.to_dict())
                } else {
                    output.render_nested_single(&status)
                }
            }
            Err(e) => anyhow::bail!("Failed to fetch settings status: {:#}", e),
        },
        SettingsCommand::List {} => match api.get_settings().await {
            Ok(settings) => {
                if output.is_flat() {
                    output.render_flat_list(&settings, &false, &[])
                } else {
                    output.render_nested_single(&settings)
                }
            }
            Err(e) => anyhow::bail!("Failed to fetch settings entries: {:#}", e),
        },
        SettingsCommand::Descriptions { long } => match api.get_settings_descriptions().await {
            Ok(descriptions) => {
                if output.is_flat() {
                    let descriptions = descriptions.iter().map(|d| d.to_flat()).collect::<Vec<_>>();
                    output.render_flat_list(
                        &descriptions,
                        long,
                        &[
                            "icon",
                            "is_template_key",
                            "is_advanced_key",
                            "is_object",
                            "is_sample_key",
                            "sample_usage",
                            "value",
                            "default_value",
                            "description",
                        ],
                    )
                } else {
                    output.render_nested_single(&descriptions)
                }
            }
            Err(e) => anyhow::bail!("Failed to fetch settings descriptions: {:#}", e),
        },
        SettingsCommand::Set { path, key, value } => {
            let entry = SettingsEntry {
                path: path.clone(),
                key: key.clone(),
                value: value.clone(),
            };
            match api.update_settings(&entry).await {
                Ok(()) => {
                    output.print(&format!("Updated {path}/{key}"));
                    Ok(())
                }
                Err(e) => anyhow::bail!("Failed to update setting {path}/{key}: {:#}", e),
            }
        }
        SettingsCommand::Command { action } => match api.settings_command(map_action(action)).await
        {
            Ok(()) => {
                output.print(&format!("Executed {action:?} command"));
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to execute settings command {action:?}: {:#}", e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::nsclient::messages::{SettingsDescription, SettingsStatus};
    use crate::rendering::StringRender;
    use anyhow::anyhow;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn rendering(format: OutputFormat, long: bool) -> (Rendering, Rc<RefCell<String>>) {
        let sink = Box::new(StringRender::new());
        let out = sink.string.clone();
        (
            Rendering::new(format, OutputStyle::Markdown, long, sink),
            out,
        )
    }

    fn description() -> SettingsDescription {
        SettingsDescription {
            default_value: "8443".into(),
            description: "The port to listen on".into(),
            icon: "icon".into(),
            is_advanced_key: false,
            is_object: false,
            is_sample_key: false,
            is_template_key: false,
            key: "port".into(),
            path: "/settings/WEB/server".into(),
            value_type: "string".into(),
            plugins: vec!["WEBServer".into()],
            sample_usage: "port=8443".into(),
            title: "Port".into(),
            value: "8443".into(),
        }
    }

    #[tokio::test]
    async fn status_text_and_json() {
        for (format, expected_fragment) in [
            (OutputFormat::Text, "| has_changed | true            |"),
            (OutputFormat::Json, "\"has_changed\": true"),
        ] {
            let mut api = MockApiClientApiImpl::new();
            api.expect_get_settings_status().returning(|| {
                Ok(SettingsStatus {
                    context: "ini:///boot.ini".into(),
                    status_type: "ini".into(),
                    has_changed: true,
                })
            });
            let (output, out) = rendering(format, false);

            route_settings_commands(output, Box::new(api), &SettingsCommand::Status {})
                .await
                .unwrap();

            let rendered = out.borrow();
            assert!(rendered.contains(expected_fragment), "{rendered}");
        }
    }

    #[tokio::test]
    async fn list_text_renders_entries() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_settings().returning(|| {
            Ok(vec![SettingsEntry {
                key: "port".into(),
                path: "/settings/WEB/server".into(),
                value: "8443".into(),
            }])
        });
        let (output, out) = rendering(OutputFormat::Text, false);

        route_settings_commands(output, Box::new(api), &SettingsCommand::List {})
            .await
            .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| key  | path                 | value |\n\
             |------|----------------------|-------|\n\
             | port | /settings/WEB/server | 8443  |\n"
        );
    }

    #[tokio::test]
    async fn descriptions_hide_details_unless_long() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_settings_descriptions()
            .returning(|| Ok(vec![description()]));
        let (output, out) = rendering(OutputFormat::Text, false);

        route_settings_commands(
            output,
            Box::new(api),
            &SettingsCommand::Descriptions { long: false },
        )
        .await
        .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| key  | path                 | type   | plugins   | title |\n\
             |------|----------------------|--------|-----------|-------|\n\
             | port | /settings/WEB/server | string | WEBServer | Port  |\n"
        );

        let mut api = MockApiClientApiImpl::new();
        api.expect_get_settings_descriptions()
            .returning(|| Ok(vec![description()]));
        let (output, out) = rendering(OutputFormat::Text, false);

        route_settings_commands(
            output,
            Box::new(api),
            &SettingsCommand::Descriptions { long: true },
        )
        .await
        .unwrap();

        let rendered = out.borrow();
        assert!(rendered.contains("The port to listen on"), "{rendered}");
        assert!(rendered.contains("default_value"), "{rendered}");
    }

    #[tokio::test]
    async fn descriptions_json_keeps_plugin_list() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_settings_descriptions()
            .returning(|| Ok(vec![description()]));
        let (output, out) = rendering(OutputFormat::Json, false);

        route_settings_commands(
            output,
            Box::new(api),
            &SettingsCommand::Descriptions { long: false },
        )
        .await
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["plugins"][0], "WEBServer");
        assert_eq!(parsed[0]["type"], "string");
    }

    #[tokio::test]
    async fn set_sends_entry_and_confirms() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_update_settings()
            .withf(|entry| {
                entry.path == "/settings/WEB/server" && entry.key == "port" && entry.value == "80"
            })
            .times(1)
            .returning(|_| Ok(()));
        let (output, out) = rendering(OutputFormat::Text, false);

        route_settings_commands(
            output,
            Box::new(api),
            &SettingsCommand::Set {
                path: "/settings/WEB/server".into(),
                key: "port".into(),
                value: "80".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(out.borrow().as_str(), "Updated /settings/WEB/server/port\n");
    }

    #[tokio::test]
    async fn command_maps_every_action() {
        for (cli_action, api_action) in [
            (SettingsCommandActionCli::Load, "load"),
            (SettingsCommandActionCli::Save, "save"),
            (SettingsCommandActionCli::Reload, "reload"),
        ] {
            let mut api = MockApiClientApiImpl::new();
            api.expect_settings_command()
                .withf(move |action| {
                    serde_json::to_value(action).unwrap() == serde_json::json!(api_action)
                })
                .times(1)
                .returning(|_| Ok(()));
            let (output, out) = rendering(OutputFormat::Text, false);

            route_settings_commands(
                output,
                Box::new(api),
                &SettingsCommand::Command { action: cli_action },
            )
            .await
            .unwrap();

            assert!(out.borrow().starts_with("Executed "), "{}", out.borrow());
        }
    }

    #[tokio::test]
    async fn errors_are_reported_with_context() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_update_settings()
            .returning(|_| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text, false);

        let err = route_settings_commands(
            output,
            Box::new(api),
            &SettingsCommand::Set {
                path: "/p".into(),
                key: "k".into(),
                value: "v".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Failed to update setting /p/k: boom");
    }
}
