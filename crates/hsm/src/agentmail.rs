//! Finding and shelling out to the other product's binary.
//!
//! Optional by design: `hsm` works fully without it, and only the `m` key and
//! the `setup-chat` action need it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

/// A send is a subprocess on the UI thread, so it gets a deadline. Long enough
/// for agentmail to resolve a peer and hand the message over, short enough that
/// a wedged child cannot hold the popup.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(15);
/// How often the wait loop looks at the child.
const POLL: Duration = Duration::from_millis(25);

/// Next to our own executable first — one `cargo build` produces both binaries
/// into the same directory — then the configured name on `PATH`.
pub fn find(configured: &str) -> Option<PathBuf> {
    sibling().or_else(|| which(configured))
}

fn sibling() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe
        .parent()?
        .join(format!("agentmail{}", std::env::consts::EXE_SUFFIX));
    candidate.is_file().then_some(candidate)
}

fn which(name: &str) -> Option<PathBuf> {
    let named = Path::new(name);
    if named.is_absolute() || name.contains(std::path::MAIN_SEPARATOR) {
        return is_executable(named).then(|| named.to_path_buf());
    }
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(&file))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

/// `agentmail send <address> <text> [--from <address>]`, reduced to the one
/// line the popup shows. `from` is the session in the invoking pane; without
/// one agentmail decides for itself who the sender is.
pub fn send(bin: &Path, address: &str, text: &str, from: Option<&str>) -> Result<String> {
    send_with_timeout(bin, address, text, from, SEND_TIMEOUT)
}

pub fn send_with_timeout(
    bin: &Path,
    address: &str,
    text: &str,
    from: Option<&str>,
    timeout: Duration,
) -> Result<String> {
    let mut command = Command::new(bin);
    command.arg("send").arg(address).arg(text);
    if let Some(from) = from {
        command.arg("--from").arg(from);
    }
    // No stdin: a child that decides to prompt must fail, not hang the popup.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("running {}", bin.display()))?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!(
                    "agentmail send did not answer within {}s",
                    timeout.as_secs_f32().round()
                ));
            }
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => return Err(anyhow!(e).context("waiting for agentmail")),
        }
    }

    // The child is already gone, so this only drains the pipes.
    let out = child
        .wait_with_output()
        .context("reading agentmail output")?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let line = last_line(&stdout).or_else(|| last_line(&stderr));
    if out.status.success() {
        Ok(line.unwrap_or_else(|| format!("sent to {address}")))
    } else {
        Err(anyhow!(line.unwrap_or_else(|| format!(
            "agentmail send failed ({})",
            out.status
        ))))
    }
}

/// Hands the terminal to `agentmail setup`. On unix this replaces the process,
/// so the popup keeps one pid and closes when setup ends.
#[cfg(unix)]
pub fn run_setup(bin: &Path) -> Result<std::convert::Infallible> {
    use std::os::unix::process::CommandExt;
    // `exec` only returns when it failed.
    Err(anyhow!(Command::new(bin).arg("setup").exec())
        .context(format!("running {} setup", bin.display())))
}

#[cfg(not(unix))]
pub fn run_setup(bin: &Path) -> Result<std::process::ExitStatus> {
    Command::new(bin)
        .arg("setup")
        .status()
        .with_context(|| format!("running {} setup", bin.display()))
}

fn last_line(s: &str) -> Option<String> {
    s.lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_takes_an_absolute_path_as_given() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = dir
            .path()
            .join(format!("am{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&bin, "#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        assert_eq!(
            which(&bin.to_string_lossy()).as_deref(),
            Some(bin.as_path())
        );

        let missing = dir.path().join("nope");
        assert!(which(&missing.to_string_lossy()).is_none());
    }

    #[test]
    fn which_rejects_a_missing_name() {
        assert!(which("definitely-not-a-binary-93f1").is_none());
    }

    #[test]
    fn last_line_skips_trailing_blanks() {
        assert_eq!(last_line("a\nb\n\n").as_deref(), Some("b"));
        assert_eq!(last_line("   \n"), None);
    }

    /// A stand-in `agentmail`: a shell script is enough to see the argv and to
    /// hang on purpose.
    #[cfg(unix)]
    fn script(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("agentmail");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[cfg(unix)]
    #[test]
    fn send_passes_from_and_returns_the_last_line() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = script(dir.path(), r#"echo "argv: $*"; echo queued"#);

        let line = send(&bin, "codex:01a08ad8", "ping", Some("claude:8890a685")).expect("send");
        assert_eq!(line, "queued");

        let bin = script(dir.path(), r#"echo "argv: $*""#);
        let line = send(&bin, "codex:01a08ad8", "ping", Some("claude:8890a685")).expect("send");
        assert_eq!(
            line,
            "argv: send codex:01a08ad8 ping --from claude:8890a685"
        );

        // Without a sender agentmail picks one itself, so the flag is absent.
        let line = send(&bin, "codex:01a08ad8", "ping", None).expect("send");
        assert_eq!(line, "argv: send codex:01a08ad8 ping");
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_send_reports_its_last_line() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = script(dir.path(), "echo 'no such session' >&2; exit 2");
        let err = send(&bin, "codex:nope", "ping", None).expect_err("should fail");
        assert_eq!(err.to_string(), "no such session");
    }

    #[cfg(unix)]
    #[test]
    fn a_wedged_send_is_killed_at_the_deadline() {
        let dir = tempfile::tempdir().expect("tmp");
        let bin = script(dir.path(), "sleep 30");
        let started = Instant::now();
        let err = send_with_timeout(
            &bin,
            "codex:01a08ad8",
            "ping",
            None,
            Duration::from_millis(200),
        )
        .expect_err("should time out");
        assert!(err.to_string().contains("did not answer"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it waited too long"
        );
    }
}
