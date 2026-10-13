use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

const CHECKED_CRATES: &[&str] = &[
    "core",
    "notify",
    "vegapunk-client",
    "macro-analyst",
    "backtest",
];

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

enum ScanState {
    Code,
    QuotedString,
    RawString { hashes: usize },
}

fn raw_string_open(bytes: &[u8], after_r: usize) -> Option<(usize, usize)> {
    let mut j = after_r;
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    if bytes.get(j) != Some(&b'"') {
        return None;
    }
    Some((j - after_r, j + 1))
}

fn raw_string_closes_at(bytes: &[u8], quote: usize, hashes: usize) -> bool {
    bytes
        .get(quote + 1..quote + 1 + hashes)
        .is_some_and(|s| s.iter().all(|&b| b == b'#'))
}

fn char_literal_end(text: &str, quote: usize) -> usize {
    let bytes = text.as_bytes();
    match bytes.get(quote + 1) {
        Some(b'\\') => {
            let mut j = quote + 3;
            while j < bytes.len() && bytes[j] != b'\'' {
                j += 1;
            }
            (j + 1).min(bytes.len())
        }
        Some(_) => {
            let Some(c) = text[quote + 1..].chars().next() else {
                return quote + 1;
            };
            let after = quote + 1 + c.len_utf8();
            if bytes.get(after) == Some(&b'\'') {
                after + 1
            } else {
                quote + 1
            }
        }
        None => quote + 1,
    }
}

fn is_safety_comment_at(text: &str, slashes: usize) -> bool {
    let line_start = text[..slashes].rfind('\n').map_or(0, |p| p + 1);
    text[line_start..slashes].trim().is_empty() && text[slashes..].starts_with("// SAFETY:")
}

fn comment_line_numbers(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut line = 1usize;
    let mut state = ScanState::Code;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        match state {
            ScanState::Code => match b {
                b'\n' => {
                    line += 1;
                    i += 1;
                }
                b'"' => {
                    state = ScanState::QuotedString;
                    i += 1;
                }
                b'r' => match raw_string_open(bytes, i + 1) {
                    Some((hashes, next)) => {
                        state = ScanState::RawString { hashes };
                        i = next;
                    }
                    None => i += 1,
                },
                b'\'' => i = char_literal_end(text, i),
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    if !is_safety_comment_at(text, i) {
                        found.push(line);
                    }
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    found.push(line);
                    let mut j = i + 2;
                    while j < bytes.len() && !(bytes[j] == b'*' && bytes.get(j + 1) == Some(&b'/'))
                    {
                        if bytes[j] == b'\n' {
                            line += 1;
                        }
                        j += 1;
                    }
                    i = (j + 2).min(bytes.len());
                }
                _ => i += 1,
            },
            ScanState::QuotedString => match b {
                b'\\' => {
                    if bytes.get(i + 1) == Some(&b'\n') {
                        line += 1;
                    }
                    i += 2;
                }
                b'"' => {
                    state = ScanState::Code;
                    i += 1;
                }
                b'\n' => {
                    line += 1;
                    i += 1;
                }
                _ => i += 1,
            },
            ScanState::RawString { hashes } => match b {
                b'"' if raw_string_closes_at(bytes, i, hashes) => {
                    state = ScanState::Code;
                    i += 1 + hashes;
                }
                b'\n' => {
                    line += 1;
                    i += 1;
                }
                _ => i += 1,
            },
        }
    }
    found.dedup();
    found
}

fn comment_lines_in(file: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let text = fs::read_to_string(file)
        .map_err(|e| format!("failed to read file \"{}\": {e}", file.display()))?;
    let lines: Vec<&str> = text.lines().collect();
    Ok(comment_line_numbers(&text)
        .into_iter()
        .map(|n| format!("{}:{n}: {}", file.display(), lines[n - 1].trim()))
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

fn has_comment(src: &str) -> bool {
    !comment_line_numbers(src).is_empty()
}

#[test]
fn line_comment_is_forbidden() {
    assert!(has_comment("    // explain"));
}

#[test]
fn doc_comment_is_forbidden() {
    assert!(has_comment("/// doc"));
    assert!(has_comment("//! module doc"));
}

#[test]
fn block_comment_start_is_forbidden() {
    assert!(has_comment("/* block */"));
}

#[test]
fn safety_comment_is_allowed() {
    assert!(!has_comment("    // SAFETY: fd is owned"));
}

#[test]
fn url_inside_string_literal_is_not_a_comment() {
    assert!(!has_comment(r#"let url = "https://example.com";"#));
}

#[test]
fn trailing_comment_after_code_is_forbidden() {
    assert!(has_comment("let x = 5; // 0.5"));
}

#[test]
fn url_inside_raw_string_literal_is_not_a_comment() {
    assert!(!has_comment(r#"let s = r"http://example.com";"#));
    assert!(!has_comment(r##"let s = r#"http://example.com"#;"##));
}

#[test]
fn comment_after_string_with_escaped_quote_is_forbidden() {
    assert!(has_comment(r#"let s = "a\"b"; // comment"#));
}

#[test]
fn trailing_comment_after_multiline_raw_string_close_is_forbidden() {
    let src = "let s = r#\"\nurl = http://example.com\n\"#; // comment\n";
    assert_eq!(comment_line_numbers(src), vec![3]);
}

#[test]
fn slashes_inside_multiline_raw_string_are_not_a_comment() {
    let src = "let s = r#\"\nurl = http://example.com\n\"#;\n";
    assert!(comment_line_numbers(src).is_empty());
}

#[test]
fn trailing_comment_after_multiline_quoted_string_continuation_is_forbidden() {
    let src = "let s = \"foo \\\nbar\"; // comment\n";
    assert_eq!(comment_line_numbers(src), vec![2]);
}

#[test]
fn trailing_comment_after_quote_char_literal_is_forbidden() {
    assert!(has_comment("let c = '\"'; // c"));
    assert!(has_comment("let c = b'\"'; // c"));
}

#[test]
fn trailing_comment_after_lifetime_is_forbidden() {
    assert!(has_comment("fn f<'a>(x: &'a str) {} // c"));
    assert!(!has_comment("fn f<'a>(x: &'a str) {}"));
}
