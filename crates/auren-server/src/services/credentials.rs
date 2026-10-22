//! Authenticated credential encryption (spec section 14).
//!
//! AES-256-GCM with a random nonce per record. The key is derived from the
//! configured encryption key (kept separate from the encrypted records in
//! the database) using SHA-256 as a KDF for the fixed-size key.

use aes_gcm::{
    aead::{Aead, KeyInit, OsRng as AeadOsRng},
    Aes256Gcm, AeadCore, Nonce,
};
use sha2::{Digest, Sha256};

/// Derive the 256-bit key from the configured secret.
fn derive_key(secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"auren-credential-encryption-v1");
    hasher.update(secret.as_bytes());
    hasher.finalize().into()
}

/// Encrypt a plaintext credential. Returns (ciphertext, nonce).
pub fn encrypt_credential(plaintext: &str, key_secret: &str) -> Result<(Vec<u8>, Vec<u8>), anyhow::Error> {
    let cipher = Aes256Gcm::new_from_slice(&derive_key(key_secret))?;
    let nonce = Aes256Gcm::generate_nonce(&mut AeadOsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext.as_bytes())
        .map_err(|_| anyhow::anyhow!("encryption failed"))?;
    Ok((ciphertext, nonce.to_vec()))
}

/// Decrypt a stored credential.
pub fn decrypt_credential(ciphertext: &[u8], nonce: &[u8], key_secret: &str) -> Result<String, anyhow::Error> {
    let cipher = Aes256Gcm::new_from_slice(&derive_key(key_secret))?;
    let nonce = Nonce::from_slice(nonce);
    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| anyhow::anyhow!("decryption failed: wrong key or tampered data"))?;
    String::from_utf8(plaintext).map_err(|_| anyhow::anyhow!("decrypted credential is not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let (ct, nonce) = encrypt_credential("super-secret-api-key", "key-material").unwrap();
        assert_ne!(ct, b"super-secret-api-key");
        let decrypted = decrypt_credential(&ct, &nonce, "key-material").unwrap();
        assert_eq!(decrypted, "super-secret-api-key");
    }

    #[test]
    fn wrong_key_fails() {
        let (ct, nonce) = encrypt_credential("secret", "key-a").unwrap();
        assert!(decrypt_credential(&ct, &nonce, "key-b").is_err());
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let (mut ct, nonce) = encrypt_credential("secret", "key-a").unwrap();
        ct[0] ^= 0xFF;
        assert!(decrypt_credential(&ct, &nonce, "key-a").is_err());
    }

    #[test]
    fn unique_nonce_per_record() {
        let (_, n1) = encrypt_credential("same", "key").unwrap();
        let (_, n2) = encrypt_credential("same", "key").unwrap();
        assert_ne!(n1, n2);
    }
}