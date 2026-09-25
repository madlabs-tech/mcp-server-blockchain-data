//! Dashboard password: the `DASHBOARD_PASSWORD` env var if set, else `<config-dir>/dashboard_password`
//! (generated on first start, mode 0600). A pre-rename `admin_token` file is renamed to it once.

use crate::admin::random_hex;
use std::{
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

/// Shortest accepted password.
pub const MIN_LEN: usize = 12;
const FILE_NAME: &str = "dashboard_password";
const LEGACY_FILE_NAME: &str = "admin_token";

/// Where the password came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The `DASHBOARD_PASSWORD` env var (the file is never touched).
    Env,
    /// The file at [`path`]: `created` if generated just now, `migrated` if renamed from `admin_token`.
    File { created: bool, migrated: bool },
}

/// `<dir>/dashboard_password`.
pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

/// The password travels in an HTTP header: printable ASCII only, no spaces.
fn check(pw: &str) -> Result<(), String> {
    if !pw.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(
            "can only use plain letters, numbers and symbols (no spaces or accented letters)"
                .into(),
        );
    }
    if pw.len() < MIN_LEN {
        return Err(format!(
            "is too short: it needs at least {MIN_LEN} characters"
        ));
    }
    Ok(())
}

/// The password from `env` (the `DASHBOARD_PASSWORD` value) or the password file, creating the
/// file if needed. A blank `env` counts as unset (the Claude Desktop bundle always passes it).
/// `Err` is a plain-English message; the caller disables the dashboard.
pub fn ensure_dashboard_password(
    dir: &Path,
    env: Option<&str>,
) -> Result<(String, Source), String> {
    if let Some(v) = env.map(str::trim).filter(|v| !v.is_empty()) {
        check(v).map_err(|why| {
            format!("DASHBOARD_PASSWORD {why}. Fix it, or remove it to use a generated password")
        })?;
        return Ok((v.to_owned(), Source::Env));
    }
    let file = path(dir);
    let mut migrated = false;
    if !file.exists() {
        let legacy = dir.join(LEGACY_FILE_NAME);
        if legacy.exists() {
            match std::fs::rename(&legacy, &file) {
                Ok(()) => migrated = true,
                // Read-only dir (use the old file as is), or another process migrated it first
                // (the old file is gone; fall through and read the new one).
                Err(_) => {
                    if let Some(pw) = read(&legacy)?.filter(|p| p.len() >= MIN_LEN) {
                        let src = Source::File {
                            created: false,
                            migrated: false,
                        };
                        return Ok((pw, src));
                    }
                }
            }
        }
    }
    match read(&file)? {
        Some(pw) if pw.len() >= MIN_LEN => {
            let src = Source::File {
                created: false,
                migrated,
            };
            return Ok((pw, src));
        }
        // ponytail: empty or too short -> replace it; two processes doing this at once may end
        // up with different passwords (only after someone truncated the file by hand).
        Some(_) => match std::fs::remove_file(&file) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot replace {}: {e}", file.display())),
        },
        None => {}
    }
    let (pw, created) = create_if_absent(&file)?;
    Ok((pw, Source::File { created, migrated }))
}

/// Replace the password file with a new random password (keeps no `.bak`).
pub fn reset_dashboard_password(dir: &Path) -> Result<String, String> {
    let file = path(dir);
    let pw = random_hex(32);
    bdm_config::write_atomic(&file, &format!("{pw}\n"), Some(0o600))?;
    let bak = dir.join(format!("{FILE_NAME}.bak"));
    match std::fs::remove_file(&bak) {
        Ok(()) => Ok(pw),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(pw),
        Err(e) => Err(format!("cannot remove {}: {e}", bak.display())),
    }
}

/// Trimmed contents, `None` if missing. `Err` if unreadable, or long enough but with characters
/// that can't be sent (too-short contents are returned so the caller can regenerate them).
fn read(file: &Path) -> Result<Option<String>, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {}: {e}", file.display())),
    };
    let pw = text.trim();
    if pw.len() >= MIN_LEN {
        check(pw).map_err(|why| format!("the password in {} {why}", file.display()))?;
    }
    Ok(Some(pw.to_owned()))
}

/// Create the file only if nobody else did: two processes starting at once (Claude Desktop +
/// Claude Code) must end up with the same password. The full file is written under a temp name
/// and hard-linked into place, which fails with `AlreadyExists` for the loser.
/// Returns the password and whether this call created it.
fn create_if_absent(file: &Path) -> Result<(String, bool), String> {
    let dir = file.parent().ok_or("password path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let pw = random_hex(32);
    let tmp = dir.join(format!(
        ".{FILE_NAME}.tmp-{}-{}",
        std::process::id(),
        random_hex(4)
    ));
    write_new(&tmp, &format!("{pw}\n"))
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    match std::fs::hard_link(&tmp, file) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            Ok((pw, true))
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&tmp);
            read(file)?
                .filter(|p| p.len() >= MIN_LEN)
                .map(|existing| (existing, false))
                .ok_or_else(|| format!("{} is empty or too short", file.display()))
        }
        // ponytail: filesystem without hard links -> plain rename, which is not exclusive.
        Err(_) => std::fs::rename(&tmp, file)
            .map(|()| (pw, true))
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("cannot write {}: {e}", file.display())
            }),
    }
}

/// New file, mode 0600 from the start on unix.
fn write_new(path: &Path, contents: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_src(created: bool, migrated: bool) -> Source {
        Source::File { created, migrated }
    }

    #[test]
    fn env_overrides_file_and_leaves_it_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "file-password-123\n").unwrap();
        let (pw, src) = ensure_dashboard_password(dir.path(), Some(" env-password-123 ")).unwrap();
        assert_eq!((pw.as_str(), src), ("env-password-123", Source::Env));
        let empty = tempfile::tempdir().unwrap();
        ensure_dashboard_password(empty.path(), Some("env-password-123")).unwrap();
        assert!(!path(empty.path()).exists(), "env must not create the file");
        // Blank env (Claude Desktop bundle leaves it "") falls back to the file.
        for blank in ["", "   "] {
            let (pw, src) = ensure_dashboard_password(dir.path(), Some(blank)).unwrap();
            assert_eq!(
                (pw.as_str(), src),
                ("file-password-123", file_src(false, false))
            );
        }
    }

    #[test]
    fn bad_env_values_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["short", "has a space in it", "pässwörd-1234567"] {
            let e = ensure_dashboard_password(dir.path(), Some(bad)).unwrap_err();
            assert!(e.starts_with("DASHBOARD_PASSWORD"), "{e}");
        }
        assert!(ensure_dashboard_password(dir.path(), Some("short"))
            .unwrap_err()
            .contains("at least 12"));
        assert!(!path(dir.path()).exists());
    }

    #[test]
    fn migrates_admin_token() {
        let dir = tempfile::tempdir().unwrap();
        let old = "a".repeat(64);
        std::fs::write(dir.path().join("admin_token"), format!("{old}\n")).unwrap();
        let (pw, src) = ensure_dashboard_password(dir.path(), None).unwrap();
        assert_eq!((pw.as_str(), src), (old.as_str(), file_src(false, true)));
        assert!(!dir.path().join("admin_token").exists());
        assert_eq!(
            std::fs::read_to_string(path(dir.path())).unwrap().trim(),
            old
        );
        // second start: plain file read
        let (pw2, src2) = ensure_dashboard_password(dir.path(), None).unwrap();
        assert_eq!((pw2, src2), (old, file_src(false, false)));
    }

    #[test]
    fn creates_once_then_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let (pw, src) = ensure_dashboard_password(dir.path(), None).unwrap();
        assert_eq!(src, file_src(true, false));
        assert_eq!(pw.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let (again, src) = ensure_dashboard_password(dir.path(), None).unwrap();
        assert_eq!((again, src), (pw, file_src(false, false)));
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert_eq!(leftovers.len(), 1, "temp file cleaned up");
    }

    #[test]
    fn short_file_is_regenerated_and_existing_one_reused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(path(dir.path()), "short\n").unwrap();
        let (pw, src) = ensure_dashboard_password(dir.path(), None).unwrap();
        assert_eq!(src, file_src(true, false));
        assert_ne!(pw, "short");

        std::fs::write(path(dir.path()), "my-own-password\n").unwrap();
        let (pw, _) = ensure_dashboard_password(dir.path(), None).unwrap();
        assert_eq!(pw, "my-own-password");
    }

    #[test]
    fn create_if_absent_keeps_the_winner() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(dir.path());
        // Another process won the race: its file is already in place.
        std::fs::write(&file, "winner-password-1\n").unwrap();
        let (pw, created) = create_if_absent(&file).unwrap();
        assert_eq!((pw.as_str(), created), ("winner-password-1", false));

        // Real concurrency: every thread sees the same password.
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        let seen: Vec<String> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..8)
                .map(|_| s.spawn(|| ensure_dashboard_password(&d, None).unwrap().0))
                .collect();
            hs.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(seen.iter().all(|p| *p == seen[0]), "{seen:?}");
    }

    #[test]
    fn reset_changes_value_and_leaves_no_backup() {
        let dir = tempfile::tempdir().unwrap();
        let (old, _) = ensure_dashboard_password(dir.path(), None).unwrap();
        let new = reset_dashboard_password(dir.path()).unwrap();
        assert_ne!(old, new);
        let (read_back, _) = ensure_dashboard_password(dir.path(), None).unwrap();
        assert_eq!(read_back, new);
        assert!(!dir.path().join("dashboard_password.bak").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
