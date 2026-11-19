//! Keeps the station on the strongest access point of its network. Instead of
//! reconnecting to whichever AP it saw first, it scans, joins the best BSSID
//! and re-checks now and then while the signal is weak.

use std::ffi::CString;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::handle::RawHandle;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys;
use esp_idf_svc::wifi::{ClientConfiguration, Configuration, EspWifi};
use log::{info, warn};
use ws_core::backoff::Backoff;

use crate::{config, platform};

const JOIN_TIMEOUT_MS: u64 = 20_000;
const WEAK_RSSI: i8 = -75;
const WEAK_FOR_MS: u64 = 10 * 60 * 1000;
const ROAM_CHECK_EVERY_MS: u64 = 30 * 60 * 1000;
const ROAM_GAIN_DB: i16 = 8;

#[derive(Clone, Copy, Debug, Default)]
pub struct WifiStats {
    pub connects: u32,
    pub drops: u32,
    pub roams: u32,
    pub up_since_ms: u64,
    pub down_since_ms: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Down,
    Joining,
    Up,
}

pub struct WifiLink {
    wifi: EspWifi<'static>,
    ssid: &'static str,
    pass: &'static str,
    state: State,
    state_ms: u64,
    down_at_ms: u64,
    wait_ms: u64,
    last_roam_check_ms: u64,
    weak_since_ms: Option<u64>,
    backoff: Backoff,
    stats: WifiStats,
}

fn mac(b: &[u8; 6]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(":")
}

impl WifiLink {
    pub fn new(
        modem: Modem<'static>,
        sysloop: EspSystemEventLoop,
        nvs: EspDefaultNvsPartition,
        ssid: &'static str,
        pass: &'static str,
    ) -> anyhow::Result<Self> {
        let mut wifi = EspWifi::new(modem, sysloop, Some(nvs))?;
        let host = CString::new(config::HOSTNAME)?;
        // SAFETY: the netif lives as long as `wifi`; the name is copied.
        sys::esp!(unsafe { sys::esp_netif_set_hostname(wifi.sta_netif().handle(), host.as_ptr()) })?;
        // Never write credentials to flash on every join.
        sys::esp!(unsafe { sys::esp_wifi_set_storage(sys::wifi_storage_t_WIFI_STORAGE_RAM) })?;
        wifi.set_configuration(&Configuration::Client(Self::client(ssid, pass, None)))?;
        wifi.start()?;
        // Mains powered: modem sleep only costs reliability.
        sys::esp!(unsafe { sys::esp_wifi_set_ps(sys::wifi_ps_type_t_WIFI_PS_NONE) })?;
        sys::esp!(unsafe { sys::esp_wifi_set_max_tx_power(config::WIFI_TX_QUARTER_DBM) })?;
        let now = platform::millis();
        Ok(Self {
            wifi,
            ssid,
            pass,
            state: State::Down,
            state_ms: now,
            down_at_ms: now,
            wait_ms: 0,
            last_roam_check_ms: 0,
            weak_since_ms: None,
            backoff: Backoff::new(5000, 5 * 60 * 1000),
            stats: WifiStats { down_since_ms: now, ..Default::default() },
        })
    }

    fn client(ssid: &str, pass: &str, ap: Option<([u8; 6], u8)>) -> ClientConfiguration {
        ClientConfiguration {
            ssid: ssid.try_into().unwrap_or_default(),
            password: pass.try_into().unwrap_or_default(),
            bssid: ap.map(|a| a.0),
            channel: ap.map(|a| a.1),
            ..Default::default()
        }
    }

    pub fn up(&self) -> bool {
        self.state == State::Up
    }

    pub fn stats(&self) -> &WifiStats {
        &self.stats
    }

    /// RSSI and BSSID of the current AP.
    pub fn ap(&self) -> Option<(i8, [u8; 6])> {
        self.up().then(|| self.wifi.driver().get_ap_info().ok()).flatten().map(|a| (a.signal_strength, a.bssid))
    }

    pub fn ip(&self) -> String {
        if !self.up() {
            return String::new();
        }
        self.wifi.sta_netif().get_ip_info().map(|i| i.ip.to_string()).unwrap_or_default()
    }

    fn linked(&self) -> bool {
        self.wifi.is_up().unwrap_or(false)
    }

    /// Scans and joins the strongest AP of the network. With `only_if_better`,
    /// joins only if that is not the current AP and at least ROAM_GAIN_DB
    /// stronger. True if a join was started.
    fn join_best(&mut self, only_if_better: bool) -> bool {
        if !only_if_better {
            let _ = self.wifi.disconnect(); // a join still in progress makes the scan fail
        }
        let aps = self.wifi.scan().unwrap_or_default();
        let best = aps.iter().filter(|a| a.ssid.as_str() == self.ssid).max_by_key(|a| a.signal_strength);
        let target = match best {
            None if only_if_better => return false,
            None => {
                warn!("wifi: '{}' not seen in scan ({} networks), joining blind", self.ssid, aps.len());
                None
            }
            Some(b) => {
                if only_if_better {
                    let Some((rssi, bssid)) = self.ap() else { return false };
                    if b.bssid == bssid || i16::from(b.signal_strength) < i16::from(rssi) + ROAM_GAIN_DB {
                        return false;
                    }
                    info!("wifi: roaming to {} ({} dBm vs {} dBm)", mac(&b.bssid), b.signal_strength, rssi);
                    self.stats.roams += 1;
                } else {
                    info!("wifi: joining {} ch {}, {} dBm", mac(&b.bssid), b.channel, b.signal_strength);
                }
                Some((b.bssid, b.channel))
            }
        };
        let _ = self.wifi.disconnect();
        let conf = Configuration::Client(Self::client(self.ssid, self.pass, target));
        if let Err(e) = self.wifi.set_configuration(&conf).and_then(|_| self.wifi.connect()) {
            warn!("wifi: join failed to start ({e})");
        }
        true
    }

    /// `quiet` asks it not to start a blocking scan (a measurement is due).
    pub fn poll(&mut self, quiet: bool) {
        let now = platform::millis();
        let linked = self.linked();
        match self.state {
            State::Up => {
                if !linked {
                    self.stats.drops += 1;
                    self.stats.down_since_ms = now;
                    warn!("wifi: link lost after {}s", (now - self.stats.up_since_ms) / 1000);
                    self.state = State::Down;
                    self.down_at_ms = now;
                    self.wait_ms = 1000;
                    return;
                }
                match self.ap() {
                    Some((rssi, _)) if rssi < WEAK_RSSI => {
                        self.weak_since_ms.get_or_insert(now);
                    }
                    _ => self.weak_since_ms = None,
                }
                let weak_long = self.weak_since_ms.is_some_and(|t| now - t > WEAK_FOR_MS);
                if !quiet && weak_long && now - self.last_roam_check_ms > ROAM_CHECK_EVERY_MS {
                    self.last_roam_check_ms = now;
                    if self.join_best(true) {
                        self.state = State::Joining;
                        self.state_ms = platform::millis();
                        self.stats.down_since_ms = now;
                    }
                }
            }
            State::Joining => {
                if linked {
                    self.state = State::Up;
                    self.stats.up_since_ms = now;
                    self.stats.connects += 1;
                    self.backoff.reset();
                    self.weak_since_ms = None;
                    let rssi = self.ap().map_or(0, |a| a.0);
                    info!(
                        "wifi: up, {}, {} dBm, down for {}s",
                        self.ip(),
                        rssi,
                        (now - self.stats.down_since_ms) / 1000
                    );
                } else if now - self.state_ms > JOIN_TIMEOUT_MS {
                    let _ = self.wifi.disconnect();
                    self.state = State::Down;
                    self.wait_ms = u64::from(self.backoff.next(platform::random()));
                    self.down_at_ms = now;
                    warn!("wifi: join timed out, next try in {}s", self.wait_ms / 1000);
                }
            }
            State::Down => {
                if linked {
                    // Came back by itself.
                    self.state = State::Joining;
                    self.state_ms = now;
                    return;
                }
                if quiet || now - self.down_at_ms < self.wait_ms {
                    return;
                }
                self.join_best(false);
                self.state = State::Joining;
                self.state_ms = platform::millis();
            }
        }
    }
}
