use anyhow::{anyhow, Result};
use image::RgbaImage;
use std::io::Cursor;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

const MAX_UPLOAD_SIZE: usize = 32 * 1024 * 1024;
const UPLOAD_TIMEOUT_SECS: u64 = 60;
const MAX_URL_LEN: usize = 2048;
const MAX_RESPONSE_SIZE: usize = 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const MAX_FORM_NAME_LEN: usize = 64;
const MAX_RESPONSE_PATH_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum UploadService {
    #[default]
    Imgur,
    Custom(CustomUploader),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomUploader {
    pub name: String,
    pub request_url: String,
    pub file_form_name: String,
    pub response_url_path: String,
}

impl Default for CustomUploader {
    fn default() -> Self {
        Self {
            name: String::from("Custom"),
            request_url: String::new(),
            file_form_name: String::from("file"),
            response_url_path: String::from("url"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct UploadResult {
    pub url: String,
    pub delete_url: Option<String>,
}

pub struct ImageUploader {
    client: reqwest::blocking::Client,
}

impl ImageUploader {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(UPLOAD_TIMEOUT_SECS))
            .user_agent("capscr/1.0")
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .build()?;
        Ok(Self { client })
    }

    fn is_private_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ipv4) => {
                ipv4.is_loopback()
                    || ipv4.is_private()
                    || ipv4.is_link_local()
                    || ipv4.is_broadcast()
                    || ipv4.is_documentation()
                    || ipv4.is_unspecified()
                    || ipv4.octets()[0] == 100 && (ipv4.octets()[1] & 0xC0) == 64
                    || ipv4.octets() == [169, 254, 169, 254]
            }
            IpAddr::V6(ipv6) => {
                ipv6.is_loopback() || ipv6.is_unspecified()
            }
        }
    }

    fn validate_url_security(url: &str) -> Result<()> {
        let parsed = url::Url::parse(url).map_err(|_| anyhow!("Invalid URL format"))?;

        if parsed.scheme() != "https" {
            return Err(anyhow!("Only HTTPS URLs are allowed"));
        }

        let host = parsed.host_str().ok_or_else(|| anyhow!("URL has no host"))?;

        if host.is_empty() || host.len() > 253 {
            return Err(anyhow!("Invalid hostname length"));
        }

        let blocked_hosts = [
            "localhost", "127.0.0.1", "::1", "[::1]", "0.0.0.0",
            "metadata.google.internal", "metadata.google.com", "metadata",
            "instance-data", "burpcollaborator.net", "oastify.com",
        ];
        let host_lower = host.to_lowercase();
        for blocked in &blocked_hosts {
            if host_lower == *blocked || host_lower.ends_with(&format!(".{}", blocked)) {
                return Err(anyhow!("Host not allowed"));
            }
        }

        if host_lower.starts_with("169.254.") || host_lower.contains("169.254.169.254") {
            return Err(anyhow!("Cloud metadata endpoints are blocked"));
        }

        if Self::is_private_ip_string(&host_lower) {
            return Err(anyhow!("Private IP ranges are blocked"));
        }

        let port = parsed.port().unwrap_or(443);
        let blocked_ports = [0, 22, 23, 25, 110, 143, 445, 3306, 3389, 5432, 6379, 27017];
        if blocked_ports.contains(&port) {
            return Err(anyhow!("Port not allowed"));
        }

        let host_with_port = format!("{}:{}", host, port);
        let resolved_ips: Vec<IpAddr> = host_with_port
            .to_socket_addrs()
            .map(|addrs| addrs.map(|a| a.ip()).collect())
            .unwrap_or_default();

        if resolved_ips.is_empty() {
            return Err(anyhow!("Could not resolve hostname"));
        }

        for ip in &resolved_ips {
            if Self::is_private_ip(*ip) {
                return Err(anyhow!("URL resolves to private/internal IP"));
            }
        }

        std::thread::sleep(Duration::from_millis(100));

        let resolved_ips_second: Vec<IpAddr> = host_with_port
            .to_socket_addrs()
            .map(|addrs| addrs.map(|a| a.ip()).collect())
            .unwrap_or_default();

        for ip in &resolved_ips_second {
            if Self::is_private_ip(*ip) {
                return Err(anyhow!("DNS rebinding detected"));
            }
        }

        Ok(())
    }

    fn is_private_ip_string(host: &str) -> bool {
        if host.starts_with("10.") || host.starts_with("192.168.") {
            return true;
        }
        if host.starts_with("172.") {
            if let Some(second) = host.strip_prefix("172.")
                .and_then(|s| s.split('.').next())
                .and_then(|s| s.parse::<u8>().ok())
            {
                if (16..=31).contains(&second) {
                    return true;
                }
            }
        }
        if host.starts_with("fc") || host.starts_with("fd") || host.starts_with("fe80") {
            return true;
        }
        false
    }

    fn validate_form_name(name: &str) -> Result<()> {
        if name.is_empty() {
            return Err(anyhow!("Form field name cannot be empty"));
        }
        if name.len() > MAX_FORM_NAME_LEN {
            return Err(anyhow!("Form field name too long"));
        }
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(anyhow!("Form field name contains invalid characters"));
        }
        Ok(())
    }

    fn validate_response_path(path: &str) -> Result<()> {
        if path.is_empty() {
            return Err(anyhow!("Response path cannot be empty"));
        }
        if path.len() > MAX_RESPONSE_PATH_LEN {
            return Err(anyhow!("Response path too long"));
        }
        if !path.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-') {
            return Err(anyhow!("Response path contains invalid characters"));
        }
        if path.starts_with('.') || path.ends_with('.') || path.contains("..") {
            return Err(anyhow!("Response path has invalid format"));
        }
        Ok(())
    }

    pub fn upload(&self, image: &RgbaImage, service: &UploadService) -> Result<UploadResult> {
        let png_data = self.encode_png(image)?;

        if png_data.len() > MAX_UPLOAD_SIZE {
            return Err(anyhow!("Image too large to upload ({} bytes)", png_data.len()));
        }

        match service {
            UploadService::Imgur => self.upload_imgur(&png_data),
            UploadService::Custom(config) => self.upload_custom(&png_data, config),
        }
    }

    fn encode_png(&self, image: &RgbaImage) -> Result<Vec<u8>> {
        let mut buffer = Cursor::new(Vec::new());
        image.write_to(&mut buffer, image::ImageFormat::Png)?;
        Ok(buffer.into_inner())
    }

    fn upload_imgur(&self, png_data: &[u8]) -> Result<UploadResult> {
        let client_id = "546c25a59c58ad7";

        let form = reqwest::blocking::multipart::Form::new()
            .part(
                "image",
                reqwest::blocking::multipart::Part::bytes(png_data.to_vec())
                    .file_name("screenshot.png")
                    .mime_str("image/png")?,
            );

        let response = self
            .client
            .post("https://api.imgur.com/3/image")
            .header("Authorization", format!("Client-ID {}", client_id))
            .multipart(form)
            .send()?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("Imgur upload failed with status: {}", status));
        }

        let content_length = response
            .content_length()
            .unwrap_or(MAX_RESPONSE_SIZE as u64 + 1);
        if content_length > MAX_RESPONSE_SIZE as u64 {
            return Err(anyhow!("Response too large"));
        }

        let text = response.text()?;
        if text.len() > MAX_RESPONSE_SIZE {
            return Err(anyhow!("Response too large"));
        }

        let json: serde_json::Value = serde_json::from_str(&text)?;

        let success = json.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
        if !success {
            let error_msg = json
                .get("data")
                .and_then(|d| d.get("error"))
                .and_then(|e| e.as_str())
                .unwrap_or("Unknown error");
            return Err(anyhow!("Imgur error: {}", error_msg));
        }

        let link = json
            .get("data")
            .and_then(|d| d.get("link"))
            .and_then(|l| l.as_str())
            .ok_or_else(|| anyhow!("No link in response"))?;

        if link.len() > MAX_URL_LEN {
            return Err(anyhow!("URL too long"));
        }

        let delete_hash = json
            .get("data")
            .and_then(|d| d.get("deletehash"))
            .and_then(|h| h.as_str());

        let delete_url = delete_hash.map(|hash| format!("https://imgur.com/delete/{}", hash));

        Ok(UploadResult {
            url: link.to_string(),
            delete_url,
        })
    }

    fn upload_custom(&self, png_data: &[u8], config: &CustomUploader) -> Result<UploadResult> {
        if config.request_url.is_empty() {
            return Err(anyhow!("Custom uploader URL not configured"));
        }

        if config.request_url.len() > MAX_URL_LEN {
            return Err(anyhow!("Request URL too long"));
        }

        Self::validate_url_security(&config.request_url)?;
        Self::validate_form_name(&config.file_form_name)?;
        Self::validate_response_path(&config.response_url_path)?;

        let form = reqwest::blocking::multipart::Form::new()
            .part(
                config.file_form_name.clone(),
                reqwest::blocking::multipart::Part::bytes(png_data.to_vec())
                    .file_name("screenshot.png")
                    .mime_str("image/png")?,
            );

        let response = self
            .client
            .post(&config.request_url)
            .multipart(form)
            .send()?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow!("Upload failed with status: {}", status));
        }

        let content_length = response
            .content_length()
            .unwrap_or(MAX_RESPONSE_SIZE as u64 + 1);
        if content_length > MAX_RESPONSE_SIZE as u64 {
            return Err(anyhow!("Response too large"));
        }

        let text = response.text()?;
        if text.len() > MAX_RESPONSE_SIZE {
            return Err(anyhow!("Response too large"));
        }

        let url = self.extract_url_from_response(&text, &config.response_url_path)?;

        if url.len() > MAX_URL_LEN {
            return Err(anyhow!("URL too long"));
        }

        Self::validate_returned_url(&url)?;

        Ok(UploadResult {
            url,
            delete_url: None,
        })
    }

    fn validate_returned_url(url: &str) -> Result<()> {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(anyhow!("Invalid URL scheme in response"));
        }

        if url.to_lowercase().starts_with("javascript:")
            || url.to_lowercase().starts_with("data:")
            || url.to_lowercase().starts_with("vbscript:")
            || url.to_lowercase().starts_with("file:")
        {
            return Err(anyhow!("Dangerous URL scheme in response"));
        }

        if url.contains('\0') || url.contains('\n') || url.contains('\r') {
            return Err(anyhow!("URL contains invalid characters"));
        }

        if let Ok(parsed) = url::Url::parse(url) {
            if parsed.host_str().is_none() {
                return Err(anyhow!("URL has no host"));
            }
        } else {
            return Err(anyhow!("Invalid URL format in response"));
        }

        Ok(())
    }

    fn extract_url_from_response(&self, text: &str, path: &str) -> Result<String> {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
            let parts: Vec<&str> = path.split('.').collect();
            let mut current = &json;

            for part in parts {
                current = current
                    .get(part)
                    .ok_or_else(|| anyhow!("Path '{}' not found in response", path))?;
            }

            if let Some(url) = current.as_str() {
                return Ok(url.to_string());
            }
        }

        let trimmed = text.trim();
        if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
            && trimmed.len() <= MAX_URL_LEN
            && !trimmed.contains('\n')
        {
            return Ok(trimmed.to_string());
        }

        Err(anyhow!("Could not extract URL from response"))
    }
}

impl Default for ImageUploader {
    fn default() -> Self {
        Self::new().unwrap_or_else(|_| Self {
            client: reqwest::blocking::Client::new(),
        })
    }
}

pub fn copy_url_to_clipboard(url: &str) -> Result<()> {
    use arboard::Clipboard;

    if url.len() > MAX_URL_LEN {
        return Err(anyhow!("URL too long"));
    }

    let mut clipboard = Clipboard::new()?;
    clipboard.set_text(url.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_url() {
        let uploader = ImageUploader::default();
        let json = r#"{"data": {"url": "https://example.com/image.png"}}"#;
        let result = uploader.extract_url_from_response(json, "data.url");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "https://example.com/image.png");
    }

    #[test]
    fn test_extract_plain_url() {
        let uploader = ImageUploader::default();
        let text = "https://example.com/image.png";
        let result = uploader.extract_url_from_response(text, "url");
        assert!(result.is_ok());
    }

    #[test]
    fn test_custom_uploader_requires_https() {
        let uploader = ImageUploader::default();
        let config = CustomUploader {
            request_url: "http://insecure.example.com".to_string(),
            ..Default::default()
        };
        let result = uploader.upload_custom(&[0u8; 100], &config);
        assert!(result.is_err());
    }
}
