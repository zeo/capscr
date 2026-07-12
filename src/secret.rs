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
        let s = open()?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut attrs: HashMap<&str, &str> = HashMap::new();
        attrs.insert("application", "capscr");
        attrs.insert("capscr-id", &id);
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
                &(props, secret, true),
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
        let item = unlocked
            .first()
            .or_else(|| locked.first())
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
        let blob = encrypt(plain).expect("encrypt");
        assert_ne!(blob, plain, "blob must not equal plaintext");
        let back = decrypt(&blob).expect("decrypt");
        assert_eq!(back, plain);
    }
}
