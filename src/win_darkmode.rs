// Windows 10 1903+ and Windows 11 ship a dark-mode theme for the legacy Win32
// context menu (HMENU). The opt-in is undocumented but stable — Edge, Notepad,
// File Explorer, Visual Studio and most modern Microsoft surfaces use it. We
// flip it on at process startup so the tray's native HMENU and any other
// system menus capscr surfaces inherit the dark Windows-11 styling instead of
// the default light theme.
//
// The undocumented entry point is `uxtheme.dll!SetPreferredAppMode` exported
// at ordinal 135. Microsoft has shipped this on every Win10/11 build since
// 1903; we LoadLibrary + GetProcAddress so absent symbols on older Windows
// just no-op instead of crashing.

use windows::core::{s, PCSTR};
use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

#[repr(C)]
#[allow(dead_code)]
enum PreferredAppMode {
    Default = 0,
    AllowDark = 1,
    ForceDark = 2,
    ForceLight = 3,
    Max = 4,
}

type SetPreferredAppModeFn = unsafe extern "system" fn(mode: i32) -> i32;
type FlushMenuThemesFn = unsafe extern "system" fn();

pub fn enable_dark_menus() {
    unsafe {
        let module: HMODULE =
            match LoadLibraryW(windows::core::w!("uxtheme.dll")) {
                Ok(h) => h,
                Err(e) => {
                    tracing::debug!("uxtheme.dll load failed: {e}");
                    return;
                }
            };

        // ordinal 135 is SetPreferredAppMode on 1903+; older builds export
        // ordinal 135 as AllowDarkModeForApp (BOOL), which we don't bother
        // calling — Win10 pre-1903 just gets light menus.
        let set_mode_addr =
            GetProcAddress(module, PCSTR(135 as *const u8));
        let flush_addr =
            GetProcAddress(module, s!("FlushMenuThemes"));

        if let Some(set_mode_addr) = set_mode_addr {
            let set_mode: SetPreferredAppModeFn = std::mem::transmute(set_mode_addr);
            set_mode(PreferredAppMode::AllowDark as i32);
            tracing::debug!("uxtheme: SetPreferredAppMode(AllowDark) applied");
        } else {
            tracing::debug!("uxtheme: SetPreferredAppMode unavailable");
        }

        if let Some(flush_addr) = flush_addr {
            let flush: FlushMenuThemesFn = std::mem::transmute(flush_addr);
            flush();
        }
    }
}
