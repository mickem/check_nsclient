use crate::cli::AuthCommand;
use crate::config;
use crate::nsclient::ConnectionOptions;
use crate::nsclient::login_helper::login_and_fetch_key;
use crate::rendering::Rendering;

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
    options: &ConnectionOptions,
    command: &AuthCommand,
) -> anyhow::Result<()> {
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
        AuthCommand::Logout { id } => {
            if let Err(e) = config::remove_nsclient_profile(id) {
                anyhow::bail!("Failed to logout: {:#}", e);
            } else {
                output.print("Successfully logged out");
                Ok(())
            }
        }
    }
}
