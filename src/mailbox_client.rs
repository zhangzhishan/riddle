//! Paper Plane Mailbox HTTP protocol client.
//!
//! The device uploads one raw PNG at a time and polls for UTF-8 family replies.
//! Networking is intentionally independent of the e-ink UI state machine so it
//! can be exercised on the host before installing anything on the Kobo.

use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;
const MAX_REPLY_BYTES: u64 = 8 * 1024;

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
            .timeout_read(Duration::from_secs(30))
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

fn required_env(name: &str) -> io::Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{name} is not set")))
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

fn http_error(error: ureq::Error) -> io::Error {
    match error {
        ureq::Error::Status(code, response) => {
            let detail = response.into_string().unwrap_or_default();
            let detail = detail.trim();
            if detail.is_empty() {
                io::Error::other(format!("mailbox server returned HTTP {code}"))
            } else {
                io::Error::other(format!("mailbox server returned HTTP {code}: {detail}"))
            }
        }
        ureq::Error::Transport(error) => {
            io::Error::other(format!("mailbox request failed: {error}"))
        }
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
    fn rejects_non_png_before_networking() {
        let client = MailboxClient::new("http://127.0.0.1:9", "token", "ian-kobo").unwrap();
        let path = std::env::temp_dir().join(format!("mailbox-not-png-{}", std::process::id()));
        fs::write(&path, b"not png").unwrap();
        let error = client.send_png(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_file(path);
    }
}
