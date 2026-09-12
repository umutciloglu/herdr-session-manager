//! Finding the session provider binary.
//!
//! The provider is configured as an argv (`["hsm", "sessions", "--json"]` by default),
//! and `Command` only ever looks a bare name up on `PATH`. Both binaries are built side
//! by side — the herdr plugin build drops `hsm` and `agentmail` in the same directory —
//! so when `PATH` has nothing, the sibling of this executable is almost always the
//! program that was meant.

use std::path::Path;

/// Rewrites argv[0] to an absolute sibling path when, and only when, it is a bare name
/// that `PATH` cannot resolve. Anything already spelled as a path is left alone: the
/// user meant that exact file.
pub fn resolve_argv(argv: Vec<String>) -> Vec<String> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf));
    resolve_argv_in(argv, exe_dir.as_deref())
}

pub fn resolve_argv_in(mut argv: Vec<String>, exe_dir: Option<&Path>) -> Vec<String> {
    let Some(program) = argv.first() else {
        return argv;
    };
    if program.is_empty() || program.contains('/') || program.contains('\\') {
        return argv;
    }
    if on_path(program) {
        return argv;
    }
    let Some(sibling) = exe_dir.map(|dir| dir.join(with_exe_suffix(program))) else {
        return argv;
    };
    if is_executable(&sibling) {
        argv[0] = sibling.to_string_lossy().into_owned();
    }
    argv
}

fn with_exe_suffix(program: &str) -> String {
    let suffix = std::env::consts::EXE_SUFFIX;
    if suffix.is_empty() || program.ends_with(suffix) {
        program.to_string()
    } else {
        format!("{program}{suffix}")
    }
}

fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(with_exe_suffix(program))))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn argv(program: &str) -> Vec<String> {
        vec![program.into(), "sessions".into(), "--json".into()]
    }

    fn fake_binary(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        path
    }

    #[test]
    fn a_bare_name_falls_back_to_our_own_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sibling = fake_binary(dir.path(), "hsm-not-on-path");

        let got = resolve_argv_in(argv("hsm-not-on-path"), Some(dir.path()));
        assert_eq!(got[0], sibling.to_string_lossy());
        assert_eq!(
            &got[1..],
            ["sessions", "--json"],
            "the arguments are untouched"
        );
    }

    #[test]
    fn nothing_to_find_means_nothing_to_change() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            resolve_argv_in(argv("hsm-not-on-path"), Some(dir.path())),
            argv("hsm-not-on-path")
        );
        assert_eq!(
            resolve_argv_in(argv("hsm-not-on-path"), None),
            argv("hsm-not-on-path")
        );
    }

    #[test]
    fn a_spelled_out_path_is_the_users_choice() {
        let dir = tempfile::tempdir().expect("tempdir");
        fake_binary(dir.path(), "hsm-not-on-path");
        // The sibling exists, but the config asked for a relative path explicitly.
        assert_eq!(
            resolve_argv_in(argv("./hsm-not-on-path"), Some(dir.path())),
            argv("./hsm-not-on-path")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_program_on_path_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        fake_binary(dir.path(), "sh");
        assert_eq!(resolve_argv_in(argv("sh"), Some(dir.path())), argv("sh"));
    }

    #[test]
    fn an_empty_argv_is_not_a_panic() {
        assert!(resolve_argv_in(Vec::new(), None).is_empty());
    }
}
