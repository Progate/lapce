//! WASI。std が preview1 の `fd_filestat_get` / `fd_filestat_set_times` を
//! 包んでいるので、それを使う（`wasm.rs` は wasm32-unknown-unknown 用の空の実装）

use crate::FileTime;
use std::fs::{self, File, FileTimes, OpenOptions};
use std::io;
use std::path::Path;
use std::time::SystemTime;

fn to_system_time(time: FileTime) -> SystemTime {
    SystemTime::UNIX_EPOCH + std::time::Duration::new(time.unix_seconds() as u64, time.nanoseconds())
}

fn open_for_times(p: &Path) -> io::Result<File> {
    // 時刻を変えるだけなので中身は触らない。ディレクトリも開けるよう read で開く
    OpenOptions::new().read(true).open(p)
}

pub fn set_file_times(p: &Path, atime: FileTime, mtime: FileTime) -> io::Result<()> {
    set_file_handle_times(&open_for_times(p)?, Some(atime), Some(mtime))
}

pub fn set_symlink_file_times(p: &Path, atime: FileTime, mtime: FileTime) -> io::Result<()> {
    // preview1 の path_filestat_set_times はリンクを辿らない指定もできるが、std は出していない
    set_file_times(p, atime, mtime)
}

pub fn set_file_mtime(p: &Path, mtime: FileTime) -> io::Result<()> {
    set_file_handle_times(&open_for_times(p)?, None, Some(mtime))
}

pub fn set_file_atime(p: &Path, atime: FileTime) -> io::Result<()> {
    set_file_handle_times(&open_for_times(p)?, Some(atime), None)
}

pub fn from_last_modification_time(meta: &fs::Metadata) -> FileTime {
    meta.modified().map(FileTime::from_system_time).unwrap_or_else(|_| FileTime::zero())
}

pub fn from_last_access_time(meta: &fs::Metadata) -> FileTime {
    meta.accessed().map(FileTime::from_system_time).unwrap_or_else(|_| FileTime::zero())
}

pub fn from_creation_time(meta: &fs::Metadata) -> Option<FileTime> {
    meta.created().ok().map(FileTime::from_system_time)
}

pub fn set_file_handle_times(
    f: &File,
    atime: Option<FileTime>,
    mtime: Option<FileTime>,
) -> io::Result<()> {
    let mut times = FileTimes::new();
    if let Some(atime) = atime {
        times = times.set_accessed(to_system_time(atime));
    }
    if let Some(mtime) = mtime {
        times = times.set_modified(to_system_time(mtime));
    }
    f.set_times(times)
}
