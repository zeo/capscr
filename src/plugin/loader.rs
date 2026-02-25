use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(feature = "wasm-plugins")]
use super::WasmPlugin;
use super::{LoadedPlugin, PluginManifest, PluginType};

pub struct PluginLoader {
    plugins_dir: PathBuf,
}

impl PluginLoader {
    pub fn new(plugins_dir: PathBuf) -> Self {
        Self { plugins_dir }
    }

    pub fn install_from_zip(&self, zip_path: &PathBuf) -> Result<PathBuf, String> {
        let file = std::fs::File::open(zip_path)
            .map_err(|e| format!("Failed to open zip file: {}", e))?;

        let mut archive = zip::ZipArchive::new(file)
            .map_err(|e| format!("Failed to read zip archive: {}", e))?;

        let manifest = self.read_manifest_from_zip(&mut archive)?;
        manifest.validate()?;
        manifest.is_compatible()?;

        let plugin_dir = self.plugins_dir.join(&manifest.plugin.id);

        if plugin_dir.exists() {
            std::fs::remove_dir_all(&plugin_dir)
                .map_err(|e| format!("Failed to remove existing plugin: {}", e))?;
        }

        std::fs::create_dir_all(&plugin_dir)
            .map_err(|e| format!("Failed to create plugin directory: {}", e))?;

        for i in 0..archive.len() {
            let mut file = archive.by_index(i)
                .map_err(|e| format!("Failed to read zip entry: {}", e))?;

            let outpath = match file.enclosed_name() {
                Some(path) => plugin_dir.join(path),
                None => continue,
            };

            if file.is_dir() {
                std::fs::create_dir_all(&outpath)
                    .map_err(|e| format!("Failed to create directory: {}", e))?;
            } else {
                if let Some(parent) = outpath.parent() {
                    if !parent.exists() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| format!("Failed to create parent directory: {}", e))?;
                    }
                }

                let mut outfile = std::fs::File::create(&outpath)
                    .map_err(|e| format!("Failed to create file: {}", e))?;

                std::io::copy(&mut file, &mut outfile)
                    .map_err(|e| format!("Failed to extract file: {}", e))?;
            }

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Some(mode) = file.unix_mode() {
                    std::fs::set_permissions(&outpath, std::fs::Permissions::from_mode(mode)).ok();
                }
            }
        }

        std::fs::remove_file(zip_path).ok();

        Ok(plugin_dir)
    }

    fn read_manifest_from_zip(&self, archive: &mut zip::ZipArchive<std::fs::File>) -> Result<PluginManifest, String> {
        let mut manifest_file = archive.by_name("manifest.toml")
            .map_err(|_| "No manifest.toml found in plugin package".to_string())?;

        let mut content = String::new();
        manifest_file.read_to_string(&mut content)
            .map_err(|e| format!("Failed to read manifest: {}", e))?;

        PluginManifest::parse(&content)
    }

    pub fn load_from_directory(&self, dir: &Path) -> Result<LoadedPlugin, String> {
        let manifest = PluginManifest::from_directory(dir)?;
        manifest.validate()?;
        manifest.is_compatible()?;

        let library_name = manifest.library_filename()
            .ok_or_else(|| "No library specified for current platform".to_string())?;

        let library_path = dir.join(library_name);

        if !library_path.exists() {
            return Err(format!("Library file not found: {}", library_path.display()));
        }

        let handle = match manifest.plugin_type() {
            PluginType::Wasm => {
                #[cfg(not(feature = "wasm-plugins"))]
                {
                    return Err("WASM plugins are disabled in this build".to_string());
                }
                #[cfg(feature = "wasm-plugins")]
                let plugin = WasmPlugin::load(
                    &library_path,
                    manifest.plugin.name.clone(),
                    manifest.plugin.version.clone(),
                    manifest.plugin.description.clone(),
                )?;
                #[cfg(feature = "wasm-plugins")]
                {
                    super::PluginHandle::Wasm { plugin }
                }
            }
            PluginType::Native => {
                let library = unsafe {
                    libloading::Library::new(&library_path)
                        .map_err(|e| format!("Failed to load library: {}", e))?
                };

                let create_fn: libloading::Symbol<super::CreatePluginFn> = unsafe {
                    library.get(b"create_plugin")
                        .map_err(|e| format!("Failed to find create_plugin function: {}", e))?
                };

                let plugin = create_fn();
                super::PluginHandle::Native { plugin, _library: library }
            }
        };

        Ok(LoadedPlugin { manifest, handle })
    }
}
