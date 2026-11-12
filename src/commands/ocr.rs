use super::*;

#[cfg(target_os = "linux")]
#[cfg(test)]
mod ocr_locale_tests {
    use super::desired_tess_langs;

    #[test]
    fn maps_a_plain_lang_locale() {
        assert_eq!(
            desired_tess_langs(None, None, Some("de_DE.UTF-8")),
            vec!["deu"]
        );
    }

    #[test]
    fn language_priority_list_wins_and_dedupes() {
        assert_eq!(
            desired_tess_langs(Some("de_DE:en_US:de_AT"), Some("fr_FR.UTF-8"), None),
            vec!["deu", "eng"]
        );
    }

    #[test]
    fn c_and_posix_locales_yield_nothing() {
        assert!(desired_tess_langs(None, Some("C"), Some("POSIX")).is_empty());
    }

    #[test]
    fn chinese_splits_by_region() {
        assert_eq!(
            desired_tess_langs(Some("zh_TW:zh_CN"), None, None),
            vec!["chi_tra", "chi_sim"]
        );
    }

    #[test]
    fn unknown_codes_are_skipped() {
        assert_eq!(
            desired_tess_langs(Some("xx_YY:ja_JP"), None, None),
            vec!["jpn"]
        );
    }

    #[test]
    fn modifiers_and_encodings_are_stripped() {
        assert_eq!(
            desired_tess_langs(None, None, Some("sr_RS.UTF-8@latin")),
            Vec::<&str>::new()
        );
        assert_eq!(
            desired_tess_langs(None, None, Some("uk_UA.KOI8-U")),
            vec!["ukr"]
        );
    }
}

#[tauri::command]
pub async fn run_ocr(path: String, state: State<'_, AppState>) -> Result<String, String> {
    let config = state.config.lock().unwrap().clone();
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    if !is_path_allowed(&canonical, &config) {
        return Err("Path is outside the allowed directories".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(&canonical).map_err(|e| e.to_string())?;
        ocr_image_bytes(&bytes).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// run Windows OCR over an encoded image (png/jpeg/...) and return the text
#[cfg(windows)]
fn ocr_image_bytes(image_bytes: &[u8]) -> anyhow::Result<String> {
    use windows::Graphics::Imaging::BitmapDecoder;
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .map_err(|e| anyhow::anyhow!("Failed to create OCR engine: {:?}", e))?;
    let stream = InMemoryRandomAccessStream::new()
        .map_err(|e| anyhow::anyhow!("Failed to create in-memory stream: {:?}", e))?;
    let writer = DataWriter::CreateDataWriter(&stream)
        .map_err(|e| anyhow::anyhow!("Failed to create data writer: {:?}", e))?;
    writer
        .WriteBytes(image_bytes)
        .map_err(|e| anyhow::anyhow!("Failed to write bytes: {:?}", e))?;
    writer
        .StoreAsync()?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to store data: {:?}", e))?;
    writer
        .FlushAsync()?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to flush stream: {:?}", e))?;
    stream
        .Seek(0)
        .map_err(|e| anyhow::anyhow!("Failed to seek stream: {:?}", e))?;
    let decoder = BitmapDecoder::CreateAsync(&stream)?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to decode image: {:?}", e))?;
    let software_bitmap = decoder
        .GetSoftwareBitmapAsync()?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to get software bitmap: {:?}", e))?;
    let result = engine
        .RecognizeAsync(&software_bitmap)?
        .get()
        .map_err(|e| anyhow::anyhow!("Failed to run OCR: {:?}", e))?;
    Ok(result.Text()?.to_string())
}

// windows OCR picks the user's profile languages; mirror that by mapping the
// session locale onto tesseract's traineddata codes
#[cfg(target_os = "linux")]
fn locale_to_tess(lang: &str, region: Option<&str>) -> Option<&'static str> {
    Some(match lang {
        "en" => "eng",
        "de" => "deu",
        "fr" => "fra",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "ru" => "rus",
        "ja" => "jpn",
        "ko" => "kor",
        "nl" => "nld",
        "pl" => "pol",
        "sv" => "swe",
        "fi" => "fin",
        "da" => "dan",
        "nb" | "no" => "nor",
        "cs" => "ces",
        "hu" => "hun",
        "tr" => "tur",
        "uk" => "ukr",
        "el" => "ell",
        "ro" => "ron",
        "bg" => "bul",
        "ar" => "ara",
        "he" => "heb",
        "hi" => "hin",
        "th" => "tha",
        "vi" => "vie",
        "zh" => match region {
            Some("TW") | Some("HK") | Some("MO") => "chi_tra",
            _ => "chi_sim",
        },
        _ => return None,
    })
}

// LANGUAGE is a colon-separated priority list; LC_ALL then LANG hold single
// locales like de_DE.UTF-8. C/POSIX mean "no preference".
#[cfg(target_os = "linux")]
fn desired_tess_langs(
    language: Option<&str>,
    lc_all: Option<&str>,
    lang: Option<&str>,
) -> Vec<&'static str> {
    let raw: Vec<&str> = match language.filter(|v| !v.is_empty()) {
        Some(list) => list.split(':').collect(),
        None => lc_all
            .filter(|v| !v.is_empty())
            .or(lang.filter(|v| !v.is_empty()))
            .into_iter()
            .collect(),
    };
    let mut mapped = Vec::new();
    for locale in raw {
        let locale = locale.split(['.', '@']).next().unwrap_or(locale);
        if locale.is_empty() || locale == "C" || locale == "POSIX" {
            continue;
        }
        let (lang, region) = match locale.split_once('_') {
            Some((lang, region)) => (lang, Some(region)),
            None => (locale, None),
        };
        if let Some(code) = locale_to_tess(lang, region) {
            if !mapped.contains(&code) {
                mapped.push(code);
            }
        }
    }
    mapped
}

#[cfg(target_os = "linux")]
fn installed_tess_langs() -> &'static std::collections::HashSet<String> {
    static LANGS: std::sync::OnceLock<std::collections::HashSet<String>> =
        std::sync::OnceLock::new();
    LANGS.get_or_init(|| {
        std::process::Command::new("tesseract")
            .arg("--list-langs")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .skip(1)
                    .map(|line| line.trim().to_string())
                    .filter(|line| !line.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    })
}

// `-l deu+eng` style; None falls back to tesseract's default silently
#[cfg(target_os = "linux")]
fn tesseract_lang_args() -> Option<String> {
    let installed = installed_tess_langs();
    let desired: Vec<&str> = desired_tess_langs(
        std::env::var("LANGUAGE").ok().as_deref(),
        std::env::var("LC_ALL").ok().as_deref(),
        std::env::var("LANG").ok().as_deref(),
    )
    .into_iter()
    .filter(|code| installed.contains(*code))
    .collect();
    if desired.is_empty() {
        return None;
    }
    Some(desired.join("+"))
}

/// run tesseract over an encoded image and return the text. tesseract is the
/// packaged OCR engine on every major distro; the error message points at the
/// package when it's missing so the post-action isn't a dead end
#[cfg(target_os = "linux")]
fn ocr_image_bytes(image_bytes: &[u8]) -> anyhow::Result<String> {
    use std::io::Write;
    let mut command = std::process::Command::new("tesseract");
    command.args(["stdin", "stdout"]);
    if let Some(langs) = tesseract_lang_args() {
        tracing::debug!("ocr languages: {langs}");
        command.args(["-l", &langs]);
    }
    let mut child = command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| anyhow::anyhow!("OCR needs tesseract — install the tesseract-ocr package"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("tesseract stdin unavailable"))?
        .write_all(image_bytes)?;
    let output = child.wait_with_output()?;
    if !output.status.success() {
        anyhow::bail!("tesseract failed with status {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn ocr_image_bytes(_image_bytes: &[u8]) -> anyhow::Result<String> {
    anyhow::bail!("OCR is not supported on this platform")
}

/// OCR a freshly captured image by encoding it to PNG in memory first
pub(crate) fn ocr_capture(image: &RgbaImage) -> anyhow::Result<String> {
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|e| anyhow::anyhow!("failed to encode capture for OCR: {e}"))?;
    ocr_image_bytes(&png)
}
