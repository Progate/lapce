//! `directories` の代わり（WASI）。
//!
//! `directories` は WASI で置き場を決められず、何を聞いても `None` を返す。BrowserOS の
//! ファイルシステムは Linux と同じ形なので、**Linux 版の `directories` が返すのと同じ
//! 場所**（XDG Base Directory）をここで決める

use std::path::{Path, PathBuf};

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// `$XDG_*` が絶対パスならそれ、無ければ `$HOME` の下の既定の場所
fn xdg(variable: &str, fallback: &str) -> Option<PathBuf> {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| home().map(|home| home.join(fallback)))
}

pub struct BaseDirs {
    home_dir: PathBuf,
}

impl BaseDirs {
    pub fn new() -> Option<Self> {
        Some(Self { home_dir: home()? })
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }
}

pub struct UserDirs {
    home_dir: PathBuf,
}

impl UserDirs {
    pub fn new() -> Option<Self> {
        Some(Self { home_dir: home()? })
    }

    pub fn home_dir(&self) -> &Path {
        &self.home_dir
    }
}

pub struct ProjectDirs {
    config_dir: PathBuf,
    data_local_dir: PathBuf,
}

impl ProjectDirs {
    /// Linux の `directories` と同じく、名前は小文字にして空白を詰める
    pub fn from(_qualifier: &str, _organization: &str, application: &str) -> Option<Self> {
        let name: String = application
            .trim()
            .to_lowercase()
            .split_whitespace()
            .collect();
        Some(Self {
            config_dir: xdg("XDG_CONFIG_HOME", ".config")?.join(&name),
            data_local_dir: xdg("XDG_DATA_HOME", ".local/share")?.join(&name),
        })
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn data_local_dir(&self) -> &Path {
        &self.data_local_dir
    }
}
