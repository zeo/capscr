use image::RgbaImage;
use std::path::Path;
use std::sync::Arc;
use wasmtime::*;

use super::{CaptureType, Plugin, PluginEvent, PluginResponse};

const MAX_WASM_MODULE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PLUGIN_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const WASM_EVENT_FUEL: u64 = 15_000_000;
const WASM_LIFECYCLE_FUEL: u64 = 2_000_000;
const WASM_MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
const WASM_MAX_TABLE_ELEMENTS: usize = 10_000;
const WASM_MAX_INSTANCES: usize = 1;
const WASM_MAX_TABLES: usize = 4;

pub struct WasmPlugin {
    name: String,
    version: String,
    description: String,
    _engine: Engine,
    _module: Module,
    store: Store<WasmState>,
    instance: Instance,
}

struct WasmState {
    image_data: Option<Vec<u8>>,
    image_width: u32,
    image_height: u32,
    modified: bool,
    limits: StoreLimits,
}

impl WasmPlugin {
    pub fn load(path: &Path, name: String, version: String, description: String) -> Result<Self, String> {
        let mut config = Config::new();
        config.consume_fuel(true);
        let engine = Engine::new(&config).map_err(|e| format!("Failed to init engine: {}", e))?;

        let wasm_bytes = std::fs::read(path)
            .map_err(|e| format!("Failed to read WASM file: {}", e))?;
        if wasm_bytes.len() > MAX_WASM_MODULE_BYTES {
            return Err(format!(
                "WASM module too large: {} bytes (max {})",
                wasm_bytes.len(),
                MAX_WASM_MODULE_BYTES
            ));
        }

        let module = Module::new(&engine, &wasm_bytes)
            .map_err(|e| format!("Failed to compile WASM module: {}", e))?;

        let mut store = Store::new(&engine, WasmState {
            image_data: None,
            image_width: 0,
            image_height: 0,
            modified: false,
            limits: StoreLimitsBuilder::new()
                .memory_size(WASM_MAX_MEMORY_BYTES)
                .table_elements(WASM_MAX_TABLE_ELEMENTS)
                .instances(WASM_MAX_INSTANCES)
                .tables(WASM_MAX_TABLES)
                .build(),
        });
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(WASM_EVENT_FUEL)
            .map_err(|e| format!("Failed to configure WASM fuel: {}", e))?;

        let mut linker = Linker::new(&engine);

        linker.func_wrap("env", "get_image_width", |caller: Caller<'_, WasmState>| -> u32 {
            caller.data().image_width
        }).map_err(|e| format!("Failed to link get_image_width: {}", e))?;

        linker.func_wrap("env", "get_image_height", |caller: Caller<'_, WasmState>| -> u32 {
            caller.data().image_height
        }).map_err(|e| format!("Failed to link get_image_height: {}", e))?;

        linker.func_wrap("env", "get_pixel", |caller: Caller<'_, WasmState>, x: u32, y: u32| -> u32 {
            let state = caller.data();
            if let Some(ref data) = state.image_data {
                let idx = ((y * state.image_width + x) * 4) as usize;
                if idx + 3 < data.len() {
                    let r = data[idx] as u32;
                    let g = data[idx + 1] as u32;
                    let b = data[idx + 2] as u32;
                    let a = data[idx + 3] as u32;
                    return (a << 24) | (r << 16) | (g << 8) | b;
                }
            }
            0
        }).map_err(|e| format!("Failed to link get_pixel: {}", e))?;

        linker.func_wrap("env", "set_pixel", |mut caller: Caller<'_, WasmState>, x: u32, y: u32, rgba: u32| {
            let state = caller.data_mut();
            if let Some(ref mut data) = state.image_data {
                let idx = ((y * state.image_width + x) * 4) as usize;
                if idx + 3 < data.len() {
                    data[idx] = ((rgba >> 16) & 0xFF) as u8;     // R
                    data[idx + 1] = ((rgba >> 8) & 0xFF) as u8;  // G
                    data[idx + 2] = (rgba & 0xFF) as u8;         // B
                    data[idx + 3] = ((rgba >> 24) & 0xFF) as u8; // A
                    state.modified = true;
                }
            }
        }).map_err(|e| format!("Failed to link set_pixel: {}", e))?;

        linker.func_wrap("env", "log_message", |_caller: Caller<'_, WasmState>, _ptr: u32, _len: u32| {
            // Logging from WASM - could implement string reading from memory
        }).map_err(|e| format!("Failed to link log_message: {}", e))?;

        let instance = linker.instantiate(&mut store, &module)
            .map_err(|e| format!("Failed to instantiate WASM module: {}", e))?;

        Ok(Self {
            name,
            version,
            description,
            _engine: engine,
            _module: module,
            store,
            instance,
        })
    }

    fn call_event(&mut self, event_type: i32) -> i32 {
        if self.store.set_fuel(WASM_EVENT_FUEL).is_err() {
            return 0;
        }
        if let Some(func) = self.instance.get_func(&mut self.store, "on_event") {
            if let Ok(typed) = func.typed::<i32, i32>(&self.store) {
                if let Ok(result) = typed.call(&mut self.store, event_type) {
                    return result;
                }
            }
        }
        0 // Continue
    }

    fn set_image(&mut self, image: &RgbaImage) {
        let raw = image.as_raw();
        let expected_len = (image.width() as usize)
            .checked_mul(image.height() as usize)
            .and_then(|pixels| pixels.checked_mul(4));

        if expected_len != Some(raw.len()) || raw.len() > MAX_PLUGIN_IMAGE_BYTES {
            let state = self.store.data_mut();
            state.image_data = None;
            state.image_width = 0;
            state.image_height = 0;
            state.modified = false;
            return;
        }

        let state = self.store.data_mut();
        state.image_data = Some(raw.clone());
        state.image_width = image.width();
        state.image_height = image.height();
        state.modified = false;
    }

    fn get_modified_image(&mut self) -> Option<RgbaImage> {
        let state = self.store.data_mut();
        if state.modified {
            if let Some(ref data) = state.image_data {
                return RgbaImage::from_raw(state.image_width, state.image_height, data.clone());
            }
        }
        None
    }
}

impl Plugin for WasmPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn version(&self) -> &str {
        &self.version
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn on_event(&mut self, event: &PluginEvent) -> PluginResponse {
        let event_type = match event {
            PluginEvent::PreCapture { .. } => 1,
            PluginEvent::PostCapture { image, .. } => {
                self.set_image(image);
                2
            }
            PluginEvent::PreSave { image, .. } => {
                self.set_image(image);
                3
            }
            PluginEvent::PostSave { .. } => 4,
            PluginEvent::PreUpload { image } => {
                self.set_image(image);
                5
            }
            PluginEvent::PostUpload { .. } => 6,
        };

        let result = self.call_event(event_type);

        match result {
            1 => PluginResponse::Cancel,
            2 => {
                if let Some(img) = self.get_modified_image() {
                    PluginResponse::ModifiedImage(Arc::new(img))
                } else {
                    PluginResponse::Continue
                }
            }
            _ => PluginResponse::Continue,
        }
    }

    fn on_load(&mut self) {
        if self.store.set_fuel(WASM_LIFECYCLE_FUEL).is_err() {
            return;
        }
        if let Some(func) = self.instance.get_func(&mut self.store, "on_load") {
            if let Ok(typed) = func.typed::<(), ()>(&self.store) {
                let _ = typed.call(&mut self.store, ());
            }
        }
    }

    fn on_unload(&mut self) {
        if self.store.set_fuel(WASM_LIFECYCLE_FUEL).is_err() {
            return;
        }
        if let Some(func) = self.instance.get_func(&mut self.store, "on_unload") {
            if let Ok(typed) = func.typed::<(), ()>(&self.store) {
                let _ = typed.call(&mut self.store, ());
            }
        }
    }
}

fn _capture_type_to_i32(ct: &CaptureType) -> i32 {
    match ct {
        CaptureType::FullScreen => 1,
        CaptureType::Window => 2,
        CaptureType::Region => 3,
        CaptureType::Gif => 4,
    }
}
