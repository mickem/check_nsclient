//! An in-memory credential store for the unit tests, so they never touch the
//! OS keyring. Entries for the same target, service and user share one secret,
//! like entries in a real store do.

use keyring_core::api::{Credential, CredentialApi, CredentialPersistence, CredentialStoreApi};
use keyring_core::{Entry, Error, Result};
use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

type Secrets = HashMap<String, Vec<u8>>;

static SECRETS: LazyLock<Mutex<Secrets>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn secrets() -> MutexGuard<'static, Secrets> {
    SECRETS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Make the mock the default store and forget every credential stored in it
/// (call at the start of each test).
pub fn install() {
    let installed =
        keyring_core::get_default_store().is_some_and(|store| store.as_any().is::<MockStore>());
    if !installed {
        keyring_core::set_default_store(Arc::new(MockStore));
    }
    secrets().clear();
}

#[derive(Debug)]
pub struct MockCredential {
    id: String,
    service: String,
    user: String,
}

impl CredentialApi for MockCredential {
    fn set_secret(&self, secret: &[u8]) -> Result<()> {
        secrets().insert(self.id.clone(), secret.to_vec());
        Ok(())
    }

    fn get_secret(&self) -> Result<Vec<u8>> {
        secrets().get(&self.id).cloned().ok_or(Error::NoEntry)
    }

    fn delete_credential(&self) -> Result<()> {
        secrets().remove(&self.id).map(|_| ()).ok_or(Error::NoEntry)
    }

    fn get_credential(&self) -> Result<Option<Arc<Credential>>> {
        self.get_secret().map(|_| None)
    }

    fn get_specifiers(&self) -> Option<(String, String)> {
        Some((self.service.clone(), self.user.clone()))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug)]
pub struct MockStore;

impl CredentialStoreApi for MockStore {
    fn vendor(&self) -> String {
        "check_nsclient in-memory test store".to_string()
    }

    fn id(&self) -> String {
        "mock".to_string()
    }

    /// Accepts the modifiers the platform stores take (`target`, or `keychain`
    /// on macOS) and keeps entries with different ones apart.
    fn build(
        &self,
        service: &str,
        user: &str,
        modifiers: Option<&HashMap<&str, &str>>,
    ) -> Result<Entry> {
        let target = modifiers.and_then(|m| m.get("target").or_else(|| m.get("keychain")));
        let id = format!("MockCredential(target={target:?}, service={service}, user={user})");
        Ok(Entry::new_with_credential(Arc::new(MockCredential {
            id,
            service: service.to_string(),
            user: user.to_string(),
        })))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn persistence(&self) -> CredentialPersistence {
        CredentialPersistence::ProcessOnly
    }
}
