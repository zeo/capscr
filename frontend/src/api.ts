import { invoke } from "@tauri-apps/api/core";

export type CaptureMode = "region" | "window" | "fullscreen" | "active-monitor";
export type PostAction =
  | "clipboard"
  | "save-file"
  | "upload"
  | "save-and-clipboard"
  | "open-editor"
  | "prompt";

export interface HdrConfig {
  mode: "map-cll-to-display" | "normalize-to-cll";
  brightness_nits: number;
  user_brightness_scale: number;
  use_p99_max_cll: boolean;
}

export interface OutputConfig {
  directory: string;
  format: "Png" | "Jpeg" | "Gif" | "Webp" | "Bmp";
  quality: number;
  filename_template: string;
  preserve_hdr: boolean;
}

export interface CaptureConfig {
  show_cursor: boolean;
  delay_ms: number;
  gif_fps: number;
  gif_max_duration_secs: number;
  hdr: HdrConfig;
}

export interface FtpConfig {
  host: string;
  port: number;
  username: string;
  /** plaintext password as typed by the user. on every save, set_config
   *  encrypts this into password_encrypted via Windows DPAPI and clears
   *  it from disk. An empty string here means "keep the current vault" */
  password: string;
  /** DPAPI-wrapped hex blob. read-only from the frontend's perspective —
   *  set by the backend on save, used as a "vault populated" sentinel */
  password_encrypted: string;
  remote_dir: string;
  use_tls: boolean;
  public_url_template: string;
}

export interface UploadConfig {
  destination: "Imgur" | "Custom" | "Ftp";
  copy_url_to_clipboard: boolean;
  custom_url: string;
  custom_form_name: string;
  custom_response_path: string;
  imgur_client_id: string;
  ftp: FtpConfig;
}

export interface UiConfig {
  theme: "Light" | "Dark";
  show_notifications: boolean;
  copy_to_clipboard: boolean;
  minimize_to_tray: boolean;
  auto_start: boolean;
  check_updates_on_launch: boolean;
}

export interface CaptureTask {
  id: string;
  name: string;
  hotkey: string;
  capture_mode: "region" | "window" | "fullscreen" | "active-monitor" | "region-gif";
  post_action:
    | "clipboard"
    | "save-file"
    | "upload"
    | "save-and-clipboard"
    | "open-editor"
    | "prompt";
  target_destination?: "imgur" | "custom" | "ftp" | null;
}

export interface AppConfig {
  output: OutputConfig;
  capture: CaptureConfig;
  hotkeys: { screenshot: string; record_gif: string; disabled_globally: boolean };
  ui: UiConfig;
  post_capture: {
    action: PostAction;
    open_file_after_save: boolean;
    play_sound: boolean;
  };
  upload: UploadConfig;
  capture_tasks: CaptureTask[];
}

export interface HistoryEntry {
  path: string;
  filename: string;
  size_bytes: number;
  modified_unix: number;
  is_gif: boolean;
  has_hdr: boolean;
}

export interface InstalledPlugin {
  id: string;
  name: string;
  version: string;
  description: string;
  enabled: boolean;
}

export interface RegistryEntry {
  id: string;
  name: string;
  version: string;
  description: string;
  author: string;
  homepage: string;
  download_url: string;
  sha256: string;
  size_bytes: number;
  tags: string[];
  min_capscr_version: string;
  license: string;
}

export interface UpdateInfo {
  version: string;
  current_version: string;
  notes: string | null;
}

export interface HotkeyStatusEntry {
  task_id: string;
  status: "live" | "failed";
  reason: string | null;
}

export interface HotkeyDiagnostics {
  disabled_globally: boolean;
  statuses: HotkeyStatusEntry[];
}

export const api = {
  getConfig: () => invoke<AppConfig>("get_config"),
  getDefaultConfig: () => invoke<AppConfig>("get_default_config"),
  setConfig: (config: AppConfig) => invoke<void>("set_config", { config }),
  takeScreenshot: (mode: CaptureMode, post: PostAction) =>
    invoke<void>("take_screenshot", { mode, post }),
  listCaptures: () => invoke<HistoryEntry[]>("list_captures"),
  deleteCapture: (path: string) => invoke<void>("delete_capture", { path }),
  copyCaptureToClipboard: (path: string) =>
    invoke<void>("copy_capture_to_clipboard", { path }),
  reuploadCapture: (path: string) =>
    invoke<{ url: string; delete_url: string | null }>("reupload_capture", { path }),
  openInExplorer: (path: string) => invoke<void>("open_in_explorer", { path }),
  exitApp: () => invoke<void>("exit_app"),

  listInstalledPlugins: () => invoke<InstalledPlugin[]>("list_installed_plugins"),
  openPluginsFolder: () => invoke<void>("open_plugins_folder"),
  setAutostart: (enabled: boolean) => invoke<void>("set_autostart", { enabled }),
  getAutostart: () => invoke<boolean>("get_autostart"),
  uploadFile: (path: string) =>
    invoke<{ url: string; delete_url: string | null }>("upload_file", { path }),
  openEditor: (path: string) => invoke<void>("open_editor", { path }),
  checkForUpdates: () => invoke<UpdateInfo | null>("check_for_updates"),
  installUpdate: () => invoke<void>("install_update"),
  isHdrCapture: (path: string) => invoke<boolean>("is_hdr_capture", { path }),
  marketplaceBrowse: () => invoke<RegistryEntry[]>("marketplace_browse"),
  marketplaceInstall: (id: string) => invoke<void>("marketplace_install", { id }),
  marketplaceUninstall: (id: string) => invoke<void>("marketplace_uninstall", { id }),
  togglePluginEnabled: (id: string, enabled: boolean) =>
    invoke<void>("toggle_plugin_enabled", { id, enabled }),
  hotkeyDiagnostics: () => invoke<HotkeyDiagnostics>("hotkey_diagnostics"),
  setHotkeysDisabled: (disabled: boolean) =>
    invoke<void>("set_hotkeys_disabled", { disabled }),
};
