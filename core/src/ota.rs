//! Authentication of firmware updates over the network.
//!
//! The updater fetches a one-time nonce, then sends the image with
//!
//!   X-OTA-Nonce:  the nonce
//!   X-OTA-SHA256: hex SHA-256 of the image
//!   X-OTA-Auth:   hex HMAC-SHA256(key, "ws-ota-v1\n" nonce "\n" sha256 "\n" length)
//!
//! The station checks the tag before it writes anything, and the image against
//! the hash before it boots it. The key is SHA-256 of the OTA password; only the
//! key is in the firmware. tools/ota.py is the other side.

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

pub type Key = [u8; 32];

pub fn key_from_password(password: &str) -> Key {
    Sha256::digest(password.as_bytes()).into()
}

fn message(nonce: &str, sha256_hex: &str, len: usize) -> String {
    format!("ws-ota-v1\n{nonce}\n{sha256_hex}\n{len}")
}

/// The tag the updater sends.
pub fn tag(key: &Key, nonce: &str, sha256_hex: &str, len: usize) -> [u8; 32] {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("any key length");
    m.update(message(nonce, sha256_hex, len).as_bytes());
    m.finalize().into_bytes().into()
}

/// Constant-time check of a hex tag.
pub fn verify(key: &Key, nonce: &str, sha256_hex: &str, len: usize, tag_hex: &str) -> bool {
    let Some(t) = from_hex(tag_hex) else { return false };
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("any key length");
    m.update(message(nonce, sha256_hex, len).as_bytes());
    m.verify_slice(&t).is_ok()
}

pub fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Streaming SHA-256 of the received image.
#[derive(Default)]
pub struct ImageHash(Sha256);

impl ImageHash {
    pub fn update(&mut self, data: &[u8]) {
        self.0.update(data);
    }
    pub fn finish_hex(self) -> String {
        to_hex(&self.0.finalize())
    }
}

/// One outstanding nonce at a time, valid for `ttl_ms`, usable once.
pub struct Nonces {
    current: Option<(String, u64)>,
    ttl_ms: u64,
}

impl Nonces {
    pub const fn new(ttl_ms: u64) -> Self {
        Self { current: None, ttl_ms }
    }

    pub fn issue(&mut self, random: [u8; 16], now_ms: u64) -> String {
        let n = to_hex(&random);
        self.current = Some((n.clone(), now_ms));
        n
    }

    /// True once for the nonce last issued, within its lifetime.
    pub fn take(&mut self, nonce: &str, now_ms: u64) -> bool {
        match self.current.take() {
            Some((n, at)) if n == nonce && now_ms.saturating_sub(at) <= self.ttl_ms => true,
            other => {
                // A wrong guess does not burn the real nonce, but an expired one goes.
                self.current = other.filter(|(_, at)| now_ms.saturating_sub(*at) <= self.ttl_ms);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_known_answer() {
        // RFC 4231 test case 2.
        let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(b"Jefe").unwrap();
        m.update(b"what do ya want for nothing?");
        assert_eq!(
            to_hex(&m.finalize().into_bytes()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn tag_roundtrip() {
        let key = key_from_password("correct horse");
        let mut h = ImageHash::default();
        h.update(b"firmware");
        let sha = h.finish_hex();
        assert_eq!(sha, to_hex(&Sha256::digest(b"firmware")));
        let t = to_hex(&tag(&key, "abc", &sha, 8));
        assert!(verify(&key, "abc", &sha, 8, &t));
        assert!(!verify(&key, "abd", &sha, 8, &t));
        assert!(!verify(&key, "abc", &sha, 9, &t));
        assert!(!verify(&key_from_password("wrong"), "abc", &sha, 8, &t));
        assert!(!verify(&key, "abc", &sha, 8, "zz"));
        // Fixed vector, shared with tools/ota.py's self-test.
        assert_eq!(
            to_hex(&tag(&key_from_password("pw"), "00112233445566778899aabbccddeeff", &"0".repeat(64), 1234)),
            TOOL_VECTOR
        );
    }
    const TOOL_VECTOR: &str = "582918a97d97d3d3c90c3078b02e5bc955296f0c82e56ff00f4a07148f3011b4";

    #[test]
    fn nonces_are_single_use_and_expire() {
        let mut n = Nonces::new(60_000);
        let a = n.issue([7; 16], 1000);
        assert!(!n.take("nope", 1000));
        assert!(n.take(&a, 2000));
        assert!(!n.take(&a, 2000), "used");
        let b = n.issue([8; 16], 0);
        assert!(!n.take(&b, 60_001), "expired");
        assert_eq!(from_hex("0aFF"), Some(vec![0x0a, 0xff]));
        assert_eq!(from_hex("0"), None);
    }
}
