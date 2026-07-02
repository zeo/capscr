use std::io::Write as _;
use std::path::{Path, PathBuf};

// embed the signed capscr MSI, DEFLATE-compressed. CAPSCR_MSI_PATH names the
// file (CI: the tauri bundle output); unset = empty payload, ui/preview only.
// CAPSCR_APP_VERSION carries the display version (falls back to "dev")
fn main() {
    embed_manifest::embed_manifest(
        embed_manifest::new_manifest("capscr-setup")
            .dpi_awareness(embed_manifest::manifest::DpiAwareness::PerMonitorV2),
    )
    .expect("embed manifest");
    if Path::new("../icons/icon.ico").exists() {
        println!("cargo:rerun-if-changed=../icons/icon.ico");
        embed_resource::compile_for_everything("icon.rc", embed_resource::NONE);
    }

    println!("cargo:rerun-if-env-changed=CAPSCR_MSI_PATH");
    println!("cargo:rerun-if-env-changed=CAPSCR_APP_VERSION");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("msi.bin");
    let mut buf = Vec::new();
    if let Ok(p) = std::env::var("CAPSCR_MSI_PATH") {
        let raw = std::fs::read(&p).expect("read CAPSCR_MSI_PATH");
        let comp = miniz_oxide::deflate::compress_to_vec(&raw, 8);
        buf.extend_from_slice(&(raw.len() as u64).to_le_bytes());
        buf.extend_from_slice(&comp);
    } else {
        buf.extend_from_slice(&0u64.to_le_bytes());
    }
    let mut f = std::fs::File::create(&out).expect("create msi.bin");
    f.write_all(&buf).expect("write msi.bin");

    let ver = std::env::var("CAPSCR_APP_VERSION").unwrap_or_else(|_| "dev".into());
    println!("cargo:rustc-env=CAPSCR_APP_VERSION_UI={ver}");
}
