//! capscr-setup's engine. the real installer is the signed MSI built by tauri
//! — the same artifact the in-app updater consumes — so this wrapper only
//! extracts it and drives msiexec quietly. that keeps one canonical install
//! layout: what this installs, the updater can update

use std::path::PathBuf;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_READ, REG_SZ,
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

const MSI_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/msi.bin"));
pub const APP_VERSION: &str = env!("CAPSCR_APP_VERSION_UI");

pub fn has_payload() -> bool {
    MSI_BIN.len() > 8 && u64::from_le_bytes(MSI_BIN[..8].try_into().unwrap()) > 0
}

fn extract_msi() -> Result<PathBuf, String> {
    let raw_len = u64::from_le_bytes(MSI_BIN[..8].try_into().unwrap());
    if raw_len == 0 {
        return Err("this build carries no MSI (dev binary)".into());
    }
    let raw = miniz_oxide::inflate::decompress_to_vec(&MSI_BIN[8..])
        .map_err(|e| format!("decompress MSI: {e:?}"))?;
    let path = std::env::temp_dir().join(format!("capscr-{APP_VERSION}.msi"));
    std::fs::write(&path, &raw).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

/// run the embedded MSI quietly; msiexec raises its own UAC prompt. blocking —
/// call from a worker thread while the ui animates
pub fn install() -> Result<(), String> {
    let msi = extract_msi()?;
    let status = std::process::Command::new("msiexec")
        .arg("/i")
        .arg(&msi)
        .args(["/qn", "/norestart"])
        .status()
        .map_err(|e| format!("start msiexec: {e}"))?;
    let _ = std::fs::remove_file(&msi);
    match status.code() {
        // 0 = ok, 3010 = ok + reboot advised, 1602 = the user declined UAC
        Some(0) | Some(3010) => Ok(()),
        Some(1602) => Err("installation was cancelled".into()),
        Some(c) => Err(format!("msiexec exited with code {c}")),
        None => Err("msiexec was terminated".into()),
    }
}

/// uninstall through the same product msiexec knows
pub fn uninstall() -> Result<(), String> {
    let Some(product) = find_product() else {
        return Err("capscr is not installed".into());
    };
    let status = std::process::Command::new("msiexec")
        .args(["/x", &product, "/qn", "/norestart"])
        .status()
        .map_err(|e| format!("start msiexec: {e}"))?;
    match status.code() {
        Some(0) | Some(3010) => Ok(()),
        Some(1602) => Err("removal was cancelled".into()),
        Some(c) => Err(format!("msiexec exited with code {c}")),
        None => Err("msiexec was terminated".into()),
    }
}

pub fn launch_app() {
    if let Some(exe) = installed_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
}

fn installed_exe() -> Option<PathBuf> {
    let loc = arp_value("InstallLocation")?;
    let exe = PathBuf::from(loc.trim_end_matches('\\')).join("capscr.exe");
    exe.exists().then_some(exe)
}

/// the {ProductCode} of the installed capscr MSI, from either registry view
fn find_product() -> Option<String> {
    scan_arp(|sub, disp| (disp == "capscr" && sub.starts_with('{')).then(|| sub.to_string()))
}

fn arp_value(value: &str) -> Option<String> {
    scan_arp(|sub, disp| {
        if disp != "capscr" || !sub.starts_with('{') {
            return None;
        }
        read_sub_value(sub, value)
    })
}

fn read_sub_value(sub: &str, value: &str) -> Option<String> {
    unsafe {
        for (hive, root) in ROOTS {
            let mut key = HKEY::default();
            let p = wide(&format!("{root}\\{sub}"));
            if RegOpenKeyExW(*hive, PCWSTR(p.as_ptr()), Some(0), KEY_READ, &mut key)
                != ERROR_SUCCESS
            {
                continue;
            }
            let name = wide(value);
            let mut ty = REG_SZ;
            let mut buf = [0u16; 512];
            let mut bytes = (buf.len() * 2) as u32;
            let q = RegQueryValueExW(
                key,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut ty),
                Some(buf.as_mut_ptr() as *mut u8),
                Some(&mut bytes),
            );
            let _ = RegCloseKey(key);
            if q == ERROR_SUCCESS && ty == REG_SZ {
                let chars = (bytes as usize / 2).min(buf.len());
                let s = String::from_utf16_lossy(&buf[..chars]);
                let s = s.trim_end_matches('\0').to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
        }
    }
    None
}

const ROOTS: &[(HKEY, &str)] = &[
    (HKEY_LOCAL_MACHINE, "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall"),
    (HKEY_LOCAL_MACHINE, "SOFTWARE\\WOW6432Node\\Microsoft\\Windows\\CurrentVersion\\Uninstall"),
    (HKEY_CURRENT_USER, "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall"),
];

fn scan_arp<T>(mut pick: impl FnMut(&str, &str) -> Option<T>) -> Option<T> {
    unsafe {
        for (hive, root) in ROOTS {
            let mut key = HKEY::default();
            let p = wide(root);
            if RegOpenKeyExW(*hive, PCWSTR(p.as_ptr()), Some(0), KEY_READ, &mut key)
                != ERROR_SUCCESS
            {
                continue;
            }
            let mut idx = 0u32;
            loop {
                let mut name = [0u16; 256];
                let mut name_len = name.len() as u32;
                if RegEnumKeyExW(
                    key,
                    idx,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut name_len,
                    None,
                    None,
                    None,
                    None,
                ) != ERROR_SUCCESS
                {
                    break;
                }
                idx += 1;
                let sub = String::from_utf16_lossy(&name[..name_len as usize]);
                let mut skey = HKEY::default();
                let sp = wide(&format!("{root}\\{sub}"));
                if RegOpenKeyExW(*hive, PCWSTR(sp.as_ptr()), Some(0), KEY_READ, &mut skey)
                    != ERROR_SUCCESS
                {
                    continue;
                }
                let dn = wide("DisplayName");
                let mut ty = REG_SZ;
                let mut buf = [0u16; 256];
                let mut bytes = (buf.len() * 2) as u32;
                let q = RegQueryValueExW(
                    skey,
                    PCWSTR(dn.as_ptr()),
                    None,
                    Some(&mut ty),
                    Some(buf.as_mut_ptr() as *mut u8),
                    Some(&mut bytes),
                );
                let _ = RegCloseKey(skey);
                if q == ERROR_SUCCESS && ty == REG_SZ {
                    let chars = (bytes as usize / 2).min(buf.len());
                    let disp = String::from_utf16_lossy(&buf[..chars]);
                    if let Some(t) = pick(&sub, disp.trim_end_matches('\0')) {
                        let _ = RegCloseKey(key);
                        return Some(t);
                    }
                }
            }
            let _ = RegCloseKey(key);
        }
    }
    None
}
