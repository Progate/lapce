//! ソース管理（libgit2）。WASI では `git_wasi.rs` が同じ名前で置き換わる

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use git2::{
    DiffOptions, ErrorCode::NotFound, Oid, Repository, build::CheckoutBuilder,
};
use lapce_rpc::source_control::{DiffInfo, FileDiff};
use lsp_types::Url;

pub(crate) fn git_init(workspace_path: &Path) -> Result<()> {
    if Repository::discover(workspace_path).is_err() {
        Repository::init(workspace_path)?;
    };
    Ok(())
}

pub(crate) fn git_commit(
    workspace_path: &Path,
    message: &str,
    diffs: Vec<FileDiff>,
) -> Result<()> {
    let repo = Repository::discover(workspace_path)?;
    let mut index = repo.index()?;
    for diff in diffs {
        match diff {
            FileDiff::Modified(p) | FileDiff::Added(p) => {
                index.add_path(p.strip_prefix(workspace_path)?)?;
            }
            FileDiff::Renamed(a, d) => {
                index.add_path(a.strip_prefix(workspace_path)?)?;
                index.remove_path(d.strip_prefix(workspace_path)?)?;
            }
            FileDiff::Deleted(p) => {
                index.remove_path(p.strip_prefix(workspace_path)?)?;
            }
        }
    }
    index.write()?;
    let tree = index.write_tree()?;
    let tree = repo.find_tree(tree)?;

    match repo.signature() {
        Ok(signature) => {
            let parents = repo
                .head()
                .and_then(|head| Ok(vec![head.peel_to_commit()?]))
                .unwrap_or(vec![]);
            let parents_refs = parents.iter().collect::<Vec<_>>();

            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parents_refs,
            )?;
            Ok(())
        }
        Err(e) => match e.code() {
            NotFound => Err(anyhow!(
                "No user.name and/or user.email configured for this git repository."
            )),
            _ => Err(anyhow!(
                "Error while creating commit's signature: {}",
                e.message()
            )),
        },
    }
}

pub(crate) fn git_checkout(workspace_path: &Path, reference: &str) -> Result<()> {
    let repo = Repository::discover(workspace_path)?;
    let (object, reference) = repo.revparse_ext(reference)?;
    repo.checkout_tree(&object, None)?;
    repo.set_head(reference.unwrap().name().unwrap())?;
    Ok(())
}

pub(crate) fn git_discard_files_changes<'a>(
    workspace_path: &Path,
    files: impl Iterator<Item = &'a Path>,
) -> Result<()> {
    let repo = Repository::discover(workspace_path)?;

    let mut checkout_b = CheckoutBuilder::new();
    checkout_b.update_only(false).force();

    let mut had_path = false;
    for path in files {
        // Remove the workspace path so it is relative to the folder
        if let Ok(path) = path.strip_prefix(workspace_path) {
            had_path = true;
            checkout_b.path(path);
        }
    }

    if !had_path {
        // If there we no paths then we do nothing
        // because the default behavior of checkout builder is to select all files
        // if it is not given a path
        return Ok(());
    }

    repo.checkout_index(None, Some(&mut checkout_b))?;

    Ok(())
}

pub(crate) fn git_discard_workspace_changes(workspace_path: &Path) -> Result<()> {
    let repo = Repository::discover(workspace_path)?;
    let mut checkout_b = CheckoutBuilder::new();
    checkout_b.force();

    repo.checkout_index(None, Some(&mut checkout_b))?;

    Ok(())
}

fn git_delta_format(
    workspace_path: &Path,
    delta: &git2::DiffDelta,
) -> Option<(git2::Delta, git2::Oid, PathBuf)> {
    match delta.status() {
        git2::Delta::Added | git2::Delta::Untracked => Some((
            git2::Delta::Added,
            delta.new_file().id(),
            delta.new_file().path().map(|p| workspace_path.join(p))?,
        )),
        git2::Delta::Deleted => Some((
            git2::Delta::Deleted,
            delta.old_file().id(),
            delta.old_file().path().map(|p| workspace_path.join(p))?,
        )),
        git2::Delta::Modified => Some((
            git2::Delta::Modified,
            delta.new_file().id(),
            delta.new_file().path().map(|p| workspace_path.join(p))?,
        )),
        _ => None,
    }
}

pub(crate) fn git_diff_new(workspace_path: &Path) -> Option<DiffInfo> {
    let repo = Repository::discover(workspace_path).ok()?;
    let name = match repo.head() {
        Ok(head) => head.shorthand().ok()?.to_string(),
        _ => "(No branch)".to_owned(),
    };

    let mut branches = Vec::new();
    for branch in repo.branches(None).ok()? {
        branches.push(branch.ok()?.0.name().ok()??.to_string());
    }

    let mut tags = Vec::new();
    if let Ok(git_tags) = repo.tag_names(None) {
        for tag in git_tags.into_iter().flatten() {
            let Some(tag) = tag else {
                continue;
            };
            tags.push(tag.to_owned());
        }
    }

    let mut deltas = Vec::new();
    let mut diff_options = DiffOptions::new();
    let diff = repo
        .diff_index_to_workdir(
            None,
            Some(
                diff_options
                    .include_untracked(true)
                    .recurse_untracked_dirs(true),
            ),
        )
        .ok()?;
    for delta in diff.deltas() {
        if let Some(delta) = git_delta_format(workspace_path, &delta) {
            deltas.push(delta);
        }
    }

    let oid = match repo.revparse_single("HEAD^{tree}") {
        Ok(obj) => obj.id(),
        _ => Oid::ZERO_SHA1,
    };

    let cached_diff = repo
        .diff_tree_to_index(repo.find_tree(oid).ok().as_ref(), None, None)
        .ok();

    if let Some(cached_diff) = cached_diff {
        for delta in cached_diff.deltas() {
            if let Some(delta) = git_delta_format(workspace_path, &delta) {
                deltas.push(delta);
            }
        }
    }
    let mut renames = Vec::new();
    let mut renamed_deltas = HashSet::new();

    for (added_index, delta) in deltas.iter().enumerate() {
        if delta.0 == git2::Delta::Added {
            for (deleted_index, d) in deltas.iter().enumerate() {
                if d.0 == git2::Delta::Deleted && d.1 == delta.1 {
                    renames.push((added_index, deleted_index));
                    renamed_deltas.insert(added_index);
                    renamed_deltas.insert(deleted_index);
                    break;
                }
            }
        }
    }

    let mut file_diffs = Vec::new();
    for (added_index, deleted_index) in renames.iter() {
        file_diffs.push(FileDiff::Renamed(
            deltas[*added_index].2.clone(),
            deltas[*deleted_index].2.clone(),
        ));
    }
    for (i, delta) in deltas.iter().enumerate() {
        if renamed_deltas.contains(&i) {
            continue;
        }
        let diff = match delta.0 {
            git2::Delta::Added => FileDiff::Added(delta.2.clone()),
            git2::Delta::Deleted => FileDiff::Deleted(delta.2.clone()),
            git2::Delta::Modified => FileDiff::Modified(delta.2.clone()),
            _ => continue,
        };
        file_diffs.push(diff);
    }
    file_diffs.sort_by_key(|d| match d {
        FileDiff::Modified(p)
        | FileDiff::Added(p)
        | FileDiff::Renamed(p, _)
        | FileDiff::Deleted(p) => p.clone(),
    });
    Some(DiffInfo {
        head: name,
        branches,
        tags,
        diffs: file_diffs,
    })
}

pub(crate) fn file_get_head(workspace_path: &Path, path: &Path) -> Result<(String, String)> {
    let repo = Repository::discover(workspace_path)?;
    let head = repo.head()?;
    let tree = head.peel_to_tree()?;
    let tree_entry = tree.get_path(path.strip_prefix(workspace_path)?)?;
    let blob = repo.find_blob(tree_entry.id())?;
    let id = blob.id().to_string();
    let content = std::str::from_utf8(blob.content())
        .with_context(|| "content bytes to string")?
        .to_string();
    Ok((id, content))
}

pub(crate) fn git_get_remote_file_url(workspace_path: &Path, file: &Path) -> Result<String> {
    let repo = Repository::discover(workspace_path)?;
    let head = repo.head()?;
    let target_remote = repo.find_remote(
        repo.branch_upstream_remote(head.name().unwrap())?
            .as_str()
            .unwrap(),
    )?;

    // Grab URL part of remote
    let remote = target_remote
        .url()
        .map_err(|e| anyhow!("Failed to convert remote to str: {e}"))?;

    let remote_url = match Url::parse(remote) {
        Ok(url) => url,
        Err(_) => {
            // Parse URL as ssh
            Url::parse(&format!("ssh://{}", remote.replacen(':', "/", 1)))?
        }
    };

    // Get host part
    let host = remote_url
        .host_str()
        .ok_or(anyhow!("Couldn't find remote host"))?;
    // Get namespace (e.g. organisation/project in case of GitHub, org/team/team/team/../project on GitLab)
    let namespace = if let Some(stripped) = remote_url.path().strip_suffix(".git") {
        stripped
    } else {
        remote_url.path()
    };

    let commit = head.peel_to_commit()?.id();

    let file_path = file
        .strip_prefix(workspace_path)?
        .to_str()
        .ok_or(anyhow!("Couldn't convert file path to str"))?;

    let url = format!("https://{host}{namespace}/blob/{commit}/{file_path}",);

    Ok(url)
}

