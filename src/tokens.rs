use crate::constants::SERVICE_NAME;
use keyring_core::{CredentialStore, Entry};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

static WSL_WORKAROUND: LazyLock<AtomicBool> = LazyLock::new(|| AtomicBool::new(false));

pub fn enable_wsl_workaround() {
    WSL_WORKAROUND.store(true, Ordering::Relaxed);
}

/// The OS credential store for this platform: Keychain on macOS, Credential
/// Manager on Windows and the Secret Service everywhere else.
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    #[cfg(target_os = "macos")]
    return Ok(apple_native_keyring_store::keychain::Store::new()?);
    #[cfg(target_os = "windows")]
    return Ok(windows_native_keyring_store::Store::new()?);
    #[cfg(all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    ))]
    return Ok(dbus_secret_service_keyring_store::Store::new()?);
    #[cfg(not(any(windows, all(unix, not(any(target_os = "ios", target_os = "android"))))))]
    return Err(keyring_core::Error::NoDefaultStore);
}

/// Install the platform store as the keyring-core default unless a store is
/// already installed (the unit tests install an in-memory one).
///
/// Done on first use rather than at startup: creating the Secret Service store
/// connects to D-Bus, which must not fail commands that never touch a token.
fn ensure_default_store() -> keyring_core::Result<()> {
    static INIT: Mutex<()> = Mutex::new(());
    let _guard = INIT.lock().unwrap_or_else(|e| e.into_inner());
    if keyring_core::get_default_store().is_none() {
        keyring_core::set_default_store(platform_store()?);
    }
    Ok(())
}

/// The store modifiers that pin an entry to the place keyring 3.x kept it, so
/// credentials saved by older builds are still found.
///
/// Normally an entry is addressed by service and key alone. With `--wsl` it is
/// addressed by an explicit target named after the service instead, which is
/// what `Entry::new_with_target(SERVICE_NAME, SERVICE_NAME, key)` did in 3.x.
fn entry_modifiers(wsl: bool) -> HashMap<&'static str, &'static str> {
    let mut modifiers = HashMap::new();
    if cfg!(target_os = "macos") {
        // The Keychain has no free-form target: 3.x read it as a keychain name.
        if wsl {
            modifiers.insert("keychain", SERVICE_NAME);
        }
    } else if cfg!(target_os = "windows") {
        // Without a target the credential is named "<key>.<service>", as in 3.x.
        if wsl {
            modifiers.insert("target", SERVICE_NAME);
        }
    } else {
        // Secret Service: 3.x always set a `target` attribute, "default" unless
        // told otherwise, and searched on it. Leaving it out here would match
        // the plain and the `--wsl` item for the same key at once.
        modifiers.insert("target", if wsl { SERVICE_NAME } else { "default" });
    }
    modifiers
}

pub fn make_entry(key_type: &KeyType, id: &str) -> anyhow::Result<Entry> {
    ensure_default_store()?;
    let modifiers = entry_modifiers(WSL_WORKAROUND.load(Ordering::Relaxed));
    Ok(Entry::new_with_modifiers(
        SERVICE_NAME,
        &key_type.get_key(id),
        &modifiers,
    )?)
}

fn show_wsl_info() {
    if !WSL_WORKAROUND.load(Ordering::Relaxed) {
        eprintln!(
            "Failed to store token in keystore, if you're running under wsl try specifying: check_nsclient --wsl"
        );
    }
}

pub enum KeyType {
    Token,
    Password,
}

impl Display for KeyType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyType::Token => write!(f, "token"),
            KeyType::Password => write!(f, "password"),
        }
    }
}
impl KeyType {
    pub fn get_key(&self, profile_id: &str) -> String {
        match self {
            KeyType::Token => format!("{profile_id}_token"),
            KeyType::Password => format!("{profile_id}_password"),
        }
    }
}
pub fn store_token(key_type: KeyType, id: &str, token: &str) -> anyhow::Result<()> {
    let entry = make_entry(&key_type, id)?;
    if let Err(e) = entry.set_password(token) {
        show_wsl_info();
        anyhow::bail!("Failed to store token in keystore: {e:?}")
    }
    Ok(())
}

pub fn load_token(key_type: KeyType, id: &str) -> anyhow::Result<String> {
    let entry = make_entry(&key_type, id)?;
    match entry.get_password() {
        Ok(token) => Ok(token),
        Err(err) => {
            show_wsl_info();
            anyhow::bail!("No {key_type} found for user {id}: {err:?}")
        }
    }
}

pub fn clear_token(id: &str) -> anyhow::Result<()> {
    for key_type in &[KeyType::Token, KeyType::Password] {
        let entry = make_entry(key_type, id)?;
        let _ = entry.delete_credential();
    }
    Ok(())
}

pub fn token_exists(key_type: KeyType, id: &str) -> bool {
    match make_entry(&key_type, id) {
        Ok(entry) => entry.get_password().is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--wsl` must address a different entry than the default, and on the
    /// Secret Service both must carry the `target` keyring 3.x searched on.
    #[test]
    fn wsl_entries_use_the_service_name_as_target() {
        let plain = entry_modifiers(false);
        let wsl = entry_modifiers(true);
        assert_ne!(plain, wsl);
        assert!(wsl.values().all(|v| *v == SERVICE_NAME));
        if cfg!(all(unix, not(target_os = "macos"))) {
            assert_eq!(plain.get("target"), Some(&"default"));
            assert_eq!(wsl.get("target"), Some(&SERVICE_NAME));
        } else {
            assert!(plain.is_empty());
        }
    }
}
