use anyhow::{anyhow, Result};
use image::RgbaImage;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::time::Duration;

// keep at least this much of the volume free for the rest of the system
const DISK_RESERVE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_SPOOL_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MIN_SPOOL_BYTES: u64 = 64 * 1024 * 1024;
// re-probe free space every N frames so another process filling the volume
// mid-recording still stops us before the disk runs dry
const REPROBE_INTERVAL: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct FrameMeta {
    pub at: Duration,
    pub width: u32,
    pub height: u32,
    offset: u64,
}

// disk-backed frame store: recording RAM stays flat no matter how long the
// capture runs. frames are raw rgba so the capture thread pays one sequential
// write per frame and no encode cost; the file lives in the temp dir and is
// removed on drop
pub struct FrameSpool {
    file: File,
    path: PathBuf,
    metas: Vec<FrameMeta>,
    bytes_written: u64,
    byte_budget: u64,
}

impl FrameSpool {
    pub fn create() -> Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "capscr_frames_{}.rgba",
            uuid::Uuid::new_v4().as_simple()
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        let byte_budget = spool_budget(free_disk_space(&path));
        Ok(Self {
            file,
            path,
            metas: Vec::new(),
            bytes_written: 0,
            byte_budget,
        })
    }

    /// appends a frame. Ok(false) means the disk budget is exhausted and the
    /// frame was not written — the recording should stop gracefully
    pub fn push(&mut self, image: &RgbaImage, at: Duration) -> Result<bool> {
        let len = image.as_raw().len() as u64;
        if self.bytes_written.saturating_add(len) > self.byte_budget {
            return Ok(false);
        }
        if self.metas.len() % REPROBE_INTERVAL == 0 {
            let live = spool_budget(free_disk_space(&self.path).map(|f| f + self.bytes_written));
            self.byte_budget = self.byte_budget.min(live);
            if self.bytes_written.saturating_add(len) > self.byte_budget {
                return Ok(false);
            }
        }
        self.file.write_all(image.as_raw())?;
        self.metas.push(FrameMeta {
            at,
            width: image.width(),
            height: image.height(),
            offset: self.bytes_written,
        });
        self.bytes_written += len;
        Ok(true)
    }

    pub fn len(&self) -> usize {
        self.metas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.metas.is_empty()
    }

    pub fn metas(&self) -> &[FrameMeta] {
        &self.metas
    }

    pub fn read_frame(&mut self, idx: usize) -> Result<RgbaImage> {
        let meta = *self
            .metas
            .get(idx)
            .ok_or_else(|| anyhow!("frame index {idx} out of range"))?;
        let len = (meta.width as usize)
            .checked_mul(meta.height as usize)
            .and_then(|p| p.checked_mul(4))
            .ok_or_else(|| anyhow!("frame dimensions overflow"))?;
        let mut buffer = vec![0u8; len];
        self.file.seek(SeekFrom::Start(meta.offset))?;
        self.file.read_exact(&mut buffer)?;
        // subsequent pushes append at the tracked offset regardless of the
        // cursor, but reads normally only happen after capture has ended
        self.file.seek(SeekFrom::Start(self.bytes_written))?;
        RgbaImage::from_raw(meta.width, meta.height, buffer)
            .ok_or_else(|| anyhow!("frame buffer does not match dimensions"))
    }
}

impl Drop for FrameSpool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn spool_budget(free: Option<u64>) -> u64 {
    match free {
        Some(f) => f
            .saturating_sub(DISK_RESERVE_BYTES)
            .clamp(MIN_SPOOL_BYTES, MAX_SPOOL_BYTES),
        None => MAX_SPOOL_BYTES,
    }
}

#[cfg(windows)]
fn free_disk_space(path: &std::path::Path) -> Option<u64> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let dir = path.parent()?;
    let wide: Vec<u16> = dir
        .as_os_str()
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut available: u64 = 0;
    unsafe {
        GetDiskFreeSpaceExW(PCWSTR(wide.as_ptr()), Some(&mut available), None, None).ok()?;
    }
    Some(available)
}

#[cfg(not(windows))]
fn free_disk_space(_path: &std::path::Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid_frame(w: u32, h: u32, v: u8) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba([v, v, v, 255]))
    }

    #[test]
    fn roundtrips_frames_through_disk() {
        let mut spool = FrameSpool::create().unwrap();
        for i in 0..5u8 {
            let ok = spool
                .push(&solid_frame(16, 8, i * 40), Duration::from_millis(i as u64 * 100))
                .unwrap();
            assert!(ok);
        }
        assert_eq!(spool.len(), 5);
        for i in (0..5usize).rev() {
            let frame = spool.read_frame(i).unwrap();
            assert_eq!(frame.width(), 16);
            assert_eq!(frame.get_pixel(3, 3)[0], i as u8 * 40);
        }
        assert_eq!(spool.metas()[3].at, Duration::from_millis(300));
    }

    #[test]
    fn push_still_appends_after_a_read() {
        let mut spool = FrameSpool::create().unwrap();
        spool.push(&solid_frame(4, 4, 10), Duration::ZERO).unwrap();
        let _ = spool.read_frame(0).unwrap();
        spool
            .push(&solid_frame(4, 4, 20), Duration::from_millis(50))
            .unwrap();
        assert_eq!(spool.read_frame(0).unwrap().get_pixel(0, 0)[0], 10);
        assert_eq!(spool.read_frame(1).unwrap().get_pixel(0, 0)[0], 20);
    }

    #[test]
    fn rejects_frames_past_budget() {
        let mut spool = FrameSpool::create().unwrap();
        spool.byte_budget = 100;
        assert!(spool.push(&solid_frame(4, 4, 1), Duration::ZERO).unwrap());
        assert!(!spool.push(&solid_frame(4, 4, 2), Duration::ZERO).unwrap());
        assert_eq!(spool.len(), 1);
    }

    #[test]
    fn removes_file_on_drop() {
        let path;
        {
            let mut spool = FrameSpool::create().unwrap();
            spool.push(&solid_frame(4, 4, 1), Duration::ZERO).unwrap();
            path = spool.path.clone();
            assert!(path.exists());
        }
        assert!(!path.exists());
    }

    #[test]
    fn budget_respects_reserve_and_caps() {
        assert_eq!(spool_budget(None), MAX_SPOOL_BYTES);
        assert_eq!(spool_budget(Some(0)), MIN_SPOOL_BYTES);
        assert_eq!(
            spool_budget(Some(DISK_RESERVE_BYTES + MIN_SPOOL_BYTES * 2)),
            MIN_SPOOL_BYTES * 2
        );
        assert_eq!(spool_budget(Some(u64::MAX)), MAX_SPOOL_BYTES);
    }
}
