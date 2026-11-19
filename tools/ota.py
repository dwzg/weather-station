#!/usr/bin/env python3
"""Updates the station over Wi-Fi.

    tools/ota.py 192.168.178.57 ws.bin    (ws.bin: espflash save-image, see README)

The password comes from $OTA_PASSWORD or ~/.config/homelab/weather-station/ota.pass.
The station hands out a one-time nonce, checks an HMAC over the nonce and the
image's SHA-256 before it writes anything, and the image against that hash
before it boots it. See core/src/ota.rs.

The new image boots unconfirmed and confirms itself once it has measured and
the app has accepted a reading (up to ~10 min); if it resets before that, the
station goes back to the previous image. The tool waits for that verdict and
exits 0 only when the new image is confirmed. With --no-wait it stops once the
station runs the new image, still unconfirmed.

    tools/ota.py --self-test     prints the test vector shared with ota.rs
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
    no_wait = "--no-wait" in sys.argv[1:]
    args = [a for a in sys.argv[1:] if a != "--no-wait"]
    if len(args) != 2 or any(a.startswith("--") for a in args):
        print(__doc__.strip(), file=sys.stderr)
        return 2
    host, path = args[0], pathlib.Path(args[1])
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
            before = wait_for_restart(host, before)
            if before is None:
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

    after = wait_for_restart(host, before)
    if after is None:
        return 1
    # A freshly written image always boots unconfirmed. Confirmed right after
    # the restart means the bootloader went back to the old one.
    if not after.get("ota", {}).get("unconfirmed"):
        print(f"{host} restarted but runs {after.get('firmware', '?')}, confirmed: "
              "the new image did not boot (rolled back)", file=sys.stderr)
        return 1
    print(f"{host} now runs {after.get('firmware', '?')}, unconfirmed until it has measured and uploaded")
    if no_wait:
        return 0
    return wait_for_confirmation(host, after)


def restarted(s, prev):
    """True if status `s` is from a later boot than status `prev`."""
    return s.get("boot_count") != prev.get("boot_count") or s.get("uptime_s", 0) < prev.get("uptime_s", 0)


def wait_for_confirmation(host, booted, seconds=900):
    """Waits until the new image confirms itself (0) or the station restarts
    before that, which rolls it back (1)."""
    deadline = time.monotonic() + seconds
    last = booted
    while time.monotonic() < deadline:
        time.sleep(10)
        s = status(host)
        if s is None:
            continue
        if restarted(s, last):
            print(f"{host} restarted before confirming; it now runs {s.get('firmware', '?')} "
                  "(rolled back)", file=sys.stderr)
            return 1
        last = s
        if not s.get("ota", {}).get("unconfirmed"):
            print(f"{host} confirmed {s.get('firmware', '?')}")
            return 0
    print(f"{host} still unconfirmed after {seconds // 60} min "
          f"(last error: {last.get('upload', {}).get('last_error') or 'none'})", file=sys.stderr)
    return 1


def status(host):
    """The station's status JSON, or None if it does not answer."""
    try:
        with urllib.request.urlopen(f"http://{host}/", timeout=5) as r:
            return json.loads(r.read().decode())
    except (OSError, ValueError):  # URLError, timeouts and resets are OSErrors
        return None


def wait_for_restart(host, prev, seconds=180):
    """Waits until the station answers from a later boot than status `prev`
    and returns that status, or None. Judged by boot count and uptime, not by
    the station going away: the restart can come late (after a running upload)
    or so quickly that no poll misses it."""
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        s = status(host)
        if s is not None:
            if restarted(s, prev):
                return s
            prev = s
        time.sleep(1)
    print(f"{host} did not restart within {seconds} s (it may have a new address)", file=sys.stderr)
    return None


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
