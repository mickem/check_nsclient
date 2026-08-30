mod api;
mod auth_commands;
pub mod client;
mod generic_commands;
mod login_helper;
mod logs_commands;
mod messages;
mod metrics_commands;
mod module_commands;
mod query_commands;
mod scripts_commands;
mod settings_commands;

use crate::cli::{NSClientCommandOptions, NSClientCommands};
use crate::config;
use crate::nsclient::api::{ApiClient, ApiClientApi, Auth};
use crate::nsclient::auth_commands::route_auth_commands;
use crate::nsclient::generic_commands::{handle_ping_command, handle_version_command};
use crate::nsclient::logs_commands::route_log_commands;
use crate::nsclient::metrics_commands::route_metrics_commands;
use crate::nsclient::module_commands::route_module_commands;
use crate::nsclient::query_commands::route_query_commands;
use crate::nsclient::scripts_commands::route_script_commands;
use crate::nsclient::settings_commands::route_settings_commands;
use crate::rendering::Rendering;
use reqwest::Certificate;
use std::time::Duration;

/// HTTP client settings shared by every request made on behalf of one CLI invocation,
/// including the login performed when a token has to be refreshed.
#[derive(Clone, Debug)]
pub struct ConnectionOptions {
    pub timeout_s: u64,
    pub user_agent: String,
}

impl ConnectionOptions {
    pub fn from_args(args: &NSClientCommandOptions) -> Self {
        Self {
            timeout_s: args.timeout_s,
            user_agent: args.user_agent.clone(),
        }
    }
}

fn preprocess_url(url: &str) -> String {
    let url = url.trim_end_matches('/');
    format!("{url}/")
}

pub fn build_client_from_profile(
    args: &NSClientCommandOptions,
) -> anyhow::Result<Box<dyn ApiClientApi>> {
    let profile = match args.profile.as_ref() {
        Some(profile_id) => match config::get_nsclient_profile(profile_id)? {
            Some(profile) => profile,
            None => anyhow::bail!("NSClient++ profile '{}' not found.", profile_id),
        },
        None => match config::get_default_nsclient_profile()? {
            Some(profile) => profile,
            None => anyhow::bail!(
                "No default NSClient++ profile set. Please specify a profile using --profile or set a default profile."
            ),
        },
    };
    let api_key = config::get_api_key(&profile.id)?;
    build_client(
        &profile.url,
        &ConnectionOptions::from_args(args),
        Auth::Token(api_key),
        profile.insecure,
        Some(profile.id.to_owned()),
        profile.ca,
    )
}

pub fn build_client(
    url: &str,
    options: &ConnectionOptions,
    auth: Auth,
    insecure: bool,
    profile_id: Option<String>,
    ca_file: Option<String>,
) -> anyhow::Result<Box<dyn ApiClientApi>> {
    let url = preprocess_url(url);
    let mut client = reqwest::Client::builder()
        .timeout(Duration::from_secs(options.timeout_s))
        .user_agent(&options.user_agent)
        .danger_accept_invalid_certs(insecure);
    if let Some(ca_file) = ca_file {
        let ca_pem = std::fs::read(&ca_file)?;
        let cert = Certificate::from_pem(&ca_pem)?;
        client = client.tls_certs_merge(vec![cert]);
    }
    let client = ApiClient::new(client, &url, auth, profile_id, options.clone())?;
    Ok(Box::new(client))
}

/// Route an `nsclient` sub command and return the process exit code.
pub async fn route_ns_client(
    output: Rendering,
    args: &NSClientCommandOptions,
) -> anyhow::Result<i32> {
    match &args.command {
        NSClientCommands::Ping {} => {
            handle_ping_command(output, build_client_from_profile(args)?).await?
        }
        NSClientCommands::Version {} => {
            handle_version_command(output, build_client_from_profile(args)?).await?
        }
        NSClientCommands::Modules { command } => {
            route_module_commands(output, build_client_from_profile(args)?, command).await?
        }
        NSClientCommands::Queries { command } => {
            return route_query_commands(output, build_client_from_profile(args)?, command).await;
        }
        NSClientCommands::Logs { command } => {
            route_log_commands(output, build_client_from_profile(args)?, command).await?
        }
        NSClientCommands::Scripts { command } => {
            route_script_commands(output, build_client_from_profile(args)?, command).await?
        }
        NSClientCommands::Settings { command } => {
            route_settings_commands(output, build_client_from_profile(args)?, command).await?
        }
        NSClientCommands::Metrics { command } => {
            route_metrics_commands(output, build_client_from_profile(args)?, command).await?
        }
        NSClientCommands::Auth { command } => {
            route_auth_commands(output, &ConnectionOptions::from_args(args), command).await?
        }
        NSClientCommands::Client {} | NSClientCommands::Test {} => {
            client::run_client(build_client_from_profile(args)?).await?
        }
    }
    Ok(0)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::OutputFormat;
    use crate::cli::OutputStyle;
    use crate::config::{add_nsclient_profile, mock_test_config};
    use crate::rendering::StringRender;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn options(profile: Option<&str>, command: NSClientCommands) -> NSClientCommandOptions {
        NSClientCommandOptions {
            command,
            timeout_s: 30,
            user_agent: "nscp-client".to_owned(),
            profile: profile.map(str::to_string),
        }
    }

    #[test]
    fn preprocess_url_ensures_single_trailing_slash() {
        assert_eq!(preprocess_url("https://host:8443"), "https://host:8443/");
        assert_eq!(preprocess_url("https://host:8443/"), "https://host:8443/");
        assert_eq!(preprocess_url("https://host:8443///"), "https://host:8443/");
    }

    #[test]
    fn connection_options_come_from_cli_args() {
        let args = NSClientCommandOptions {
            command: NSClientCommands::Ping {},
            timeout_s: 7,
            user_agent: "custom-agent".into(),
            profile: None,
        };
        let options = ConnectionOptions::from_args(&args);
        assert_eq!(options.timeout_s, 7);
        assert_eq!(options.user_agent, "custom-agent");
    }

    #[test]
    #[serial_test::serial(config)]
    fn build_client_from_profile_reports_unknown_profile() {
        let tmp = mock_test_config();
        let err = build_client_from_profile(&options(Some("nope"), NSClientCommands::Ping {}))
            .err()
            .expect("unknown profile must fail");
        assert_eq!(err.to_string(), "NSClient++ profile 'nope' not found.");
        drop(tmp);
    }

    #[test]
    #[serial_test::serial(config)]
    fn build_client_from_profile_reports_missing_default() {
        let tmp = mock_test_config();
        let err = build_client_from_profile(&options(None, NSClientCommands::Ping {}))
            .err()
            .expect("missing default must fail");
        assert!(
            err.to_string()
                .starts_with("No default NSClient++ profile set"),
            "{err}"
        );
        drop(tmp);
    }

    #[test]
    #[serial_test::serial(config)]
    fn build_client_from_profile_uses_default_profile() {
        let tmp = mock_test_config();
        add_nsclient_profile("dflt", "http://localhost:1", false, "u", "p", "k", None).unwrap();
        assert!(build_client_from_profile(&options(None, NSClientCommands::Ping {})).is_ok());
        drop(tmp);
    }

    #[test]
    #[serial_test::serial(config)]
    fn build_client_from_profile_reports_missing_ca_file() {
        let tmp = mock_test_config();
        add_nsclient_profile(
            "ca",
            "http://localhost:1",
            false,
            "u",
            "p",
            "k",
            Some("/definitely/not/here.pem".into()),
        )
        .unwrap();
        assert!(
            build_client_from_profile(&options(Some("ca"), NSClientCommands::Ping {})).is_err()
        );
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn test_ping() {
        let tmp = mock_test_config();
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v2/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "NSClient++",
                "version": "0.5.2.35"
            })))
            .mount(&mock_server)
            .await;
        add_nsclient_profile(
            "test-profile-ping",
            &mock_server.uri(),
            false,
            "username",
            "password",
            "test-token",
            None,
        )
        .unwrap();

        let output_sink = Box::new(StringRender::new());
        let output_ref = output_sink.string.clone();

        let output = Rendering::new(OutputFormat::Text, OutputStyle::Rounded, false, output_sink);

        let result = route_ns_client(
            output,
            &NSClientCommandOptions {
                command: NSClientCommands::Ping {},
                timeout_s: 30,
                user_agent: "nscp-client".to_owned(),
                profile: Some("test-profile-ping".into()),
            },
        )
        .await;

        assert!(
            result.is_ok(),
            "Ping command failed: {:?}",
            result.unwrap_err()
        );
        assert_eq!(
            output_ref.borrow().as_str(),
            "Successfully pinged NSClient++ version 0.5.2.35\n"
        );
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn test_version() {
        let tmp = mock_test_config();
        let mock_server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/api/v2/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "NSClient++",
                "version": "0.5.2.35"
            })))
            .mount(&mock_server)
            .await;
        add_nsclient_profile(
            "test-profile-version",
            &mock_server.uri(),
            false,
            "username",
            "password",
            "test-token",
            None,
        )
        .unwrap();

        let mock_output = StringRender::new();
        let output_sink = Box::new(mock_output);
        let output_ref = output_sink.string.clone();

        let output = Rendering::new(OutputFormat::Text, OutputStyle::Rounded, false, output_sink);

        let result = route_ns_client(
            output,
            &NSClientCommandOptions {
                command: NSClientCommands::Version {},
                timeout_s: 30,
                user_agent: "nscp-client".to_owned(),
                profile: Some("test-profile-version".into()),
            },
        )
        .await;

        assert!(
            result.is_ok(),
            "Version command failed: {:?}",
            result.unwrap_err()
        );
        assert_eq!(
            output_ref.borrow().as_str(),
            r#"╭─────────┬────────────╮
│ name    │ NSClient++ │
│ version │ 0.5.2.35   │
╰─────────┴────────────╯
"#
        );
        drop(tmp);
    }
}
