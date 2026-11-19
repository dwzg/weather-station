#!/usr/bin/env python3
"""Updates the ESP32-S2 (Rust) station over Wi-Fi.

    tools/ota-s2.py 192.168.178.57 rust/firmware/target/weather-station.bin

The password comes from $OTA_PASSWORD or ~/.config/homelab/weather-station/ota.pass.
The station hands out a one-time nonce, checks an HMAC over the nonce and the
image's SHA-256 before it writes anything, and the image against that hash
before it boots it. See rust/core/src/ota.rs.

    tools/ota-s2.py --self-test     prints the test vector shared with ota.rs
"""
import hashlib
import hmac
import os
import pathlib
import sys
import urllib.request


def key_from_password(pw: str) -> bytes:
    return hashlib.sha256(pw.encode()).digest()


def tag(key: bytes, nonce: str, sha256_hex: str, length: int) -> str:
    msg = f"ws-ota-v1\n{nonce}\n{sha256_hex}\n{length}".encode()
    return hmac.new(key, msg, hashlib.sha256).hexdigest()


def main() -> int:
    if sys.argv[1:] == ["--self-test"]:
        print(tag(key_from_password("pw"), "00112233445566778899aabbccddeeff", "0" * 64, 1234))
        return 0
    if len(sys.argv) != 3:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    host, path = sys.argv[1], pathlib.Path(sys.argv[2])
    pw = os.environ.get("OTA_PASSWORD")
    if not pw:
        pw = (pathlib.Path.home() / ".config/homelab/weather-station/ota.pass").read_text().strip()
    image = path.read_bytes()
    sha = hashlib.sha256(image).hexdigest()

    with urllib.request.urlopen(f"http://{host}/ota/nonce", timeout=10) as r:
        nonce = r.read().decode().strip()
    req = urllib.request.Request(
        f"http://{host}/ota",
        data=image,
        method="POST",
        headers={
            "Content-Type": "application/octet-stream",
            "X-OTA-Nonce": nonce,
            "X-OTA-SHA256": sha,
            "X-OTA-Auth": tag(key_from_password(pw), nonce, sha, len(image)),
        },
    )
    print(f"sending {len(image)} bytes ({sha[:12]}…) to {host}")
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            print(r.read().decode().strip())
    except urllib.error.HTTPError as e:
        print(f"refused: HTTP {e.code} {e.read().decode().strip()}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
