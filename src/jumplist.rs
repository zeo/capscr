// Windows taskbar jump list — populates the right-click menu on the capscr
// taskbar button with capture shortcuts. Each entry launches the exe with a
// `--jump=<kind>` arg; tauri-plugin-single-instance forwards that to the
// already-running process (see main.rs).
//
// the COM calls here are all best-effort: if any step fails, the jump list
// silently stays as Windows' default. We never want this to block startup.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows::core::{Interface, PCWSTR};
use windows::Win32::System::Com::StructuredStorage::InitPropVariantFromStringVector;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY};
use windows::Win32::UI::Shell::{
    DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW,
    SetCurrentProcessExplicitAppUserModelID, ShellLink,
};

pub const APP_USER_MODEL_ID: &str = "io.rot.capscr";

// PKEY_Title — the property key for shell-link display title. Lives in
// propkey.h as PSGUID_SUMMARYINFORMATION = {f29f85e0-4ff9-1068-ab91-08002b27b3d9},
// pid = 2.
const PKEY_TITLE: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
    pid: 2,
};

struct Task {
    arg: &'static str,
    title: &'static str,
    desc: &'static str,
}

const TASKS: &[Task] = &[
    Task {
        arg: "--jump=region",
        title: "Capture region",
        desc: "Drag a rectangle to capture",
    },
    Task {
        arg: "--jump=window",
        title: "Capture window",
        desc: "Pick a window to capture",
    },
    Task {
        arg: "--jump=fullscreen",
        title: "Capture fullscreen",
        desc: "Capture the whole screen",
    },
    Task {
        arg: "--jump=captures",
        title: "Open captures folder",
        desc: "Open the output directory",
    },
    Task {
        arg: "--jump=hub",
        title: "Open hub",
        desc: "Open the capscr hub window",
    },
];

pub fn set_app_user_model_id() {
    let id = wide(APP_USER_MODEL_ID);
    unsafe {
        let _ = SetCurrentProcessExplicitAppUserModelID(PCWSTR(id.as_ptr()));
    }
}

pub fn register() -> windows::core::Result<()> {
    unsafe {
        // COINIT_APARTMENTTHREADED matches what most desktop apps use; safe to
        // call repeatedly (returns RPC_E_CHANGED_MODE on conflict, which we
        // ignore — Tauri may have already initialised COM on this thread).
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let list: ICustomDestinationList =
            CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;

        let app_id = wide(APP_USER_MODEL_ID);
        list.SetAppID(PCWSTR(app_id.as_ptr()))?;

        let mut min_slots = 0u32;
        let _removed: IObjectArray = list.BeginList(&mut min_slots)?;

        let collection: IObjectCollection =
            CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;

        let exe = std::env::current_exe()
            .map_err(|_| windows::core::Error::from_win32())?;
        let exe_wide = wide_path(&exe);

        for t in TASKS {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(PCWSTR(exe_wide.as_ptr()))?;

            let args = wide(t.arg);
            link.SetArguments(PCWSTR(args.as_ptr()))?;

            let desc = wide(t.desc);
            link.SetDescription(PCWSTR(desc.as_ptr()))?;

            // use the exe itself as the icon source (index 0 = first icon).
            link.SetIconLocation(PCWSTR(exe_wide.as_ptr()), 0)?;

            // the visible label in the jump list comes from PKEY_Title on the
            // shell link's property store, not from SetDescription.
            let store: IPropertyStore = link.cast()?;
            let title = wide(t.title);
            let title_ptr = PCWSTR(title.as_ptr());
            let prop = InitPropVariantFromStringVector(Some(&[title_ptr]))?;
            store.SetValue(&PKEY_TITLE, &prop)?;
            store.Commit()?;

            collection.AddObject(&link)?;
        }

        let array: IObjectArray = collection.cast()?;
        list.AddUserTasks(&array)?;
        list.CommitList()?;
    }
    Ok(())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn wide_path(p: &std::path::Path) -> Vec<u16> {
    OsStr::new(p)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}
