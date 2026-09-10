use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use super::auth::Tokens;

const MAX_STORE_BYTES: u64 = 32 * 1024 * 1024;
const AAD: &[u8] = b"api-manager/codex/credentials/v1";

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Account {
    pub id: String,
    pub email: Option<String>,
    pub label: String,
    pub plan_type: Option<String>,
    pub enabled: bool,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub cooldown_until: Option<i64>,
    #[serde(default)]
    pub cooldown_reason: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Record {
    pub account: Account,
    pub tokens: Tokens,
    #[serde(default)]
    pub verified: bool,
    // In-flight observation CAS only; no request survives a manager restart.
    #[serde(skip)]
    pub quota_version: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(super) struct Accounts {
    pub accounts: Vec<Record>,
    pub active_account_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    nonce: String,
    ciphertext: String,
}

pub(super) struct Vault {
    dir: PathBuf,
    cipher: Aes256Gcm,
}

fn private_open(path: &Path, create: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    if create {
        options.write(true).create_new(true);
    } else {
        options.read(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn private_read(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let file = private_open(path, false)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::other("credential file exceeds size limit"));
    }
    Ok(bytes)
}

impl Vault {
    pub fn open(data_dir: PathBuf) -> Result<(Self, Accounts), String> {
        let dir = data_dir.join("codex");
        if let Ok(metadata) = fs::symlink_metadata(&dir) {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err("Codex credential directory must not be a symlink".into());
            }
        }
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&dir)
            .map_err(|_| "Cannot create private Codex credential directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
                .map_err(|_| "Cannot secure Codex credential directory")?;
        }
        let key_path = dir.join("key");
        let credentials = dir.join("accounts.enc.json");
        let key = match private_read(&key_path, 32) {
            Ok(key) => key,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if credentials.exists() {
                    return Err("Codex encryption key is missing; restore it with the encrypted account file".into());
                }
                let mut key = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut key);
                // Publish only a fully written key. hard_link is create-if-absent, so competing
                // initializers cannot replace the key used by another process.
                let temporary = dir.join(format!(".key-{}.tmp", uuid::Uuid::new_v4()));
                let result = (|| -> std::io::Result<()> {
                    let mut file = private_open(&temporary, true)?;
                    file.write_all(&key)?;
                    file.sync_all()?;
                    fs::hard_link(&temporary, &key_path)
                })();
                let _ = fs::remove_file(&temporary);
                match result {
                    Ok(()) => {
                        File::open(&dir)
                            .and_then(|file| file.sync_all())
                            .map_err(|_| "Cannot sync Codex credential directory")?;
                        key.to_vec()
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        private_read(&key_path, 32)
                            .map_err(|_| "Cannot read Codex encryption key")?
                    }
                    Err(_) => return Err("Cannot create private Codex encryption key".into()),
                }
            }
            Err(_) => return Err("Cannot read Codex encryption key".into()),
        };
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| "Invalid Codex encryption key")?;
        let vault = Self { dir, cipher };
        let accounts = match private_read(&credentials, MAX_STORE_BYTES) {
            Ok(bytes) => {
                let envelope: Envelope = serde_json::from_slice(&bytes)
                    .map_err(|_| "Invalid Codex credential envelope")?;
                if envelope.version != 1 {
                    return Err("Unsupported Codex credential envelope version".into());
                }
                let nonce = STANDARD
                    .decode(envelope.nonce)
                    .map_err(|_| "Invalid Codex credential nonce")?;
                if nonce.len() != 12 {
                    return Err("Invalid Codex credential nonce".into());
                }
                let ciphertext = STANDARD
                    .decode(envelope.ciphertext)
                    .map_err(|_| "Invalid Codex ciphertext")?;
                let plaintext = vault.cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &ciphertext, aad: AAD })
                    .map_err(|_| "Cannot decrypt Codex credentials; restore the matching key and account file")?;
                serde_json::from_slice(&plaintext)
                    .map_err(|_| "Invalid decrypted Codex account data")?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Accounts::default(),
            Err(_) => return Err("Cannot read Codex credentials".into()),
        };
        Ok((vault, accounts))
    }

    pub fn save(&self, accounts: &Accounts) -> Result<(), String> {
        let plaintext =
            serde_json::to_vec(accounts).map_err(|_| "Cannot encode Codex credentials")?;
        let mut nonce = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &plaintext,
                    aad: AAD,
                },
            )
            .map_err(|_| "Cannot encrypt Codex credentials")?;
        let bytes = serde_json::to_vec(&Envelope {
            version: 1,
            nonce: STANDARD.encode(nonce),
            ciphertext: STANDARD.encode(ciphertext),
        })
        .map_err(|_| "Cannot encode Codex credential envelope")?;
        if bytes.len() as u64 > MAX_STORE_BYTES {
            return Err("Codex credential store is full".into());
        }
        let temporary = self
            .dir
            .join(format!(".accounts-{}.tmp", uuid::Uuid::new_v4()));
        let result = (|| -> std::io::Result<()> {
            let mut file = private_open(&temporary, true)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temporary, self.dir.join("accounts.enc.json"))?;
            File::open(&self.dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result.map_err(|_| "Cannot atomically persist Codex credentials".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn account_dto_never_serializes_credentials_and_vault_detects_tampering() {
        let temp = tempfile::tempdir().unwrap();
        let (vault, mut accounts) = Vault::open(temp.path().to_path_buf()).unwrap();
        let tokens = Tokens::from_auth_json(&json!({"tokens": {
            "access_token": "secret-access", "refresh_token": "secret-refresh", "id_token": "secret-id", "account_id": "workspace"
        }})).unwrap();
        let record = Record {
            account: Account {
                id: "local-id".into(),
                email: None,
                label: "Account".into(),
                plan_type: None,
                enabled: true,
                expires_at: None,
                last_used_at: None,
                last_error: None,
                cooldown_until: None,
                cooldown_reason: None,
            },
            tokens,
            verified: true,
            quota_version: 0,
        };
        let dto = serde_json::to_string(&record.account).unwrap();
        assert!(!dto.contains("secret-"));
        assert!(!dto.contains("token"));
        accounts.accounts.push(record);
        vault.save(&accounts).unwrap();
        let file = temp.path().join("codex/accounts.enc.json");
        let ciphertext = fs::read_to_string(&file).unwrap();
        assert!(!ciphertext.contains("secret-"));
        let (_, restored) = Vault::open(temp.path().to_path_buf()).unwrap();
        assert_eq!(restored.accounts[0].tokens.refresh_token, "secret-refresh");
        let mut envelope: Envelope = serde_json::from_str(&ciphertext).unwrap();
        let mut bytes = STANDARD.decode(&envelope.ciphertext).unwrap();
        bytes[0] ^= 1;
        envelope.ciphertext = STANDARD.encode(bytes);
        fs::write(file, serde_json::to_vec(&envelope).unwrap()).unwrap();
        assert!(Vault::open(temp.path().to_path_buf()).is_err());
    }
}
