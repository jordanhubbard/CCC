//! AES-256-GCM secret vault with Argon2id key derivation.
//!
//! Ported from tokenhub/internal/vault/vault.go.
//!
//! Secrets are encrypted at rest — each value is stored as `nonce || ciphertext`.
//! The master key is derived from a password via Argon2id and held only in memory.
//! The vault auto-locks after inactivity (default 30 min) and can be unlocked with
//! a password, or configured to auto-unlock via `VAULT_PASSWORD` env var.

use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Key, Nonce,
};
use argon2::{Argon2, Params};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use std::{
    collections::HashMap,
    fmt,
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

// Argon2id parameters — OWASP recommended minimums (same as tokenhub).
const ARGON2_MEM: u32 = 65536; // 64 MB
const ARGON2_TIME: u32 = 3;
const ARGON2_LANES: u32 = 4;
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;

pub const DEFAULT_AUTO_LOCK: Duration = Duration::from_secs(30 * 60);

#[derive(Debug)]
pub enum VaultError {
    Locked,
    NotEnabled,
    NotFound(String),
    PasswordTooShort,
    WrongPassword,
    Crypto(String),
    Decode(String),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::Locked => write!(f, "vault is locked"),
            VaultError::NotEnabled => write!(f, "vault is not enabled"),
            VaultError::NotFound(k) => write!(f, "key not found: {k}"),
            VaultError::PasswordTooShort => write!(f, "password too short (min 8 chars)"),
            VaultError::WrongPassword => write!(f, "old password does not match"),
            VaultError::Crypto(s) => write!(f, "crypto error: {s}"),
            VaultError::Decode(s) => write!(f, "decode error: {s}"),
        }
    }
}

impl std::error::Error for VaultError {}

struct Inner {
    enabled: bool,
    locked: bool,
    salt: Option<Vec<u8>>,
    key: Option<Vec<u8>>,             // in-memory only; zeroed on lock
    values: HashMap<String, Vec<u8>>, // nonce || ciphertext (or plaintext if disabled)
    last_activity: Instant,
}

/// Thread-safe encrypted vault. Cheap to clone (Arc inside).
#[derive(Clone)]
pub struct Vault(Arc<RwLock<Inner>>);

impl Vault {
    pub fn new(enabled: bool) -> Self {
        Self(Arc::new(RwLock::new(Inner {
            enabled,
            locked: enabled,
            salt: None,
            key: None,
            values: HashMap::new(),
            last_activity: Instant::now(),
        })))
    }

    pub async fn is_locked(&self) -> bool {
        let g = self.0.read().await;
        g.enabled && g.locked
    }

    pub async fn is_enabled(&self) -> bool {
        self.0.read().await.enabled
    }

    /// Unlock with `password`. Generates a random salt on first call;
    /// reuses the stored salt on subsequent calls (so the same key is derived).
    pub async fn unlock(&self, password: &[u8]) -> Result<(), VaultError> {
        if password.len() < 8 {
            return Err(VaultError::PasswordTooShort);
        }
        let mut g = self.0.write().await;
        if !g.enabled {
            return Ok(());
        }
        let salt = match &g.salt {
            Some(s) => s.clone(),
            None => {
                let s = gen_salt();
                g.salt = Some(s.clone());
                s
            }
        };
        let key = derive_key(password, &salt)?;
        g.key = Some(key);
        g.locked = false;
        g.last_activity = Instant::now();
        Ok(())
    }

    pub async fn lock(&self) {
        let mut g = self.0.write().await;
        zero_key(&mut g.key);
        g.locked = true;
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<(), VaultError> {
        let mut g = self.0.write().await;
        check_unlocked(&g)?;
        let stored = if g.enabled {
            encrypt(g.key.as_deref().unwrap(), value.as_bytes())?
        } else {
            value.as_bytes().to_vec()
        };
        g.values.insert(key.to_string(), stored);
        g.last_activity = Instant::now();
        Ok(())
    }

    pub async fn get(&self, key: &str) -> Result<String, VaultError> {
        let g = self.0.read().await;
        check_unlocked(&g)?;
        let data = g
            .values
            .get(key)
            .ok_or_else(|| VaultError::NotFound(key.to_string()))?;
        let plain = if g.enabled {
            decrypt(g.key.as_deref().unwrap(), data)?
        } else {
            data.clone()
        };
        drop(g);
        self.0.write().await.last_activity = Instant::now();
        Ok(String::from_utf8(plain).map_err(|e| VaultError::Crypto(e.to_string()))?)
    }

    pub async fn delete(&self, key: &str) -> bool {
        let mut g = self.0.write().await;
        g.values.remove(key).is_some()
    }

    /// Returns sorted keys matching `prefix` (empty = all).
    pub async fn keys(&self, prefix: &str) -> Result<Vec<String>, VaultError> {
        let g = self.0.read().await;
        check_unlocked(&g)?;
        let mut keys: Vec<String> = g
            .values
            .keys()
            .filter(|k| prefix.is_empty() || k.starts_with(prefix))
            .cloned()
            .collect();
        keys.sort();
        Ok(keys)
    }

    /// Count of stored secrets (does not require unlock).
    pub async fn count(&self) -> usize {
        self.0.read().await.values.len()
    }

    /// Export encrypted blobs + salt for persistence.
    /// Returns (salt, key→base64_blob) map.
    pub async fn export(&self) -> (Option<Vec<u8>>, HashMap<String, String>) {
        let g = self.0.read().await;
        let salt = g.salt.clone();
        let exported = g
            .values
            .iter()
            .map(|(k, v)| (k.clone(), B64.encode(v)))
            .collect();
        (salt, exported)
    }

    /// Import encrypted blobs (from DB or tokenhub migration).
    /// Call `set_salt` then `unlock` before reading values.
    pub async fn import(&self, data: HashMap<String, String>) -> Result<(), VaultError> {
        let mut g = self.0.write().await;
        for (k, b64) in data {
            let decoded = B64
                .decode(&b64)
                .map_err(|e| VaultError::Decode(format!("{k}: {e}")))?;
            g.values.insert(k, decoded);
        }
        Ok(())
    }

    /// Restore a persisted salt (must be called before `unlock` on restart).
    pub async fn set_salt(&self, salt: Vec<u8>) {
        self.0.write().await.salt = Some(salt);
    }

    /// Re-encrypt all values under a new password (new random salt).
    pub async fn rotate_password(&self, old_pw: &[u8], new_pw: &[u8]) -> Result<(), VaultError> {
        if new_pw.len() < 8 {
            return Err(VaultError::PasswordTooShort);
        }
        let mut g = self.0.write().await;
        if !g.enabled {
            return Err(VaultError::NotEnabled);
        }
        if g.locked {
            return Err(VaultError::Locked);
        }

        // Verify old password against current key.
        let salt = g.salt.as_deref().unwrap_or(&[]);
        let expected = derive_key(old_pw, salt)?;
        let current = g.key.as_deref().unwrap_or(&[]);
        if expected.ct_eq(current).unwrap_u8() != 1 {
            return Err(VaultError::WrongPassword);
        }

        // Decrypt all values with current key.
        let mut plaintexts = HashMap::with_capacity(g.values.len());
        for (k, enc) in &g.values {
            plaintexts.insert(k.clone(), decrypt(current, enc)?);
        }

        // Derive new key from new salt.
        let new_salt = gen_salt();
        let new_key = derive_key(new_pw, &new_salt)?;

        // Re-encrypt under new key.
        let mut new_values = HashMap::with_capacity(plaintexts.len());
        for (k, plain) in plaintexts {
            new_values.insert(k, encrypt(&new_key, &plain)?);
        }

        g.salt = Some(new_salt);
        g.key = Some(new_key);
        g.values = new_values;
        g.last_activity = Instant::now();
        Ok(())
    }

    /// Check and trigger auto-lock if inactive beyond `timeout`.
    /// Returns true if the vault was just locked.
    pub async fn check_auto_lock(&self, timeout: Duration) -> bool {
        let mut g = self.0.write().await;
        if !g.enabled || g.locked {
            return false;
        }
        if g.last_activity.elapsed() > timeout {
            zero_key(&mut g.key);
            g.locked = true;
            return true;
        }
        false
    }
}

// ── Crypto helpers ────────────────────────────────────────────────────────────

fn gen_salt() -> Vec<u8> {
    use rand::RngCore;
    let mut s = [0u8; SALT_LEN];
    rand::rngs::OsRng.fill_bytes(&mut s);
    s.to_vec()
}

fn derive_key(password: &[u8], salt: &[u8]) -> Result<Vec<u8>, VaultError> {
    let params = Params::new(ARGON2_MEM, ARGON2_TIME, ARGON2_LANES, Some(KEY_LEN))
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    let argon = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = vec![0u8; KEY_LEN];
    argon
        .hash_password_into(password, salt, &mut key)
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    Ok(key)
}

fn encrypt(key: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
    let k = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(k);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    let mut out = Vec::with_capacity(nonce.len() + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend(ciphertext);
    Ok(out)
}

fn decrypt(key: &[u8], data: &[u8]) -> Result<Vec<u8>, VaultError> {
    const NONCE_LEN: usize = 12; // GCM standard nonce size
    if data.len() < NONCE_LEN {
        return Err(VaultError::Crypto("ciphertext too short".into()));
    }
    let k = Key::<Aes256Gcm>::from_slice(key);
    let cipher = Aes256Gcm::new(k);
    let nonce = Nonce::from_slice(&data[..NONCE_LEN]);
    cipher
        .decrypt(nonce, &data[NONCE_LEN..])
        .map_err(|_| VaultError::Crypto("decryption failed (wrong password?)".into()))
}

fn check_unlocked(g: &Inner) -> Result<(), VaultError> {
    if !g.enabled {
        return Ok(());
    }
    if g.locked {
        return Err(VaultError::Locked);
    }
    Ok(())
}

fn zero_key(key: &mut Option<Vec<u8>>) {
    if let Some(k) = key.as_mut() {
        for b in k.iter_mut() {
            *b = 0;
        }
    }
    *key = None;
}

// ── Auto-lock task ────────────────────────────────────────────────────────────

/// Spawn a background task that checks auto-lock every minute.
/// Stops when the vault is dropped (Arc refcount reaches zero).
pub fn spawn_auto_lock(vault: Vault, timeout: Duration) {
    if timeout.is_zero() {
        return;
    }
    tokio::spawn(async move {
        let interval = std::cmp::min(timeout / 2, Duration::from_secs(60));
        let mut ticker = tokio::time::interval(interval.max(Duration::from_millis(100)));
        loop {
            ticker.tick().await;
            if vault.check_auto_lock(timeout).await {
                tracing::info!("[vault] auto-locked after inactivity");
            }
        }
    });
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const PW: &[u8] = b"a]strong-password-for-testing!!";

    async fn unlocked() -> Vault {
        let v = Vault::new(true);
        v.unlock(PW).await.unwrap();
        v
    }

    #[tokio::test]
    async fn set_and_get() {
        let v = unlocked().await;
        v.set("k", "secret").await.unwrap();
        assert_eq!(v.get("k").await.unwrap(), "secret");
    }

    #[tokio::test]
    async fn locked_get_fails() {
        let v = unlocked().await;
        v.set("k", "secret").await.unwrap();
        v.lock().await;
        assert!(matches!(v.get("k").await, Err(VaultError::Locked)));
    }

    #[tokio::test]
    async fn delete_removes_key() {
        let v = unlocked().await;
        v.set("k", "secret").await.unwrap();
        v.delete("k").await;
        assert!(matches!(v.get("k").await, Err(VaultError::NotFound(_))));
    }

    #[tokio::test]
    async fn export_import_round_trip() {
        let v1 = Vault::new(true);
        v1.unlock(PW).await.unwrap();
        v1.set("a", "val-a").await.unwrap();
        v1.set("b", "val-b").await.unwrap();

        let (salt, exported) = v1.export().await;
        let salt = salt.unwrap();

        let v2 = Vault::new(true);
        v2.set_salt(salt).await;
        v2.unlock(PW).await.unwrap();
        v2.import(exported).await.unwrap();

        assert_eq!(v2.get("a").await.unwrap(), "val-a");
        assert_eq!(v2.get("b").await.unwrap(), "val-b");
    }

    #[tokio::test]
    async fn rotate_password() {
        let v = unlocked().await;
        v.set("k", "secret").await.unwrap();
        v.rotate_password(PW, b"new-password-also-strong!!")
            .await
            .unwrap();
        assert_eq!(v.get("k").await.unwrap(), "secret");
    }

    #[tokio::test]
    async fn wrong_old_password_fails_rotate() {
        let v = unlocked().await;
        assert!(matches!(
            v.rotate_password(b"wrong-password!!", b"new-password-ok!!")
                .await,
            Err(VaultError::WrongPassword)
        ));
    }

    #[tokio::test]
    async fn disabled_vault_works_without_password() {
        let v = Vault::new(false);
        v.set("k", "plain").await.unwrap();
        assert_eq!(v.get("k").await.unwrap(), "plain");
    }
}
