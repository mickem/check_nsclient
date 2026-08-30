use crate::cli::ProfileCommands;
use crate::config::{self, NSClientProfile};
use crate::rendering::Rendering;
use crate::tokens;
use crate::tokens::KeyType;
use indexmap::IndexMap;
use serde::Serialize;
use tabled::Tabled;

/// A profile together with the state of its stored credentials, as shown to the user.
#[derive(Tabled, Serialize)]
struct ProfileRow {
    #[tabled()]
    id: String,
    #[tabled()]
    url: String,
    #[tabled()]
    username: String,
    #[tabled(display("crate::profile::display_bool"))]
    insecure: bool,
    #[tabled(display("crate::profile::display_option"))]
    ca: Option<String>,
    #[tabled(display("crate::profile::display_bool"))]
    default: bool,
    #[tabled(display("crate::profile::display_bool"))]
    has_token: bool,
    #[tabled(display("crate::profile::display_bool"))]
    has_password: bool,
}

fn display_bool(value: &bool) -> String {
    if *value {
        "yes".to_string()
    } else {
        "no".to_string()
    }
}

fn display_option(value: &Option<String>) -> String {
    value.clone().unwrap_or_default()
}

fn map_profile_to_row(profile: &NSClientProfile, default_id: Option<&str>) -> ProfileRow {
    let has_token = tokens::token_exists(KeyType::Token, &profile.id);
    let has_password = tokens::token_exists(KeyType::Password, &profile.id);
    ProfileRow {
        id: profile.id.clone(),
        url: profile.url.clone(),
        username: profile.username.clone(),
        insecure: profile.insecure,
        ca: profile.ca.clone(),
        default: default_id == Some(profile.id.as_str()),
        has_token,
        has_password,
    }
}

fn row_to_indexmap(row: &ProfileRow) -> IndexMap<String, String> {
    let mut map = IndexMap::new();
    map.insert("id".to_string(), row.id.clone());
    map.insert("url".to_string(), row.url.clone());
    map.insert("username".to_string(), row.username.clone());
    map.insert("insecure".to_string(), row.insecure.to_string());
    map.insert("ca".to_string(), display_option(&row.ca));
    map.insert("default".to_string(), row.default.to_string());
    map.insert("has_token".to_string(), row.has_token.to_string());
    map.insert("has_password".to_string(), row.has_password.to_string());
    map
}

pub async fn route_profile(output: Rendering, command: &ProfileCommands) -> anyhow::Result<()> {
    match &command {
        ProfileCommands::List {} => {
            let (profiles, default_id) = config::list_nsclient_profiles()?;
            if profiles.is_empty() {
                output.print("No profiles configured");
                return Ok(());
            }
            let rows: Vec<ProfileRow> = profiles
                .iter()
                .map(|profile| map_profile_to_row(profile, default_id.as_deref()))
                .collect();
            output.render_rows(&rows, &false, &["ca"])?;
        }
        ProfileCommands::Show { id } => {
            let profile = config::get_nsclient_profile(id)?
                .ok_or_else(|| anyhow::anyhow!("Profile with id '{id}' does not exist"))?;
            let default_id = config::get_default_nsclient_profile()?.map(|p| p.id);
            let row = map_profile_to_row(&profile, default_id.as_deref());
            output.render_single(&row, row_to_indexmap)?;
        }
        ProfileCommands::SetDefault { id } => {
            config::set_default_nsclient_profile(id)?;
            output.print("Default profile updated");
        }
        ProfileCommands::Remove { id } => {
            config::remove_nsclient_profile(id)?;
            output.print("Profile removed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::config::{add_nsclient_profile, mock_test_config};
    use crate::rendering::StringRender;
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
    #[serial_test::serial(config)]
    async fn list_without_profiles() {
        let tmp = mock_test_config();
        let (output, out) = rendering(OutputFormat::Text);
        route_profile(output, &ProfileCommands::List {})
            .await
            .unwrap();
        assert_eq!(out.borrow().as_str(), "No profiles configured\n");
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn list_text_shows_username_default_and_credential_state() {
        let tmp = mock_test_config();
        add_nsclient_profile("one", "https://one:8443", false, "admin", "pw", "key", None).unwrap();
        add_nsclient_profile(
            "two",
            "https://two:8443",
            true,
            "operator",
            "pw",
            "key",
            Some("/ca.pem".into()),
        )
        .unwrap();
        let (output, out) = rendering(OutputFormat::Text);

        route_profile(output, &ProfileCommands::List {})
            .await
            .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| id  | url              | username | insecure | default | has_token | has_password |\n\
             |-----|------------------|----------|----------|---------|-----------|--------------|\n\
             | one | https://one:8443 | admin    | no       | yes     | yes       | yes          |\n\
             | two | https://two:8443 | operator | yes      | no      | yes       | yes          |\n"
        );
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn list_json_includes_credential_state_and_ca() {
        let tmp = mock_test_config();
        add_nsclient_profile(
            "one",
            "https://one:8443",
            false,
            "admin",
            "pw",
            "key",
            Some("/ca.pem".into()),
        )
        .unwrap();
        let (output, out) = rendering(OutputFormat::Json);

        route_profile(output, &ProfileCommands::List {})
            .await
            .unwrap();

        let parsed: serde_json::Value = serde_json::from_str(&out.borrow()).unwrap();
        assert_eq!(parsed[0]["id"], "one");
        assert_eq!(parsed[0]["username"], "admin");
        assert_eq!(parsed[0]["ca"], "/ca.pem");
        assert_eq!(parsed[0]["default"], true);
        assert_eq!(parsed[0]["has_token"], true);
        assert_eq!(parsed[0]["has_password"], true);
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn show_text_renders_all_fields() {
        let tmp = mock_test_config();
        add_nsclient_profile("one", "https://one:8443", false, "admin", "pw", "key", None).unwrap();
        let (output, out) = rendering(OutputFormat::Text);

        route_profile(output, &ProfileCommands::Show { id: "one".into() })
            .await
            .unwrap();

        assert_eq!(
            out.borrow().as_str(),
            "| id           | one              |\n\
             | url          | https://one:8443 |\n\
             | username     | admin            |\n\
             | insecure     | false            |\n\
             | ca           |                  |\n\
             | default      | true             |\n\
             | has_token    | true             |\n\
             | has_password | true             |\n"
        );
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn show_unknown_profile_is_an_error() {
        let tmp = mock_test_config();
        let (output, _) = rendering(OutputFormat::Text);
        let err = route_profile(
            output,
            &ProfileCommands::Show {
                id: "missing".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Profile with id 'missing' does not exist");
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn set_default_and_remove() {
        let tmp = mock_test_config();
        add_nsclient_profile("one", "u", false, "a", "pw", "key", None).unwrap();
        add_nsclient_profile("two", "u", false, "a", "pw", "key", None).unwrap();

        let (output, out) = rendering(OutputFormat::Text);
        route_profile(output, &ProfileCommands::SetDefault { id: "two".into() })
            .await
            .unwrap();
        assert_eq!(out.borrow().as_str(), "Default profile updated\n");
        assert_eq!(
            config::get_default_nsclient_profile().unwrap().unwrap().id,
            "two"
        );

        let (output, out) = rendering(OutputFormat::Text);
        route_profile(output, &ProfileCommands::Remove { id: "two".into() })
            .await
            .unwrap();
        assert_eq!(out.borrow().as_str(), "Profile removed\n");
        assert!(config::get_nsclient_profile("two").unwrap().is_none());
        assert!(!tokens::token_exists(KeyType::Token, "two"));

        let (output, _) = rendering(OutputFormat::Text);
        assert!(
            route_profile(output, &ProfileCommands::Remove { id: "two".into() },)
                .await
                .is_err()
        );
        drop(tmp);
    }
}
