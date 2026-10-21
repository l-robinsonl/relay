//! The secret store.
//!
//! Secret variable values never enter the SQLite database, an export or a
//! sync payload. They live here, keyed by environment and variable name, and
//! are fetched only for the moment a request is actually sent.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use relay_models::EnvironmentId;

/// Where a secret is kept.
pub trait SecretStore: Send + Sync + 'static {
    /// Reads a secret, if it is set.
    fn get(&self, environment: &EnvironmentId, key: &str) -> Option<String>;
    /// Writes a secret.
    fn set(&self, environment: &EnvironmentId, key: &str, value: &str) -> Result<(), String>;
    /// Removes a secret.
    fn delete(&self, environment: &EnvironmentId, key: &str) -> Result<(), String>;
    /// A human-readable name for the backing store, shown in settings.
    fn backend_name(&self) -> &'static str;
}

/// An in-process store that keeps secrets only for the current session.
///
/// This is the fallback when no OS credential store is reachable — a headless
/// Linux session with no D-Bus, for instance. Secrets survive until the
/// application exits and are never written anywhere, which is a defensible
/// default: losing them on restart is annoying, writing them to a plaintext
/// file would be worse.
#[derive(Default, Clone)]
pub struct MemorySecretStore {
    entries: Arc<RwLock<BTreeMap<String, String>>>,
}

impl MemorySecretStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn compose_key(environment: &EnvironmentId, key: &str) -> String {
        format!("{environment}\u{1}{key}")
    }

    /// How many secrets are held. For diagnostics and tests.
    pub fn len(&self) -> usize {
        self.entries.read().map(|e| e.len()).unwrap_or(0)
    }

    /// True when nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, environment: &EnvironmentId, key: &str) -> Option<String> {
        self.entries
            .read()
            .ok()?
            .get(&Self::compose_key(environment, key))
            .cloned()
    }

    fn set(&self, environment: &EnvironmentId, key: &str, value: &str) -> Result<(), String> {
        self.entries
            .write()
            .map_err(|_| "the secret store lock was poisoned".to_string())?
            .insert(Self::compose_key(environment, key), value.to_string());
        Ok(())
    }

    fn delete(&self, environment: &EnvironmentId, key: &str) -> Result<(), String> {
        self.entries
            .write()
            .map_err(|_| "the secret store lock was poisoned".to_string())?
            .remove(&Self::compose_key(environment, key));
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "in-memory (this session only)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_round_trip() {
        let store = MemorySecretStore::new();
        let env = EnvironmentId::from("e1");

        assert_eq!(store.get(&env, "token"), None);
        store.set(&env, "token", "sk-live-1").unwrap();
        assert_eq!(store.get(&env, "token").as_deref(), Some("sk-live-1"));

        store.delete(&env, "token").unwrap();
        assert_eq!(store.get(&env, "token"), None);
    }

    #[test]
    fn secrets_are_scoped_per_environment() {
        let store = MemorySecretStore::new();
        let dev = EnvironmentId::from("dev");
        let prod = EnvironmentId::from("prod");

        store.set(&dev, "token", "dev-token").unwrap();
        store.set(&prod, "token", "prod-token").unwrap();

        // Switching environment must not leak a production credential into
        // a development request.
        assert_eq!(store.get(&dev, "token").as_deref(), Some("dev-token"));
        assert_eq!(store.get(&prod, "token").as_deref(), Some("prod-token"));
    }

    #[test]
    fn composed_keys_cannot_collide_across_environments() {
        // A naive "env:key" scheme collides when an id or key contains the
        // separator. The separator used here cannot appear in either.
        let store = MemorySecretStore::new();
        let a = EnvironmentId::from("e1:x");
        let b = EnvironmentId::from("e1");
        store.set(&a, "token", "A").unwrap();
        store.set(&b, "x:token", "B").unwrap();
        assert_eq!(store.get(&a, "token").as_deref(), Some("A"));
        assert_eq!(store.get(&b, "x:token").as_deref(), Some("B"));
    }
}
