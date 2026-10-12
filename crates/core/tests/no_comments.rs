use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

const CHECKED_CRATES: &[&str] = &[];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn collect_rust_files_without_following_symlinks(
    dir: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), Box<dyn Error>> {
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("failed to read directory \"{}\": {e}", dir.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|e| format!("failed to read directory entry \"{}\": {e}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("failed to read file type \"{}\": {e}", path.display()))?;
        if file_type.is_symlink() {
            return Err(format!("unsupported entry type: symlink \"{}\"", path.display()).into());
        }
        if file_type.is_dir() {
            collect_rust_files_without_following_symlinks(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

fn is_forbidden_comment_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("// SAFETY:") {
        return false;
    }
    trimmed.starts_with("//") || trimmed.starts_with("/*")
}

fn comment_lines_in(file: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let text = fs::read_to_string(file)
        .map_err(|e| format!("failed to read file \"{}\": {e}", file.display()))?;
    Ok(text
        .lines()
        .enumerate()
        .filter(|(_, line)| is_forbidden_comment_line(line))
        .map(|(i, line)| format!("{}:{}: {}", file.display(), i + 1, line.trim()))
        .collect())
}

#[test]
fn checked_crates_have_no_comment_lines_in_src() -> Result<(), Box<dyn Error>> {
    let root = workspace_root();
    let mut violations = Vec::new();
    for crate_dir in CHECKED_CRATES {
        let mut files = Vec::new();
        collect_rust_files_without_following_symlinks(
            &root.join("crates").join(crate_dir).join("src"),
            &mut files,
        )?;
        for file in &files {
            violations.extend(comment_lines_in(file)?);
        }
    }
    assert!(
        violations.is_empty(),
        "comment lines found:\n{}",
        violations.join("\n")
    );
    Ok(())
}

#[test]
fn missing_directory_error_names_the_path() {
    let missing = workspace_root().join("crates").join("does-not-exist");
    let mut files = Vec::new();
    let message = match collect_rust_files_without_following_symlinks(&missing, &mut files) {
        Ok(()) => panic!("expected an error for a missing directory"),
        Err(e) => e.to_string(),
    };
    assert!(message.contains("failed to read directory"), "{message}");
    assert!(message.contains("does-not-exist"), "{message}");
}

#[cfg(unix)]
#[test]
fn rejects_symlinked_directory() -> Result<(), Box<dyn Error>> {
    let base = std::env::temp_dir().join(format!(
        "auto-trader-no-comments-{}-rejects_symlinked_directory",
        std::process::id()
    ));
    let link = base.join("link");
    match base.symlink_metadata() {
        Ok(_) => fs::remove_dir_all(&base)
            .map_err(|e| format!("failed to remove directory \"{}\": {e}", base.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(format!("failed to read metadata \"{}\": {e}", base.display()).into());
        }
    }

    let outcome = (|| -> Result<Result<(), Box<dyn Error>>, Box<dyn Error>> {
        let real = base.join("real");
        fs::create_dir_all(&real)
            .map_err(|e| format!("failed to create directory \"{}\": {e}", real.display()))?;
        std::os::unix::fs::symlink(&real, &link)
            .map_err(|e| format!("failed to create symlink \"{}\": {e}", link.display()))?;
        let mut files = Vec::new();
        Ok(collect_rust_files_without_following_symlinks(
            &base, &mut files,
        ))
    })();

    let cleanup = fs::remove_dir_all(&base)
        .map_err(|e| format!("failed to remove directory \"{}\": {e}", base.display()));
    let result = outcome?;
    cleanup?;
    let message = match result {
        Ok(()) => panic!("expected an error for a symlinked directory"),
        Err(e) => e.to_string(),
    };
    assert!(
        message.contains("unsupported entry type: symlink"),
        "{message}"
    );
    assert!(message.contains(&link.display().to_string()), "{message}");
    Ok(())
}

#[test]
fn line_comment_is_forbidden() {
    assert!(is_forbidden_comment_line("    // explain"));
}

#[test]
fn doc_comment_is_forbidden() {
    assert!(is_forbidden_comment_line("/// doc"));
    assert!(is_forbidden_comment_line("//! module doc"));
}

#[test]
fn block_comment_start_is_forbidden() {
    assert!(is_forbidden_comment_line("/* block */"));
}

#[test]
fn safety_comment_is_allowed() {
    assert!(!is_forbidden_comment_line("    // SAFETY: fd is owned"));
}

#[test]
fn url_inside_string_literal_is_not_a_comment() {
    assert!(!is_forbidden_comment_line(
        r#"let url = "https://example.com";"#
    ));
}
