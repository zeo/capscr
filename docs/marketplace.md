# marketplace registry — server-side spec

this is what `rot.lt` (or any mirror) MUST serve at the registry URL configured in capscr.

default URL: `https://rot.lt/capscr/registry.json` (overridable via `config.marketplace.registry_url`).

---

## 1. transport

- **HTTPS only.** the client refuses to talk to plain HTTP.
- **content-type**: `application/json` (the client parses by extension/content, but set it for sanity).
- **cache-control**: anything you like. capscr fetches on demand from the Marketplace tab, no aggressive polling. `max-age=300` is reasonable for a curated list.
- **CORS**: not required — capscr fetches from the Rust process, not the browser.
- **size cap (client-side)**: 2 MB. budget your registry accordingly.

## 2. shape

```json
{
  "version": 1,
  "updated_unix": 1715990400,
  "plugins": [
    {
      "id": "ocr-tesseract",
      "name": "OCR (Tesseract)",
      "version": "1.0.0",
      "description": "Extract text from captures via Tesseract.",
      "author": "rot",
      "homepage": "https://rot.lt/capscr/plugins/ocr-tesseract",
      "download_url": "https://rot.lt/capscr/plugins/ocr-tesseract-1.0.0.zip",
      "sha256": "abc123def456...",
      "size_bytes": 12345,
      "tags": ["ocr", "text"],
      "min_capscr_version": "0.3.28",
      "license": "MIT"
    }
  ]
}
```

### top level

| field | type | required | meaning |
|---|---|---|---|
| `version` | u32 | yes | schema version. **must equal 1** for the current client. bump if you break the shape. |
| `updated_unix` | u64 | yes | unix seconds; informational, displayed nowhere yet but reserved. |
| `plugins` | array | yes | the listing. empty array is fine. |

### per-entry

| field | type | required | constraint |
|---|---|---|---|
| `id` | string | yes | matches `^[a-z0-9][a-z0-9_-]{0,63}$`. used as the on-disk folder name and as the registry key — **cannot change** after first publish without breaking installs. |
| `name` | string | yes | display name. free-form. |
| `version` | string | yes | semver-ish display string. capscr doesn't parse it (yet), but `min_capscr_version` is parsed. |
| `description` | string | no | one-line blurb shown under the title in the Marketplace tab. |
| `author` | string | no | "by …" in the meta row. |
| `homepage` | string | no | URL opened by the `site` button. HTTP/HTTPS scheme; otherwise the button does nothing useful. |
| `download_url` | string | yes | **HTTPS** zip URL the client GETs on install. |
| `sha256` | string | yes | lowercase hex sha256 of the zip body. **verified before extraction** — mismatch aborts the install. |
| `size_bytes` | u64 | yes | exact zip size. capscr aborts if the download exceeds 50 MB regardless. |
| `tags` | string[] | no | shown as chips. |
| `min_capscr_version` | string | no | currently informational; capscr doesn't enforce yet. set it anyway. |
| `license` | string | no | SPDX identifier (e.g. `MIT`, `Apache-2.0`). |

### unknown fields

clients ignore unknown fields. you can add `download_size_compressed`, `screenshots`, `repo`, whatever you want — capscr's 0.3.28 client just won't render them. when capscr starts to consume new fields it'll bump `version`.

## 3. plugin zip layout

the zip body MUST contain a `plugin.toml` at the root. anything else is up to you.

```
ocr-tesseract-1.0.0.zip
├── plugin.toml         (required, at root)
├── README.md           (optional)
└── ...                 (any additional files the plugin format supports)
```

### `plugin.toml`

```toml
name = "OCR (Tesseract)"
version = "1.0.0"
description = "Extract text from captures via Tesseract."
enabled = true
```

| field | type | required | default |
|---|---|---|---|
| `name` | string | yes | — |
| `version` | string | no | `""` |
| `description` | string | no | `""` |
| `enabled` | bool | no | `true` |

future versions will add a runtime hook table (WASM entry, hotkey hooks, etc.) — for now this is metadata.

### zip integrity rules (enforced client-side)

- **max files**: 256. zips with more entries are rejected.
- **max per-file size**: 16 MB.
- **no `..` traversal**. enforced via `enclosed_name` plus a defence-in-depth pass.
- **no absolute paths.**
- **no symlinks** (the zip crate refuses them by default).
- **must contain `plugin.toml`** at the root — otherwise the install is rolled back.

## 4. install flow (what the client does)

1. user clicks **install** in the Marketplace tab.
2. client re-fetches the registry (defends against stale UI listing a plugin that's since been pulled).
3. client GETs `download_url` with a 120s timeout and 50 MB cap.
4. client computes sha256 of the response body; compares to entry's `sha256` (case-insensitive). mismatch → abort.
5. client extracts the zip into `%APPDATA%/com.capscr.capscr/data/plugins/.staging-<id>/`.
6. client validates `plugin.toml` exists in the staging dir; if not → wipe staging, abort.
7. client `std::fs::rename`s the staging dir to `%APPDATA%/com.capscr.capscr/data/plugins/<id>/` atomically.
8. client refreshes the installed list — your plugin shows up in the "installed" section.

if any step fails, the existing install (if any) is untouched.

## 5. publishing checklist

- [ ] zip the plugin folder so `plugin.toml` is at the root, not nested.
- [ ] compute `sha256sum plugin.zip` (or `Get-FileHash plugin.zip -Algorithm SHA256` on Windows).
- [ ] host the zip at a stable HTTPS URL — capscr stores the URL in registry, not the bytes, so cache-busting is your call.
- [ ] add the entry to `registry.json` and serve it.
- [ ] sanity check from a clean capscr install: open Marketplace tab, see your plugin in browse, click install, see it appear under installed.

## 6. mirroring

anyone can run a mirror. just serve the same `registry.json` shape. users override `marketplace.registry_url` in their `%APPDATA%/com.capscr.capscr/config/config.toml`:

```toml
[marketplace]
registry_url = "https://my-mirror.example/capscr/registry.json"
```

mirror URLs are still HTTPS-validated client-side.
