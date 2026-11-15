//! Paper Plane Mailbox HTTP protocol client.
//!
//! The device uploads one raw PNG at a time and polls for UTF-8 family replies.
//! Networking is intentionally independent of the e-ink UI state machine so it
//! can be exercised on the host before installing anything on the Kobo.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_REPLY_BYTES: u64 = 8 * 1024;
const MAX_REFINED_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_HISTORY_BYTES: u64 = 256 * 1024;
const MAX_CACHE_FILES: usize = 64;
const MAX_CACHE_BYTES: u64 = 96 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryKind {
    Message,
    Refinement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryImageSide {
    Output,
    Input,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub kind: HistoryKind,
    pub id: u64,
    pub created_at: String,
    pub input_available: bool,
    pub has_reply: bool,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage {
    pub items: Vec<HistoryEntry>,
    pub next_cursor: Option<String>,
}

fn invalid_history() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid mailbox history response",
    )
}

fn valid_history_cursor(cursor: &str) -> bool {
    let fields: Vec<_> = cursor.split('|').collect();
    if fields.len() != 3 || !matches!(fields[1], "1" | "2") {
        return false;
    }
    let time = fields[0].as_bytes();
    time.len() == 20
        && time.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
        && positive_id(fields[2]).is_some_and(|id| id <= 9_007_199_254_740_991)
}

fn positive_id(text: &str) -> Option<u64> {
    if text.starts_with('0') || text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn parse_history(bytes: &[u8], next_cursor: Option<String>) -> io::Result<HistoryPage> {
    if bytes.len() as u64 > MAX_HISTORY_BYTES
        || (!bytes.is_empty() && !bytes.ends_with(b"\n"))
        || next_cursor
            .as_deref()
            .is_some_and(|cursor| !valid_history_cursor(cursor))
    {
        return Err(invalid_history());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid_history())?;
    let mut items = Vec::new();
    for line in text.split_terminator('\n') {
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 6 || line.contains('\r') {
            return Err(invalid_history());
        }
        let flag = |text| match text {
            "0" => Ok(false),
            "1" => Ok(true),
            _ => Err(invalid_history()),
        };
        items.push(HistoryEntry {
            kind: match fields[0] {
                "message" => HistoryKind::Message,
                "refinement" => HistoryKind::Refinement,
                _ => return Err(invalid_history()),
            },
            id: positive_id(fields[1]).ok_or_else(invalid_history)?,
            created_at: fields[2].to_string(),
            input_available: flag(fields[3])?,
            has_reply: flag(fields[4])?,
            model: fields[5].to_string(),
        });
        if items.len() > 50 {
            return Err(invalid_history());
        }
    }
    if items.is_empty() && next_cursor.is_some() {
        return Err(invalid_history());
    }
    Ok(HistoryPage { items, next_cursor })
}

fn history_tsv(items: &[HistoryEntry]) -> String {
    items
        .iter()
        .map(|entry| {
            let kind = match entry.kind {
                HistoryKind::Message => "message",
                HistoryKind::Refinement => "refinement",
            };
            format!(
                "{kind}\t{}\t{}\t{}\t{}\t{}\n",
                entry.id,
                entry.created_at,
                u8::from(entry.input_available),
                u8::from(entry.has_reply),
                entry.model
            )
        })
        .collect()
}

pub struct HistoryCache {
    root: PathBuf,
}

impl HistoryCache {
    pub fn new(outbox: impl AsRef<Path>) -> Self {
        Self {
            root: outbox.as_ref().join("history-cache"),
        }
    }

    pub fn image_path(&self, kind: HistoryKind, id: u64, side: HistoryImageSide) -> PathBuf {
        let name = match (kind, side) {
            (HistoryKind::Message, _) => format!("message-{id}.png"),
            (HistoryKind::Refinement, HistoryImageSide::Output) => {
                format!("refinement-{id}-output.png")
            }
            (HistoryKind::Refinement, HistoryImageSide::Input) => {
                format!("refinement-{id}-input.png")
            }
        };
        self.root.join(name)
    }

    pub fn list_history(
        &self,
        client: &MailboxClient,
        cursor: Option<&str>,
        limit: usize,
    ) -> io::Result<HistoryPage> {
        match client.list_history(cursor, limit) {
            Ok(page) => {
                if cursor.is_none() {
                    fs::create_dir_all(&self.root)?;
                    atomic_write(
                        &self.root.join("index.tsv"),
                        history_tsv(&page.items).as_bytes(),
                    )?;
                }
                Ok(page)
            }
            Err(error)
                if cursor.is_none()
                    && error.kind() != io::ErrorKind::InvalidData
                    && error.kind() != io::ErrorKind::InvalidInput =>
            {
                let cached = fs::File::open(self.root.join("index.tsv"));
                let Ok(file) = cached else {
                    return Err(error);
                };
                let mut bytes = Vec::new();
                file.take(MAX_HISTORY_BYTES + 1).read_to_end(&mut bytes)?;
                let mut page = parse_history(&bytes, None)?;
                page.items.truncate(limit);
                Ok(page)
            }
            Err(error) => Err(error),
        }
    }

    pub fn fetch_image(
        &self,
        client: &MailboxClient,
        kind: HistoryKind,
        id: u64,
        side: HistoryImageSide,
    ) -> io::Result<PathBuf> {
        if id == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid history ID",
            ));
        }
        let path = self.image_path(kind, id, side);
        if cached_png_valid(&path) {
            return Ok(path);
        }
        fs::create_dir_all(&self.root)?;
        client.fetch_history_image(kind, id, side, &path)?;
        self.prune(&path)?;
        Ok(path)
    }

    fn prune(&self, protected: &Path) -> io::Result<()> {
        let mut files = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_type()?.is_file()
                && entry.path().extension().is_some_and(|ext| ext == "png")
            {
                let metadata = entry.metadata()?;
                files.push((metadata.modified()?, entry.path(), metadata.len()));
            }
        }
        files.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        let mut count = files.len();
        let mut bytes: u64 = files.iter().map(|file| file.2).sum();
        for (_, path, length) in files {
            if count <= MAX_CACHE_FILES && bytes <= MAX_CACHE_BYTES {
                break;
            }
            if path == protected {
                continue;
            }
            fs::remove_file(path)?;
            count -= 1;
            bytes -= length;
        }
        Ok(())
    }
}

fn cached_png_valid(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        metadata.is_file() && metadata.len() > 0 && metadata.len() <= MAX_REFINED_IMAGE_BYTES
    }) && fs::File::open(path).is_ok_and(|mut file| {
        let mut signature = [0; 8];
        file.read_exact(&mut signature).is_ok() && is_png(&signature)
    })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temp_path = path.with_extension(format!(
        "{}.part",
        path.extension().unwrap_or_default().to_string_lossy()
    ));
    let result = (|| {
        let mut file = fs::File::create(&temp_path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp_path, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp_path);
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub id: u64,
    pub body: String,
}

pub struct MailboxClient {
    base_url: String,
    device_token: String,
    device_id: String,
    agent: ureq::Agent,
}

impl MailboxClient {
    pub fn from_env() -> io::Result<Self> {
        let base_url = required_env("MAILBOX_BASE_URL")?;
        let device_token = required_env("MAILBOX_DEVICE_TOKEN")?;
        let device_id = std::env::var("MAILBOX_DEVICE_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "ian-kobo".to_string());
        Self::new(base_url, device_token, device_id)
    }

    pub fn new(
        base_url: impl Into<String>,
        device_token: impl Into<String>,
        device_id: impl Into<String>,
    ) -> io::Result<Self> {
        let base_url = base_url.into().trim().trim_end_matches('/').to_string();
        let device_token = device_token.into();
        let device_id = device_id.into();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MAILBOX_BASE_URL must start with http:// or https://",
            ));
        }
        if device_token.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MAILBOX_DEVICE_TOKEN is empty",
            ));
        }
        if device_id.trim().is_empty() || device_id.contains(['\r', '\n']) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "MAILBOX_DEVICE_ID is empty or invalid",
            ));
        }
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(180))
            .timeout_write(Duration::from_secs(30))
            .build();
        Ok(Self {
            base_url,
            device_token,
            device_id,
            agent,
        })
    }

    pub fn send_png(&self, path: impl AsRef<Path>) -> io::Result<u64> {
        let png = fs::read(path)?;
        if png.len() > MAX_PAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mailbox page exceeds 4 MiB",
            ));
        }
        if !is_png(&png) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mailbox page is not a PNG",
            ));
        }
        let response = self
            .agent
            .post(&format!("{}/api/device/messages", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.device_token))
            .set("Content-Type", "image/png")
            .set("X-Device-Id", &self.device_id)
            .send_bytes(&png)
            .map_err(http_error)?;
        if response.status() != 201 {
            return Err(io::Error::other(format!(
                "mailbox upload returned HTTP {}",
                response.status()
            )));
        }
        let body = response.into_string().map_err(io::Error::other)?;
        body.trim().parse::<u64>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "mailbox upload returned an invalid message ID",
            )
        })
    }

    pub fn list_history(&self, cursor: Option<&str>, limit: usize) -> io::Result<HistoryPage> {
        if !(1..=50).contains(&limit) || cursor.is_some_and(|value| !valid_history_cursor(value)) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid history query",
            ));
        }
        let mut request = self
            .agent
            .get(&format!("{}/api/device/history", self.base_url))
            .query("limit", &limit.to_string())
            .set("Authorization", &format!("Bearer {}", self.device_token));
        if let Some(cursor) = cursor {
            request = request.query("cursor", cursor);
        }
        let response = request.call().map_err(http_error)?;
        if response.status() != 200 {
            return Err(invalid_history());
        }
        let next_cursor = response.header("X-Next-Cursor").map(str::to_string);
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_HISTORY_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| io::Error::other("mailbox history read failed"))?;
        let page = parse_history(&bytes, next_cursor)?;
        if page.items.len() > limit {
            return Err(invalid_history());
        }
        Ok(page)
    }

    pub fn fetch_history_image(
        &self,
        kind: HistoryKind,
        id: u64,
        side: HistoryImageSide,
        output_path: impl AsRef<Path>,
    ) -> io::Result<()> {
        if id == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid history ID",
            ));
        }
        let route = match (kind, side) {
            (HistoryKind::Message, _) => format!("messages/{id}.png"),
            (HistoryKind::Refinement, HistoryImageSide::Output) => {
                format!("refinements/{id}/output.png")
            }
            (HistoryKind::Refinement, HistoryImageSide::Input) => {
                format!("refinements/{id}/input.png")
            }
        };
        let response = self
            .agent
            .get(&format!("{}/api/device/history/{route}", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.device_token))
            .call()
            .map_err(http_error)?;
        download_png(response, output_path.as_ref())
    }

    pub fn refine_png(
        &self,
        input_path: impl AsRef<Path>,
        output_path: impl AsRef<Path>,
    ) -> io::Result<()> {
        let png = fs::read(input_path)?;
        if png.len() > MAX_PAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "refinement input exceeds 4 MiB",
            ));
        }
        if !is_png(&png) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "refinement input is not a PNG",
            ));
        }
        let response = self
            .agent
            .post(&format!("{}/api/device/refinements", self.base_url))
            .set("Authorization", &format!("Bearer {}", self.device_token))
            .set("Content-Type", "image/png")
            .set("X-Device-Id", &self.device_id)
            .set("X-Refinement-Key", &refinement_key(&png))
            .send_bytes(&png)
            .map_err(http_error)?;
        download_png(response, output_path.as_ref())
    }

    pub fn poll_reply(&self, after: u64) -> io::Result<Option<Reply>> {
        let response = self
            .agent
            .get(&format!("{}/api/device/replies", self.base_url))
            .query("after", &after.to_string())
            .set("Authorization", &format!("Bearer {}", self.device_token))
            .call()
            .map_err(http_error)?;
        if response.status() == 204 {
            return Ok(None);
        }
        if response.status() != 200 {
            return Err(io::Error::other(format!(
                "mailbox poll returned HTTP {}",
                response.status()
            )));
        }
        let id = response
            .header("X-Reply-Id")
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "mailbox reply is missing X-Reply-Id",
                )
            })?
            .parse::<u64>()
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "mailbox reply has an invalid X-Reply-Id",
                )
            })?;
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_REPLY_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_REPLY_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "mailbox reply is too large",
            ));
        }
        let body = String::from_utf8(bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "mailbox reply is not valid UTF-8",
            )
        })?;
        Ok(Some(Reply { id, body }))
    }
}

fn download_png(response: ureq::Response, output_path: &Path) -> io::Result<()> {
    if response.status() != 200 {
        return Err(io::Error::other(format!(
            "mailbox image returned HTTP {}",
            response.status()
        )));
    }
    let content_type = response
        .header("Content-Type")
        .unwrap_or_default()
        .split(';')
        .next()
        .unwrap_or_default()
        .trim();
    if content_type != "image/png" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "refinement response is not image/png",
        ));
    }
    if response
        .header("Content-Length")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_REFINED_IMAGE_BYTES)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "refinement response exceeds 16 MiB",
        ));
    }
    let mut refined = Vec::new();
    response
        .into_reader()
        .take(MAX_REFINED_IMAGE_BYTES + 1)
        .read_to_end(&mut refined)?;
    if refined.len() as u64 > MAX_REFINED_IMAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "refinement response exceeds 16 MiB",
        ));
    }
    if !is_png(&refined) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "refinement response is not a PNG",
        ));
    }

    atomic_write(output_path, &refined)
}

fn required_env(name: &str) -> io::Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{name} is not set")))
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn refinement_key(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn http_error(error: ureq::Error) -> io::Error {
    match error {
        ureq::Error::Status(code, _) => {
            io::Error::other(format!("mailbox server returned HTTP {code}"))
        }
        ureq::Error::Transport(_) => io::Error::other("mailbox request failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;

    #[derive(Debug)]
    struct CapturedRequest {
        head: String,
        body: Vec<u8>,
    }

    fn mock_server(response: &'static [u8]) -> (String, mpsc::Receiver<CapturedRequest>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_request(&mut stream);
            tx.send(request).unwrap();
            stream.write_all(response).unwrap();
        });
        (format!("http://{address}"), rx)
    }

    fn read_request(stream: &mut TcpStream) -> CapturedRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0, "request ended before headers");
            bytes.extend_from_slice(&chunk[..n]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let head = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let content_length = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while bytes.len() - header_end < content_length {
            let n = stream.read(&mut chunk).unwrap();
            assert!(n > 0, "request body ended early");
            bytes.extend_from_slice(&chunk[..n]);
        }
        CapturedRequest {
            head,
            body: bytes[header_end..header_end + content_length].to_vec(),
        }
    }

    fn test_png() -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(b"test-page");
        png
    }

    #[test]
    fn uploads_raw_png_with_auth_and_device_id() {
        let (base, requests) = mock_server(
            b"HTTP/1.1 201 Created\r\nContent-Length: 3\r\nConnection: close\r\n\r\n42\n",
        );
        let path = std::env::temp_dir().join(format!("mailbox-page-{}.png", std::process::id()));
        fs::write(&path, test_png()).unwrap();
        let client = MailboxClient::new(base, "secret-device", "ian-kobo").unwrap();
        assert_eq!(client.send_png(&path).unwrap(), 42);
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request
            .head
            .starts_with("POST /api/device/messages HTTP/1.1\r\n"));
        assert!(request
            .head
            .contains("Authorization: Bearer secret-device\r\n"));
        assert!(request.head.contains("Content-Type: image/png\r\n"));
        assert!(request.head.contains("X-Device-Id: ian-kobo\r\n"));
        assert_eq!(request.body, test_png());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn poll_returns_none_for_204() {
        let (base, requests) = mock_server(
            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let client = MailboxClient::new(base, "token", "ian-kobo").unwrap();
        assert_eq!(client.poll_reply(7).unwrap(), None);
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request
            .head
            .starts_with("GET /api/device/replies?after=7 HTTP/1.1\r\n"));
        assert!(request.head.contains("Authorization: Bearer token\r\n"));
    }

    #[test]
    fn poll_decodes_reply_id_and_utf8_body() {
        let body = "Ian，你的纸飞机收到了！";
        let response = format!(
            "HTTP/1.1 200 OK\r\nX-Reply-Id: 9\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let response: &'static [u8] = Box::leak(response.into_bytes().into_boxed_slice());
        let (base, _requests) = mock_server(response);
        let client = MailboxClient::new(base, "token", "ian-kobo").unwrap();
        assert_eq!(
            client.poll_reply(3).unwrap(),
            Some(Reply {
                id: 9,
                body: body.to_string()
            })
        );
    }

    #[test]
    fn reports_http_error_without_leaking_token() {
        let (base, _requests) = mock_server(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\nunauthorized",
        );
        let client = MailboxClient::new(base, "very-secret", "ian-kobo").unwrap();
        let error = client.poll_reply(0).unwrap_err().to_string();
        assert!(error.contains("HTTP 401"));
        assert!(!error.contains("very-secret"));
    }

    #[test]
    fn refines_png_with_stable_key_and_writes_returned_png() {
        let refined = test_png();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            refined.len()
        )
        .into_bytes()
        .into_iter()
        .chain(refined.iter().copied())
        .collect::<Vec<_>>();
        let response: &'static [u8] = Box::leak(response.into_boxed_slice());
        let (base, requests) = mock_server(response);
        let suffix = format!("{}-{}", std::process::id(), refinement_key(&test_png()));
        let input = std::env::temp_dir().join(format!("mailbox-refine-input-{suffix}.png"));
        let output = std::env::temp_dir().join(format!("mailbox-refine-output-{suffix}.png"));
        fs::write(&input, test_png()).unwrap();

        let client = MailboxClient::new(base, "secret-device", "ian-kobo").unwrap();
        client.refine_png(&input, &output).unwrap();
        assert_eq!(fs::read(&output).unwrap(), refined);
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request
            .head
            .starts_with("POST /api/device/refinements HTTP/1.1\r\n"));
        assert!(request.head.contains("Authorization: Bearer "));
        assert!(request.head.contains("Content-Type: image/png\r\n"));
        assert!(request.head.contains("X-Device-Id: ian-kobo\r\n"));
        assert!(request.head.contains(&format!(
            "X-Refinement-Key: {}\r\n",
            refinement_key(&test_png())
        )));
        assert_eq!(request.body, test_png());
        let _ = fs::remove_file(input);
        let _ = fs::remove_file(output);
    }

    #[test]
    fn refinement_key_is_stable_and_sensitive_to_image_bytes() {
        assert_eq!(refinement_key(&test_png()), refinement_key(&test_png()));
        let mut changed = test_png();
        changed.push(1);
        assert_ne!(refinement_key(&test_png()), refinement_key(&changed));
    }

    #[test]
    fn rejects_non_png_before_networking() {
        let client = MailboxClient::new("http://127.0.0.1:9", "token", "ian-kobo").unwrap();
        let path = std::env::temp_dir().join(format!("mailbox-not-png-{}", std::process::id()));
        fs::write(&path, b"not png").unwrap();
        let error = client.send_png(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_file(path);
    }

    fn history_response(body: &[u8], headers: &str) -> &'static [u8] {
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(body);
        Box::leak(response.into_boxed_slice())
    }

    fn history_test_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mailbox-history-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn history_list_encodes_cursor_and_parses_tsv_and_header() {
        let cursor = "2026-11-15T02:36:40Z|2|3";
        let body = b"message\t2\t2026-11-15T02:36:40Z\t0\t1\t\nrefinement\t1\tlegacy\t1\t0\tMAI-Image-2.6\n";
        let (base, requests) = mock_server(history_response(
            body,
            "X-Next-Cursor: 0000-00-00T00:00:00Z|1|1\r\n",
        ));
        let client = MailboxClient::new(base, "history-secret", "kobo").unwrap();
        let page = client.list_history(Some(cursor), 2).unwrap();
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].kind, HistoryKind::Message);
        assert!(page.items[0].has_reply);
        assert!(page.items[1].input_available);
        assert_eq!(
            page.next_cursor.as_deref(),
            Some("0000-00-00T00:00:00Z|1|1")
        );
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request
            .head
            .contains("Authorization: Bearer history-secret\r\n"));
        assert!(request
            .head
            .contains("limit=2&cursor=2026-11-15T02%3A36%3A40Z%7C2%7C3"));
    }

    #[test]
    fn history_strict_parser_rejects_invalid_fields_and_oversized_body() {
        for body in [
            b"unknown\t1\tlegacy\t0\t0\t\n".as_slice(),
            b"message\t0\tlegacy\t0\t0\t\n",
            b"message\t01\tlegacy\t0\t0\t\n",
            b"message\t1\tlegacy\t2\t0\t\n",
            b"message\t1\tlegacy\t0\tx\t\n",
            b"message\t1\tlegacy\t0\t0\n",
            b"message\t1\tlegacy\t0\t0\t",
            b"message\t1\tlegacy\t0\t0\t\r\n",
            b"\xff\n",
        ] {
            assert!(parse_history(body, None).is_err());
        }
        assert!(parse_history(&vec![b'x'; MAX_HISTORY_BYTES as usize + 1], None).is_err());
        assert!(parse_history(b"", Some("not-a-cursor".into())).is_err());
        for (body, headers) in [
            (vec![b'x'; MAX_HISTORY_BYTES as usize + 1], ""),
            (
                b"message\t1\tlegacy\t0\t0\t\n".to_vec(),
                "X-Next-Cursor: wrong\r\n",
            ),
        ] {
            let (base, _requests) = mock_server(history_response(&body, headers));
            let client = MailboxClient::new(base, "secret", "kobo").unwrap();
            assert!(client.list_history(None, 20).is_err());
        }
    }

    #[test]
    fn history_images_use_device_routes_and_cache_hits_skip_network() {
        let dir = history_test_dir("images");
        let cache = HistoryCache::new(&dir);
        for (kind, id, side, route, filename) in [
            (
                HistoryKind::Message,
                1,
                HistoryImageSide::Output,
                "messages/1.png",
                "message-1.png",
            ),
            (
                HistoryKind::Refinement,
                2,
                HistoryImageSide::Output,
                "refinements/2/output.png",
                "refinement-2-output.png",
            ),
            (
                HistoryKind::Refinement,
                2,
                HistoryImageSide::Input,
                "refinements/2/input.png",
                "refinement-2-input.png",
            ),
        ] {
            let (base, requests) =
                mock_server(history_response(&test_png(), "Content-Type: image/png\r\n"));
            let client = MailboxClient::new(base, "history-secret", "kobo").unwrap();
            let path = cache.fetch_image(&client, kind, id, side).unwrap();
            assert_eq!(path.file_name().unwrap(), filename);
            let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(request
                .head
                .starts_with(&format!("GET /api/device/history/{route} HTTP/1.1\r\n")));
            assert!(request
                .head
                .contains("Authorization: Bearer history-secret\r\n"));
            assert_eq!(fs::read(&path).unwrap(), test_png());
            assert_eq!(cache.fetch_image(&client, kind, id, side).unwrap(), path);
            assert!(!path.with_extension("png.part").exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_cache_snapshot_survives_offline_and_invalid_cache_fails_cleanly() {
        let dir = history_test_dir("offline");
        let cache = HistoryCache::new(&dir);
        let (base, _requests) = mock_server(history_response(b"message\t1\tlegacy\t0\t0\t\n", ""));
        let online = MailboxClient::new(base, "secret", "kobo").unwrap();
        let page = cache.list_history(&online, None, 20).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let offline = MailboxClient::new(base, "secret", "kobo").unwrap();
        assert_eq!(cache.list_history(&offline, None, 20).unwrap(), page);
        assert!(cache
            .list_history(&offline, Some("0000-00-00T00:00:00Z|2|1"), 20)
            .is_err());
        fs::write(cache.root.join("index.tsv"), "bad").unwrap();
        assert_eq!(
            cache.list_history(&offline, None, 20).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        fs::remove_file(cache.root.join("index.tsv")).unwrap();
        assert!(cache.list_history(&offline, None, 20).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_cache_prunes_file_count_and_bytes_and_protects_current_image() {
        let dir = history_test_dir("prune");
        let cache = HistoryCache::new(&dir);
        fs::create_dir_all(&cache.root).unwrap();
        let protected = cache.root.join("message-1.png");
        fs::write(&protected, test_png()).unwrap();
        for id in 2..=70 {
            fs::write(cache.root.join(format!("message-{id}.png")), test_png()).unwrap();
        }
        fs::write(cache.root.join("index.tsv"), "snapshot").unwrap();
        cache.prune(&protected).unwrap();
        assert!(protected.exists());
        assert_eq!(
            fs::read_dir(&cache.root)
                .unwrap()
                .filter(|entry| entry
                    .as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == "png"))
                .count(),
            64
        );
        assert!(cache.root.join("index.tsv").exists());
        for entry in fs::read_dir(&cache.root).unwrap() {
            let path = entry.unwrap().path();
            if path != protected && path.extension().is_some_and(|ext| ext == "png") {
                fs::remove_file(path).unwrap();
            }
        }
        for id in 2..=8 {
            let file =
                fs::File::create(cache.root.join(format!("refinement-{id}-output.png"))).unwrap();
            file.set_len(MAX_REFINED_IMAGE_BYTES).unwrap();
        }
        cache.prune(&protected).unwrap();
        let bytes: u64 = fs::read_dir(&cache.root)
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
            .map(|entry| entry.metadata().unwrap().len())
            .sum();
        assert!(bytes <= MAX_CACHE_BYTES);
        assert!(protected.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn history_http_errors_never_include_echoed_secrets() {
        let (base, _requests) = mock_server(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\nsecret-token");
        let client = MailboxClient::new(base, "secret-token", "kobo").unwrap();
        let error = client.list_history(None, 20).unwrap_err().to_string();
        assert!(error.contains("401"));
        assert!(!error.contains("secret-token"));
    }

    #[test]
    fn history_image_validation_preserves_destination_and_repairs_invalid_cache() {
        let dir = history_test_dir("invalid-images");
        let path = dir.join("output.png");
        for response in [history_response(&test_png(), "Content-Type: text/plain\r\n"),
            history_response(b"not a PNG", "Content-Type: image/png\r\n"),
            b"HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: 16777217\r\nConnection: close\r\n\r\n"] {
            fs::write(&path, test_png()).unwrap();
            let (base, _requests) = mock_server(response);
            let client = MailboxClient::new(base, "secret", "kobo").unwrap();
            assert!(client.fetch_history_image(HistoryKind::Message, 1, HistoryImageSide::Output, &path).is_err());
            assert_eq!(fs::read(&path).unwrap(), test_png());
            assert!(!path.with_extension("png.part").exists());
        }
        let cache = HistoryCache::new(&dir);
        fs::create_dir_all(&cache.root).unwrap();
        let cached = cache.image_path(HistoryKind::Message, 1, HistoryImageSide::Output);
        for size in [0, MAX_REFINED_IMAGE_BYTES + 1] {
            let mut file = fs::File::create(&cached).unwrap();
            if size > 0 {
                file.write_all(&test_png()).unwrap();
            }
            file.set_len(size).unwrap();
            assert!(!cached_png_valid(&cached));
            let (base, requests) =
                mock_server(history_response(&test_png(), "Content-Type: image/png\r\n"));
            let client = MailboxClient::new(base, "secret", "kobo").unwrap();
            assert_eq!(
                cache
                    .fetch_image(&client, HistoryKind::Message, 1, HistoryImageSide::Output)
                    .unwrap(),
                cached
            );
            requests.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(cached_png_valid(&cached));
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
