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
import json
import os
import pathlib
import sys
import time
import urllib.error
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
    key = key_from_password(pw)

    before = status(host)
    if before is None:
        print(f"{host} does not answer", file=sys.stderr)
        return 1
    print(f"{host} runs {before.get('firmware', '?')}; sending {len(image)} bytes ({sha[:12]}…)")
    for attempt in range(2):
        code, text = send(host, image, sha, key)
        if code == 409 and text.startswith("rollback") and attempt == 0:
            # The running image is unconfirmed: the station goes back to the
            # previous one, which can take the update.
            print("the running image is unconfirmed; the station returns to the previous one, then we retry")
            if not wait_for_restart(host):
                return 1
            continue
        break
    if code == 200:
        print(text)
    elif code is None:
        # The answer can be lost when the station restarts right after the update.
        print(f"no answer ({text}); checking what the station runs now", file=sys.stderr)
    else:
        print(f"refused: HTTP {code} {text}", file=sys.stderr)
        return 1

    if not wait_for_restart(host):
        return 1
    after = status(host) or {}
    print(f"{host} now runs {after.get('firmware', '?')}"
          + (" (unconfirmed until it has measured and uploaded)" if after.get("ota", {}).get("unconfirmed") else ""))
    return 0 if after.get("firmware") != before.get("firmware") or code == 200 else 1


def status(host):
    """The station's status JSON, or None if it does not answer."""
    try:
        with urllib.request.urlopen(f"http://{host}/", timeout=5) as r:
            return json.loads(r.read().decode())
    except (OSError, ValueError):  # URLError, timeouts and resets are OSErrors
        return None


def wait_for_restart(host, seconds=120):
    """Waits until the station has gone away (it restarts about a second after
    answering) and answers again."""
    deadline = time.monotonic() + seconds
    gone_by = time.monotonic() + 15
    while time.monotonic() < gone_by and status(host) is not None:
        time.sleep(0.5)
    while time.monotonic() < deadline:
        if status(host) is not None:
            return True
        time.sleep(2)
    print(f"{host} did not come back within {seconds} s", file=sys.stderr)
    return False


def send(host, image, sha, key):
    """One authenticated upload: (HTTP status or None, text)."""
    try:
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
                "X-OTA-Auth": tag(key, nonce, sha, len(image)),
            },
        )
        with urllib.request.urlopen(req, timeout=180) as r:
            return r.status, r.read().decode().strip()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace").strip()
    except OSError as e:
        return None, str(e)


if __name__ == "__main__":
    sys.exit(main())
