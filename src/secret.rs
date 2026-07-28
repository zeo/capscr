// secret-at-rest for capscr's config.toml.
//
// windows: Win32 DPAPI (CryptProtectData / CryptUnprotectData) — values are
// scoped to the current user account, so copying config.toml to another
// machine or user makes the blob unrecoverable.
//
// linux: the freedesktop Secret Service (gnome-keyring / kwallet). the value
// itself lives in the login keyring; config.toml only carries an opaque
// `keyring:<id>` reference
//
// other targets keep the plain hex fallback for tests.

use anyhow::{anyhow, Result};

/// encrypt `plaintext` and return a blob safe to drop into config.toml.
pub fn encrypt(plaintext: &str) -> Result<String> {
    #[cfg(windows)]
    {
        encrypt_win(plaintext)
    }
    #[cfg(target_os = "linux")]
    {
        secret_service::store(plaintext)
            .map_err(|e| anyhow!("system keyring unavailable; credential was not saved: {e:#}"))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Ok(hex::encode(plaintext.as_bytes()))
    }
}

#[derive(Default)]
pub struct SecretTransaction {
    #[cfg(target_os = "linux")]
    staged: Vec<String>,
    #[cfg(target_os = "linux")]
    previous: Vec<String>,
    #[cfg(target_os = "linux")]
    committed: bool,
}

impl SecretTransaction {
    pub(crate) fn retire(&mut self, existing: &str) {
        #[cfg(target_os = "linux")]
        if existing.starts_with("keyring:")
            && !self.previous.iter().any(|reference| reference == existing)
        {
            self.previous.push(existing.to_string());
        }
        #[cfg(not(target_os = "linux"))]
        let _ = existing;
    }

    pub fn replace(&mut self, plaintext: &str, existing: &str) -> Result<String> {
        let reference = encrypt(plaintext)?;
        #[cfg(target_os = "linux")]
        {
            self.staged.push(reference.clone());
            self.retire(existing);
        }
        #[cfg(not(target_os = "linux"))]
        let _ = existing;
        Ok(reference)
    }

    pub fn commit(self) {
        #[cfg(target_os = "linux")]
        {
            let mut transaction = self;
            transaction.committed = true;
            transaction.staged.clear();
            if let Err(error) = secret_service::delete_references(&transaction.previous) {
                tracing::warn!("couldn't remove replaced keyring credentials: {error:#}");
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = self;
    }
}

impl Drop for SecretTransaction {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        if !self.committed && !self.staged.is_empty() {
            if let Err(error) = secret_service::delete_references(&self.staged) {
                tracing::warn!("couldn't roll back staged keyring credentials: {error:#}");
            }
        }
    }
}

/// decrypt a blob previously produced by `encrypt`.
pub fn decrypt(blob: &str) -> Result<String> {
    #[cfg(windows)]
    {
        decrypt_win(blob)
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "linux")]
        if let Some(id) = blob.strip_prefix("keyring:") {
            return secret_service::retrieve(id);
        }
        let bytes = hex::decode(blob).map_err(|e| anyhow!("bad hex: {e}"))?;
        String::from_utf8(bytes).map_err(|e| anyhow!("bad utf-8: {e}"))
    }
}

// minimal client for org.freedesktop.secrets over the session bus. a plain
// (unencrypted) transport session is used — the session bus is local
// kernel-enforced IPC, and the keyring daemon encrypts at rest either way.
#[cfg(target_os = "linux")]
mod secret_service {
    use anyhow::{anyhow, Result};
    use std::collections::HashMap;
    use zbus::blocking::Connection;
    use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

    const BUS: &str = "org.freedesktop.secrets";
    const ROOT: &str = "/org/freedesktop/secrets";

    struct Session {
        conn: Connection,
        session: OwnedObjectPath,
        collection: OwnedObjectPath,
    }

    fn open() -> Result<Session> {
        let conn = Connection::session()?;
        let (_, session): (OwnedValue, OwnedObjectPath) = conn
            .call_method(
                Some(BUS),
                ROOT,
                Some("org.freedesktop.Secret.Service"),
                "OpenSession",
                &("plain", Value::from("")),
            )?
            .body()
            .deserialize()?;
        let mut collection: OwnedObjectPath = conn
            .call_method(
                Some(BUS),
                ROOT,
                Some("org.freedesktop.Secret.Service"),
                "ReadAlias",
                &("default",),
            )?
            .body()
            .deserialize()?;
        if collection.as_str() == "/" {
            // fresh keyrings have no default collection; create one. a
            // prompt requirement (password-protected daemon) means we can't
            // proceed non-interactively
            let mut props: HashMap<&str, Value> = HashMap::new();
            props.insert(
                "org.freedesktop.Secret.Collection.Label",
                Value::from("Default keyring"),
            );
            let (created, prompt): (OwnedObjectPath, OwnedObjectPath) = conn
                .call_method(
                    Some(BUS),
                    ROOT,
                    Some("org.freedesktop.Secret.Service"),
                    "CreateCollection",
                    &(props, "default"),
                )?
                .body()
                .deserialize()?;
            if created.as_str() != "/" {
                collection = created;
            } else if prompt.as_str() != "/" {
                // a password-less daemon completes the prompt without UI;
                // trigger it and poll for the alias to materialize
                let _ = conn.call_method(
                    Some(BUS),
                    prompt.as_str(),
                    Some("org.freedesktop.Secret.Prompt"),
                    "Prompt",
                    &("",),
                );
                for _ in 0..20 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    let aliased: OwnedObjectPath = conn
                        .call_method(
                            Some(BUS),
                            ROOT,
                            Some("org.freedesktop.Secret.Service"),
                            "ReadAlias",
                            &("default",),
                        )?
                        .body()
                        .deserialize()?;
                    if aliased.as_str() != "/" {
                        collection = aliased;
                        break;
                    }
                }
                if collection.as_str() == "/" {
                    return Err(anyhow!(
                        "creating a default keyring collection needs an interactive prompt"
                    ));
                }
            } else {
                return Err(anyhow!("keyring refused to create a default collection"));
            }
        }
        // unlock is a no-op when the login keyring is already open; a prompt
        // requirement is treated as unavailable rather than blocking capture
        let (unlocked, prompt): (Vec<OwnedObjectPath>, OwnedObjectPath) = conn
            .call_method(
                Some(BUS),
                ROOT,
                Some("org.freedesktop.Secret.Service"),
                "Unlock",
                &(vec![ObjectPath::from(&collection)],),
            )?
            .body()
            .deserialize()?;
        if unlocked.is_empty() && prompt.as_str() != "/" {
            return Err(anyhow!("keyring is locked and needs an interactive prompt"));
        }
        Ok(Session {
            conn,
            session,
            collection,
        })
    }

    pub fn store(plaintext: &str) -> Result<String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        store_with_id(plaintext, &id)
    }

    fn store_with_id(plaintext: &str, id: &str) -> Result<String> {
        let s = open()?;
        let mut attrs: HashMap<&str, &str> = HashMap::new();
        attrs.insert("application", "capscr");
        attrs.insert("capscr-id", id);
        let mut props: HashMap<&str, Value> = HashMap::new();
        props.insert(
            "org.freedesktop.Secret.Item.Label",
            Value::from("capscr upload credential"),
        );
        props.insert("org.freedesktop.Secret.Item.Attributes", Value::from(attrs));
        let secret = (
            ObjectPath::from(&s.session),
            Vec::<u8>::new(),
            plaintext.as_bytes().to_vec(),
            "text/plain; charset=utf8",
        );
        let (item, _prompt): (OwnedObjectPath, OwnedObjectPath) = s
            .conn
            .call_method(
                Some(BUS),
                s.collection.as_str(),
                Some("org.freedesktop.Secret.Collection"),
                "CreateItem",
                &(props, secret, false),
            )?
            .body()
            .deserialize()?;
        if item.as_str() == "/" {
            return Err(anyhow!("keyring did not store the item"));
        }
        Ok(format!("keyring:{id}"))
    }

    pub fn retrieve(id: &str) -> Result<String> {
        let s = open()?;
        let item = find_items(&s, id)?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("secret {id} not found in keyring"))?;
        let (_session, _params, value, _content_type): (OwnedObjectPath, Vec<u8>, Vec<u8>, String) =
            s.conn
                .call_method(
                    Some(BUS),
                    item.as_str(),
                    Some("org.freedesktop.Secret.Item"),
                    "GetSecret",
                    &(ObjectPath::from(&s.session),),
                )?
                .body()
                .deserialize()?;
        String::from_utf8(value).map_err(|e| anyhow!("bad utf-8 from keyring: {e}"))
    }

    pub fn delete_references(references: &[String]) -> Result<()> {
        if references.is_empty() {
            return Ok(());
        }
        let s = open()?;
        let mut first_error = None;
        for reference in references {
            let Some(id) = reference.strip_prefix("keyring:") else {
                continue;
            };
            match delete_items(&s, id) {
                Ok(()) => {}
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn find_items(s: &Session, id: &str) -> Result<Vec<OwnedObjectPath>> {
        let mut attrs: HashMap<&str, &str> = HashMap::new();
        attrs.insert("application", "capscr");
        attrs.insert("capscr-id", id);
        let (unlocked, locked): (Vec<OwnedObjectPath>, Vec<OwnedObjectPath>) = s
            .conn
            .call_method(
                Some(BUS),
                ROOT,
                Some("org.freedesktop.Secret.Service"),
                "SearchItems",
                &(attrs,),
            )?
            .body()
            .deserialize()?;
        Ok(unlocked.into_iter().chain(locked).collect())
    }

    fn delete_items(s: &Session, id: &str) -> Result<()> {
        for item in find_items(s, id)? {
            let prompt: OwnedObjectPath = s
                .conn
                .call_method(
                    Some(BUS),
                    item.as_str(),
                    Some("org.freedesktop.Secret.Item"),
                    "Delete",
                    &(),
                )?
                .body()
                .deserialize()?;
            if prompt.as_str() != "/" {
                return Err(anyhow!(
                    "deleting keyring credential {id} needs an interactive prompt"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
fn encrypt_win(plaintext: &str) -> Result<String> {
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Foundation::HLOCAL;
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};

    let mut input = plaintext.as_bytes().to_vec();
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB::default();
    let entropy = b"capscr/config/v1".to_vec();
    let mut entropy_mut = entropy.clone();
    let entropy_blob = CRYPT_INTEGER_BLOB {
        cbData: entropy_mut.len() as u32,
        pbData: entropy_mut.as_mut_ptr(),
    };
    unsafe {
        CryptProtectData(
            &in_blob,
            None,
            Some(&entropy_blob),
            None,
            None,
            0,
            &mut out_blob,
        )
        .map_err(|e| anyhow!("CryptProtectData: {e}"))?;
    }
    let slice =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(HLOCAL(out_blob.pbData as *mut _));
    }
    Ok(hex::encode(slice))
}

#[cfg(windows)]
fn decrypt_win(blob: &str) -> Result<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::LocalFree;
    use windows::Win32::Foundation::HLOCAL;
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

    let mut bytes = hex::decode(blob).map_err(|e| anyhow!("bad hex: {e}"))?;
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_mut_ptr(),
    };
    let mut entropy = b"capscr/config/v1".to_vec();
    let entropy_blob = CRYPT_INTEGER_BLOB {
        cbData: entropy.len() as u32,
        pbData: entropy.as_mut_ptr(),
    };
    let mut out_blob = CRYPT_INTEGER_BLOB::default();
    let mut desc = PWSTR::null();
    unsafe {
        CryptUnprotectData(
            &in_blob,
            Some(&mut desc),
            Some(&entropy_blob),
            None,
            None,
            0,
            &mut out_blob,
        )
        .map_err(|e| anyhow!("CryptUnprotectData: {e}"))?;
    }
    let plaintext_bytes =
        unsafe { std::slice::from_raw_parts(out_blob.pbData, out_blob.cbData as usize).to_vec() };
    unsafe {
        let _ = LocalFree(HLOCAL(out_blob.pbData as *mut _));
        if !desc.is_null() {
            let _ = LocalFree(HLOCAL(desc.0 as *mut _));
        }
    }
    String::from_utf8(plaintext_bytes).map_err(|e| anyhow!("bad utf-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let plain = "hunter2 — with spaces and unicode ✓";
        let blob = match encrypt(plain) {
            Ok(blob) => blob,
            #[cfg(target_os = "linux")]
            Err(error)
                if error
                    .to_string()
                    .contains("org.freedesktop.DBus.Error.ServiceUnknown") =>
            {
                return
            }
            Err(error) => panic!("encrypt: {error:#}"),
        };
        assert_ne!(blob, plain, "blob must not equal plaintext");
        let back = decrypt(&blob).expect("decrypt");
        assert_eq!(back, plain);
    }

    #[test]
    #[cfg(not(target_os = "linux"))]
    fn replacement_roundtrip() {
        let old_blob = encrypt("old secret").expect("encrypt old secret");
        let mut transaction = SecretTransaction::default();
        let new_blob = transaction
            .replace("new secret", &old_blob)
            .expect("replace secret");
        transaction.commit();
        assert_eq!(
            decrypt(&new_blob).expect("decrypt replacement"),
            "new secret"
        );
    }
}
