// ABOUTME: OAuth flow helpers and encrypted session persistence
// ABOUTME: AES-256-GCM encryption for session cookies stored at rest
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use std::env;
use std::path::Path;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use ring::aead;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::fs;
use tracing::{debug, warn};

use crate::config::session_dir;
use crate::error::{ScraperError, ScraperResult};
use crate::models::AuthSession;

const SESSION_FILE: &str = "session.enc";
const KEY_FILE: &str = "session.key";

/// Environment variable holding a base64-encoded 32-byte AES-256-GCM key.
/// When set (e.g. sourced from a secret manager or the deployment's `.envrc`),
/// the key is taken from here and is never written to disk next to the
/// ciphertext. When unset, the key falls back to the on-disk `session.key`.
const KEY_ENV: &str = "DRAVR_SCIOTTE_SESSION_KEY";

/// AES-256-GCM key length in bytes.
const KEY_LEN: usize = 32;

/// Save an authenticated session to disk (encrypted)
pub async fn save_session(session: &AuthSession) -> ScraperResult<()> {
    let dir = session_dir();
    fs::create_dir_all(&dir)
        .await
        .map_err(|e| ScraperError::Internal {
            reason: format!("Failed to create session dir: {e}"),
        })?;

    let key = load_or_create_key(&dir).await?;
    let plaintext = serde_json::to_vec(session).map_err(|e| ScraperError::Internal {
        reason: format!("Failed to serialize session: {e}"),
    })?;

    let encrypted = encrypt(&key, &plaintext)?;
    let encoded = BASE64_STANDARD.encode(&encrypted);

    fs::write(dir.join(SESSION_FILE), encoded.as_bytes())
        .await
        .map_err(|e| ScraperError::Internal {
            reason: format!("Failed to write session file: {e}"),
        })?;

    debug!("Session saved to {}", dir.join(SESSION_FILE).display());
    Ok(())
}

/// Load a previously saved session from disk
pub async fn load_session() -> ScraperResult<Option<AuthSession>> {
    let dir = session_dir();
    let session_path = dir.join(SESSION_FILE);

    if !session_path.exists() {
        return Ok(None);
    }

    let key = load_or_create_key(&dir).await?;
    let encoded = fs::read_to_string(&session_path)
        .await
        .map_err(|e| ScraperError::Internal {
            reason: format!("Failed to read session file: {e}"),
        })?;

    let encrypted = BASE64_STANDARD
        .decode(encoded.trim())
        .map_err(|e| ScraperError::Internal {
            reason: format!("Failed to decode session: {e}"),
        })?;

    let plaintext = decrypt(&key, &encrypted)?;
    let session: AuthSession =
        serde_json::from_slice(&plaintext).map_err(|e| ScraperError::Internal {
            reason: format!("Failed to deserialize session: {e}"),
        })?;

    debug!("Session loaded from {}", session_path.display());
    Ok(Some(session))
}

/// Delete the saved session
pub async fn clear_session() -> ScraperResult<()> {
    let dir = session_dir();
    let session_path = dir.join(SESSION_FILE);

    if session_path.exists() {
        fs::remove_file(&session_path)
            .await
            .map_err(|e| ScraperError::Internal {
                reason: format!("Failed to remove session file: {e}"),
            })?;
        debug!("Session cleared");
    } else {
        warn!("No session file to clear");
    }
    Ok(())
}

// ============================================================================
// Encryption helpers (AES-256-GCM)
// ============================================================================

async fn load_or_create_key(dir: &Path) -> ScraperResult<aead::LessSafeKey> {
    // Prefer an externally-supplied key (secret manager / env) so the key does
    // not have to live in plaintext on disk next to the ciphertext it protects.
    // This branch never reads or writes the on-disk key file.
    if let Ok(encoded) = env::var(KEY_ENV) {
        if !encoded.trim().is_empty() {
            let key_bytes =
                BASE64_STANDARD
                    .decode(encoded.trim())
                    .map_err(|e| ScraperError::Config {
                        reason: format!("{KEY_ENV} is not valid base64: {e}"),
                    })?;
            if key_bytes.len() != KEY_LEN {
                return Err(ScraperError::Config {
                    reason: format!(
                        "{KEY_ENV} must decode to {KEY_LEN} bytes, got {}",
                        key_bytes.len()
                    ),
                });
            }
            return build_key(&key_bytes);
        }
    }

    let key_path = dir.join(KEY_FILE);

    let key_bytes = if key_path.exists() {
        let encoded = fs::read_to_string(&key_path)
            .await
            .map_err(|e| ScraperError::Internal {
                reason: format!("Failed to read key file: {e}"),
            })?;
        BASE64_STANDARD
            .decode(encoded.trim())
            .map_err(|e| ScraperError::Internal {
                reason: format!("Failed to decode key: {e}"),
            })?
    } else {
        let rng = SystemRandom::new();
        let mut key_bytes = vec![0u8; 32];
        rng.fill(&mut key_bytes)
            .map_err(|_| ScraperError::Internal {
                reason: "Failed to generate encryption key".to_owned(),
            })?;
        let encoded = BASE64_STANDARD.encode(&key_bytes);
        fs::write(&key_path, encoded.as_bytes())
            .await
            .map_err(|e| ScraperError::Internal {
                reason: format!("Failed to write key file: {e}"),
            })?;
        key_bytes
    };

    build_key(&key_bytes)
}

/// Wrap raw key bytes into an AES-256-GCM `LessSafeKey`.
fn build_key(key_bytes: &[u8]) -> ScraperResult<aead::LessSafeKey> {
    let unbound_key = aead::UnboundKey::new(&aead::AES_256_GCM, key_bytes).map_err(|_| {
        ScraperError::Internal {
            reason: "Invalid encryption key".to_owned(),
        }
    })?;

    Ok(aead::LessSafeKey::new(unbound_key))
}

fn encrypt(key: &aead::LessSafeKey, plaintext: &[u8]) -> ScraperResult<Vec<u8>> {
    let rng = SystemRandom::new();
    let mut nonce_bytes = [0u8; 12];
    rng.fill(&mut nonce_bytes)
        .map_err(|_| ScraperError::Internal {
            reason: "Failed to generate nonce".to_owned(),
        })?;

    let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
    let mut in_out = plaintext.to_vec();
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut in_out)
        .map_err(|_| ScraperError::Internal {
            reason: "Encryption failed".to_owned(),
        })?;

    // Prepend nonce to ciphertext
    let mut result = nonce_bytes.to_vec();
    result.extend_from_slice(&in_out);
    Ok(result)
}

fn decrypt(key: &aead::LessSafeKey, data: &[u8]) -> ScraperResult<Vec<u8>> {
    if data.len() < 12 {
        return Err(ScraperError::Internal {
            reason: "Encrypted data too short".to_owned(),
        });
    }

    let (nonce_bytes, ciphertext) = data.split_at(12);
    let nonce_array: [u8; 12] = nonce_bytes.try_into().map_err(|_| ScraperError::Internal {
        reason: "Invalid nonce length".to_owned(),
    })?;
    let nonce = aead::Nonce::assume_unique_for_key(nonce_array);

    let mut in_out = ciphertext.to_vec();
    let plaintext = key
        .open_in_place(nonce, aead::Aad::empty(), &mut in_out)
        .map_err(|_| ScraperError::Auth {
            reason: "Failed to decrypt session — key may have changed, re-login required"
                .to_owned(),
        })?;

    Ok(plaintext.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let rng = SystemRandom::new();
        let mut key_bytes = vec![0u8; 32];
        rng.fill(&mut key_bytes).unwrap(); // Safe: test with valid buffer
        let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &key_bytes).unwrap(); // Safe: test with valid key size
        let key = aead::LessSafeKey::new(unbound);

        let plaintext = b"hello strava session data";
        let encrypted = encrypt(&key, plaintext).unwrap(); // Safe: test with valid key and plaintext
        let decrypted = decrypt(&key, &encrypted).unwrap(); // Safe: test decrypting just-encrypted data
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn decrypt_too_short() {
        let rng = SystemRandom::new();
        let mut key_bytes = vec![0u8; 32];
        rng.fill(&mut key_bytes).unwrap(); // Safe: test with valid buffer
        let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &key_bytes).unwrap(); // Safe: test with valid key size
        let key = aead::LessSafeKey::new(unbound);

        let result = decrypt(&key, &[0u8; 5]);
        assert!(result.is_err());
    }

    // A key supplied via `DRAVR_SCIOTTE_SESSION_KEY` is used directly and never
    // written to disk (the env branch returns before touching `dir`), and a
    // wrong-length env value is rejected with a `Config` error. Both cases share
    // one test to serialize the process-global env mutation.
    #[tokio::test]
    async fn env_key_is_used_and_validated() {
        // Path deliberately does not exist: the env branch must not read/write it.
        let nonexistent = Path::new("/dravr-sciotte-nonexistent-key-dir");

        // Valid 32-byte env key round-trips encrypt/decrypt without disk access.
        let raw = [7u8; 32];
        let encoded = BASE64_STANDARD.encode(raw);
        env::set_var(KEY_ENV, &encoded); // Safe: edition 2021 test mutation, removed below

        let key = load_or_create_key(nonexistent)
            .await
            .expect("env key loads"); // Safe: test assertion
        let plaintext = b"session bytes via env key";
        let ciphertext = encrypt(&key, plaintext).expect("encrypt"); // Safe: test assertion
        let decrypted = decrypt(&key, &ciphertext).expect("decrypt"); // Safe: test assertion
        assert_eq!(decrypted, plaintext);

        // Wrong-length env key is rejected before any AEAD construction.
        env::set_var(KEY_ENV, BASE64_STANDARD.encode([0u8; 16]));
        let err = load_or_create_key(nonexistent).await;
        assert!(
            matches!(err, Err(ScraperError::Config { .. })),
            "wrong-length env key must be a Config error"
        );

        env::remove_var(KEY_ENV);
    }
}
