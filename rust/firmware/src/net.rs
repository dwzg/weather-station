//! HTTPS to the weather app. The certificate is verified against Let's
//! Encrypt's roots (certs/ at the repository root), so renewals and new
//! intermediates need no firmware update.

use esp_idf_svc::http::client::{Configuration, EspHttpConnection};
use esp_idf_svc::http::Method;
use esp_idf_svc::tls::X509;
use ws_core::uploader::{Response, Transport};

use crate::{config, platform};

/// ISRG Root X1, X2 and the 2025 generation YR, YE; mbedTLS parses all PEM
/// blocks of the buffer. NUL-terminated as ESP-IDF wants it.
static TRUST_ANCHORS: &[u8] = concat!(
    include_str!("../../../certs/isrgrootx1.pem"),
    include_str!("../../../certs/isrg-root-x2.pem"),
    include_str!("../../../certs/root-ye.pem"),
    include_str!("../../../certs/root-yr.pem"),
    "\0"
)
.as_bytes();

pub struct Https {
    conn: EspHttpConnection,
    url: &'static str,
    key: &'static str,
    user_agent: String,
}

impl Https {
    /// Sets up a client (it connects on the first request and keeps the
    /// connection for the rest of the batch).
    pub fn connect(url: &'static str, key: &'static str) -> Result<Self, String> {
        let (free, block) = (platform::free_heap(), platform::largest_block());
        if free < config::TLS_MIN_FREE_HEAP || block < config::TLS_MIN_BLOCK {
            return Err(format!("low memory (free {free}, block {block})"));
        }
        let conf = Configuration {
            timeout: Some(config::HTTP_TIMEOUT),
            server_certificate: Some(X509::pem_until_nul(TRUST_ANCHORS)),
            buffer_size: Some(1024),
            buffer_size_tx: Some(1024),
            ..Default::default()
        };
        let conn = EspHttpConnection::new(&conf).map_err(|e| format!("http client: {e}"))?;
        Ok(Self { conn, url, key, user_agent: format!("weather-station/{}", env!("FW_VERSION")) })
    }
}

impl Transport for Https {
    fn post(&mut self, body: &str) -> Result<Response, String> {
        let len = body.len().to_string();
        let headers = [
            ("Content-Type", "application/json"),
            ("Content-Length", len.as_str()),
            ("X-API-Key", self.key),
            ("User-Agent", self.user_agent.as_str()),
        ];
        let c = &mut self.conn;
        c.initiate_request(Method::Post, self.url, &headers).map_err(|e| format!("connect: {e}"))?;
        c.write_all(body.as_bytes()).map_err(|e| format!("send: {e:?}"))?;
        c.initiate_response().map_err(|e| format!("response: {e}"))?;
        let status = c.status();
        // Keep the start of the answer for the log, drain the rest so the
        // connection can carry the next request.
        let mut text = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            match c.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if text.len() < 256 {
                        text.extend_from_slice(&buf[..n.min(256 - text.len())]);
                    }
                }
                Err(e) => return Err(format!("read: {e}")),
            }
        }
        Ok(Response { status, body: String::from_utf8_lossy(&text).trim().to_string() })
    }
}
