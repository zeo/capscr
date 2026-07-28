#![allow(dead_code)]

pub mod known_hosts;

use anyhow::{anyhow, Result};
use image::RgbaImage;
use std::io::{Cursor, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
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
    S3(S3Target),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct S3Target {
    pub bucket: String,
    pub region: String,
    pub endpoint: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    pub public_url_template: String,
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

// hostname connections use this resolver at connect time
// parsed-url checks cover literal addresses that bypass dns resolution
pub(crate) struct ValidatingResolver;

/// a reqwest DNS resolver that refuses any host resolving to a private/internal
/// address. share it with other outbound clients (the marketplace) so they get
/// the same SSRF guard the uploader has.
pub(crate) fn ssrf_validating_resolver() -> Arc<ValidatingResolver> {
    Arc::new(ValidatingResolver)
}

pub(crate) fn validate_outbound_url(raw: &str) -> Result<url::Url> {
    let parsed = url::Url::parse(raw).map_err(|_| anyhow!("Invalid URL format"))?;
    if parsed.scheme() != "https" {
        return Err(anyhow!("only https URLs are allowed"));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(anyhow!("URL credentials are not allowed"));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("URL has no host"))?;
    if host.is_empty() || host.len() > 253 {
        return Err(anyhow!("Invalid hostname length"));
    }

    let host_lower = host.to_lowercase();
    let blocked_hosts = [
        "localhost",
        "metadata.google.internal",
        "metadata.google.com",
        "metadata",
        "instance-data",
        "burpcollaborator.net",
        "oastify.com",
    ];
    if blocked_hosts
        .iter()
        .any(|blocked| host_lower == *blocked || host_lower.ends_with(&format!(".{blocked}")))
    {
        return Err(anyhow!("Host not allowed"));
    }

    let literal = host_lower.trim_matches(|character| character == '[' || character == ']');
    if literal
        .parse::<IpAddr>()
        .is_ok_and(ImageUploader::is_private_ip)
    {
        return Err(anyhow!("Private IP ranges are blocked"));
    }

    let port = parsed.port().unwrap_or(443);
    let blocked_ports = [0, 22, 23, 25, 110, 143, 445, 3306, 3389, 5432, 6379, 27017];
    if blocked_ports.contains(&port) {
        return Err(anyhow!("Port not allowed"));
    }
    Ok(parsed)
}

// resolve a hostname and keep only public addresses, rejecting the whole lookup
// if any resolved address is private/internal (a rebinding resolver mixing one
// public and one private answer must not slip the private one through)
fn resolve_public_addrs(host: &str) -> std::result::Result<Vec<SocketAddr>, String> {
    // getaddrinfo needs a port; the value is irrelevant to resolution
    let addrs: Vec<SocketAddr> = (host, 0u16)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .collect();
    if addrs.is_empty() {
        return Err("hostname did not resolve".into());
    }
    if let Some(bad) = addrs.iter().find(|a| ImageUploader::is_private_ip(a.ip())) {
        return Err(format!("host resolves to blocked address {}", bad.ip()));
    }
    Ok(addrs)
}

impl reqwest::dns::Resolve for ValidatingResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            match resolve_public_addrs(&host) {
                Ok(addrs) => {
                    let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
                    Ok(iter)
                }
                Err(e) => Err(e.into()),
            }
        })
    }
}

impl ImageUploader {
    pub fn new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(UPLOAD_TIMEOUT_SECS))
            .user_agent("capscr/1.0")
            .https_only(true)
            .no_proxy()
            .dns_resolver(Arc::new(ValidatingResolver))
            // a cheap first pass on each redirect target; the dns resolver above
            // is what actually stops a redirect to a private/internal IP (SSRF)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= MAX_REDIRECTS {
                    return attempt.error("too many redirects");
                }
                if validate_outbound_url(attempt.url().as_str()).is_err() {
                    return attempt.error("redirect target blocked");
                }
                attempt.follow()
            }))
            .build()?;
        Ok(Self { client })
    }

    pub(crate) fn is_private_ip(ip: IpAddr) -> bool {
        match ip {
            IpAddr::V4(ipv4) => {
                let octets = ipv4.octets();
                ipv4.is_loopback()
                    || ipv4.is_private()
                    || ipv4.is_link_local()
                    || ipv4.is_broadcast()
                    || ipv4.is_documentation()
                    || ipv4.is_multicast()
                    || ipv4.is_unspecified()
                    || octets[0] == 0
                    || octets[0] == 100 && (octets[1] & 0xC0) == 64
                    || octets[0] == 192 && octets[1] == 0 && octets[2] == 0
                    || octets[0] == 192 && octets[1] == 88 && octets[2] == 99
                    || octets[0] == 198 && (octets[1] & 0xFE) == 18
                    || octets[0] >= 240
            }
            IpAddr::V6(ipv6) => {
                if ipv6.is_loopback() || ipv6.is_unspecified() || ipv6.is_multicast() {
                    return true;
                }
                let o = ipv6.octets();
                // ULA: fc00::/7
                if o[0] & 0xFE == 0xFC {
                    return true;
                }
                // link-local: fe80::/10
                if o[0] == 0xFE && (o[1] & 0xC0) == 0x80 {
                    return true;
                }
                // deprecated site-local: fec0::/10
                if o[0] == 0xFE && (o[1] & 0xC0) == 0xC0 {
                    return true;
                }
                // benchmarking and documentation ranges
                if (o[..6] == [0x20, 0x01, 0x00, 0x02, 0x00, 0x00])
                    || (o[..4] == [0x20, 0x01, 0x0D, 0xB8])
                    || (o[0] == 0x3F && o[1] == 0xFF && (o[2] & 0xF0) == 0)
                {
                    return true;
                }
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
        let parsed = validate_outbound_url(url)?;
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("URL has no host"))?;
        let port = parsed.port().unwrap_or(443);
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
                UploadService::Imgur => self.upload_imgur(data, mime, file_name, "546c25a59c58ad7"),
                UploadService::ImgurWithClientId(cid) => {
                    self.upload_imgur(data, mime, file_name, cid)
                }
                UploadService::Custom(config) => self.upload_custom(data, mime, file_name, config),
                UploadService::Ftp(target) => upload_ftp(data, file_name, target),
                UploadService::Sftp(target) => upload_sftp(data, file_name, target),
                UploadService::S3(target) => upload_s3(data, file_name, target),
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
        return Err(anyhow!(
            "public_url_template must start with https:// or http://"
        ));
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
    if !host
        .chars()
        .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == ':')
    {
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

/// reject hosts that resolve to private / loopback / cloud-metadata IP ranges,
/// and return the vetted socket addresses. the caller must connect to these
/// exact addresses rather than re-resolving the hostname: a rebinding resolver
/// can hand a public IP to a validation-only lookup and a private one to the
/// real connect, so pinning the checked address is what actually closes the gap.
pub(crate) fn validate_resolved_host(host: &str, port: u16) -> Result<Vec<SocketAddr>> {
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
    let resolved: Vec<SocketAddr> = host_with_port
        .to_socket_addrs()
        .map_err(|e| anyhow!("Could not resolve hostname: {}", e))?
        .collect();
    if resolved.is_empty() {
        return Err(anyhow!("Could not resolve hostname"));
    }
    for addr in &resolved {
        if ImageUploader::is_private_ip(addr.ip()) {
            return Err(anyhow!("Host resolves to private/internal IP"));
        }
    }

    Ok(resolved)
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

pub fn test_connection_ftp(_target: &FtpTarget) -> Result<Vec<TestStep>> {
    Ok(vec![TestStep::fail(
        "transport",
        "plain FTP is disabled; use SFTP".into(),
    )])
}

#[cfg(feature = "sftp")]
fn accept_sftp_host_key(
    path: &std::path::Path,
    host_port: &str,
    fingerprint: &str,
    rejection: &Arc<std::sync::Mutex<Option<String>>>,
) -> bool {
    if fingerprint.is_empty() {
        *rejection.lock().unwrap() = Some(format!(
            "SSH server {host_port} offered an empty host fingerprint"
        ));
        return false;
    }
    match known_hosts::verify_or_trust(path, host_port, fingerprint) {
        Ok(known_hosts::TrustDecision::Trusted) => true,
        Ok(known_hosts::TrustDecision::FirstSeen) => {
            tracing::info!("ssh host trust-on-first-use: {host_port} -> {fingerprint}");
            true
        }
        Ok(known_hosts::TrustDecision::Mismatch { stored }) => {
            *rejection.lock().unwrap() = Some(format!(
                "SSH host key mismatch for {host_port}: stored {stored}, server now offers \
                 {fingerprint}. If this is intentional, forget the host in Settings > SSH \
                 known hosts and reconnect."
            ));
            false
        }
        Err(error) => {
            *rejection.lock().unwrap() = Some(format!(
                "SSH host trust check failed for {host_port}: {error:#}"
            ));
            false
        }
    }
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

    let addrs = match validate_resolved_host(&target.host, target.port.max(1)) {
        Ok(a) => a,
        Err(e) => {
            steps.push(TestStep::fail("resolve-host", e.to_string()));
            return Ok(steps);
        }
    };
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
            Ok(accept_sftp_host_key(
                &self.known_hosts_path,
                &self.host_port,
                &fp,
                &self.mismatch_error,
            ))
        }
    }

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
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
        // dial the vetted address, not the hostname; the host key is still
        // verified against the hostname-keyed known_hosts entry
        let mut session = client::connect(config, &addrs[..], handler)
            .await
            .map_err(|e| format!("{}", e))?;
        steps.push(TestStep::ok("connect", format!("{}:{}", host, port)));

        let mut auth_ok = false;
        if !key_path.is_empty() {
            match load_private_key(&key_path, &key_pass) {
                Ok(pk) => {
                    let pkwha =
                        russh::keys::key::PrivateKeyWithHashAlg::new(std::sync::Arc::new(pk), None);
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

        let probe_path = if remote_dir.is_empty() {
            "."
        } else {
            remote_dir.as_str()
        };
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
        return Ok(vec![TestStep::fail("host-key", msg)]);
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
            format!(
                "HTTP {} — {}",
                status,
                body.chars().take(200).collect::<String>()
            ),
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

    if let Err(error) = ImageUploader::validate_url_security(url) {
        steps.push(TestStep::fail("validate-url", error.to_string()));
        return Ok(steps);
    }
    if let Some(host) = parsed.host_str() {
        steps.push(TestStep::ok(
            "resolve-host",
            format!("{}:{}", host, parsed.port_or_known_default().unwrap_or(443)),
        ));
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("capscr/1.0")
        .https_only(true)
        .no_proxy()
        .dns_resolver(ssrf_validating_resolver())
        .redirect(reqwest::redirect::Policy::none())
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
            format!(
                "HTTP {} — {}",
                status,
                body.chars().take(200).collect::<String>()
            ),
        ));
    }
    Ok(steps)
}

pub fn upload_ftp(_data: &[u8], _file_name: &str, _target: &FtpTarget) -> Result<UploadResult> {
    Err(anyhow!("plain FTP is disabled; use SFTP"))
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
    let addrs = validate_resolved_host(&target.host, target.port.max(1))?;

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
            let fp = key.fingerprint(HashAlg::Sha256).to_string();
            Ok(accept_sftp_host_key(
                &self.known_hosts_path,
                &self.host_port,
                &fp,
                &self.mismatch_error,
            ))
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
        // dial the vetted address, not the hostname; the host key is still
        // verified against the hostname-keyed known_hosts entry
        let mut session = client::connect(config, &addrs[..], handler)
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
                    let pkwha =
                        russh::keys::key::PrivateKeyWithHashAlg::new(std::sync::Arc::new(pk), None);
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
    Ok(UploadResult {
        url,
        delete_url: None,
    })
}

#[cfg(not(feature = "sftp"))]
pub fn upload_sftp(_data: &[u8], _file_name: &str, _target: &SftpTarget) -> Result<UploadResult> {
    Err(anyhow!(
        "SFTP support not compiled in — rebuild with --features sftp (or restore the default feature set)"
    ))
}

#[cfg(feature = "sftp")]
fn load_private_key(path: &str, passphrase: &str) -> Result<russh::keys::ssh_key::PrivateKey> {
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
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    let now = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let id = &uuid::Uuid::new_v4().as_simple().to_string()[..8];
    format!("{}_{}_{}.{}", stem, now, id, ext)
}

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

fn hmac_sha256(key: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| anyhow!("invalid hmac key: {e}"))?;
    mac.update(data);
    Ok(mac.finalize().into_bytes().to_vec())
}

fn sign_s3_request(
    method: &str,
    request_url: &url::Url,
    region: &str,
    access_key_id: &str,
    secret_access_key: &str,
    payload_sha256: &str,
    date_utc: chrono::DateTime<chrono::Utc>,
) -> Result<String> {
    let date_str = date_utc.format("%Y%m%dT%H%M%SZ").to_string();
    let date_only = date_utc.format("%Y%m%d").to_string();

    let path = request_url.path();
    let query = request_url.query().unwrap_or("");
    let host = request_url
        .host_str()
        .ok_or_else(|| anyhow!("no host in url"))?;
    let host_header = if let Some(port) = request_url.port() {
        format!("{}:{}", host, port)
    } else {
        host.to_string()
    };

    let canonical_headers = format!(
        "host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
        host_header, payload_sha256, date_str
    );
    let signed_headers = "host;x-amz-content-sha256;x-amz-date";

    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method, path, query, canonical_headers, signed_headers, payload_sha256
    );

    let mut hasher = Sha256::new();
    hasher.update(canonical_request.as_bytes());
    let canonical_request_hash = hex::encode(hasher.finalize());

    let credential_scope = format!("{}/{}/s3/aws4_request", date_only, region);
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        date_str, credential_scope, canonical_request_hash
    );

    let k_secret = format!("AWS4{}", secret_access_key);
    let k_date = hmac_sha256(k_secret.as_bytes(), date_only.as_bytes())?;
    let k_region = hmac_sha256(&k_date, region.as_bytes())?;
    let k_service = hmac_sha256(&k_region, b"s3")?;
    let k_signing = hmac_sha256(&k_service, b"aws4_request")?;

    let signature_bytes = hmac_sha256(&k_signing, string_to_sign.as_bytes())?;
    let signature = hex::encode(signature_bytes);

    let auth_header = format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
        access_key_id, credential_scope, signed_headers, signature
    );

    Ok(auth_header)
}

fn validate_s3_target(target: &S3Target) -> Result<()> {
    if target.bucket.is_empty() || target.bucket.len() > 255 {
        return Err(anyhow!("S3 bucket name has an invalid length"));
    }
    let region = target.region.as_bytes();
    if region.is_empty()
        || region.len() > 64
        || !region[0].is_ascii_alphanumeric()
        || !region[region.len() - 1].is_ascii_alphanumeric()
        || !region
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
    {
        return Err(anyhow!("S3 region has an invalid format"));
    }
    if target.endpoint.is_empty() {
        let bucket = target.bucket.as_bytes();
        let valid_labels = target.bucket.split('.').all(|label| {
            let bytes = label.as_bytes();
            !bytes.is_empty()
                && bytes[0].is_ascii_alphanumeric()
                && bytes[bytes.len() - 1].is_ascii_alphanumeric()
                && bytes.iter().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-'
                })
        });
        if bucket.len() < 3
            || bucket.len() > 63
            || !valid_labels
            || target.bucket.parse::<Ipv4Addr>().is_ok()
        {
            return Err(anyhow!("S3 bucket name has an invalid format"));
        }
    }
    Ok(())
}

pub fn upload_s3(data: &[u8], file_name: &str, target: &S3Target) -> Result<UploadResult> {
    validate_s3_target(target)?;
    let safe = sanitize_remote_filename(file_name);
    let filename = uniquify_remote_filename(&safe);

    let request_url_str = if target.endpoint.is_empty() {
        format!(
            "https://{}.s3.{}.amazonaws.com/{}",
            target.bucket, target.region, filename
        )
    } else {
        let mut ep = target.endpoint.clone();
        if !ep.contains("://") {
            ep = format!("https://{}", ep);
        }
        let mut url =
            url::Url::parse(&ep).map_err(|e| anyhow!("invalid custom endpoint URL: {e}"))?;
        {
            let mut path_segments = url
                .path_segments_mut()
                .map_err(|_| anyhow!("cannot modify path of endpoint"))?;
            path_segments.push(&target.bucket);
            path_segments.push(&filename);
        }
        url.to_string()
    };

    let request_url = url::Url::parse(&request_url_str)?;
    validate_outbound_url(request_url.as_str())?;

    let mut hasher = Sha256::new();
    hasher.update(data);
    let payload_sha256 = hex::encode(hasher.finalize());

    let date_utc = chrono::Utc::now();
    let date_str = date_utc.format("%Y%m%dT%H%M%SZ").to_string();

    let auth_header = sign_s3_request(
        "PUT",
        &request_url,
        &target.region,
        &target.access_key_id,
        &target.secret_access_key,
        &payload_sha256,
        date_utc,
    )?;

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(UPLOAD_TIMEOUT_SECS))
        .user_agent("capscr/1.0")
        .https_only(true)
        .no_proxy()
        .dns_resolver(ssrf_validating_resolver())
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let response = client
        .put(request_url.clone())
        .header("Authorization", auth_header)
        .header("x-amz-date", date_str)
        .header("x-amz-content-sha256", payload_sha256)
        .body(data.to_vec())
        .send()
        .map_err(|e| anyhow!("S3 upload request failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let mut err_body = Vec::new();
        response
            .take(MAX_RESPONSE_SIZE as u64 + 1)
            .read_to_end(&mut err_body)?;
        let truncated = err_body.len() > MAX_RESPONSE_SIZE;
        err_body.truncate(MAX_RESPONSE_SIZE);
        let mut err_body = String::from_utf8_lossy(&err_body).into_owned();
        if truncated {
            err_body.push_str("...");
        }
        return Err(anyhow!(
            "S3 upload failed with status {}: {}",
            status,
            err_body
        ));
    }

    let public_url = if !target.public_url_template.is_empty() {
        target.public_url_template.replace("{filename}", &filename)
    } else if target.endpoint.is_empty() {
        format!(
            "https://{}.s3.{}.amazonaws.com/{}",
            target.bucket, target.region, filename
        )
    } else {
        request_url.to_string()
    };

    Ok(UploadResult {
        url: public_url,
        delete_url: None,
    })
}

pub fn test_connection_s3(target: &S3Target) -> Result<Vec<TestStep>> {
    let mut steps = Vec::new();

    if let Err(error) = validate_s3_target(target) {
        steps.push(TestStep::fail("config", error.to_string()));
        return Ok(steps);
    }
    if target.access_key_id.is_empty() {
        steps.push(TestStep::fail("config", "Access Key ID is empty".into()));
        return Ok(steps);
    }
    if target.secret_access_key.is_empty() {
        steps.push(TestStep::fail(
            "config",
            "Secret Access Key is empty".into(),
        ));
        return Ok(steps);
    }
    steps.push(TestStep::ok(
        "config",
        "Configuration parameters valid".into(),
    ));

    let test_data = b"capscr connection test";
    let file_name = "connection_test.txt";

    match upload_s3(test_data, file_name, target) {
        Ok(res) => {
            steps.push(TestStep::ok(
                "upload",
                format!("Uploaded test file successfully! Public URL: {}", res.url),
            ));
        }
        Err(e) => {
            steps.push(TestStep::fail("upload", format!("Upload failed: {e}")));
        }
    }

    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sign_s3_request_validity() {
        let method = "PUT";
        let request_url =
            url::Url::parse("https://my-bucket.s3.us-east-1.amazonaws.com/test_file.png").unwrap();
        let region = "us-east-1";
        let access_key_id = "AKIAIOSFODNN7EXAMPLE";
        let secret_access_key = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
        let payload_sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        let date_utc = chrono::DateTime::parse_from_rfc3339("2013-05-24T15:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let result = sign_s3_request(
            method,
            &request_url,
            region,
            access_key_id,
            secret_access_key,
            payload_sha256,
            date_utc,
        );

        assert!(result.is_ok());
        let auth_header = result.unwrap();
        assert!(auth_header.contains("AWS4-HMAC-SHA256"));
        assert!(auth_header
            .contains("Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request"));
        assert!(auth_header.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
        assert!(auth_header.contains("Signature="));
    }

    #[test]
    fn s3_rejects_cleartext_http_endpoint() {
        // an http:// custom endpoint is refused before any network call so the
        // sigv4 credentials never go out in the clear
        let target = S3Target {
            bucket: "b".into(),
            region: "us-east-1".into(),
            endpoint: "http://minio.local:9000".into(),
            access_key_id: "AK".into(),
            secret_access_key: "SK".into(),
            public_url_template: String::new(),
        };
        let err = upload_s3(b"x", "f.png", &target).unwrap_err();
        assert!(err.to_string().contains("https"), "got: {err}");
    }

    #[test]
    fn s3_rejects_private_literal_endpoints_before_connecting() {
        for endpoint in ["https://127.0.0.1:4443", "https://[::1]:4443"] {
            let target = S3Target {
                bucket: "captures".into(),
                region: "us-east-1".into(),
                endpoint: endpoint.into(),
                access_key_id: "AK".into(),
                secret_access_key: "SK".into(),
                public_url_template: String::new(),
            };
            let error = upload_s3(b"x", "f.png", &target).expect_err(endpoint);
            assert!(error.to_string().contains("Private IP"), "got: {error}");
        }
    }

    #[test]
    fn default_s3_endpoint_requires_dns_safe_bucket_and_region() {
        let mut target = S3Target {
            bucket: "user@127.0.0.1".into(),
            region: "us-east-1".into(),
            endpoint: String::new(),
            access_key_id: "AK".into(),
            secret_access_key: "SK".into(),
            public_url_template: String::new(),
        };
        assert!(validate_s3_target(&target).is_err());
        target.bucket = "valid-captures".into();
        target.region = "us-east-1/../../".into();
        assert!(validate_s3_target(&target).is_err());
    }

    #[test]
    fn outbound_url_validation_rejects_private_literals() {
        for url in [
            "https://0.0.0.1/resource",
            "https://127.0.0.2/resource",
            "https://10.0.0.5/resource",
            "https://192.0.0.1/resource",
            "https://198.18.0.1/resource",
            "https://240.0.0.1/resource",
            "https://[::1]/resource",
            "https://[fc00::1]/resource",
            "https://[fec0::1]/resource",
            "https://[2001:2::1]/resource",
            "https://[2001:db8::1]/resource",
            "https://[3fff::1]/resource",
        ] {
            assert!(validate_outbound_url(url).is_err(), "{url} should be blocked");
        }
    }

    #[test]
    fn transient_classifier_retries_network_failures() {
        assert!(is_transient_upload_error(&anyhow!("operation timed out")));
        assert!(is_transient_upload_error(&anyhow!(
            "connection reset by peer"
        )));
        assert!(is_transient_upload_error(&anyhow!("status code: 503")));
        assert!(is_transient_upload_error(&anyhow!("tls handshake failed")));
    }

    #[test]
    fn transient_classifier_skips_real_failures() {
        assert!(!is_transient_upload_error(&anyhow!("401 unauthorized")));
        assert!(!is_transient_upload_error(&anyhow!(
            "imgur error: Image too big"
        )));
        assert!(!is_transient_upload_error(&anyhow!(
            "invalid JSON in response"
        )));
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
    fn ftp_transport_is_disabled() {
        let target = FtpTarget::default();
        let err = upload_ftp(b"capture", "capture.png", &target).unwrap_err();
        assert!(err.to_string().contains("disabled"));
        assert!(!test_connection_ftp(&target).unwrap()[0].ok);
    }

    #[test]
    fn ftp_rejects_loopback() {
        let err = validate_resolved_host("127.0.0.1", 21).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Private") || msg.contains("Host not allowed") || msg.contains("private"),
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

    #[test]
    fn resolver_rejects_private_literal() {
        // the http/redirect ssrf guard: a name resolving to a private address
        // must be refused before connect
        assert!(resolve_public_addrs("127.0.0.1").is_err());
        assert!(resolve_public_addrs("10.0.0.5").is_err());
        assert!(resolve_public_addrs("localhost").is_err());
    }

    #[test]
    fn resolver_accepts_public_literal() {
        let addrs = resolve_public_addrs("93.184.216.34").expect("public ip should pass");
        assert!(addrs.iter().all(|a| !ImageUploader::is_private_ip(a.ip())));
    }
}
