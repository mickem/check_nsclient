use crate::cli::{AuthCommand, NSClientCommandOptions};
use crate::config;
use crate::nsclient::login_helper::login_and_fetch_key;
use crate::nsclient::{ConnectionOptions, build_client_for_profile, resolve_profile};
use crate::rendering::Rendering;
use indexmap::IndexMap;
use serde::Serialize;

/// What `auth status` reports about the profile currently in use.
#[derive(Serialize)]
struct AuthStatus {
    profile: String,
    url: String,
    username: String,
    /// The user the server says the stored credentials belong to.
    user: String,
    authenticated: bool,
}

impl AuthStatus {
    fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("profile".to_string(), self.profile.clone());
        map.insert("url".to_string(), self.url.clone());
        map.insert("username".to_string(), self.username.clone());
        map.insert("user".to_string(), self.user.clone());
        map.insert("authenticated".to_string(), self.authenticated.to_string());
        map
    }
}

/// Use the password given on the command line / environment, or prompt for it.
fn resolve_password(password: &Option<String>) -> anyhow::Result<String> {
    if let Some(password) = password {
        return Ok(password.clone());
    }
    let password = rpassword::prompt_password("Password: ")
        .map_err(|e| anyhow::anyhow!("Failed to read password: {e}"))?;
    if password.is_empty() {
        anyhow::bail!("No password given");
    }
    Ok(password)
}

pub async fn route_auth_commands(
    output: Rendering,
    args: &NSClientCommandOptions,
    command: &AuthCommand,
) -> anyhow::Result<()> {
    let options = &ConnectionOptions::from_args(args);
    match command {
        AuthCommand::Login {
            id,
            username,
            password,
            url,
            insecure,
            ca,
        } => {
            let password = resolve_password(password)?;
            let key = match login_and_fetch_key(
                url,
                username,
                &password,
                *insecure,
                ca.to_owned(),
                options,
            )
            .await
            {
                Ok(key) => key,
                Err(e) => anyhow::bail!("Failed to login: {:#}", e),
            };
            if let Err(e) = config::add_nsclient_profile(
                id,
                url,
                *insecure,
                username,
                &password,
                &key,
                ca.to_owned(),
            ) {
                anyhow::bail!("Failed to save profile: {:#}", e);
            }
            output.print("Successfully logged in");
            Ok(())
        }
        AuthCommand::Refresh { id } => {
            let profile = match config::get_nsclient_profile(id)? {
                Some(profile) => profile,
                None => anyhow::bail!("Profile with id '{id}' does not exist"),
            };
            let password = config::get_password(id)?;
            let key = match login_and_fetch_key(
                &profile.url,
                &profile.username,
                &password,
                profile.insecure,
                profile.ca,
                options,
            )
            .await
            {
                Ok(key) => key,
                Err(e) => anyhow::bail!("Failed to login: {:#}", e),
            };
            config::update_token(id, &key)?;
            output.print("Token successfully refreshed");
            Ok(())
        }
        AuthCommand::Status {} => {
            let profile = resolve_profile(args.profile.as_deref())?;
            let api = build_client_for_profile(&profile, options)?;
            let details = match api.login().await {
                Ok(details) => details,
                Err(e) => anyhow::bail!("Not authenticated as profile '{}': {:#}", profile.id, e),
            };
            let status = AuthStatus {
                profile: profile.id.clone(),
                url: profile.url.clone(),
                username: profile.username.clone(),
                // Servers before 0.18 do not report the user; fall back to the
                // name the profile logged in with so the column is never blank.
                user: if details.user.is_empty() {
                    profile.username.clone()
                } else {
                    details.user
                },
                authenticated: true,
            };
            output.render_single(&status, AuthStatus::to_dict)
        }
        AuthCommand::Logout { id } => {
            let profile = match config::get_nsclient_profile(id)? {
                Some(profile) => profile,
                None => anyhow::bail!("Profile with id '{id}' does not exist"),
            };
            // Revoke the token on the server first so it cannot be used by
            // anyone who got hold of it. A server that is unreachable (or has
            // already expired the token) must not stop us from forgetting the
            // local credentials, so failures are reported but not fatal.
            match build_client_for_profile(&profile, options) {
                Ok(api) => {
                    if let Err(e) = api.logout().await {
                        output.print(&format!(
                            "Warning: could not revoke the token on the server: {e:#}"
                        ));
                    }
                }
                Err(e) => output.print(&format!(
                    "Warning: could not connect to the server to revoke the token: {e:#}"
                )),
            }
            if let Err(e) = config::remove_nsclient_profile(id) {
                anyhow::bail!("Failed to logout: {:#}", e);
            }
            output.print("Successfully logged out");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::config::{add_nsclient_profile, mock_test_config};
    use crate::rendering::StringRender;
    use crate::tokens::{self, KeyType};
    use std::cell::RefCell;
    use std::rc::Rc;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn rendering() -> (Rendering, Rc<RefCell<String>>) {
        let sink = Box::new(StringRender::new());
        let out = sink.string.clone();
        (
            Rendering::new(OutputFormat::Text, OutputStyle::Rounded, false, sink),
            out,
        )
    }

    fn args(profile: Option<&str>) -> NSClientCommandOptions {
        NSClientCommandOptions {
            command: crate::cli::NSClientCommands::Ping {},
            timeout_s: 5,
            user_agent: "test-agent".into(),
            profile: profile.map(str::to_string),
        }
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn logout_revokes_the_token_before_removing_the_profile() {
        let tmp = mock_test_config();
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/login"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        add_nsclient_profile(
            "bye",
            &server.uri(),
            false,
            "admin",
            "pw",
            "the-token",
            None,
        )
        .unwrap();

        let (output, out) = rendering();
        route_auth_commands(
            output,
            &args(None),
            &AuthCommand::Logout { id: "bye".into() },
        )
        .await
        .unwrap();

        assert_eq!(out.borrow().as_str(), "Successfully logged out\n");
        assert!(config::get_nsclient_profile("bye").unwrap().is_none());
        assert!(!tokens::token_exists(KeyType::Token, "bye"));
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn logout_still_removes_the_profile_when_the_server_rejects_it() {
        let tmp = mock_test_config();
        let server = MockServer::start().await;
        // 500 (not 401) so the client does not try to refresh the token first.
        Mock::given(method("DELETE"))
            .and(path("/api/v2/login"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        add_nsclient_profile(
            "bye2",
            &server.uri(),
            false,
            "admin",
            "pw",
            "the-token",
            None,
        )
        .unwrap();

        let (output, out) = rendering();
        route_auth_commands(
            output,
            &args(None),
            &AuthCommand::Logout { id: "bye2".into() },
        )
        .await
        .unwrap();

        let rendered = out.borrow();
        assert!(
            rendered.contains("Warning: could not revoke the token on the server"),
            "{rendered}"
        );
        assert!(rendered.contains("Successfully logged out"), "{rendered}");
        assert!(config::get_nsclient_profile("bye2").unwrap().is_none());
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn status_reports_the_authenticated_user() {
        let tmp = mock_test_config();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/login"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"user": "admin", "key": "the-token"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        add_nsclient_profile(
            "who",
            &server.uri(),
            false,
            "admin",
            "pw",
            "the-token",
            None,
        )
        .unwrap();

        let (output, out) = rendering();
        route_auth_commands(output, &args(Some("who")), &AuthCommand::Status {})
            .await
            .unwrap();

        let rendered = out.borrow();
        assert!(rendered.contains("│ profile"), "{rendered}");
        assert!(rendered.contains("who"), "{rendered}");
        assert!(rendered.contains("│ user"), "{rendered}");
        assert!(rendered.contains("admin"), "{rendered}");
        assert!(rendered.contains("authenticated"), "{rendered}");
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn status_falls_back_to_the_profile_user_when_the_server_omits_it() {
        let tmp = mock_test_config();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/login"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"key": "the-token"})),
            )
            .mount(&server)
            .await;
        add_nsclient_profile("old", &server.uri(), false, "operator", "pw", "t", None).unwrap();

        let (output, out) = rendering();
        route_auth_commands(output, &args(Some("old")), &AuthCommand::Status {})
            .await
            .unwrap();

        assert!(out.borrow().contains("operator"), "{}", out.borrow());
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn status_reports_a_rejected_token() {
        let tmp = mock_test_config();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/login"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        add_nsclient_profile("bad", &server.uri(), false, "admin", "pw", "t", None).unwrap();

        let (output, _) = rendering();
        let err = route_auth_commands(output, &args(Some("bad")), &AuthCommand::Status {})
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Not authenticated as profile 'bad'"),
            "{err}"
        );
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn logout_reports_an_unknown_profile() {
        let tmp = mock_test_config();
        let (output, _) = rendering();
        let err = route_auth_commands(
            output,
            &args(None),
            &AuthCommand::Logout { id: "nope".into() },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Profile with id 'nope' does not exist");
        drop(tmp);
    }
}
