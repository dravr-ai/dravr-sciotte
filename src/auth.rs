// ABOUTME: OAuth flow helpers and encrypted session persistence
// ABOUTME: AES-256-GCM encryption for session cookies stored at rest
//
// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (c) 2026 dravr.ai

use base64::Engine;
use ring::aead;
use ring::rand::{SecureRandom, SystemRandom};
use tracing::{debug, warn};

use crate::config::session_dir;
use crate::error::{ScraperError, ScraperResult};
use crate::models::AuthSession;

const SESSION_FILE: &str = "session.enc";
const KEY_FILE: &str = "session.key";

/// Save an authenticated session to disk (encrypted)
pub async fn save_session(session: &AuthSession) -> ScraperResult<()> {
    let dir = session_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ScraperError::Internal {
            reason: format!("Failed to create session dir: {e}"),
        })?;

    let key = load_or_create_key(&dir).await?;
    let plaintext = serde_json::to_vec(session).map_err(|e| ScraperError::Internal {
        reason: format!("Failed to serialize session: {e}"),
    })?;

    let encrypted = encrypt(&key, &plaintext)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&encrypted);

    tokio::fs::write(dir.join(SESSION_FILE), encoded.as_bytes())
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
    let encoded = tokio::fs::read_to_string(&session_path)
        .await
        .map_err(|e| ScraperError::Internal {
            reason: format!("Failed to read session file: {e}"),
        })?;

    let encrypted = base64::engine::general_purpose::STANDARD
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
        tokio::fs::remove_file(&session_path)
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

async fn load_or_create_key(dir: &std::path::Path) -> ScraperResult<aead::LessSafeKey> {
    let key_path = dir.join(KEY_FILE);

    let key_bytes = if key_path.exists() {
        let encoded =
            tokio::fs::read_to_string(&key_path)
                .await
                .map_err(|e| ScraperError::Internal {
                    reason: format!("Failed to read key file: {e}"),
                })?;
        base64::engine::general_purpose::STANDARD
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
        let encoded = base64::engine::general_purpose::STANDARD.encode(&key_bytes);
        tokio::fs::write(&key_path, encoded.as_bytes())
            .await
            .map_err(|e| ScraperError::Internal {
                reason: format!("Failed to write key file: {e}"),
            })?;
        key_bytes
    };

    let unbound_key = aead::UnboundKey::new(&aead::AES_256_GCM, &key_bytes).map_err(|_| {
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
        rng.fill(&mut key_bytes).unwrap();
        let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &key_bytes).unwrap();
        let key = aead::LessSafeKey::new(unbound);

        let plaintext = b"hello strava session data";
        let encrypted = encrypt(&key, plaintext).unwrap();
        let decrypted = decrypt(&key, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn decrypt_too_short() {
        let rng = SystemRandom::new();
        let mut key_bytes = vec![0u8; 32];
        rng.fill(&mut key_bytes).unwrap();
        let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &key_bytes).unwrap();
        let key = aead::LessSafeKey::new(unbound);

        let result = decrypt(&key, &[0u8; 5]);
        assert!(result.is_err());
    }
}
