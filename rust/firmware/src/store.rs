//! The queue's segments as files <base>/q/<id> on LittleFS; the cursor in
//! <base>/q/cursor, replaced atomically by writing a temporary file and renaming it.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};

use esp_idf_svc::sys;
use log::warn;
use ws_core::queue::SegmentStore;

pub struct LittleFsStore {
    dir: String,
    label: CString,
}

impl LittleFsStore {
    /// Mounts the partition (formatting it if it cannot be mounted).
    pub fn mount(base: &str, partition: &str) -> Option<Self> {
        let c_base = CString::new(base).ok()?;
        let label = CString::new(partition).ok()?;
        let mut conf = sys::esp_vfs_littlefs_conf_t {
            base_path: c_base.as_ptr(),
            partition_label: label.as_ptr(),
            ..Default::default()
        };
        conf.set_format_if_mount_failed(1);
        // SAFETY: the strings outlive the call; the VFS copies the base path.
        if let Err(e) = sys::esp!(unsafe { sys::esp_vfs_littlefs_register(&conf) }) {
            warn!("fs: mount failed ({e})");
            return None;
        }
        let dir = format!("{base}/q");
        if fs::metadata(&dir).is_err() {
            if let Err(e) = fs::create_dir(&dir) {
                warn!("fs: cannot create {dir}: {e}");
                return None;
            }
        }
        Some(Self { dir, label })
    }

    fn seg_path(&self, id: u32) -> String {
        format!("{}/{id:08x}", self.dir)
    }

    /// (used, total) bytes.
    pub fn usage(&self) -> (usize, usize) {
        let (mut total, mut used) = (0usize, 0usize);
        // SAFETY: plain out-parameters.
        let err = unsafe { sys::esp_littlefs_info(self.label.as_ptr(), &mut total, &mut used) };
        if err == sys::ESP_OK {
            (used, total)
        } else {
            (0, 0)
        }
    }
}

impl SegmentStore for LittleFsStore {
    fn list_segments(&mut self) -> Option<Vec<u32>> {
        let ids = fs::read_dir(&self.dir)
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|e| {
                let name = e.file_name().into_string().ok()?;
                // Skips the cursor files.
                (name.len() == 8).then(|| u32::from_str_radix(&name, 16).ok()).flatten()
            })
            .filter(|&id| id > 0)
            .collect();
        Some(ids)
    }

    fn segment_size(&mut self, id: u32) -> Option<u32> {
        fs::metadata(self.seg_path(id)).ok().map(|m| m.len() as u32)
    }

    fn append(&mut self, id: u32, data: &[u8]) -> bool {
        let Ok(mut f) = OpenOptions::new().create(true).append(true).open(self.seg_path(id)) else { return false };
        // LittleFS commits on sync/close; sync_all reports a failed commit.
        f.write_all(data).is_ok() && f.sync_all().is_ok()
    }

    fn read(&mut self, id: u32, offset: u32, buf: &mut [u8]) -> bool {
        let Ok(mut f) = File::open(self.seg_path(id)) else { return false };
        f.seek(SeekFrom::Start(u64::from(offset))).is_ok() && f.read_exact(buf).is_ok()
    }

    fn remove_segment(&mut self, id: u32) -> bool {
        fs::remove_file(self.seg_path(id)).is_ok()
    }

    fn load_cursor(&mut self, buf: &mut [u8]) -> bool {
        let Ok(mut f) = File::open(format!("{}/cursor", self.dir)) else { return false };
        f.metadata().is_ok_and(|m| m.len() == buf.len() as u64) && f.read_exact(buf).is_ok()
    }

    fn save_cursor(&mut self, data: &[u8]) -> bool {
        let tmp = format!("{}/cursor.tmp", self.dir);
        let ok = File::create(&tmp).is_ok_and(|mut f| f.write_all(data).is_ok() && f.sync_all().is_ok());
        ok && fs::rename(&tmp, format!("{}/cursor", self.dir)).is_ok()
    }
}
