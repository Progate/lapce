//! `open` クレートの代わり（WASI）。
//!
//! Linux ではファイルや URL を「既定のアプリで開く」のは `xdg-open` の仕事で、
//! BrowserOS にはまだその係が居ない。**開けなかったことを返す**だけにする

use std::{ffi::OsStr, io};

pub fn that<T: AsRef<OsStr>>(path: T) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "cannot open {} with another application on WASI",
            path.as_ref().to_string_lossy()
        ),
    ))
}
