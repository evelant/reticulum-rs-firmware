use std::{
    collections::BTreeMap,
    env,
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Eq, PartialEq)]
enum TreeRecord {
    Directory,
    File { executable: bool, bytes: Vec<u8> },
    Symlink { target: OsString },
}

#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

pub(crate) const SOURCE_GIT_ENVIRONMENT_POLICY: &str = "env-clear;ambient=PATH;locale=C;no-replace-objects;system-global-config=null;hooks-fsmonitor-external-attributes-disabled;canonical-worktree;raw-tree-proof-v1";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SourceIdentity {
    pub(crate) commit: String,
    pub(crate) root_tree: String,
}

pub(crate) fn create_source_archive(
    root: &Path,
    output: &Path,
    commit: &str,
    root_tree: &str,
) -> Result<(), String> {
    verify_source_identity(root, commit, root_tree)?;
    let archive = git_archive(root, commit)?;
    write_new(output, &archive)?;
    verify_source_archive(root, output, commit, root_tree)
}

pub(crate) fn verify_source_archive(
    root: &Path,
    archive: &Path,
    commit: &str,
    root_tree: &str,
) -> Result<(), String> {
    verify_source_identity(root, commit, root_tree)?;
    let embedded = source_archive_commit(archive)?;
    if embedded != commit {
        return Err(format!(
            "source archive embedded commit {embedded} does not match recorded commit {commit}"
        ));
    }

    let temporary = TemporaryDirectory::below(&env::temp_dir(), "phase1-source-proof")?;
    ensure_path_outside_workspace(root, temporary.path())?;
    let preserved = temporary.path().join("preserved");
    fs::create_dir(&preserved).map_err(|error| {
        format!(
            "could not create preserved source proof tree {}: {error}",
            preserved.display()
        )
    })?;
    extract_source_archive(archive, &preserved)?;

    let preserved_tree = collect_tree(&preserved)?;
    // Do not compare one `git archive` invocation with another: committed or
    // repository-local attributes can apply export-ignore/export-subst to both
    // and make a filtered archive look self-consistent. Reconstruct the exact
    // expected files, modes, symlinks and directories from raw tree/blob
    // objects instead.
    let expected_tree = collect_raw_commit_tree(root, commit)?;
    if preserved_tree == expected_tree {
        Ok(())
    } else {
        let first_difference = preserved_tree
            .keys()
            .chain(expected_tree.keys())
            .find(|path| preserved_tree.get(*path) != expected_tree.get(*path))
            .cloned()
            .unwrap_or_else(|| "unknown path".to_owned());
        Err(format!(
            "source archive tree differs from recorded commit {commit} at {first_difference:?}"
        ))
    }
}

pub(crate) fn extract_source_archive(
    archive_path: &Path,
    destination: &Path,
) -> Result<(), String> {
    validate_archive_entries(archive_path)?;
    let file = File::open(archive_path).map_err(|error| {
        format!(
            "could not open source archive {}: {error}",
            archive_path.display()
        )
    })?;
    let mut archive = tar::Archive::new(file);
    archive.set_preserve_permissions(true);
    archive.unpack(destination).map_err(|error| {
        format!(
            "could not safely extract source archive {} into {}: {error}",
            archive_path.display(),
            destination.display()
        )
    })
}

pub(crate) fn ensure_path_outside_workspace(root: &Path, path: &Path) -> Result<(), String> {
    let workspace = fs::canonicalize(root).map_err(|error| {
        format!(
            "could not canonicalize workspace {}: {error}",
            root.display()
        )
    })?;
    let actual = fs::canonicalize(path)
        .map_err(|error| format!("could not canonicalize path {}: {error}", path.display()))?;
    if actual.starts_with(&workspace) {
        Err(format!(
            "qualification temporary path must be outside workspace {}: {}",
            workspace.display(),
            actual.display()
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn ensure_output_location_does_not_dirty_source(
    root: &Path,
    output: &Path,
) -> Result<(), String> {
    if !output.is_absolute() {
        return Err(format!(
            "qualification output path must be absolute after resolution: {}",
            output.display()
        ));
    }
    for component in output.components() {
        if matches!(component, Component::CurDir | Component::ParentDir) {
            return Err(format!(
                "qualification output path must not contain . or .. components: {}",
                output.display()
            ));
        }
    }

    let mut existing = output;
    let mut suffix = Vec::new();
    while fs::symlink_metadata(existing).is_err() {
        let name = existing.file_name().ok_or_else(|| {
            format!(
                "could not find an existing ancestor for qualification output {}",
                output.display()
            )
        })?;
        suffix.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            format!(
                "qualification output has no existing parent: {}",
                output.display()
            )
        })?;
    }
    let metadata = fs::metadata(existing).map_err(|error| {
        format!(
            "could not inspect existing qualification-output ancestor {}: {error}",
            existing.display()
        )
    })?;
    if !metadata.is_dir() {
        return Err(format!(
            "qualification-output ancestor is not a directory: {}",
            existing.display()
        ));
    }
    let mut effective = fs::canonicalize(existing).map_err(|error| {
        format!(
            "could not canonicalize qualification-output ancestor {}: {error}",
            existing.display()
        )
    })?;
    for component in suffix.into_iter().rev() {
        effective.push(component);
    }

    let workspace = fs::canonicalize(root).map_err(|error| {
        format!(
            "could not canonicalize workspace {}: {error}",
            root.display()
        )
    })?;
    let Ok(relative) = effective.strip_prefix(&workspace) else {
        return Ok(());
    };
    let relative = relative_path_text(relative)?;
    let git_output = run_git(
        &workspace,
        [
            OsStr::new("check-ignore"),
            OsStr::new("--quiet"),
            OsStr::new("--no-index"),
            OsStr::new("--"),
            OsStr::new(&relative),
        ],
        "check qualification output ignore policy",
    )?;
    if git_output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "qualification output resolves inside the workspace and must be ignored by git: {} -> {}",
            output.display(),
            effective.display()
        ))
    }
}

pub(crate) fn clean_source_identity(root: &Path) -> Result<SourceIdentity, String> {
    let status = capture_git_stdout(
        root,
        [
            "status",
            "--porcelain=v1",
            "--untracked-files=normal",
            "--ignore-submodules=none",
        ],
        "inspect qualification worktree status",
    )?;
    if !status.is_empty() {
        return Err(format!(
            "qualification preparation requires a clean committed worktree; first change: {}",
            status.lines().next().unwrap_or("unknown change")
        ));
    }
    reject_nonstandard_index_flags(root)?;
    source_identity(root, "HEAD")
}

pub(crate) fn ensure_source_identity_unchanged(
    root: &Path,
    expected: &SourceIdentity,
) -> Result<(), String> {
    let current = clean_source_identity(root)?;
    if current == *expected {
        Ok(())
    } else {
        Err(format!(
            "source identity changed during artifact preparation: expected commit {} tree {}, found commit {} tree {}",
            expected.commit, expected.root_tree, current.commit, current.root_tree
        ))
    }
}

pub(crate) fn source_identity(root: &Path, revision: &str) -> Result<SourceIdentity, String> {
    let commit = capture_git_stdout(
        root,
        ["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
        "resolve source commit",
    )?
    .trim()
    .to_owned();
    let root_tree = capture_git_stdout(
        root,
        ["rev-parse", "--verify", &format!("{commit}^{{tree}}")],
        "resolve source root tree",
    )?
    .trim()
    .to_owned();
    validate_git_object_id(&commit, "source commit")?;
    validate_git_object_id(&root_tree, "source root tree")?;
    Ok(SourceIdentity { commit, root_tree })
}

pub(crate) fn verify_source_identity(
    root: &Path,
    commit: &str,
    root_tree: &str,
) -> Result<(), String> {
    validate_git_object_id(commit, "source commit")?;
    validate_git_object_id(root_tree, "source root tree")?;
    let actual = source_identity(root, commit)?;
    if actual.commit != commit || actual.root_tree != root_tree {
        Err(format!(
            "recorded source identity {commit}/{root_tree} does not match the raw Git object graph {}/{}",
            actual.commit, actual.root_tree
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn capture_git_stdout<I, S>(
    root: &Path,
    arguments: I,
    action: &str,
) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = run_git(root, arguments, action)?;
    if !output.status.success() {
        return Err(format!(
            "could not {action}: {}",
            first_output_line(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|error| format!("{action} output was not UTF-8: {error}"))
}

pub(crate) fn git_version(root: &Path) -> Result<String, String> {
    Ok(capture_git_stdout(root, ["--version"], "read Git version")?
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_owned())
}

pub(crate) fn reject_ambient_ancestor_cargo_configs(source_root: &Path) -> Result<(), String> {
    let source_root = fs::canonicalize(source_root).map_err(|error| {
        format!(
            "could not canonicalize archived source root {}: {error}",
            source_root.display()
        )
    })?;
    let mut ancestor = source_root.parent();
    while let Some(directory) = ancestor {
        for relative in [".cargo/config.toml", ".cargo/config"] {
            let candidate = directory.join(relative);
            if fs::symlink_metadata(&candidate).is_ok() {
                return Err(format!(
                    "ambient Cargo configuration outside archived source is not allowed: {}",
                    candidate.display()
                ));
            }
        }
        ancestor = directory.parent();
    }
    Ok(())
}

fn validate_archive_entries(archive_path: &Path) -> Result<(), String> {
    let file = File::open(archive_path).map_err(|error| {
        format!(
            "could not open source archive {}: {error}",
            archive_path.display()
        )
    })?;
    let mut archive = tar::Archive::new(file);
    let entries = archive.entries().map_err(|error| {
        format!(
            "could not enumerate source archive {}: {error}",
            archive_path.display()
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "could not parse source archive entry in {}: {error}",
                archive_path.display()
            )
        })?;
        let path = entry.path().map_err(|error| {
            format!(
                "could not decode source archive path in {}: {error}",
                archive_path.display()
            )
        })?;
        relative_path_text(&path)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions()
            || kind.is_pax_local_extensions()
            || kind.is_gnu_longname()
            || kind.is_gnu_longlink()
        {
            continue;
        }
        if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|error| {
                    format!(
                        "could not decode source symlink target at {}: {error}",
                        path.display()
                    )
                })?
                .ok_or_else(|| format!("source symlink has no target at {}", path.display()))?;
            validate_confined_symlink_target(&path, &target)?;
        } else if !kind.is_file() && !kind.is_dir() {
            return Err(format!(
                "source archive contains unsupported entry type {kind:?} at {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn validate_confined_symlink_target(path: &Path, target: &Path) -> Result<(), String> {
    let mut depth = path.parent().map_or(0, |parent| {
        parent
            .components()
            .filter(|component| matches!(component, Component::Normal(_)))
            .count()
    });
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "source symlink target escapes archive root: {} -> {}",
                    path.display(),
                    target.display()
                ));
            }
        }
    }
    Ok(())
}

fn collect_tree(root: &Path) -> Result<BTreeMap<String, TreeRecord>, String> {
    let mut records = BTreeMap::new();
    collect_tree_below(root, root, &mut records)?;
    Ok(records)
}

fn collect_tree_below(
    root: &Path,
    directory: &Path,
    records: &mut BTreeMap<String, TreeRecord>,
) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|error| {
        format!(
            "could not list source tree {}: {error}",
            directory.display()
        )
    })? {
        let entry = entry.map_err(|error| {
            format!(
                "could not inspect source tree entry in {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("could not stat source tree {}: {error}", path.display()))?;
        let relative = path.strip_prefix(root).map_err(|_| {
            format!(
                "source tree path escaped root {}: {}",
                root.display(),
                path.display()
            )
        })?;
        let relative = relative_path_text(relative)?;
        let record = if metadata.file_type().is_dir() {
            TreeRecord::Directory
        } else if metadata.file_type().is_file() {
            TreeRecord::File {
                executable: is_executable(&metadata),
                bytes: fs::read(&path).map_err(|error| {
                    format!(
                        "could not read source tree file {}: {error}",
                        path.display()
                    )
                })?,
            }
        } else if metadata.file_type().is_symlink() {
            TreeRecord::Symlink {
                target: fs::read_link(&path)
                    .map_err(|error| {
                        format!("could not read source symlink {}: {error}", path.display())
                    })?
                    .into_os_string(),
            }
        } else {
            return Err(format!(
                "source tree contains unsupported file kind: {}",
                path.display()
            ));
        };
        if records.insert(relative, record).is_some() {
            return Err(format!(
                "source tree contains duplicate path: {}",
                path.display()
            ));
        }
        if metadata.file_type().is_dir() {
            collect_tree_below(root, &path, records)?;
        }
    }
    Ok(())
}

fn collect_raw_commit_tree(
    root: &Path,
    commit: &str,
) -> Result<BTreeMap<String, TreeRecord>, String> {
    validate_git_object_id(commit, "raw source-tree commit")?;
    let output = run_git(
        root,
        ["ls-tree", "-r", "-t", "-z", "--full-tree", commit],
        "enumerate raw source tree objects",
    )?;
    if !output.status.success() {
        return Err(format!(
            "could not enumerate raw source tree objects: {}",
            first_output_line(&output.stderr)
        ));
    }
    let mut records = BTreeMap::new();
    for raw in output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
    {
        let separator = raw
            .iter()
            .position(|byte| *byte == b'\t')
            .ok_or_else(|| "Git ls-tree emitted a record without a path separator".to_owned())?;
        let (metadata, path_with_separator) = raw.split_at(separator);
        let path = &path_with_separator[1..];
        let metadata = std::str::from_utf8(metadata)
            .map_err(|error| format!("Git ls-tree metadata was not UTF-8: {error}"))?;
        let mut fields = metadata.split(' ');
        let mode = fields
            .next()
            .ok_or_else(|| "Git ls-tree record lacks a mode".to_owned())?;
        let kind = fields
            .next()
            .ok_or_else(|| "Git ls-tree record lacks an object type".to_owned())?;
        let object = fields
            .next()
            .ok_or_else(|| "Git ls-tree record lacks an object id".to_owned())?;
        if fields.next().is_some() {
            return Err("Git ls-tree record has unexpected metadata fields".to_owned());
        }
        validate_git_object_id(object, "source tree object")?;
        let path = std::str::from_utf8(path)
            .map_err(|error| format!("source tree path is not UTF-8: {error}"))?;
        if relative_path_text(Path::new(path))? != path {
            return Err(format!("source tree has non-canonical path {path:?}"));
        }
        let record = match (mode, kind) {
            ("040000", "tree") => TreeRecord::Directory,
            ("100644", "blob") | ("100755", "blob") => TreeRecord::File {
                executable: mode == "100755",
                bytes: raw_git_blob(root, object)?,
            },
            ("120000", "blob") => TreeRecord::Symlink {
                target: git_symlink_target(raw_git_blob(root, object)?)?,
            },
            ("160000", "commit") => {
                return Err(format!(
                    "source commit contains unsupported non-self-contained gitlink {path:?}"
                ));
            }
            _ => {
                return Err(format!(
                    "source commit contains unsupported Git mode/type {mode}/{kind} at {path:?}"
                ));
            }
        };
        if records.insert(path.to_owned(), record).is_some() {
            return Err(format!("source commit repeats path {path:?}"));
        }
    }
    if records.is_empty() {
        return Err("source commit tree is empty".to_owned());
    }
    Ok(records)
}

fn raw_git_blob(root: &Path, object: &str) -> Result<Vec<u8>, String> {
    let output = run_git(root, ["cat-file", "blob", object], "read raw source blob")?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "could not read raw source blob {object}: {}",
            first_output_line(&output.stderr)
        ))
    }
}

#[cfg(unix)]
fn git_symlink_target(bytes: Vec<u8>) -> Result<OsString, String> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes))
}

#[cfg(not(unix))]
fn git_symlink_target(bytes: Vec<u8>) -> Result<OsString, String> {
    String::from_utf8(bytes)
        .map(OsString::from)
        .map_err(|error| format!("Git symlink target is not UTF-8: {error}"))
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

fn git_archive(root: &Path, commit: &str) -> Result<Vec<u8>, String> {
    validate_git_object_id(commit, "source archive commit")?;
    let output = run_git(
        root,
        ["archive", "--format=tar", commit],
        "archive the source commit",
    )?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(format!(
            "git archive for {commit} failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("unknown error")
        ))
    }
}

fn source_archive_commit(archive: &Path) -> Result<String, String> {
    let output = safe_git_command()
        .args(["get-tar-commit-id"])
        .stdin(
            File::open(archive)
                .map_err(|error| format!("could not open {}: {error}", archive.display()))?,
        )
        .output()
        .map_err(|error| format!("could not inspect source archive commit: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git get-tar-commit-id failed for {}: {}",
            archive.display(),
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("unknown error")
        ));
    }
    let commit = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    validate_git_object_id(&commit, "source archive commit")?;
    Ok(commit)
}

fn validate_git_object_id(value: &str, label: &str) -> Result<(), String> {
    if value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!(
            "{label} is not a canonical SHA-1 object id: {value:?}"
        ))
    }
}

fn run_git<I, S>(root: &Path, arguments: I, action: &str) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let repository = validate_repository_root(root)?;
    safe_git_command()
        .arg("-C")
        .arg(&repository)
        .args(arguments)
        .output()
        .map_err(|error| format!("could not {action} in {}: {error}", repository.display()))
}

fn validate_repository_root(root: &Path) -> Result<PathBuf, String> {
    let root = fs::canonicalize(root).map_err(|error| {
        format!(
            "could not canonicalize workspace {}: {error}",
            root.display()
        )
    })?;
    if !root.is_dir() {
        return Err(format!("workspace is not a directory: {}", root.display()));
    }
    let top = run_git_unchecked(
        &root,
        ["rev-parse", "--show-toplevel"],
        "locate workspace root",
    )?;
    let top = String::from_utf8(top.stdout)
        .map_err(|error| format!("workspace-root output was not UTF-8: {error}"))?;
    let top = fs::canonicalize(top.trim()).map_err(|error| {
        format!(
            "could not canonicalize Git workspace root {:?}: {error}",
            top.trim()
        )
    })?;
    if top != root {
        return Err(format!(
            "qualification workspace {} resolves through Git to unrelated root {}",
            root.display(),
            top.display()
        ));
    }

    let common = run_git_unchecked(
        &root,
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        "locate Git common directory",
    )?;
    let common = String::from_utf8(common.stdout)
        .map_err(|error| format!("Git common-directory output was not UTF-8: {error}"))?;
    let common = fs::canonicalize(common.trim()).map_err(|error| {
        format!(
            "could not canonicalize Git common directory {:?}: {error}",
            common.trim()
        )
    })?;
    let attributes = common.join("info/attributes");
    match fs::symlink_metadata(&attributes) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "could not inspect ambient Git attributes {}: {error}",
                attributes.display()
            ));
        }
        Ok(_) => {
            return Err(format!(
                "ambient Git info attributes are forbidden for exact source archives: {}",
                attributes.display()
            ));
        }
    }
    Ok(root)
}

fn run_git_unchecked<I, S>(
    canonical_root: &Path,
    arguments: I,
    action: &str,
) -> Result<Output, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = safe_git_command()
        .arg("-C")
        .arg(canonical_root)
        .args(arguments)
        .output()
        .map_err(|error| {
            format!(
                "could not {action} in {}: {error}",
                canonical_root.display()
            )
        })?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "could not {action}: {}",
            first_output_line(&output.stderr)
        ))
    }
}

fn safe_git_command() -> Command {
    let path = env::var_os("PATH");
    let mut command = Command::new("git");
    command
        .env_clear()
        .envs(path.map(|value| ("PATH", value)))
        .env("HOME", NULL_DEVICE)
        .env("XDG_CONFIG_HOME", NULL_DEVICE)
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", NULL_DEVICE)
        .env("GIT_CONFIG_GLOBAL", NULL_DEVICE)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("--no-replace-objects")
        .arg("-c")
        .arg(format!("core.attributesFile={NULL_DEVICE}"))
        .args(["-c", "core.fsmonitor=false"])
        .arg("-c")
        .arg(format!("core.hooksPath={NULL_DEVICE}"))
        .args(["-c", "core.untrackedCache=false"]);
    command
}

fn reject_nonstandard_index_flags(root: &Path) -> Result<(), String> {
    let records = capture_git_stdout(root, ["ls-files", "-v"], "inspect tracked-file index flags")?;
    if let Some(record) = records.lines().find(|record| {
        record
            .as_bytes()
            .first()
            .is_none_or(|prefix| *prefix != b'H')
    }) {
        Err(format!(
            "qualification rejects assume-unchanged, skip-worktree or nonstandard index state: {record}"
        ))
    } else {
        Ok(())
    }
}

fn first_output_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("unknown Git error")
        .to_owned()
}

fn relative_path_text(path: &Path) -> Result<String, String> {
    let mut pieces = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(piece) => pieces.push(
                piece
                    .to_str()
                    .ok_or_else(|| format!("source path is not UTF-8: {}", path.display()))?,
            ),
            _ => {
                return Err(format!(
                    "source archive path must be confined and relative: {}",
                    path.display()
                ));
            }
        }
    }
    if pieces.is_empty() {
        return Err("source archive path must not be empty".to_owned());
    }
    Ok(pieces.join("/"))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", path.display()))
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn below(parent: &Path, label: &str) -> Result<Self, String> {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "could not create temporary directory parent {}: {error}",
                parent.display()
            )
        })?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?
            .as_nanos();
        let path = parent.join(format!("{label}-{}-{nonce}", std::process::id()));
        fs::create_dir(&path).map_err(|error| {
            format!(
                "could not create temporary directory {}: {error}",
                path.display()
            )
        })?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path) {
            eprintln!(
                "warning: could not remove temporary directory {}: {error}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    #[test]
    fn archive_tree_must_exactly_match_the_recorded_commit() {
        let temporary = TemporaryDirectory::below(&env::temp_dir(), "phase1-source-test").unwrap();
        let repository = temporary.path().join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Phase One Test"]);
        git(
            &repository,
            &["config", "user.email", "phase-one@example.invalid"],
        );
        fs::write(repository.join("tracked.txt"), b"original-content").unwrap();
        git(&repository, &["add", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "fixture"]);
        let identity = source_identity(&repository, "HEAD").unwrap();
        let archive = temporary.path().join("source.tar");
        create_source_archive(&repository, &archive, &identity.commit, &identity.root_tree)
            .unwrap();
        verify_source_archive(&repository, &archive, &identity.commit, &identity.root_tree)
            .unwrap();

        let mut bytes = fs::read(&archive).unwrap();
        let position = bytes
            .windows(b"original-content".len())
            .position(|window| window == b"original-content")
            .unwrap();
        bytes[position..position + b"tampered-content".len()].copy_from_slice(b"tampered-content");
        fs::write(&archive, bytes).unwrap();
        assert!(
            verify_source_archive(&repository, &archive, &identity.commit, &identity.root_tree)
                .is_err()
        );
    }

    #[test]
    fn replacement_refs_cannot_equivocate_source_identity_or_archive_bytes() {
        let temporary =
            TemporaryDirectory::below(&env::temp_dir(), "phase1-source-replace-test").unwrap();
        let repository = temporary.path().join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Phase One Test"]);
        git(
            &repository,
            &["config", "user.email", "phase-one@example.invalid"],
        );
        fs::write(repository.join("tracked.txt"), b"official\n").unwrap();
        git(&repository, &["add", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "official"]);
        let official = git(&repository, &["rev-parse", "HEAD"]);
        let official_tree = git(
            &repository,
            &["--no-replace-objects", "rev-parse", "HEAD^{tree}"],
        );

        fs::write(repository.join("tracked.txt"), b"replacement\n").unwrap();
        git(&repository, &["commit", "--quiet", "-am", "replacement"]);
        let replacement = git(&repository, &["rev-parse", "HEAD"]);
        git(&repository, &["replace", &official, &replacement]);
        git(
            &repository,
            &[
                "--no-replace-objects",
                "reset",
                "--hard",
                "--quiet",
                &official,
            ],
        );

        let ambient_archive = temporary.path().join("ambient.tar");
        let ambient = Command::new("git")
            .current_dir(&repository)
            .args(["archive", "--format=tar", &official])
            .output()
            .unwrap();
        assert!(ambient.status.success());
        fs::write(&ambient_archive, ambient.stdout).unwrap();
        let ambient_tree = temporary.path().join("ambient");
        fs::create_dir(&ambient_tree).unwrap();
        extract_source_archive(&ambient_archive, &ambient_tree).unwrap();
        assert_eq!(
            fs::read(ambient_tree.join("tracked.txt")).unwrap(),
            b"replacement\n"
        );

        let identity = clean_source_identity(&repository).unwrap();
        assert_eq!(identity.commit, official);
        assert_eq!(identity.root_tree, official_tree);
        let archive = temporary.path().join("source.tar");
        create_source_archive(&repository, &archive, &identity.commit, &identity.root_tree)
            .unwrap();
        let extracted = temporary.path().join("extracted");
        fs::create_dir(&extracted).unwrap();
        extract_source_archive(&archive, &extracted).unwrap();
        assert_eq!(
            fs::read(extracted.join("tracked.txt")).unwrap(),
            b"official\n"
        );
    }

    #[test]
    fn archive_is_proved_against_raw_tree_not_export_attributes() {
        let temporary =
            TemporaryDirectory::below(&env::temp_dir(), "phase1-source-attributes-test").unwrap();
        let repository = temporary.path().join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Phase One Test"]);
        git(
            &repository,
            &["config", "user.email", "phase-one@example.invalid"],
        );
        fs::write(repository.join("tracked.txt"), b"must remain\n").unwrap();
        fs::write(
            repository.join(".gitattributes"),
            b"tracked.txt export-ignore\n",
        )
        .unwrap();
        git(&repository, &["add", "tracked.txt", ".gitattributes"]);
        git(&repository, &["commit", "--quiet", "-m", "fixture"]);
        let identity = clean_source_identity(&repository).unwrap();
        let archive = temporary.path().join("source.tar");
        let error =
            create_source_archive(&repository, &archive, &identity.commit, &identity.root_tree)
                .unwrap_err();
        assert!(error.contains("source archive tree differs"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn git_common_dir_info_attributes_are_rejected_even_when_empty_or_symlinked() {
        let temporary =
            TemporaryDirectory::below(&env::temp_dir(), "phase1-source-info-attributes-test")
                .unwrap();
        let repository = temporary.path().join("repository");
        fs::create_dir(&repository).unwrap();
        git(&repository, &["init", "--quiet"]);
        git(&repository, &["config", "user.name", "Phase One Test"]);
        git(
            &repository,
            &["config", "user.email", "phase-one@example.invalid"],
        );
        fs::write(repository.join("tracked.txt"), b"must remain\n").unwrap();
        git(&repository, &["add", "tracked.txt"]);
        git(&repository, &["commit", "--quiet", "-m", "fixture"]);
        let identity = clean_source_identity(&repository).unwrap();
        let common_dir = PathBuf::from(git(
            &repository,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ));
        let attributes = common_dir.join("info/attributes");
        let archive = temporary.path().join("source.tar");

        fs::write(&attributes, b"").unwrap();
        let identity_error = clean_source_identity(&repository).unwrap_err();
        assert!(
            identity_error.contains("ambient Git info attributes are forbidden"),
            "{identity_error}"
        );
        let archive_error =
            create_source_archive(&repository, &archive, &identity.commit, &identity.root_tree)
                .unwrap_err();
        assert!(
            archive_error.contains("ambient Git info attributes are forbidden"),
            "{archive_error}"
        );

        fs::remove_file(&attributes).unwrap();
        let symlink_target = temporary.path().join("empty-attributes");
        fs::write(&symlink_target, b"").unwrap();
        std::os::unix::fs::symlink(&symlink_target, &attributes).unwrap();
        let identity_error = clean_source_identity(&repository).unwrap_err();
        assert!(
            identity_error.contains("ambient Git info attributes are forbidden"),
            "{identity_error}"
        );
        let archive_error =
            create_source_archive(&repository, &archive, &identity.commit, &identity.root_tree)
                .unwrap_err();
        assert!(
            archive_error.contains("ambient Git info attributes are forbidden"),
            "{archive_error}"
        );
    }

    #[test]
    fn source_git_ignores_ambient_repository_and_config_redirection() {
        const CHILD_MODE: &str = "RETICULUM_SOURCE_GIT_POISON_CHILD_MODE";
        const CHILD_ROOT: &str = "RETICULUM_SOURCE_GIT_POISON_ROOT";
        const CHILD_OUTPUT: &str = "RETICULUM_SOURCE_GIT_POISON_OUTPUT";
        const CHILD_COMMIT: &str = "RETICULUM_SOURCE_GIT_POISON_COMMIT";
        const CHILD_TREE: &str = "RETICULUM_SOURCE_GIT_POISON_TREE";
        const TEST_NAME: &str = concat!(
            "phase1_source::tests::",
            "source_git_ignores_ambient_repository_and_config_redirection"
        );

        if env::var_os(CHILD_MODE).is_some() {
            let root = PathBuf::from(env::var_os(CHILD_ROOT).unwrap());
            let output = PathBuf::from(env::var_os(CHILD_OUTPUT).unwrap());
            let expected = SourceIdentity {
                commit: env::var(CHILD_COMMIT).unwrap(),
                root_tree: env::var(CHILD_TREE).unwrap(),
            };
            assert_eq!(clean_source_identity(&root).unwrap(), expected);
            create_source_archive(&root, &output, &expected.commit, &expected.root_tree).unwrap();
            verify_source_archive(&root, &output, &expected.commit, &expected.root_tree).unwrap();
            return;
        }

        let temporary =
            TemporaryDirectory::below(&env::temp_dir(), "phase1-source-env-test").unwrap();
        let official = temporary.path().join("official");
        let unrelated = temporary.path().join("unrelated");
        for (repository, bytes) in [
            (&official, b"official\n".as_slice()),
            (&unrelated, b"unrelated\n".as_slice()),
        ] {
            fs::create_dir(repository).unwrap();
            git(repository, &["init", "--quiet"]);
            git(repository, &["config", "user.name", "Phase One Test"]);
            git(
                repository,
                &["config", "user.email", "phase-one@example.invalid"],
            );
            fs::write(repository.join("tracked.txt"), bytes).unwrap();
            git(repository, &["add", "tracked.txt"]);
            git(repository, &["commit", "--quiet", "-m", "fixture"]);
        }
        let identity = source_identity(&official, "HEAD").unwrap();

        let poison_attributes = temporary.path().join("poison-attributes");
        fs::write(&poison_attributes, b"tracked.txt export-ignore\n").unwrap();
        git(
            &official,
            &[
                "config",
                "core.attributesFile",
                poison_attributes.to_str().unwrap(),
            ],
        );
        git(&official, &["config", "core.fsmonitor", "false"]);
        let poison_home = temporary.path().join("poison-home");
        let poison_xdg = temporary.path().join("poison-xdg");
        fs::create_dir_all(&poison_home).unwrap();
        fs::create_dir_all(poison_xdg.join("git")).unwrap();
        let poison_config = format!(
            "[core]\n\tworktree = {}\n\tattributesFile = {}\n",
            unrelated.display(),
            poison_attributes.display()
        );
        fs::write(poison_home.join(".gitconfig"), &poison_config).unwrap();
        fs::write(poison_xdg.join("git/config"), &poison_config).unwrap();
        let poison_global = temporary.path().join("poison-global.gitconfig");
        let poison_system = temporary.path().join("poison-system.gitconfig");
        fs::write(&poison_global, &poison_config).unwrap();
        fs::write(&poison_system, &poison_config).unwrap();
        let output = temporary.path().join("source.tar");

        let child = Command::new(env::current_exe().unwrap())
            .arg("--exact")
            .arg(TEST_NAME)
            .arg("--nocapture")
            .env(CHILD_MODE, "1")
            .env(CHILD_ROOT, &official)
            .env(CHILD_OUTPUT, &output)
            .env(CHILD_COMMIT, &identity.commit)
            .env(CHILD_TREE, &identity.root_tree)
            .env("GIT_DIR", unrelated.join(".git"))
            .env("GIT_WORK_TREE", &unrelated)
            .env("GIT_COMMON_DIR", unrelated.join(".git"))
            .env("GIT_OBJECT_DIRECTORY", unrelated.join(".git/objects"))
            .env(
                "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                unrelated.join(".git/objects"),
            )
            .env("GIT_INDEX_FILE", unrelated.join(".git/index"))
            .env("GIT_NAMESPACE", "poison")
            .env("GIT_REPLACE_REF_BASE", "refs/replace/poison")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.worktree")
            .env("GIT_CONFIG_VALUE_0", &unrelated)
            .env("GIT_CONFIG_PARAMETERS", "'core.bare'='true'")
            .env("GIT_CONFIG_NOSYSTEM", "0")
            .env("GIT_CONFIG_SYSTEM", &poison_system)
            .env("GIT_CONFIG_GLOBAL", &poison_global)
            .env("GIT_ATTR_NOSYSTEM", "0")
            .env("HOME", &poison_home)
            .env("XDG_CONFIG_HOME", &poison_xdg)
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "poisoned source-Git child failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }

    #[test]
    fn outside_workspace_and_ancestor_cargo_config_checks_are_canonical() {
        let temporary =
            TemporaryDirectory::below(&env::temp_dir(), "phase1-boundary-test").unwrap();
        let workspace = temporary.path().join("workspace");
        let source = workspace.join("target/source");
        fs::create_dir_all(&source).unwrap();
        assert!(ensure_path_outside_workspace(&workspace, &source).is_err());

        let outside = TemporaryDirectory::below(&env::temp_dir(), "phase1-outside-test").unwrap();
        ensure_path_outside_workspace(&workspace, outside.path()).unwrap();
        reject_ambient_ancestor_cargo_configs(&source).unwrap();
        fs::create_dir_all(workspace.join("target/.cargo")).unwrap();
        fs::write(workspace.join("target/.cargo/config.toml"), b"[build]\n").unwrap();
        assert!(reject_ambient_ancestor_cargo_configs(&source).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn tree_comparison_detects_executable_bit_changes() {
        let temporary = TemporaryDirectory::below(&env::temp_dir(), "phase1-mode-test").unwrap();
        let executable = temporary.path().join("executable");
        let regular = temporary.path().join("regular");
        fs::create_dir(&executable).unwrap();
        fs::create_dir(&regular).unwrap();
        fs::write(executable.join("script"), b"same bytes\n").unwrap();
        fs::write(regular.join("script"), b"same bytes\n").unwrap();
        fs::set_permissions(executable.join("script"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(regular.join("script"), fs::Permissions::from_mode(0o644)).unwrap();
        assert_ne!(
            collect_tree(&executable).unwrap(),
            collect_tree(&regular).unwrap()
        );
    }

    #[test]
    fn symlink_targets_must_remain_lexically_confined_to_the_archive() {
        validate_confined_symlink_target(Path::new("crate/link"), Path::new("../shared")).unwrap();
        assert!(
            validate_confined_symlink_target(Path::new("crate/link"), Path::new("../../outside"))
                .is_err()
        );
        assert!(
            validate_confined_symlink_target(Path::new("crate/link"), Path::new("/absolute"))
                .is_err()
        );
    }
}
