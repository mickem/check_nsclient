use crate::nsclient::api::Auth;
use crate::nsclient::{ConnectionOptions, build_client};

/// Log in with username/password and return the API key issued by NSClient++.
pub async fn login_and_fetch_key(
    url: &str,
    username: &str,
    password: &str,
    insecure: bool,
    ca: Option<String>,
    options: &ConnectionOptions,
) -> anyhow::Result<String> {
    let api = build_client(
        url,
        options,
        Auth::Password(username.to_owned(), password.to_owned()),
        insecure,
        None,
        ca,
    )?;
    match api.login().await {
        Ok(details) => Ok(details.key),
        Err(e) => anyhow::bail!("Failed to login: {:#}", e),
    }
}
