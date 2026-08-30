use std::{env, fs, path::Path, process::Command};

fn emit_build_version(extra_watch_paths: &[&str]) {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR");
    let repository = Path::new(&manifest_dir).parent().unwrap_or(Path::new("."));
    watch_git_revision(repository, extra_watch_paths);

    let package = env::var("CARGO_PKG_VERSION").expect("Cargo sets CARGO_PKG_VERSION");
    let revision = git_revision(repository).unwrap_or_else(|| "unknown".into());
    let dirty = if git_is_dirty(repository) { ".dirty" } else { "" };
    println!("cargo:rustc-env=SERVATORY_BUILD_VERSION={package}+g{revision}{dirty}");
}

fn git_is_dirty(repository: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repository)
        .output()
        .is_ok_and(|output| output.status.success() && !output.stdout.is_empty())
}

fn git_revision(repository: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=10", "HEAD"])
        .current_dir(repository)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8(output.stdout).ok()?;
    let revision = revision.trim();
    (!revision.is_empty()
        && revision
            .chars()
            .all(|character| character.is_ascii_hexdigit()))
    .then(|| revision.to_owned())
}

fn watch_git_revision(repository: &Path, extra_watch_paths: &[&str]) {
    let git = repository.join(".git");
    let head = git.join("HEAD");
    for path in [".git/HEAD", ".git/packed-refs", "protocol/src"]
        .into_iter()
        .chain(extra_watch_paths.iter().copied())
    {
        println!("cargo:rerun-if-changed={}", repository.join(path).display());
    }
    if let Ok(value) = fs::read_to_string(head)
        && let Some(reference) = value.trim().strip_prefix("ref: ")
    {
        println!("cargo:rerun-if-changed={}", git.join(reference).display());
    }
}
