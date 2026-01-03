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
    _tray_icon: TrayIcon,
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
            _tray_icon: tray_icon,
            menu_screenshot_id: screenshot_id,
            menu_gif_id: gif_id,
            menu_settings_id: settings_id,
            menu_exit_id: exit_id,
            is_recording: false,
        })
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
