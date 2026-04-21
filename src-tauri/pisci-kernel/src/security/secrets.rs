use anyhow::Result;
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use std::path::Path;

const KEY_FILE: &str = ".secret_key";

pub struct SecretStore {
    key: [u8; 32],
}

impl SecretStore {
    pub fn new(app_dir: &Path) -> Result<Self> {
        let key_path = app_dir.join(KEY_FILE);
        let key = if key_path.exists() {
            let data = std::fs::read(&key_path)?;
            if data.len() != 32 {
                return Err(anyhow::anyhow!("Invalid secret key file"));
            }
            let mut key = [0u8; 32];
            key.copy_from_slice(&data);
            key
        } else {
            use std::io::Write;
            let mut key = [0u8; 32];
            getrandom::getrandom(&mut key)
                .map_err(|e| anyhow::anyhow!("Failed to generate random key: {}", e))?;
            if let Some(parent) = key_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut f = std::fs::File::create(&key_path)?;
            f.write_all(&key)?;
            key
        };
        Ok(Self { key })
    }

    pub fn encrypt(&self, plaintext: &str) -> Result<Vec<u8>> {
        if plaintext.is_empty() {
            return Ok(Vec::new());
        }
        let cipher = ChaCha20Poly1305::new((&self.key).into());
        let mut nonce_bytes = [0u8; 12];
        getrandom::getrandom(&mut nonce_bytes)
            .map_err(|e| anyhow::anyhow!("Failed to generate nonce: {}", e))?;
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| anyhow::anyhow!("Encryption failed: {}", e))?;
        let mut result = Vec::with_capacity(12 + ciphertext.len());
        result.extend_from_slice(&nonce_bytes);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }

    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<String> {
        if ciphertext.is_empty() {
            return Ok(String::new());
        }
        if ciphertext.len() < 12 + 16 {
            return Err(anyhow::anyhow!(
                "Ciphertext too short for ChaCha20-Poly1305"
            ));
        }
        let cipher = ChaCha20Poly1305::new((&self.key).into());
        let nonce = Nonce::from_slice(&ciphertext[..12]);
        let plaintext = cipher
            .decrypt(nonce, &ciphertext[12..])
            .map_err(|_| anyhow::anyhow!("Decryption failed — wrong key or corrupted data"))?;
        String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("UTF-8 decode failed: {}", e))
    }

    pub fn encrypt_hex(&self, plaintext: &str) -> Result<String> {
        let bytes = self.encrypt(plaintext)?;
        Ok(hex::encode(bytes))
    }

    pub fn decrypt_hex(&self, hex_str: &str) -> Result<String> {
        if hex_str.is_empty() {
            return Ok(String::new());
        }
        let bytes = hex::decode(hex_str)?;
        self.decrypt(&bytes)
    }
}
