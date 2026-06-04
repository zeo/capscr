#![allow(dead_code)]

pub mod known_hosts;

use anyhow::{anyhow, Result};
use image::RgbaImage;
use std::io::Cursor;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::sync::OnceLock;
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
    ImgurWithClientId(String),
    Custom(CustomUploader),
    Ftp(FtpTarget),
    Sftp(SftpTarget),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FtpTarget {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub remote_dir: String,
    pub use_tls: bool,
    pub public_url_template: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SftpTarget {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub remote_dir: String,
    pub public_url_template: String,
    pub private_key_path: String,
    pub private_key_passphrase: String,
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

static SHARED_UPLOADER: OnceLock<std::result::Result<ImageUploader, String>> = OnceLock::new();

impl ImageUploader {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(UPLOAD_TIMEOUT_SECS))
            .user_agent("capscr/1.0")
            // validate each redirect target so an attacker-controlled server
            // can't redirect uploads to a private/internal IP (SSRF)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                let url = attempt.url();
                if url.scheme() != "https" {
                    return attempt.error("redirect to non-https url blocked");
                }
                if let Some(host) = url.host_str() {
                    let h = host.to_lowercase();
                    let blocked = [
                        "localhost", "127.0.0.1", "0.0.0.0",
                        "metadata.google.internal", "metadata.google.com",
                        "instance-data",
                    ];
                    if blocked.iter().any(|b| h == *b || h.ends_with(&format!(".{b}"))) {
                        return attempt.error("redirect to blocked host");
                    }
                    if h.starts_with("169.254.") {
                        return attempt.error("redirect to link-local/metadata ip blocked");
                    }
                    if ImageUploader::is_private_ip_string(&h) {
                        return attempt.error("redirect to private ip blocked");
                    }
                } else {
                    return attempt.error("redirect has no host");
                }
                attempt.follow()
            }))
            .build()?;
        Ok(Self { client })
    }

    pub(crate) fn is_private_ip(ip: IpAddr) -> bool {
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
                if ipv6.is_loopback() || ipv6.is_unspecified() {
                    return true;
                }
                let o = ipv6.octets();
                // ULA: fc00::/7
                if o[0] & 0xFE == 0xFC { return true; }
                // link-local: fe80::/10
                if o[0] == 0xFE && (o[1] & 0xC0) == 0x80 { return true; }
                // IPv4-mapped: ::ffff:0:0/96 — check the embedded IPv4
                if o[..10] == [0u8; 10] && o[10] == 0xFF && o[11] == 0xFF {
                    let v4 = IpAddr::V4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
                    return Self::is_private_ip(v4);
                }
                false
            }
        }
    }

    fn validate_url_security(url: &str) -> Result<()> {
        let parsed = url::Url::parse(url).map_err(|_| anyhow!("Invalid URL format"))?;

        if parsed.scheme() != "https" {
            return Err(anyhow!("Only HTTPS URLs are allowed"));
        }

        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("URL has no host"))?;

        if host.is_empty() || host.len() > 253 {
            return Err(anyhow!("Invalid hostname length"));
        }

        let blocked_hosts = [
            "localhost",
            "127.0.0.1",
            "::1",
            "[::1]",
            "0.0.0.0",
            "metadata.google.internal",
            "metadata.google.com",
            "metadata",
            "instance-data",
            "burpcollaborator.net",
            "oastify.com",
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

    pub(crate) fn is_private_ip_string(host: &str) -> bool {
        // url::Url::host_str() wraps IPv6 in brackets; strip before pattern matching
        let host = host.trim_matches(|c| c == '[' || c == ']');
        if host.starts_with("10.") || host.starts_with("192.168.") {
            return true;
        }
        if host.starts_with("172.") {
            if let Some(second) = host
                .strip_prefix("172.")
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
        if !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
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
        if !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
        {
            return Err(anyhow!("Response path contains invalid characters"));
        }
        if path.starts_with('.') || path.ends_with('.') || path.contains("..") {
            return Err(anyhow!("Response path has invalid format"));
        }
        Ok(())
    }

    pub fn upload(&self, image: &RgbaImage, service: &UploadService) -> Result<UploadResult> {
        let png_data = self.encode_png(image)?;
        self.upload_raw(&png_data, "image/png", "screenshot.png", service)
    }

    pub fn upload_raw(
        &self,
        data: &[u8],
        mime: &str,
        file_name: &str,
        service: &UploadService,
    ) -> Result<UploadResult> {
        if data.len() > MAX_UPLOAD_SIZE {
            return Err(anyhow!("Upload too large ({} bytes)", data.len()));
        }
        // retry transient network failures up to 3 times with exponential
        // backoff (300ms, 600ms). HTTP-status errors and parser errors are
        // NOT retried — those indicate a real problem at the destination,
        // not a flaky link.
        let attempts = 3u32;
        let mut delay_ms = 300u64;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..attempts {
            let result = match service {
                UploadService::Imgur => {
                    self.upload_imgur(data, mime, file_name, "546c25a59c58ad7")
                }
                UploadService::ImgurWithClientId(cid) => {
                    self.upload_imgur(data, mime, file_name, cid)
                }
                UploadService::Custom(config) => {
                    self.upload_custom(data, mime, file_name, config)
                }
                UploadService::Ftp(target) => upload_ftp(data, file_name, target),
                UploadService::Sftp(target) => upload_sftp(data, file_name, target),
            };
            match result {
                Ok(r) => return Ok(r),
                Err(e) => {
                    let transient = is_transient_upload_error(&e);
                    if !transient || attempt + 1 == attempts {
                        return Err(e);
                    }
                    tracing::info!(
                        "upload attempt {} failed transiently ({e}); retrying in {}ms",
                        attempt + 1,
                        delay_ms
                    );
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(delay_ms));
                    delay_ms = delay_ms.saturating_mul(2);
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow!("upload failed after retries")))
    }

    fn encode_png(&self, image: &RgbaImage) -> Result<Vec<u8>> {
        let mut buffer = Cursor::new(Vec::new());
        image.write_to(&mut buffer, image::ImageFormat::Png)?;
        Ok(buffer.into_inner())
    }

    fn upload_imgur(
        &self,
        data: &[u8],
        mime: &str,
        file_name: &str,
        client_id: &str,
    ) -> Result<UploadResult> {
        let form = reqwest::blocking::multipart::Form::new().part(
            "image",
            reqwest::blocking::multipart::Part::bytes(data.to_vec())
                .file_name(file_name.to_string())
                .mime_str(mime)?,
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

        let success = json
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
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

        Self::validate_returned_url(link)?;

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

    fn upload_custom(
        &self,
        data: &[u8],
        mime: &str,
        file_name: &str,
        config: &CustomUploader,
    ) -> Result<UploadResult> {
        if config.request_url.is_empty() {
            return Err(anyhow!("Custom uploader URL not configured"));
        }

        if config.request_url.len() > MAX_URL_LEN {
            return Err(anyhow!("Request URL too long"));
        }

        Self::validate_url_security(&config.request_url)?;
        Self::validate_form_name(&config.file_form_name)?;
        Self::validate_response_path(&config.response_url_path)?;

        let form = reqwest::blocking::multipart::Form::new().part(
            config.file_form_name.clone(),
            reqwest::blocking::multipart::Part::bytes(data.to_vec())
                .file_name(file_name.to_string())
                .mime_str(mime)?,
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

pub fn shared_uploader() -> Result<&'static ImageUploader> {
    let cached = SHARED_UPLOADER.get_or_init(|| ImageUploader::new().map_err(|e| e.to_string()));
    match cached {
        Ok(uploader) => Ok(uploader),
        Err(err) => Err(anyhow!(err.clone())),
    }
}

pub fn copy_url_to_clipboard(url: &str) -> Result<()> {
    if url.len() > MAX_URL_LEN {
        return Err(anyhow!("URL too long"));
    }
    // use ClipboardManager's retry logic so clipboard contention doesn't drop
    // the upload URL silently (direct arboard call fails immediately if busy)
    crate::clipboard::ClipboardManager::new()?.copy_text(url)
}

fn generate_remote_filename() -> String {
    let now = chrono::Local::now();
    let ts = now.format("%Y%m%d_%H%M%S").to_string();
    let uuid = uuid::Uuid::new_v4();
    format!("capscr_{}_{}.png", ts, &uuid.as_simple().to_string()[..8])
}

fn build_url(template: &str, filename: &str) -> Result<String> {
    if template.is_empty() {
        return Err(anyhow!(
            "public_url_template is empty; set it to something like https://files.example.com/{{filename}}"
        ));
    }
    if !template.starts_with("https://") && !template.starts_with("http://") {
        return Err(anyhow!("public_url_template must start with https:// or http://"));
    }
    let url = template.replace("{filename}", filename);
    if url.len() > MAX_URL_LEN {
        return Err(anyhow!("Constructed URL too long"));
    }
    Ok(url)
}

fn validate_remote_dir(dir: &str) -> Result<()> {
    if dir.contains("..") {
        return Err(anyhow!("remote_dir cannot contain '..'"));
    }
    if dir.len() > 256 {
        return Err(anyhow!("remote_dir too long"));
    }
    Ok(())
}

fn validate_host(host: &str) -> Result<()> {
    if host.is_empty() {
        return Err(anyhow!("host is empty"));
    }
    if host.len() > 253 {
        return Err(anyhow!("host too long"));
    }
    if !host.chars().all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == ':') {
        return Err(anyhow!("host contains invalid characters"));
    }
    Ok(())
}

const BLOCKED_HOSTS: &[&str] = &[
    "localhost",
    "127.0.0.1",
    "::1",
    "0.0.0.0",
    "metadata.google.internal",
    "metadata.google.com",
    "metadata",
    "instance-data",
    "burpcollaborator.net",
    "oastify.com",
];

/// reject hosts that resolve to private / loopback / cloud-metadata IP ranges.
/// resolves DNS twice (with a small sleep between) so a malicious resolver
/// can't pass the check and then return a private IP on the real connect call.
pub(crate) fn validate_resolved_host(host: &str, port: u16) -> Result<()> {
    let host_lower = host.to_lowercase();
    for blocked in BLOCKED_HOSTS {
        if host_lower == *blocked || host_lower.ends_with(&format!(".{}", blocked)) {
            return Err(anyhow!("Host not allowed: {}", blocked));
        }
    }
    if host_lower.starts_with("169.254.") || host_lower.contains("169.254.169.254") {
        return Err(anyhow!("Cloud metadata endpoints are blocked"));
    }
    if ImageUploader::is_private_ip_string(&host_lower) {
        return Err(anyhow!("Private IP ranges are blocked"));
    }

    let host_with_port = format!("{}:{}", host, port);
    let resolved: Vec<IpAddr> = host_with_port
        .to_socket_addrs()
        .map_err(|e| anyhow!("Could not resolve hostname: {}", e))?
        .map(|a| a.ip())
        .collect();
    if resolved.is_empty() {
        return Err(anyhow!("Could not resolve hostname"));
    }
    for ip in &resolved {
        if ImageUploader::is_private_ip(*ip) {
            return Err(anyhow!("Host resolves to private/internal IP"));
        }
    }

    std::thread::sleep(Duration::from_millis(100));

    let resolved_second: Vec<IpAddr> = host_with_port
        .to_socket_addrs()
        .map(|addrs| addrs.map(|a| a.ip()).collect())
        .unwrap_or_default();
    for ip in &resolved_second {
        if ImageUploader::is_private_ip(*ip) {
            return Err(anyhow!("DNS rebinding detected"));
        }
    }

    Ok(())
}

// classify whether an upload error is worth retrying. We retry on
// timeouts, connection resets, dropped DNS, and 5xx-shaped server errors —
// not on auth failures or 4xx (retrying those would just hammer a server
// telling us "no"). Heuristic matches against the anyhow chain text, so we
// don't have to thread reqwest::Error types through every layer.
fn is_transient_upload_error(e: &anyhow::Error) -> bool {
    let text = format!("{:#}", e).to_lowercase();
    let transient_markers = [
        "timed out",
        "timeout",
        "connection reset",
        "connection refused",
        "broken pipe",
        "tls handshake",
        "name resolution",
        "name or service not known",
        "temporary failure",
        "server misbehaving",
        "stream closed",
        "502",
        "503",
        "504",
    ];
    transient_markers.iter().any(|m| text.contains(m))
}

// connect + auth + cwd dry-run for the FTP target. used by the
// `test_upload_connection` command so users can validate credentials
// without doing an actual capture upload. logs out cleanly on every exit
// path; never writes anything to the remote.
pub fn test_connection_ftp(target: &FtpTarget) -> Result<Vec<TestStep>> {
    use suppaftp::FtpStream;
    let mut steps: Vec<TestStep> = Vec::new();

    if let Err(e) = validate_host(&target.host) {
        steps.push(TestStep::fail("validate-host", e.to_string()));
        return Ok(steps);
    }
    steps.push(TestStep::ok("validate-host", target.host.clone()));

    if let Err(e) = validate_remote_dir(&target.remote_dir) {
        steps.push(TestStep::fail("validate-remote-dir", e.to_string()));
        return Ok(steps);
    }

    if let Err(e) = validate_resolved_host(&target.host, target.port.max(1)) {
        steps.push(TestStep::fail("resolve-host", e.to_string()));
        return Ok(steps);
    }
    steps.push(TestStep::ok(
        "resolve-host",
        format!("{}:{}", target.host, target.port.max(1)),
    ));

    if target.use_tls {
        steps.push(TestStep::fail(
            "tls-mode",
            "FTPS not yet implemented; disable use_tls or use SFTP".into(),
        ));
        return Ok(steps);
    }

    let address = format!("{}:{}", target.host, target.port.max(1));
    let mut stream = match FtpStream::connect(&address) {
        Ok(s) => s,
        Err(e) => {
            steps.push(TestStep::fail("connect", e.to_string()));
            return Ok(steps);
        }
    };
    steps.push(TestStep::ok("connect", address));

    if let Err(e) = stream.login(&target.username, &target.password) {
        let _ = stream.quit();
        steps.push(TestStep::fail("login", e.to_string()));
        return Ok(steps);
    }
    steps.push(TestStep::ok("login", target.username.clone()));

    if !target.remote_dir.is_empty() {
        if let Err(e) = stream.cwd(&target.remote_dir) {
            let _ = stream.quit();
            steps.push(TestStep::fail("cwd", e.to_string()));
            return Ok(steps);
        }
        steps.push(TestStep::ok("cwd", target.remote_dir.clone()));
    }

    let _ = stream.quit();
    Ok(steps)
}

#[cfg(feature = "sftp")]
pub fn test_connection_sftp(target: &SftpTarget) -> Result<Vec<TestStep>> {
    use russh::client;
    use russh::keys::HashAlg;
    use russh_sftp::client::SftpSession;
    use std::sync::{Arc, Mutex};

    let mut steps: Vec<TestStep> = Vec::new();

    if let Err(e) = validate_host(&target.host) {
        steps.push(TestStep::fail("validate-host", e.to_string()));
        return Ok(steps);
    }
    steps.push(TestStep::ok("validate-host", target.host.clone()));

    if let Err(e) = validate_remote_dir(&target.remote_dir) {
        steps.push(TestStep::fail("validate-remote-dir", e.to_string()));
        return Ok(steps);
    }

    if let Err(e) = validate_resolved_host(&target.host, target.port.max(1)) {
        steps.push(TestStep::fail("resolve-host", e.to_string()));
        return Ok(steps);
    }
    steps.push(TestStep::ok(
        "resolve-host",
        format!("{}:{}", target.host, target.port.max(1)),
    ));

    let known_hosts_path = match known_hosts::KnownHosts::default_path() {
        Some(p) => p,
        None => {
            steps.push(TestStep::fail(
                "known-hosts",
                "can't resolve config dir for ssh_known_hosts.toml".into(),
            ));
            return Ok(steps);
        }
    };

    let host = target.host.clone();
    let port = target.port.max(1);
    let username = target.username.clone();
    let password = target.password.clone();
    let key_path = target.private_key_path.clone();
    let key_pass = target.private_key_passphrase.clone();
    let remote_dir = target.remote_dir.clone();
    let host_port = known_hosts::host_key(&host, port);
    let mismatch_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let mismatch_for_handler = Arc::clone(&mismatch_error);

    struct VerifyHostKey {
        host_port: String,
        known_hosts_path: std::path::PathBuf,
        mismatch_error: Arc<Mutex<Option<String>>>,
    }

    impl client::Handler for VerifyHostKey {
        type Error = russh::Error;
        async fn check_server_key(
            &mut self,
            key: &russh::keys::ssh_key::PublicKey,
        ) -> std::result::Result<bool, Self::Error> {
            let fp = key.fingerprint(HashAlg::Sha256).to_string();
            let mut store = known_hosts::KnownHosts::load(&self.known_hosts_path);
            match store.lookup(&self.host_port) {
                Some(entry) if entry.fingerprint == fp => Ok(true),
                Some(entry) => {
                    *self.mismatch_error.lock().unwrap() = Some(format!(
                        "stored {}, server now offering {}",
                        entry.fingerprint, fp
                    ));
                    Ok(false)
                }
                None => {
                    store.insert(self.host_port.clone(), fp.clone());
                    if let Err(e) = store.save(&self.known_hosts_path) {
                        tracing::warn!("ssh_known_hosts save (test) failed: {e}");
                    }
                    Ok(true)
                }
            }
        }
    }

    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(r) => r,
        Err(e) => {
            steps.push(TestStep::fail("runtime", e.to_string()));
            return Ok(steps);
        }
    };

    let result = runtime.block_on(async move {
        let handler = VerifyHostKey {
            host_port: host_port.clone(),
            known_hosts_path: known_hosts_path.clone(),
            mismatch_error: mismatch_for_handler,
        };
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(UPLOAD_TIMEOUT_SECS)),
            ..Default::default()
        });
        let mut session = client::connect(config, (host.as_str(), port), handler)
            .await
            .map_err(|e| format!("{}", e))?;
        steps.push(TestStep::ok("connect", format!("{}:{}", host, port)));

        let mut auth_ok = false;
        if !key_path.is_empty() {
            match load_private_key(&key_path, &key_pass) {
                Ok(pk) => {
                    let pkwha = russh::keys::key::PrivateKeyWithHashAlg::new(
                        std::sync::Arc::new(pk),
                        None,
                    );
                    match session.authenticate_publickey(&username, pkwha).await {
                        Ok(r) if r.success() => {
                            steps.push(TestStep::ok("auth-publickey", key_path.clone()));
                            auth_ok = true;
                        }
                        Ok(_) => steps.push(TestStep::fail(
                            "auth-publickey",
                            "server rejected the key (not in authorized_keys?)".into(),
                        )),
                        Err(e) => steps.push(TestStep::fail("auth-publickey", e.to_string())),
                    }
                }
                Err(e) => steps.push(TestStep::fail("auth-publickey", e.to_string())),
            }
        }
        if !auth_ok && !password.is_empty() {
            match session.authenticate_password(&username, &password).await {
                Ok(r) if r.success() => {
                    steps.push(TestStep::ok("auth-password", username.clone()));
                    auth_ok = true;
                }
                Ok(_) => steps.push(TestStep::fail(
                    "auth-password",
                    "server rejected the password".into(),
                )),
                Err(e) => steps.push(TestStep::fail("auth-password", e.to_string())),
            }
        }
        if !auth_ok {
            return Err("no auth method succeeded".to_string());
        }

        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| format!("channel: {e}"))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| format!("sftp subsystem: {e}"))?;
        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| format!("sftp session: {e}"))?;
        steps.push(TestStep::ok("sftp-subsystem", "opened".into()));

        let probe_path = if remote_dir.is_empty() { "." } else { remote_dir.as_str() };
        match sftp.read_dir(probe_path).await {
            Ok(_) => {
                steps.push(TestStep::ok(
                    "read-remote-dir",
                    format!("{} listed", probe_path),
                ));
            }
            Err(e) => steps.push(TestStep::fail("read-remote-dir", e.to_string())),
        }

        Ok::<Vec<TestStep>, String>(steps)
    });

    if let Some(msg) = mismatch_error.lock().unwrap().take() {
        return Ok(vec![TestStep::fail(
            "host-key-mismatch",
            format!(
                "{msg} — forget the host in Settings → SSH known hosts and reconnect"
            ),
        )]);
    }

    match result {
        Ok(steps) => Ok(steps),
        Err(e) => Err(anyhow!("{e}")),
    }
}

#[cfg(not(feature = "sftp"))]
pub fn test_connection_sftp(_target: &SftpTarget) -> Result<Vec<TestStep>> {
    Err(anyhow!(
        "SFTP support not compiled in — rebuild with --features sftp"
    ))
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TestStep {
    pub step: String,
    pub ok: bool,
    pub detail: String,
}

impl TestStep {
    fn ok(step: &str, detail: String) -> Self {
        Self {
            step: step.to_string(),
            ok: true,
            detail,
        }
    }
    fn fail(step: &str, detail: String) -> Self {
        Self {
            step: step.to_string(),
            ok: false,
            detail,
        }
    }
}

// dry-run probe for Imgur: hits api.imgur.com/3/credits with the configured
// Client-ID. 200 = creds work and rate-limit is reported in the detail string.
// 401/403 = bad client-id. anything else = the API itself is unhappy.
pub fn test_connection_imgur(client_id: &str) -> Result<Vec<TestStep>> {
    let mut steps: Vec<TestStep> = Vec::new();
    let effective_cid = if client_id.trim().is_empty() {
        steps.push(TestStep::ok("client-id", "(shared bot key)".into()));
        "546c25a59c58ad7"
    } else {
        steps.push(TestStep::ok("client-id", "(custom)".into()));
        client_id.trim()
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("capscr/1.0")
        .build()
        .map_err(|e| anyhow!("HTTP client init failed: {e}"))?;

    let resp = match client
        .get("https://api.imgur.com/3/credits")
        .header("Authorization", format!("Client-ID {}", effective_cid))
        .send()
    {
        Ok(r) => r,
        Err(e) => {
            steps.push(TestStep::fail("request", e.to_string()));
            return Ok(steps);
        }
    };
    let status = resp.status();
    let body = resp.text().unwrap_or_default();
    if status.is_success() {
        // try to surface the rate-limit fields without pulling serde_json
        // — quick string carving is fine for an opportunistic probe
        let snippet = body
            .chars()
            .take(200)
            .collect::<String>()
            .replace('\n', " ");
        steps.push(TestStep::ok("api-credits", snippet));
    } else if status.as_u16() == 401 || status.as_u16() == 403 {
        steps.push(TestStep::fail(
            "api-credits",
            format!("{} — client-id rejected", status),
        ));
    } else {
        steps.push(TestStep::fail(
            "api-credits",
            format!("HTTP {} — {}", status, body.chars().take(200).collect::<String>()),
        ));
    }
    Ok(steps)
}

// dry-run probe for Custom HTTP: sends an OPTIONS request to the configured
// URL. 2xx/3xx/405 = endpoint exists and is reachable. anything else = the
// configured URL is wrong, the host is down, or the SSRF guard rejected it.
pub fn test_connection_custom(uploader: &CustomUploader) -> Result<Vec<TestStep>> {
    let mut steps: Vec<TestStep> = Vec::new();
    let url = uploader.request_url.trim();
    if url.is_empty() {
        steps.push(TestStep::fail("url", "post url is empty".into()));
        return Ok(steps);
    }
    steps.push(TestStep::ok("url", url.into()));

    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(e) => {
            steps.push(TestStep::fail("parse-url", e.to_string()));
            return Ok(steps);
        }
    };
    if parsed.scheme() != "https" {
        steps.push(TestStep::fail(
            "scheme",
            "https only — plain http is rejected by the uploader".into(),
        ));
        return Ok(steps);
    }
    steps.push(TestStep::ok("scheme", "https".into()));

    if let Some(host) = parsed.host_str() {
        if let Err(e) = validate_host(host) {
            steps.push(TestStep::fail("validate-host", e.to_string()));
            return Ok(steps);
        }
        let port = parsed.port_or_known_default().unwrap_or(443);
        if let Err(e) = validate_resolved_host(host, port) {
            steps.push(TestStep::fail("resolve-host", e.to_string()));
            return Ok(steps);
        }
        steps.push(TestStep::ok("resolve-host", format!("{}:{}", host, port)));
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("capscr/1.0")
        .build()
        .map_err(|e| anyhow!("HTTP client init failed: {e}"))?;

    let resp = match client.request(reqwest::Method::OPTIONS, url).send() {
        Ok(r) => r,
        Err(e) => {
            steps.push(TestStep::fail("options-request", e.to_string()));
            return Ok(steps);
        }
    };
    let status = resp.status();
    // OPTIONS isn't universally supported. treat 2xx, 3xx, and 405
    // (Method Not Allowed — server is reachable but doesn't speak OPTIONS) as
    // OK; anything else means we couldn't reach a working endpoint
    let ok = status.is_success() || status.is_redirection() || status.as_u16() == 405;
    if ok {
        steps.push(TestStep::ok(
            "options-request",
            format!("HTTP {} — endpoint reachable", status),
        ));
    } else {
        let body = resp.text().unwrap_or_default();
        steps.push(TestStep::fail(
            "options-request",
            format!("HTTP {} — {}", status, body.chars().take(200).collect::<String>()),
        ));
    }
    Ok(steps)
}

pub fn upload_ftp(data: &[u8], file_name: &str, target: &FtpTarget) -> Result<UploadResult> {
    use std::io::Cursor;
    use suppaftp::FtpStream;

    validate_host(&target.host)?;
    validate_remote_dir(&target.remote_dir)?;
    validate_resolved_host(&target.host, target.port.max(1))?;
    if target.use_tls {
        return Err(anyhow!("FTPS not yet implemented; disable use_tls or use SFTP"));
    }

    // sanitize and uniquify the remote filename so callers can't smuggle path
    // traversal and so two captures at the same second don't collide.
    let safe = sanitize_remote_filename(file_name);
    let filename = uniquify_remote_filename(&safe);
    let address = format!("{}:{}", target.host, target.port.max(1));

    let mut stream = FtpStream::connect(&address)
        .map_err(|e| anyhow!("FTP connect to {} failed: {}", address, e))?;

    // helper to log out and tear down the socket no matter which step below
    // failed — without this the connection lingered until the OS GC'd it,
    // which on some servers blocked the next upload while the slot expired.
    let close_quietly = |mut s: FtpStream| {
        let _ = s.quit();
    };
    let with_cleanup = |res: Result<UploadResult>, s: FtpStream, partial: Option<&str>| {
        if res.is_err() {
            if let Some(name) = partial {
                // best-effort: remove the half-written remote file so the
                // server doesn't accumulate corrupt artefacts from retries.
                let mut s = s;
                let _ = s.rm(name);
                close_quietly(s);
            } else {
                close_quietly(s);
            }
        } else {
            close_quietly(s);
        }
        res
    };

    if let Err(e) = stream.login(&target.username, &target.password) {
        return with_cleanup(
            Err(anyhow!("FTP login failed: {}", e)),
            stream,
            None,
        );
    }

    if !target.remote_dir.is_empty() {
        if let Err(e) = stream.cwd(&target.remote_dir) {
            return with_cleanup(
                Err(anyhow!("FTP cwd to '{}' failed: {}", target.remote_dir, e)),
                stream,
                None,
            );
        }
    }

    let mut reader = Cursor::new(data.to_vec());
    if let Err(e) = stream.put_file(&filename, &mut reader) {
        return with_cleanup(
            Err(anyhow!("FTP put_file failed: {}", e)),
            stream,
            Some(&filename),
        );
    }

    let url = match build_url(&target.public_url_template, &filename) {
        Ok(u) => u,
        Err(e) => {
            close_quietly(stream);
            return Err(e);
        }
    };
    let result = UploadResult {
        url,
        delete_url: None,
    };
    close_quietly(stream);
    Ok(result)
}

#[cfg(feature = "sftp")]
pub fn upload_sftp(data: &[u8], file_name: &str, target: &SftpTarget) -> Result<UploadResult> {
    use russh::client;
    use russh::keys::HashAlg;
    use russh_sftp::client::SftpSession;
    use russh_sftp::protocol::OpenFlags;
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncWriteExt;

    validate_host(&target.host)?;
    validate_remote_dir(&target.remote_dir)?;
    validate_resolved_host(&target.host, target.port.max(1))?;

    let safe = sanitize_remote_filename(file_name);
    let filename = uniquify_remote_filename(&safe);
    let host = target.host.clone();
    let port = target.port.max(1);
    let username = target.username.clone();
    let password = target.password.clone();
    let remote_dir = target.remote_dir.clone();
    let url_template = target.public_url_template.clone();
    let target_key_path = target.private_key_path.clone();
    let target_key_passphrase = target.private_key_passphrase.clone();
    let data_owned = data.to_vec();

    // host-key TOFU. First connect to a host:port stores the SHA256 fingerprint;
    // subsequent connects compare against the store. Mismatch aborts the
    // upload — the user must explicitly forget the stored fingerprint via the
    // hub UI before capscr will re-trust a new key (legitimate rotation or MITM
    // both look the same at the wire level).
    let known_hosts_path = known_hosts::KnownHosts::default_path()
        .ok_or_else(|| anyhow!("can't resolve config dir for ssh_known_hosts.toml"))?;
    // mismatch_error captures the structured rejection reason inside the
    // async handler so we can surface a friendly message after block_on returns
    let mismatch_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    struct VerifyHostKey {
        host_port: String,
        known_hosts_path: std::path::PathBuf,
        mismatch_error: Arc<Mutex<Option<String>>>,
    }

    impl client::Handler for VerifyHostKey {
        type Error = russh::Error;
        async fn check_server_key(
            &mut self,
            key: &russh::keys::ssh_key::PublicKey,
        ) -> std::result::Result<bool, Self::Error> {
            let fp = match key.fingerprint(HashAlg::Sha256).to_string() {
                s if s.is_empty() => "SHA256:<empty>".to_string(),
                s => s,
            };
            let mut store = known_hosts::KnownHosts::load(&self.known_hosts_path);
            match store.lookup(&self.host_port) {
                Some(entry) if entry.fingerprint == fp => Ok(true),
                Some(entry) => {
                    let stored = entry.fingerprint.clone();
                    *self.mismatch_error.lock().unwrap() = Some(format!(
                        "SSH host key mismatch for {} — stored {}, server now offering {}. \
                         If this is intentional (e.g. key rotation), forget the host in \
                         Settings → SSH known hosts and reconnect.",
                        self.host_port, stored, fp
                    ));
                    Ok(false)
                }
                None => {
                    store.insert(self.host_port.clone(), fp.clone());
                    if let Err(e) = store.save(&self.known_hosts_path) {
                        tracing::warn!("ssh_known_hosts save failed: {e}");
                    }
                    tracing::info!(
                        "ssh host trust-on-first-use: {} -> {}",
                        self.host_port,
                        fp
                    );
                    Ok(true)
                }
            }
        }
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| anyhow!("SFTP runtime init failed: {e}"))?;

    let host_port = known_hosts::host_key(&host, port);
    let upload_filename = filename.clone();
    let mismatch_error_for_handler = Arc::clone(&mismatch_error);
    let connect_result: Result<()> = runtime.block_on(async move {
        let handler = VerifyHostKey {
            host_port: host_port.clone(),
            known_hosts_path: known_hosts_path.clone(),
            mismatch_error: mismatch_error_for_handler,
        };
        let config = Arc::new(client::Config {
            inactivity_timeout: Some(std::time::Duration::from_secs(UPLOAD_TIMEOUT_SECS)),
            ..Default::default()
        });
        let mut session = client::connect(config, (host.as_str(), port), handler)
            .await
            .map_err(|e| anyhow!("SFTP connect to {}:{} failed: {}", host, port, e))?;

        // try public-key auth first when a key path is configured; fall
        // through to password only on key-auth failure with a password set.
        // key path + password BOTH empty errors out below.
        let key_path = target_key_path.clone();
        let key_pass = target_key_passphrase.clone();
        let mut auth_ok = false;
        let mut auth_diag: Vec<String> = Vec::new();
        if !key_path.is_empty() {
            match load_private_key(&key_path, &key_pass) {
                Ok(pk) => {
                    let pkwha = russh::keys::key::PrivateKeyWithHashAlg::new(
                        std::sync::Arc::new(pk),
                        None,
                    );
                    match session.authenticate_publickey(&username, pkwha).await {
                        Ok(r) if r.success() => auth_ok = true,
                        Ok(_) => auth_diag.push(
                            "publickey: server rejected the key (not in authorized_keys?)".into(),
                        ),
                        Err(e) => auth_diag.push(format!("publickey: {e}")),
                    }
                }
                Err(e) => auth_diag.push(format!("publickey: {e}")),
            }
        }
        if !auth_ok && !password.is_empty() {
            match session.authenticate_password(&username, &password).await {
                Ok(r) if r.success() => auth_ok = true,
                Ok(_) => auth_diag.push("password: server rejected the password".into()),
                Err(e) => auth_diag.push(format!("password: {e}")),
            }
        }
        if !auth_ok {
            let summary = if auth_diag.is_empty() {
                "no authentication method configured (set a private key or password)".to_string()
            } else {
                auth_diag.join("; ")
            };
            return Err(anyhow!("SFTP authentication failed — {summary}"));
        }

        let channel = session
            .channel_open_session()
            .await
            .map_err(|e| anyhow!("SFTP channel_open_session failed: {e}"))?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .map_err(|e| anyhow!("SFTP request_subsystem failed: {e}"))?;

        let sftp = SftpSession::new(channel.into_stream())
            .await
            .map_err(|e| anyhow!("SFTP session init failed: {e}"))?;

        let target_path = if remote_dir.is_empty() {
            upload_filename.clone()
        } else {
            let trimmed = remote_dir.trim_end_matches('/');
            format!("{}/{}", trimmed, upload_filename)
        };

        let mut file = sftp
            .open_with_flags(
                &target_path,
                OpenFlags::CREATE | OpenFlags::TRUNCATE | OpenFlags::WRITE,
            )
            .await
            .map_err(|e| anyhow!("SFTP open '{}' for write failed: {e}", target_path))?;

        if let Err(e) = file.write_all(&data_owned).await {
            // best-effort cleanup so a partial upload doesn't leave a 0-byte
            // or truncated file on the server.
            let _ = file.shutdown().await;
            let _ = sftp.remove_file(&target_path).await;
            return Err(anyhow!("SFTP write_all failed: {e}"));
        }
        if let Err(e) = file.shutdown().await {
            return Err(anyhow!("SFTP file close failed: {e}"));
        }

        Ok(())
    });

    // host-key mismatch surfaces as a connection-aborted-by-handler russh
    // error; prefer the structured message captured by VerifyHostKey so the
    // user knows it's a fingerprint problem and not a network blip.
    if let Some(msg) = mismatch_error.lock().unwrap().take() {
        return Err(anyhow!("{}", msg));
    }
    connect_result?;

    let url = build_url(&url_template, &filename)?;
    Ok(UploadResult { url, delete_url: None })
}

#[cfg(not(feature = "sftp"))]
pub fn upload_sftp(_data: &[u8], _file_name: &str, _target: &SftpTarget) -> Result<UploadResult> {
    Err(anyhow!(
        "SFTP support not compiled in — rebuild with --features sftp (or restore the default feature set)"
    ))
}

#[cfg(feature = "sftp")]
fn load_private_key(
    path: &str,
    passphrase: &str,
) -> Result<russh::keys::ssh_key::PrivateKey> {
    use russh::keys::ssh_key::PrivateKey;

    let path_buf = std::path::PathBuf::from(path);
    let canonical = path_buf
        .canonicalize()
        .map_err(|e| anyhow!("can't canonicalize SSH key path '{}': {e}", path))?;
    // canonicalize collapses any '..' before we read; this rejects nothing
    // operationally (the user picks the file) but means logs always show the
    // real on-disk location instead of whatever they typed.
    let body = std::fs::read(&canonical)
        .map_err(|e| anyhow!("can't read SSH key from {:?}: {e}", canonical))?;
    let key = PrivateKey::from_openssh(&body)
        .map_err(|e| anyhow!("SSH key parse failed (expected OpenSSH PEM): {e}"))?;
    if key.is_encrypted() {
        if passphrase.is_empty() {
            return Err(anyhow!(
                "SSH key at {:?} is passphrase-protected — set the passphrase in Destinations",
                canonical
            ));
        }
        key.decrypt(passphrase.as_bytes())
            .map_err(|e| anyhow!("SSH key decrypt failed (bad passphrase?): {e}"))
    } else {
        Ok(key)
    }
}

fn sanitize_remote_filename(name: &str) -> String {
    let trimmed = std::path::Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload");
    let cleaned: String = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .take(120)
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "upload".to_string()
    } else {
        cleaned
    }
}

fn uniquify_remote_filename(name: &str) -> String {
    let path = std::path::Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("upload");
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin");
    let now = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let id = &uuid::Uuid::new_v4().as_simple().to_string()[..8];
    format!("{}_{}_{}.{}", stem, now, id, ext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_classifier_retries_network_failures() {
        assert!(is_transient_upload_error(&anyhow!("operation timed out")));
        assert!(is_transient_upload_error(&anyhow!("connection reset by peer")));
        assert!(is_transient_upload_error(&anyhow!("status code: 503")));
        assert!(is_transient_upload_error(&anyhow!("tls handshake failed")));
    }

    #[test]
    fn transient_classifier_skips_real_failures() {
        assert!(!is_transient_upload_error(&anyhow!("401 unauthorized")));
        assert!(!is_transient_upload_error(&anyhow!("imgur error: Image too big")));
        assert!(!is_transient_upload_error(&anyhow!("invalid JSON in response")));
    }

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
        let result = uploader.upload_custom(&[0u8; 100], "image/png", "test.png", &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_shared_uploader_singleton() {
        let first = shared_uploader().unwrap() as *const ImageUploader;
        let second = shared_uploader().unwrap() as *const ImageUploader;
        assert_eq!(first, second);
    }

    #[test]
    fn ftp_rejects_loopback() {
        let err = validate_resolved_host("127.0.0.1", 21).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Private")
                || msg.contains("Host not allowed")
                || msg.contains("private"),
            "expected loopback rejection, got: {msg}"
        );
    }

    #[test]
    fn ftp_rejects_rfc1918_literal() {
        let err = validate_resolved_host("10.0.0.5", 21).unwrap_err();
        assert!(err.to_string().contains("Private"));
    }

    #[test]
    fn ftp_rejects_aws_metadata() {
        let err = validate_resolved_host("169.254.169.254", 21).unwrap_err();
        assert!(err.to_string().contains("metadata"));
    }

    #[test]
    fn ftp_rejects_localhost_label() {
        let err = validate_resolved_host("localhost", 21).unwrap_err();
        assert!(err.to_string().contains("not allowed"));
    }
}
