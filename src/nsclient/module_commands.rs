use crate::cli::ModulesCommand;
use crate::nsclient::api::ApiClientApi;
use crate::nsclient::messages::{ListModulesResult, ModulesResult};
use crate::rendering::Rendering;

pub async fn route_module_commands(
    output: Rendering,
    api: Box<dyn ApiClientApi>,
    command: &ModulesCommand,
) -> anyhow::Result<()> {
    match &command {
        ModulesCommand::List { all, long } => match api.list_modules(all).await {
            Ok(modules) => output.render_list(
                &modules,
                ListModulesResult::to_flat,
                long,
                &["description", "name", "plugin_id"],
            ),
            Err(e) => anyhow::bail!("Failed to fetch modules: {:#}", e),
        },
        &ModulesCommand::Show { id } => match api.get_module(id).await {
            Ok(module) => output.render_single(&module, ModulesResult::to_dict),
            Err(e) => anyhow::bail!("Failed to fetch module {id}: {:#}", e),
        },
        &ModulesCommand::Load { id } => match api.module_command(id, "load").await {
            Ok(()) => {
                output.print(&format!(
                    "Successfully loaded module {id}, you can now interact with it but next time the service is restarted it will be unloaded"
                ));
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to load module {id}: {:#}", e),
        },
        &ModulesCommand::Unload { id } => match api.module_command(id, "unload").await {
            Ok(()) => {
                output.print(&format!("Successfully unloaded module {id}"));
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to unload module {id}: {:#}", e),
        },
        &ModulesCommand::Enable { id } => match api.module_command(id, "enable").await {
            Ok(()) => {
                output.print(&format!(
                    "Successfully enabled module {id}, this module will be available if you restart the service or if you load it"
                ));
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to enable module {id}: {:#}", e),
        },
        &ModulesCommand::Disable { id } => match api.module_command(id, "disable").await {
            Ok(()) => {
                output.print(&format!(
                    "Successfully disabled module {id}, this module will not be available if you restart the service"
                ));
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to disable module {id}: {:#}", e),
        },
        ModulesCommand::Upload { id, file } => {
            let archive =
                std::fs::read(file).map_err(|e| anyhow::anyhow!("Failed to read {file}: {e}"))?;
            match api.upload_module(id, archive).await {
                Ok(()) => {
                    output.print(&format!("Uploaded and loaded module {id}"));
                    Ok(())
                }
                Err(e) => anyhow::bail!("Failed to upload module {id}: {:#}", e),
            }
        }
        &ModulesCommand::Use { id } => {
            if let Err(e) = api.module_command(id, "load").await {
                anyhow::bail!("Failed to load module {id}: {:#}", e);
            }
            if let Err(e) = api.module_command(id, "enable").await {
                anyhow::bail!("Failed to enable module {id}: {:#}", e);
            }
            output.print(&format!("Successfully loaded and enabled module {id}"));
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nsclient::api::mocks::MockApiClientApiImpl;
    use crate::nsclient::messages::ListModulesMetadata;
    use crate::rendering::StringRender;
    use anyhow::anyhow;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn rendering(format: OutputFormat) -> (Rendering, Rc<RefCell<String>>) {
        let output_sink = Box::new(StringRender::new());
        let output_ref = output_sink.string.clone();
        (
            Rendering::new(format, OutputStyle::Rounded, false, output_sink),
            output_ref,
        )
    }

    fn sample_module() -> ListModulesResult {
        ListModulesResult {
            id: "CheckSystem".into(),
            name: "CheckSystem".into(),
            title: "Check System".into(),
            description: "System checks".into(),
            enabled: true,
            loaded: false,
            metadata: ListModulesMetadata {
                alias: "sys".into(),
                plugin_id: "7".into(),
            },
        }
    }

    #[tokio::test]
    async fn list_text_hides_long_columns_by_default() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_modules()
            .withf(|all| !*all)
            .returning(|_| Ok(vec![sample_module()]));
        let (output, output_ref) = rendering(OutputFormat::Text);

        route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::List {
                all: false,
                long: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            output_ref.borrow().as_str(),
            r#"╭─────────────┬──────────────┬─────────┬────────┬───────╮
│ id          │ title        │ enabled │ loaded │ alias │
├─────────────┼──────────────┼─────────┼────────┼───────┤
│ CheckSystem │ Check System │ true    │ false  │ sys   │
╰─────────────┴──────────────┴─────────┴────────┴───────╯
"#
        );
    }

    #[tokio::test]
    async fn list_long_passes_all_flag_and_shows_every_column() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_modules()
            .withf(|all| *all)
            .returning(|_| Ok(vec![sample_module()]));
        let (output, output_ref) = rendering(OutputFormat::Text);

        route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::List {
                all: true,
                long: true,
            },
        )
        .await
        .unwrap();

        let rendered = output_ref.borrow();
        for column in ["description", "name", "plugin_id"] {
            assert!(rendered.contains(column), "missing {column}: {rendered}");
        }
    }

    #[tokio::test]
    async fn list_json_keeps_nested_metadata() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_modules()
            .returning(|_| Ok(vec![sample_module()]));
        let (output, output_ref) = rendering(OutputFormat::Json);

        route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::List {
                all: false,
                long: false,
            },
        )
        .await
        .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&output_ref.borrow()).unwrap();
        assert_eq!(parsed[0]["metadata"]["alias"], "sys");
    }

    #[tokio::test]
    async fn show_text_renders_flat_dictionary() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_get_module()
            .withf(|id| id == "CheckSystem")
            .returning(|_| {
                Ok(ModulesResult {
                    id: "CheckSystem".into(),
                    name: "CheckSystem".into(),
                    title: "Check System".into(),
                    description: "System checks".into(),
                    enabled: true,
                    loaded: false,
                    metadata: ListModulesMetadata {
                        alias: "sys".into(),
                        plugin_id: "7".into(),
                    },
                })
            });
        let (output, output_ref) = rendering(OutputFormat::Text);

        route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::Show {
                id: "CheckSystem".into(),
            },
        )
        .await
        .unwrap();

        let rendered = output_ref.borrow();
        assert!(rendered.contains("│ plugin_id   │ 7"), "{rendered}");
        assert!(rendered.contains("│ alias       │ sys"), "{rendered}");
    }

    #[tokio::test]
    async fn module_commands_send_the_matching_api_command() {
        let cases: Vec<(ModulesCommand, &str, &str)> = vec![
            (ModulesCommand::Load { id: "M".into() }, "load", "loaded"),
            (
                ModulesCommand::Unload { id: "M".into() },
                "unload",
                "unloaded",
            ),
            (
                ModulesCommand::Enable { id: "M".into() },
                "enable",
                "enabled",
            ),
            (
                ModulesCommand::Disable { id: "M".into() },
                "disable",
                "disabled",
            ),
        ];
        for (command, api_command, expected_word) in cases {
            let mut api = MockApiClientApiImpl::new();
            api.expect_module_command()
                .withf(move |id, cmd| id == "M" && cmd == api_command)
                .times(1)
                .returning(|_, _| Ok(()));
            let (output, output_ref) = rendering(OutputFormat::Text);

            route_module_commands(output, Box::new(api), &command)
                .await
                .unwrap();

            let rendered = output_ref.borrow();
            assert!(
                rendered.starts_with(&format!("Successfully {expected_word} module M")),
                "{api_command}: {rendered}"
            );
        }
    }

    #[tokio::test]
    async fn use_loads_then_enables() {
        let mut api = MockApiClientApiImpl::new();
        let mut seq = mockall::Sequence::new();
        api.expect_module_command()
            .withf(|id, cmd| id == "M" && cmd == "load")
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));
        api.expect_module_command()
            .withf(|id, cmd| id == "M" && cmd == "enable")
            .times(1)
            .in_sequence(&mut seq)
            .returning(|_, _| Ok(()));
        let (output, output_ref) = rendering(OutputFormat::Text);

        route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::Use { id: "M".into() },
        )
        .await
        .unwrap();

        assert_eq!(
            output_ref.borrow().as_str(),
            "Successfully loaded and enabled module M\n"
        );
    }

    #[tokio::test]
    async fn use_stops_after_failed_load() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_module_command()
            .withf(|_, cmd| cmd == "load")
            .times(1)
            .returning(|_, _| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::Use { id: "M".into() },
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("Failed to load module M"), "{err}");
    }

    #[tokio::test]
    async fn upload_sends_the_archive_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("MyModule.zip");
        std::fs::write(&file, [0x50u8, 0x4b, 0x05, 0x06]).unwrap();

        let mut api = MockApiClientApiImpl::new();
        api.expect_upload_module()
            .withf(|id, archive| id == "MyModule" && archive.as_slice() == [0x50, 0x4b, 0x05, 0x06])
            .times(1)
            .returning(|_, _| Ok(()));
        let (output, out) = rendering(OutputFormat::Text);

        route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::Upload {
                id: "MyModule".into(),
                file: file.to_string_lossy().into_owned(),
            },
        )
        .await
        .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "Uploaded and loaded module MyModule\n"
        );
    }

    #[tokio::test]
    async fn upload_reports_a_missing_file_without_calling_the_api() {
        let api = MockApiClientApiImpl::new();
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::Upload {
                id: "MyModule".into(),
                file: "definitely/not/here.zip".into(),
            },
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("Failed to read definitely/not/here.zip"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn upload_error_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("m.zip");
        std::fs::write(&file, b"x").unwrap();

        let mut api = MockApiClientApiImpl::new();
        api.expect_upload_module()
            .returning(|_, _| Err(anyhow!("Invalid module name")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::Upload {
                id: "m".into(),
                file: file.to_string_lossy().into_owned(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Failed to upload module m: Invalid module name"
        );
    }

    #[tokio::test]
    async fn errors_are_reported_with_context() {
        let mut api = MockApiClientApiImpl::new();
        api.expect_list_modules()
            .returning(|_| Err(anyhow!("boom")));
        let (output, _) = rendering(OutputFormat::Text);

        let err = route_module_commands(
            output,
            Box::new(api),
            &ModulesCommand::List {
                all: false,
                long: false,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Failed to fetch modules: boom");
    }
}
