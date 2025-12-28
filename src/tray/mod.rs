use anyhow::Result;
use std::sync::mpsc::{channel, Receiver, Sender};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    Icon, TrayIcon, TrayIconBuilder,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Show,
    CaptureScreen,
    CaptureWindow,
    CaptureRegion,
    RecordGif,
    Settings,
    Quit,
}

pub struct SystemTray {
    _tray: TrayIcon,
    menu_receiver: Receiver<MenuEvent>,
    show_id: u32,
    screen_id: u32,
    window_id: u32,
    region_id: u32,
    gif_id: u32,
    settings_id: u32,
    quit_id: u32,
}

impl SystemTray {
    pub fn new() -> Result<Self> {
        let show_item = MenuItem::new("Show", true, None);
        let screen_item = MenuItem::new("Capture Screen", true, None);
        let window_item = MenuItem::new("Capture Window", true, None);
        let region_item = MenuItem::new("Capture Region", true, None);
        let gif_item = MenuItem::new("Record GIF", true, None);
        let settings_item = MenuItem::new("Settings", true, None);
        let quit_item = MenuItem::new("Quit", true, None);

        let show_id = show_item.id().0;
        let screen_id = screen_item.id().0;
        let window_id = window_item.id().0;
        let region_id = region_item.id().0;
        let gif_id = gif_item.id().0;
        let settings_id = settings_item.id().0;
        let quit_id = quit_item.id().0;

        let menu = Menu::new();
        menu.append(&show_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&screen_item)?;
        menu.append(&window_item)?;
        menu.append(&region_item)?;
        menu.append(&gif_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&settings_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&quit_item)?;

        let icon = create_default_icon()?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("capscr - Screen Capture")
            .with_icon(icon)
            .build()?;

        let menu_receiver = MenuEvent::receiver().clone();

        Ok(Self {
            _tray: tray,
            menu_receiver,
            show_id,
            screen_id,
            window_id,
            region_id,
            gif_id,
            settings_id,
            quit_id,
        })
    }

    pub fn poll(&self) -> Option<TrayAction> {
        if let Ok(event) = self.menu_receiver.try_recv() {
            let id = event.id.0;
            if id == self.show_id {
                return Some(TrayAction::Show);
            } else if id == self.screen_id {
                return Some(TrayAction::CaptureScreen);
            } else if id == self.window_id {
                return Some(TrayAction::CaptureWindow);
            } else if id == self.region_id {
                return Some(TrayAction::CaptureRegion);
            } else if id == self.gif_id {
                return Some(TrayAction::RecordGif);
            } else if id == self.settings_id {
                return Some(TrayAction::Settings);
            } else if id == self.quit_id {
                return Some(TrayAction::Quit);
            }
        }
        None
    }
}

fn create_default_icon() -> Result<Icon> {
    let size = 32u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);

    for y in 0..size {
        for x in 0..size {
            let in_border = x < 2 || x >= size - 2 || y < 2 || y >= size - 2;
            let in_inner = x >= 8 && x < size - 8 && y >= 8 && y < size - 8;

            if in_border {
                rgba.extend_from_slice(&[100, 100, 100, 255]);
            } else if in_inner {
                rgba.extend_from_slice(&[200, 200, 200, 255]);
            } else {
                rgba.extend_from_slice(&[50, 50, 50, 255]);
            }
        }
    }

    let icon = Icon::from_rgba(rgba, size, size)?;
    Ok(icon)
}
