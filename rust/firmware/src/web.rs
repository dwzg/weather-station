//! The station's own web server:
//!
//!   GET  /           status JSON (read-only, no secrets)
//!   GET  /log        the last 40 log lines
//!   GET  /ota/nonce  a one-time nonce for an update
//!   POST /ota        a firmware image, authenticated (see ws_core::ota)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::anyhow;
use esp_idf_svc::http::server::{Configuration, EspHttpConnection, EspHttpServer, Request};
use esp_idf_svc::http::Method;
use esp_idf_svc::io::Write;
use esp_idf_svc::ota::EspOta;
use log::{info, warn};
use ws_core::ota::{self, ImageHash, Key, Nonces};

use crate::{config, logger, platform};

static STATUS: Mutex<String> = Mutex::new(String::new());
static NONCES: Mutex<Nonces> = Mutex::new(Nonces::new(config::OTA_NONCE_TTL_MS));
static OTA_BUSY: AtomicBool = AtomicBool::new(false);
static RESTART_FOR_UPDATE: AtomicBool = AtomicBool::new(false);
static ROLLBACK_FOR_UPDATE: AtomicBool = AtomicBool::new(false);
/// Biggest image an app slot takes (partitions.csv).
const MAX_IMAGE: usize = 0x18_0000;

/// The main loop renders the status; the server only hands it out.
pub fn set_status(json: String) {
    *STATUS.lock().unwrap() = json;
}

/// An update was written and activated: commit, then restart.
pub fn restart_requested() -> bool {
    RESTART_FOR_UPDATE.load(Ordering::Relaxed)
}

/// An authenticated update arrived while the running image is unconfirmed:
/// go back to the previous image, so the update can be installed there.
pub fn rollback_requested() -> bool {
    ROLLBACK_FOR_UPDATE.load(Ordering::Relaxed)
}

pub fn clear_rollback() {
    ROLLBACK_FOR_UPDATE.store(false, Ordering::Relaxed);
}

pub fn ota_in_progress() -> bool {
    OTA_BUSY.load(Ordering::Relaxed)
}

const NO_STORE: (&str, &str) = ("Cache-Control", "no-store");

pub fn start(ota_key: Key) -> anyhow::Result<EspHttpServer<'static>> {
    let conf = Configuration { stack_size: 10_240, ..Default::default() };
    let mut server = EspHttpServer::new(&conf)?;

    server.fn_handler("/", Method::Get, |req| -> anyhow::Result<()> {
        let body = STATUS.lock().unwrap().clone();
        req.into_response(200, None, &[("Content-Type", "application/json"), NO_STORE])?.write_all(body.as_bytes())?;
        Ok(())
    })?;

    server.fn_handler("/log", Method::Get, |req| -> anyhow::Result<()> {
        let body = logger::dump();
        req.into_response(200, None, &[("Content-Type", "text/plain; charset=utf-8"), NO_STORE])?
            .write_all(body.as_bytes())?;
        Ok(())
    })?;

    server.fn_handler("/ota/nonce", Method::Get, |req| -> anyhow::Result<()> {
        let n = NONCES.lock().unwrap().issue(platform::random16(), platform::millis());
        req.into_response(200, None, &[("Content-Type", "text/plain"), NO_STORE])?.write_all(n.as_bytes())?;
        Ok(())
    })?;

    server.fn_handler("/ota", Method::Post, move |mut req| -> anyhow::Result<()> {
        let header = |name: &str| req.header(name).unwrap_or("").trim().to_string();
        let (nonce, sha, auth) =
            (header("X-OTA-Nonce"), header("X-OTA-SHA256").to_ascii_lowercase(), header("X-OTA-Auth"));
        let len: usize = header("Content-Length").parse().unwrap_or(0);

        if !NONCES.lock().unwrap().take(&nonce, platform::millis()) {
            return refuse(req, 403, "unknown or expired nonce");
        }
        if !ota::verify(&ota_key, &nonce, &sha, len, &auth) {
            return refuse(req, 403, "bad signature");
        }
        if len == 0 || len > MAX_IMAGE {
            return refuse(req, 413, "image size");
        }
        // Only an authenticated update holds off uploads and planned restarts:
        // anyone on the network can send a request.
        if OTA_BUSY.swap(true, Ordering::AcqRel) {
            return refuse(req, 409, "another update is running");
        }
        struct Done;
        impl Drop for Done {
            fn drop(&mut self) {
                OTA_BUSY.store(false, Ordering::Release);
            }
        }
        let _done = Done;

        if platform::running_image_unconfirmed() {
            // ESP-IDF writes no new image while the running one is unconfirmed.
            // Return to the previous, confirmed image; the updater retries there.
            ROLLBACK_FOR_UPDATE.store(true, Ordering::Release);
            return refuse(req, 409, "rollback: this image is unconfirmed, returning to the previous one; retry then");
        }

        info!("ota: receiving {len} bytes");
        let mut esp_ota = EspOta::new()?;
        let mut update = esp_ota.initiate_update_with_known_size(len)?;
        let mut hash = ImageHash::default();
        let mut buf = vec![0u8; 4096];
        let mut got = 0usize;
        while got < len {
            let n = req.read(&mut buf).map_err(|e| anyhow!("receive: {e:?}"))?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
            update.write(&buf[..n])?;
            got += n;
        }
        if got != len {
            drop(update); // aborts: the slot stays inactive
            return refuse(req, 400, "image truncated");
        }
        if hash.finish_hex() != sha {
            drop(update);
            return refuse(req, 400, "image does not match its hash");
        }
        // Checks the image (chip, header, checksum) and makes it the boot image.
        if let Err(e) = update.complete() {
            return refuse(req, 400, &format!("image rejected: {e}"));
        }
        info!("ota: update written, restarting");
        req.into_ok_response()?.write_all(b"ok, restarting\n")?;
        RESTART_FOR_UPDATE.store(true, Ordering::Release);
        Ok(())
    })?;

    Ok(server)
}

fn refuse(req: Request<&mut EspHttpConnection<'_>>, code: u16, why: &str) -> anyhow::Result<()> {
    warn!("ota: refused ({why})");
    req.into_status_response(code)?.write_all(why.as_bytes())?;
    Ok(())
}
