use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct InstalledPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub enabled: bool,
    pub has_settings: bool,
    // human-readable "area:grant" strings (e.g. "image:read", "fetch:api.x.com")
    // so the plugins tab can show what a plugin was granted at install time
    pub capabilities: Vec<String>,
}

// legacy flat plugin.toml (the metadata-only era: top-level name/version/...).
// modern plugins use the sectioned schema parsed by crate::plugin::PluginManifest.
#[derive(Debug, Deserialize)]
struct LegacyFlatManifest {
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_true")]
    enabled: bool,
}

fn default_true() -> bool {
    true
}

struct PluginListing {
    name: String,
    version: String,
    description: String,
    enabled: bool,
    has_settings: bool,
    capabilities: Vec<String>,
}

/// flatten a plugin's `[capabilities]` table into sorted "area:grant" labels
/// (e.g. "clipboard:write", "fetch:https://api.x.com/*") for display
fn flatten_capabilities(caps: &std::collections::HashMap<String, Vec<String>>) -> Vec<String> {
    let mut out: Vec<String> = caps
        .iter()
        .flat_map(|(k, vs)| vs.iter().map(move |v| format!("{k}:{v}")))
        .collect();
    out.sort();
    out
}

/// read the plugins-list fields from a plugin.toml body. tries the sectioned
/// runtime schema first — what real WASM plugins use — then falls back to the
/// legacy flat schema. None if neither parses. without the sectioned attempt,
/// installed WASM plugins (whose name lives under `[plugin]`) silently fail to
/// parse and never appear in the list.
fn read_plugin_listing(body: &str) -> Option<PluginListing> {
    if let Ok(m) = toml::from_str::<crate::plugin::PluginManifest>(body) {
        return Some(PluginListing {
            name: m.plugin.name,
            version: m.plugin.version,
            description: m.plugin.description.unwrap_or_default(),
            enabled: m.enabled,
            has_settings: !m.settings.is_empty(),
            capabilities: flatten_capabilities(&m.capabilities),
        });
    }
    if let Ok(m) = toml::from_str::<LegacyFlatManifest>(body) {
        return Some(PluginListing {
            name: m.name,
            version: m.version,
            description: m.description,
            enabled: m.enabled,
            has_settings: false,
            capabilities: Vec::new(),
        });
    }
    None
}

#[cfg(test)]
mod plugin_listing_tests {
    use super::read_plugin_listing;

    #[test]
    fn reads_sectioned_wasm_manifest() {
        let body = "enabled = false\n\
            [plugin]\nid = \"grayscale\"\nname = \"Grayscale\"\nversion = \"0.1.0\"\ndescription = \"gray\"\n\
            [runtime]\ntype = \"wasm\"\nfile = \"plugin.wasm\"\n\
            [hooks]\non_capture = \"capscr_on_capture\"\n\
            [capabilities]\nimage = [\"read\"]\nfetch = [\"https://api.example.com/*\"]\n";
        let listing = read_plugin_listing(body).expect("sectioned parses");
        assert_eq!(listing.name, "Grayscale");
        assert_eq!(listing.version, "0.1.0");
        assert_eq!(listing.description, "gray");
        assert!(!listing.enabled);
        assert_eq!(
            listing.capabilities,
            vec!["fetch:https://api.example.com/*", "image:read"]
        );
    }

    #[test]
    fn reads_legacy_flat_manifest() {
        let body =
            "name = \"Sounds\"\nversion = \"0.1.0\"\ndescription = \"sfx\"\nenabled = true\n";
        let listing = read_plugin_listing(body).expect("flat parses");
        assert_eq!(listing.name, "Sounds");
        assert_eq!(listing.version, "0.1.0");
        assert!(listing.enabled);
        assert!(listing.capabilities.is_empty());
    }

    #[test]
    fn rejects_garbage() {
        assert!(read_plugin_listing("not [ valid toml").is_none());
    }
}

fn plugins_dir() -> Result<PathBuf, String> {
    let project = crate::config::project_dirs()
        .ok_or_else(|| "cannot resolve plugins directory".to_string())?;
    Ok(project.data_dir().to_path_buf().join("plugins"))
}

#[tauri::command]
pub fn list_installed_plugins() -> Result<Vec<InstalledPlugin>, String> {
    let dir = plugins_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if !entry.file_type().is_ok_and(|file_type| file_type.is_dir()) {
            continue;
        }
        let path = entry.path();
        let manifest_path = path.join("plugin.toml");
        if !manifest_path.exists() {
            continue;
        }
        let body = match std::fs::read_to_string(&manifest_path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let listing = match read_plugin_listing(&body) {
            Some(t) => t,
            None => {
                tracing::warn!("plugin {:?}: unparseable manifest", path.file_name());
                continue;
            }
        };
        out.push(InstalledPlugin {
            id: entry.file_name().to_string_lossy().to_string(),
            name: listing.name,
            version: listing.version,
            description: listing.description,
            enabled: listing.enabled,
            has_settings: listing.has_settings,
            capabilities: listing.capabilities,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

// failures from the latest plugin load pass, surfaced to the plugins tab
#[tauri::command]
pub fn plugin_load_errors(state: State<AppState>) -> Vec<String> {
    state.plugin_load_errors.lock().unwrap().clone()
}

#[tauri::command]
pub fn list_upload_plugins(state: State<AppState>) -> Vec<crate::plugin::UploadPlugin> {
    state.plugin_manager.read().unwrap().upload_destinations()
}

#[tauri::command]
pub async fn reload_plugins(app: AppHandle) -> Result<Vec<String>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<AppState>();
        state.load_plugins();
        crate::rebuild_tray_menu(&app);
        let errors = state.plugin_load_errors.lock().unwrap().clone();
        errors
    })
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn plugin_settings(id: String) -> Result<crate::marketplace::PluginSettings, String> {
    let plugins = plugins_dir()?;
    tokio::task::spawn_blocking(move || crate::marketplace::plugin_settings(&plugins, &id))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn set_plugin_settings(
    id: String,
    values: serde_json::Map<String, serde_json::Value>,
    app: AppHandle,
) -> Result<(), String> {
    let plugins = plugins_dir()?;
    let pending_mutation = PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let _pending_mutation = pending_mutation;
        crate::marketplace::set_plugin_settings(&plugins, &id, &values)?;
        app.state::<AppState>().load_plugins();
        crate::rebuild_tray_menu(&app);
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn take_startup_notices(state: State<AppState>) -> Vec<String> {
    std::mem::take(&mut *state.startup_notices.lock().unwrap())
}

#[tauri::command]
pub fn open_plugins_folder(app: AppHandle) -> Result<(), String> {
    let dir = plugins_dir()?;
    let _pending_mutation =
        PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn marketplace_browse(
    state: State<'_, AppState>,
) -> Result<Vec<crate::marketplace::RegistryEntry>, String> {
    let url = state
        .config
        .lock()
        .unwrap()
        .marketplace
        .registry_url
        .clone();
    // reqwest::blocking inside an async command — push to a worker thread so
    // we don't park the tokio runtime.
    tokio::task::spawn_blocking(move || crate::marketplace::fetch_registry(&url))
        .await
        .map_err(|e| e.to_string())?
        .map(|reg| reg.plugins)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn marketplace_install(id: String, app: AppHandle) -> Result<bool, String> {
    let url = app
        .state::<AppState>()
        .config
        .lock()
        .unwrap()
        .marketplace
        .registry_url
        .clone();
    let plugins = plugins_dir()?;
    let pending_mutation = PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    tokio::task::spawn_blocking(move || -> anyhow::Result<bool> {
        let _pending_mutation = pending_mutation;
        let registry = crate::marketplace::fetch_registry(&url)?;
        let entry = registry
            .plugins
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| anyhow::anyhow!("plugin '{}' not in registry", id))?;
        let installed = crate::marketplace::install_plugin(&plugins, entry)?;
        app.state::<AppState>().load_plugins();
        crate::rebuild_tray_menu(&app);
        Ok(installed)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn marketplace_uninstall(id: String, app: AppHandle) -> Result<(), String> {
    let plugins = plugins_dir()?;
    let pending_mutation = PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let _pending_mutation = pending_mutation;
        crate::marketplace::uninstall_plugin(&plugins, &id)?;
        app.state::<AppState>().load_plugins();
        crate::rebuild_tray_menu(&app);
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn toggle_plugin_enabled(
    id: String,
    enabled: bool,
    app: AppHandle,
) -> Result<(), String> {
    let plugins = plugins_dir()?;
    let pending_mutation = PendingFileMutation::begin(&app).map_err(|error| error.to_string())?;
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let _pending_mutation = pending_mutation;
        crate::marketplace::set_plugin_enabled(&plugins, &id, enabled)?;
        app.state::<AppState>().load_plugins();
        crate::rebuild_tray_menu(&app);
        Ok(())
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}
