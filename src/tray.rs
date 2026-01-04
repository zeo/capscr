use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, MenuId, PredefinedMenuItem},
    TrayIcon, TrayIconBuilder,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Screenshot,
    RecordGif,
    Settings,
    Exit,
}

pub struct TrayManager {
    tray_icon: Option<TrayIcon>,
    icon_data: Vec<u8>,
    menu_screenshot_id: MenuId,
    menu_gif_id: MenuId,
    menu_settings_id: MenuId,
    menu_exit_id: MenuId,
    is_recording: bool,
}

impl TrayManager {
    pub fn new(icon_data: &[u8]) -> anyhow::Result<Self> {
        let icon = Self::load_icon(icon_data)?;

        let menu = Menu::new();

        let screenshot_item = MenuItem::new("Screenshot (Ctrl+Shift+S)", true, None);
        let gif_item = MenuItem::new("Record GIF (Ctrl+Shift+G)", true, None);
        let separator = PredefinedMenuItem::separator();
        let settings_item = MenuItem::new("Settings", true, None);
        let exit_item = MenuItem::new("Exit", true, None);

        let screenshot_id = screenshot_item.id().clone();
        let gif_id = gif_item.id().clone();
        let settings_id = settings_item.id().clone();
        let exit_id = exit_item.id().clone();

        menu.append(&screenshot_item)?;
        menu.append(&gif_item)?;
        menu.append(&separator)?;
        menu.append(&settings_item)?;
        menu.append(&exit_item)?;

        let tray_icon = TrayIconBuilder::new()
            .with_tooltip("capscr")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .build()?;

        Ok(Self {
            tray_icon: Some(tray_icon),
            icon_data: icon_data.to_vec(),
            menu_screenshot_id: screenshot_id,
            menu_gif_id: gif_id,
            menu_settings_id: settings_id,
            menu_exit_id: exit_id,
            is_recording: false,
        })
    }

    pub fn try_recreate(&mut self) -> bool {
        if self.tray_icon.is_some() {
            return true;
        }

        match Self::create_tray_icon(&self.icon_data) {
            Ok((tray, screenshot_id, gif_id, settings_id, exit_id)) => {
                self.tray_icon = Some(tray);
                self.menu_screenshot_id = screenshot_id;
                self.menu_gif_id = gif_id;
                self.menu_settings_id = settings_id;
                self.menu_exit_id = exit_id;
                tracing::info!("Tray icon recreated successfully");
                true
            }
            Err(e) => {
                tracing::warn!("Failed to recreate tray icon: {}", e);
                false
            }
        }
    }

    fn create_tray_icon(icon_data: &[u8]) -> anyhow::Result<(TrayIcon, MenuId, MenuId, MenuId, MenuId)> {
        let icon = Self::load_icon(icon_data)?;

        let menu = Menu::new();

        let screenshot_item = MenuItem::new("Screenshot (Ctrl+Shift+S)", true, None);
        let gif_item = MenuItem::new("Record GIF (Ctrl+Shift+G)", true, None);
        let separator = PredefinedMenuItem::separator();
        let settings_item = MenuItem::new("Settings", true, None);
        let exit_item = MenuItem::new("Exit", true, None);

        let screenshot_id = screenshot_item.id().clone();
        let gif_id = gif_item.id().clone();
        let settings_id = settings_item.id().clone();
        let exit_id = exit_item.id().clone();

        menu.append(&screenshot_item)?;
        menu.append(&gif_item)?;
        menu.append(&separator)?;
        menu.append(&settings_item)?;
        menu.append(&exit_item)?;

        let tray_icon = TrayIconBuilder::new()
            .with_tooltip("capscr")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .build()?;

        Ok((tray_icon, screenshot_id, gif_id, settings_id, exit_id))
    }

    pub fn mark_for_recreation(&mut self) {
        self.tray_icon = None;
    }

    pub fn is_valid(&self) -> bool {
        self.tray_icon.is_some()
    }

    fn load_icon(data: &[u8]) -> anyhow::Result<tray_icon::Icon> {
        let img = image::load_from_memory(data)?;
        let rgba = img.to_rgba8();
        let (width, height) = (rgba.width(), rgba.height());
        let icon = tray_icon::Icon::from_rgba(rgba.into_raw(), width, height)?;
        Ok(icon)
    }

    pub fn poll(&self) -> Option<TrayAction> {
        if let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = event.id();
            if *id == self.menu_screenshot_id {
                return Some(TrayAction::Screenshot);
            } else if *id == self.menu_gif_id {
                return Some(TrayAction::RecordGif);
            } else if *id == self.menu_settings_id {
                return Some(TrayAction::Settings);
            } else if *id == self.menu_exit_id {
                return Some(TrayAction::Exit);
            }
        }
        None
    }

    pub fn set_recording(&mut self, recording: bool) {
        self.is_recording = recording;
    }

    pub fn is_recording(&self) -> bool {
        self.is_recording
    }
}
