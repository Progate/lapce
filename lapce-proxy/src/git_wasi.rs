//! WASI（BrowserOS）のソース管理。**`git` コマンドを子プロセスとして呼ぶ。**
//!
//! ネイティブ版は libgit2 を静的にリンクしている（→ `git.rs`）。BrowserOS には
//! 本物の git（`/usr/bin/git`）が居て、子プロセスは `posix_spawnp` で起動できる
//! （→ `wasi_process.rs`）ので、ここは VS Code と同じく CLI を呼ぶ形にする。
//! 関数の名前と意味は `git.rs` と揃えてある

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use lapce_rpc::source_control::{DiffInfo, FileDiff};

use crate::wasi_process::{self, Output};

fn git(workspace_path: &Path, args: &[&str]) -> Result<Output> {
    let mut argv = vec!["git"];
    argv.extend_from_slice(args);
    Ok(wasi_process::output(&argv, workspace_path, None)?)
}

/// 成功したときの標準出力。失敗なら標準エラーを理由にする
fn git_ok(workspace_path: &Path, args: &[&str]) -> Result<String> {
    let output = git(workspace_path, args)?;
    if !output.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::trim).filter(|line| !line.is_empty()).map(str::to_owned).collect()
}

/// リポジトリの根。ワークスペースがリポジトリの中に無ければ `None`
fn toplevel(workspace_path: &Path) -> Option<PathBuf> {
    let text = match git_ok(workspace_path, &["rev-parse", "--show-toplevel"]) {
        Ok(text) => text,
        Err(err) => {
            // リポジトリでないだけのことが多い。起動できなかったのか区別できるよう残す
            tracing::debug!("{err:?}");
            return None;
        }
    };
    let path = text.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub(crate) fn git_init(workspace_path: &Path) -> Result<()> {
    if toplevel(workspace_path).is_none() {
        git_ok(workspace_path, &["init"])?;
    }
    Ok(())
}

pub(crate) fn git_commit(
    workspace_path: &Path,
    message: &str,
    diffs: Vec<FileDiff>,
) -> Result<()> {
    for diff in &diffs {
        match diff {
            FileDiff::Modified(path) | FileDiff::Added(path) => {
                git_ok(workspace_path, &["add", "--", &path.to_string_lossy()])?;
            }
            FileDiff::Renamed(new, old) => {
                git_ok(workspace_path, &["add", "--", &new.to_string_lossy()])?;
                git_ok(workspace_path, &["rm", "--cached", "--", &old.to_string_lossy()])?;
            }
            FileDiff::Deleted(path) => {
                git_ok(workspace_path, &["rm", "--cached", "--", &path.to_string_lossy()])?;
            }
        }
    }
    git_ok(workspace_path, &["commit", "-m", message])?;
    Ok(())
}

pub(crate) fn git_checkout(workspace_path: &Path, reference: &str) -> Result<()> {
    git_ok(workspace_path, &["checkout", reference])?;
    Ok(())
}

pub(crate) fn git_discard_files_changes<'a>(
    workspace_path: &Path,
    files: impl Iterator<Item = &'a Path>,
) -> Result<()> {
    let files: Vec<String> = files.map(|path| path.to_string_lossy().into_owned()).collect();
    if files.is_empty() {
        return Ok(());
    }
    let mut args = vec!["checkout", "--"];
    args.extend(files.iter().map(String::as_str));
    git_ok(workspace_path, &args)?;
    Ok(())
}

pub(crate) fn git_discard_workspace_changes(workspace_path: &Path) -> Result<()> {
    git_ok(workspace_path, &["reset", "--hard", "HEAD"])?;
    Ok(())
}

/// `git status --porcelain` の 1 行。`XY path` か `XY old -> new`
fn parse_status_line(root: &Path, line: &str) -> Vec<FileDiff> {
    if line.len() < 4 {
        return Vec::new();
    }
    let (code, path) = line.split_at(3);
    let code = code.trim();
    if let Some((old, new)) = path.split_once(" -> ") {
        return vec![FileDiff::Renamed(root.join(new), root.join(old))];
    }
    let absolute = root.join(path);
    // 追跡していないディレクトリは中のファイルを並べる（libgit2 の recurse_untracked_dirs）
    if code == "??" && path.ends_with('/') {
        return walkdir::WalkDir::new(&absolute)
            .into_iter()
            .flatten()
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| FileDiff::Added(entry.into_path()))
            .collect();
    }
    let diff = if code == "??" || code.contains('A') {
        FileDiff::Added(absolute)
    } else if code.contains('D') {
        FileDiff::Deleted(absolute)
    } else {
        FileDiff::Modified(absolute)
    };
    vec![diff]
}

pub(crate) fn git_diff_new(workspace_path: &Path) -> Option<DiffInfo> {
    let root = toplevel(workspace_path)?;
    let head = git_ok(&root, &["branch", "--show-current"])
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "(No branch)".to_owned());
    let branches = git_ok(&root, &["branch", "--format=%(refname:short)"])
        .map(|text| lines(&text))
        .unwrap_or_default();
    let tags = git_ok(&root, &["tag"]).map(|text| lines(&text)).unwrap_or_default();
    let status = match git_ok(&root, &["status", "--porcelain"]) {
        Ok(status) => status,
        Err(err) => {
            tracing::error!("{err:?}");
            return None;
        }
    };
    let mut diffs: Vec<FileDiff> = status
        .lines()
        .flat_map(|line| parse_status_line(&root, line))
        .collect();
    diffs.sort_by_key(|diff| diff.path().clone());
    Some(DiffInfo { head, branches, tags, diffs })
}

pub(crate) fn file_get_head(
    workspace_path: &Path,
    path: &Path,
) -> Result<(String, String)> {
    let root = toplevel(workspace_path).ok_or_else(|| anyhow!("not a git repository"))?;
    let relative = path.strip_prefix(&root)?.to_string_lossy().replace('\\', "/");
    let spec = format!("HEAD:{relative}");
    let id = git_ok(&root, &["rev-parse", &spec])?.trim().to_owned();
    let content = git_ok(&root, &["show", &spec])?;
    Ok((id, content))
}

pub(crate) fn git_get_remote_file_url(workspace_path: &Path, file: &Path) -> Result<String> {
    let root = toplevel(workspace_path).ok_or_else(|| anyhow!("not a git repository"))?;
    let remote = git_ok(&root, &["config", "--get", "remote.origin.url"])?;
    let remote = remote.trim();
    let url = match lsp_types::Url::parse(remote) {
        Ok(url) => url,
        // `git@host:owner/repo` の形
        Err(_) => lsp_types::Url::parse(&format!("ssh://{}", remote.replacen(':', "/", 1)))?,
    };
    let host = url.host_str().ok_or_else(|| anyhow!("Couldn't find remote host"))?;
    let namespace = url.path().strip_suffix(".git").unwrap_or(url.path());
    let commit = git_ok(&root, &["rev-parse", "HEAD"])?;
    let file_path = file.strip_prefix(&root)?.to_string_lossy().replace('\\', "/");
    Ok(format!("https://{host}{namespace}/blob/{}/{file_path}", commit.trim()))
}
