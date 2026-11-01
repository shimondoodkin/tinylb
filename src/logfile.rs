//! Size-based rotating log file, so tinylb can manage its own log inside a
//! container where there is no logrotate.
//!
//! `tinylb.log` is the live file; on rotation it becomes `tinylb.log.1`,
//! `.1` becomes `.2`, and so on up to `max_files`; the oldest is deleted.
//! Each log line is written under one lock, so lines never split across files.
//! Write errors (e.g. disk full) are swallowed — logging must never take the
//! proxy down.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tracing_subscriber::fmt::MakeWriter;

struct Inner {
    path: PathBuf,
    file: Option<File>,
    size: u64,
    max_size: u64,
    max_files: u32,
}

impl Inner {
    fn open(path: &Path) -> io::Result<(File, u64)> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok((file, size))
    }

    fn backup(&self, n: u32) -> PathBuf {
        let mut s = self.path.clone().into_os_string();
        s.push(format!(".{}", n));
        PathBuf::from(s)
    }

    fn rotate(&mut self) {
        self.file = None; // close before renaming (required on Windows)
        if self.max_files == 0 {
            let _ = fs::remove_file(&self.path);
        } else {
            let _ = fs::remove_file(self.backup(self.max_files));
            for n in (1..self.max_files).rev() {
                let _ = fs::rename(self.backup(n), self.backup(n + 1));
            }
            let _ = fs::rename(&self.path, self.backup(1));
        }
        match Self::open(&self.path) {
            Ok((f, size)) => {
                self.file = Some(f);
                self.size = size;
            }
            Err(_) => self.size = 0, // retried on next write
        }
    }

    fn write_line(&mut self, buf: &[u8]) {
        if self.size > 0 && self.size + buf.len() as u64 > self.max_size {
            self.rotate();
        }
        if self.file.is_none() {
            if let Ok((f, size)) = Self::open(&self.path) {
                self.file = Some(f);
                self.size = size;
            }
        }
        if let Some(f) = self.file.as_mut() {
            if f.write_all(buf).is_ok() {
                self.size += buf.len() as u64;
            }
        }
    }
}

#[derive(Clone)]
pub struct RotatingFile {
    inner: Arc<Mutex<Inner>>,
}

impl RotatingFile {
    pub fn new(path: impl Into<PathBuf>, max_size: u64, max_files: u32) -> io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir)?;
        }
        let (file, size) = Inner::open(&path)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(Inner {
                path,
                file: Some(file),
                size,
                max_size: max_size.max(1),
                max_files,
            })),
        })
    }
}

pub struct RotatingWriter<'a> {
    guard: MutexGuard<'a, Inner>,
}

impl Write for RotatingWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.guard.write_line(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(f) = self.guard.file.as_mut() {
            let _ = f.flush();
        }
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for RotatingFile {
    type Writer = RotatingWriter<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        let guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        RotatingWriter { guard }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tinylb-logtest-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(rf: &RotatingFile, s: &str) {
        let mut w = rf.make_writer();
        w.write_all(s.as_bytes()).unwrap();
    }

    #[test]
    fn rotates_at_max_size_and_keeps_max_files() {
        let d = tmpdir("rotate");
        let p = d.join("lb.log");
        let rf = RotatingFile::new(&p, 100, 2).unwrap();
        let line = format!("{}\n", "x".repeat(49)); // 50 bytes
        for _ in 0..10 {
            write(&rf, &line);
        }
        // 10 lines, 2 per file → live + .1 + .2 kept, older deleted.
        assert_eq!(fs::metadata(&p).unwrap().len(), 100);
        assert_eq!(fs::metadata(d.join("lb.log.1")).unwrap().len(), 100);
        assert_eq!(fs::metadata(d.join("lb.log.2")).unwrap().len(), 100);
        assert!(!d.join("lb.log.3").exists());
        let total: u64 = fs::read_dir(&d).unwrap().map(|e| e.unwrap().metadata().unwrap().len()).sum();
        assert!(total <= 300);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn max_files_zero_truncates_instead() {
        let d = tmpdir("zero");
        let p = d.join("lb.log");
        let rf = RotatingFile::new(&p, 100, 0).unwrap();
        for _ in 0..5 {
            write(&rf, &format!("{}\n", "y".repeat(59)));
        }
        assert_eq!(fs::metadata(&p).unwrap().len(), 60);
        assert!(!d.join("lb.log.1").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn appends_to_existing_file_and_counts_its_size() {
        let d = tmpdir("append");
        let p = d.join("lb.log");
        fs::write(&p, "z".repeat(90)).unwrap();
        let rf = RotatingFile::new(&p, 100, 1).unwrap();
        write(&rf, &"a".repeat(20));
        assert_eq!(fs::metadata(d.join("lb.log.1")).unwrap().len(), 90);
        assert_eq!(fs::metadata(&p).unwrap().len(), 20);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn creates_parent_directory() {
        let d = tmpdir("mkdir");
        let p = d.join("sub/dir/lb.log");
        let rf = RotatingFile::new(&p, 100, 1).unwrap();
        write(&rf, "hi\n");
        assert!(p.exists());
        let _ = fs::remove_dir_all(&d);
    }
}
