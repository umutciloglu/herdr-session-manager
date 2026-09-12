use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::error::{Error, Result};

/// How much of the end of a transcript we read to find the last timestamp and
/// the title lines a harness appends after the conversation.
pub const TAIL_WINDOW: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub path: PathBuf,
    pub size: u64,
    pub mtime_ms: i64,
}

pub fn stat(path: &Path) -> Result<FileStat> {
    let md = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    let mtime_ms = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Ok(FileStat {
        path: path.to_path_buf(),
        size: md.len(),
        mtime_ms,
    })
}

/// Recursive walk; symlinks are not followed so a stray link cannot loop us.
pub fn files_with_ext(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let p = entry.path();
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() && p.extension().is_some_and(|e| e == ext) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Files exactly one directory below `root`. Claude keeps real transcripts at
/// `<root>/<cwd-slug>/<uuid>.jsonl`; anything deeper is a subagent transcript
/// or a memory directory, not a resumable session.
pub fn files_in_subdirs(root: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for dir in rd.flatten() {
        if !dir.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for entry in inner.flatten() {
            let p = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_file())
                && p.extension().is_some_and(|e| e == ext)
            {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

pub fn head_lines(path: &Path, n: usize) -> Result<Vec<String>> {
    let f = File::open(path).map_err(|e| Error::io(path, e))?;
    let mut out = Vec::with_capacity(n);
    for line in BufReader::new(f).lines().take(n) {
        match line {
            Ok(l) => out.push(l),
            // A transcript can hold a half-written or non-utf8 line; stop there
            // rather than failing the whole scan.
            Err(_) => break,
        }
    }
    Ok(out)
}

/// Complete lines from the last `window` bytes. The first (possibly partial)
/// line of the window is dropped.
pub fn tail_lines(path: &Path, window: u64) -> Result<Vec<String>> {
    let mut f = File::open(path).map_err(|e| Error::io(path, e))?;
    let len = f.metadata().map_err(|e| Error::io(path, e))?.len();
    let start = len.saturating_sub(window);
    f.seek(SeekFrom::Start(start))
        .map_err(|e| Error::io(path, e))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| Error::io(path, e))?;
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    Ok(lines)
}

/// Complete lines appended since `offset`, plus the offset after the last
/// complete line. A trailing partial line is left for the next pass.
pub fn lines_from(path: &Path, offset: u64) -> Result<(Vec<String>, u64)> {
    let mut f = File::open(path).map_err(|e| Error::io(path, e))?;
    let len = f.metadata().map_err(|e| Error::io(path, e))?.len();
    // Truncated or rotated file: start over.
    let start = if offset > len { 0 } else { offset };
    f.seek(SeekFrom::Start(start))
        .map_err(|e| Error::io(path, e))?;

    let mut reader = BufReader::new(f);
    let mut lines = Vec::new();
    let mut consumed = start;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        let n = reader
            .read_until(b'\n', &mut raw)
            .map_err(|e| Error::io(path, e))?;
        if n == 0 {
            break;
        }
        if raw.last() != Some(&b'\n') {
            break; // partial tail, wait for the writer
        }
        consumed += n as u64;
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end_matches(['\n', '\r']);
        if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    Ok((lines, consumed))
}

pub fn parse_ts(v: Option<&str>) -> Option<DateTime<Utc>> {
    let s = v?;
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

pub fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn lines_from_leaves_a_partial_tail() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("a.jsonl");
        std::fs::write(&p, "one\ntwo\npart").expect("write");

        let (lines, off) = lines_from(&p, 0).expect("read");
        assert_eq!(lines, vec!["one", "two"]);
        assert_eq!(off, 8);

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .expect("open");
        f.write_all(b"ial\n").expect("append");
        let (lines, off) = lines_from(&p, off).expect("read");
        assert_eq!(lines, vec!["partial"]);
        assert_eq!(off, 16);
    }

    #[test]
    fn lines_from_restarts_when_the_file_shrinks() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("a.jsonl");
        std::fs::write(&p, "x\n").expect("write");
        let (lines, _) = lines_from(&p, 9999).expect("read");
        assert_eq!(lines, vec!["x"]);
    }

    #[test]
    fn tail_drops_the_partial_first_line() {
        let dir = tempfile::tempdir().expect("tmp");
        let p = dir.path().join("a.jsonl");
        std::fs::write(&p, "aaaa\nbbbb\ncccc\n").expect("write");
        assert_eq!(tail_lines(&p, 8).expect("tail"), vec!["cccc"]);
        assert_eq!(
            tail_lines(&p, 4096).expect("tail"),
            vec!["aaaa", "bbbb", "cccc"]
        );
    }
}
