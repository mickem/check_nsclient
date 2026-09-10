//! Delivering passive check results to Nagios Core.
//!
//! Nagios has two ways for a local process to hand it a check result:
//!
//! * the **external command file** (`nagios.cmd`, a named pipe), which takes one
//!   `PROCESS_SERVICE_CHECK_RESULT` line per result, and
//! * the **check result spool directory** (`check_result_path`), which takes a
//!   file holding any number of results plus an empty `<file>.ok` marker that
//!   tells the reaper the file is complete.
//!
//! Both are written by the user Nagios runs its checks as, which is what makes
//! this usable from an active check without any extra daemon (NSCA, NRDP).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// One service check result as Nagios wants it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassiveResult {
    pub host: String,
    pub service: String,
    /// 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN (anything else is reported as UNKNOWN).
    pub status: i32,
    /// Plugin output (`message|perfdata`); may span several lines.
    pub output: String,
    /// When the check ran, seconds since the unix epoch. Nagios records this as
    /// the service's last check time, so a stale result stays visibly stale.
    pub timestamp: i64,
}

impl PassiveResult {
    fn nagios_status(&self) -> i32 {
        if (0..=3).contains(&self.status) {
            self.status
        } else {
            3
        }
    }
}

/// Seconds since the unix epoch.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Escape plugin output the way Nagios unescapes it (`\` as `\\`, newline as
/// `\n`), so multi-line output survives a single-line command.
pub fn escape_output(output: &str) -> String {
    let mut escaped = String::with_capacity(output.len());
    for c in output.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => {}
            _ => escaped.push(c),
        }
    }
    escaped
}

/// Host and service names are fields of a `;` separated command, so they
/// cannot carry a `;` (or a line break).
fn field(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            ';' | '\n' | '\r' => '_',
            _ => c,
        })
        .collect()
}

/// The `PROCESS_SERVICE_CHECK_RESULT` external command for `result`.
pub fn command_line(result: &PassiveResult) -> String {
    format!(
        "[{}] PROCESS_SERVICE_CHECK_RESULT;{};{};{};{}",
        result.timestamp,
        field(&result.host),
        field(&result.service),
        result.nagios_status(),
        escape_output(&result.output)
    )
}

/// A check result file holding every result in `results`, in the format the
/// Nagios check result reaper reads from `check_result_path`.
///
/// `file_time` must be *now*: Nagios discards files older than
/// `max_check_result_file_age` based on it. The per-result `start_time` and
/// `finish_time` carry the result's own timestamp instead.
pub fn checkresult_file(results: &[PassiveResult], file_time: i64) -> String {
    let mut content = String::new();
    content.push_str("### Passive Check Result File ###\n");
    content.push_str("# Written by check_nsclient\n");
    content.push_str(&format!("file_time={file_time}\n"));
    for result in results {
        content.push_str("\n### Nagios Service Check Result ###\n");
        content.push_str(&format!("host_name={}\n", field(&result.host)));
        content.push_str(&format!("service_description={}\n", field(&result.service)));
        content.push_str("check_type=1\n");
        content.push_str("check_options=0\n");
        content.push_str("scheduled_check=0\n");
        content.push_str("reschedule_check=0\n");
        content.push_str("latency=0.0\n");
        content.push_str(&format!("start_time={}.0\n", result.timestamp));
        content.push_str(&format!("finish_time={}.0\n", result.timestamp));
        content.push_str("early_timeout=0\n");
        content.push_str("exited_ok=1\n");
        content.push_str(&format!("return_code={}\n", result.nagios_status()));
        content.push_str(&format!("output={}\n", escape_output(&result.output)));
    }
    content
}

/// Check that the command file can be written to before anything is polled,
/// so a wrong path fails without consuming results from the cache.
pub fn check_command_file(path: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(path)
        .map_err(|e| anyhow::anyhow!("Cannot access command file {}: {e}", path.display()))?;
    if metadata.is_dir() {
        anyhow::bail!("Command file {} is a directory", path.display());
    }
    Ok(())
}

/// Check that the spool directory exists before anything is polled.
pub fn check_spool_dir(dir: &Path) -> anyhow::Result<()> {
    let metadata = std::fs::metadata(dir)
        .map_err(|e| anyhow::anyhow!("Cannot access spool directory {}: {e}", dir.display()))?;
    if !metadata.is_dir() {
        anyhow::bail!("Spool path {} is not a directory", dir.display());
    }
    Ok(())
}

/// Append one command line per result to the Nagios command file.
///
/// The lines are written with a single `write` so that, on the named pipe
/// Nagios normally uses, they are not interleaved with another writer's.
/// Opening a pipe blocks until Nagios is reading from it; the check timeout
/// bounds that when Nagios is down.
pub fn write_command_file(path: &Path, results: &[PassiveResult]) -> anyhow::Result<()> {
    if results.is_empty() {
        return Ok(());
    }
    let mut payload = String::new();
    for result in results {
        payload.push_str(&command_line(result));
        payload.push('\n');
    }
    let mut file = OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|e| anyhow::anyhow!("Cannot open command file {}: {e}", path.display()))?;
    file.write_all(payload.as_bytes())
        .map_err(|e| anyhow::anyhow!("Cannot write to command file {}: {e}", path.display()))?;
    file.flush()?;
    Ok(())
}

/// Write the results as one check result file into `dir` and mark it complete
/// with the `.ok` file the reaper waits for. Returns the path of the result file.
pub fn write_spool_file(
    dir: &Path,
    results: &[PassiveResult],
    file_time: i64,
) -> anyhow::Result<Option<PathBuf>> {
    if results.is_empty() {
        return Ok(None);
    }
    let content = checkresult_file(results, file_time);
    let (path, mut file) = create_spool_file(dir)?;
    file.write_all(content.as_bytes())
        .map_err(|e| anyhow::anyhow!("Cannot write spool file {}: {e}", path.display()))?;
    file.flush()?;
    drop(file);
    // The marker is what tells Nagios the file is complete; it is created only
    // once the content is on disk so a half written file is never reaped.
    let marker = PathBuf::from(format!("{}.ok", path.display()));
    File::create(&marker)
        .map_err(|e| anyhow::anyhow!("Cannot create marker {}: {e}", marker.display()))?;
    Ok(Some(path))
}

/// Create a uniquely named file the way Nagios's own `mkstemp("cXXXXXX")` does:
/// a `c` followed by six characters, created exclusively so two feeds running
/// at once cannot share a file.
fn create_spool_file(dir: &Path) -> anyhow::Result<(PathBuf, File)> {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ ((std::process::id() as u64) << 32);
    for _ in 0..32 {
        // A small xorshift is plenty: uniqueness comes from the exclusive create.
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut name = String::from("c");
        let mut value = seed;
        for _ in 0..6 {
            name.push(ALPHABET[(value % ALPHABET.len() as u64) as usize] as char);
            value /= ALPHABET.len() as u64;
        }
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => anyhow::bail!("Cannot create spool file {}: {e}", path.display()),
        }
    }
    anyhow::bail!(
        "Could not find a free file name in spool directory {}",
        dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(status: i32, output: &str) -> PassiveResult {
        PassiveResult {
            host: "srv1".into(),
            service: "Disk C".into(),
            status,
            output: output.into(),
            timestamp: 1757232000,
        }
    }

    #[test]
    fn command_line_matches_the_external_command_format() {
        assert_eq!(
            command_line(&result(1, "WARNING: 85% used|'C:'=85%;80;90")),
            "[1757232000] PROCESS_SERVICE_CHECK_RESULT;srv1;Disk C;1;WARNING: 85% used|'C:'=85%;80;90"
        );
    }

    #[test]
    fn output_is_escaped_and_fields_are_kept_on_one_line() {
        let mut r = result(0, "line one\r\nline two\\end");
        r.host = "bad;host\n".into();
        assert_eq!(
            command_line(&r),
            "[1757232000] PROCESS_SERVICE_CHECK_RESULT;bad_host_;Disk C;0;line one\\nline two\\\\end"
        );
    }

    #[test]
    fn unexpected_status_is_reported_as_unknown() {
        assert!(command_line(&result(7, "x")).contains(";Disk C;3;x"));
        assert!(command_line(&result(-1, "x")).contains(";Disk C;3;x"));
    }

    #[test]
    fn checkresult_file_holds_every_result() {
        let content = checkresult_file(&[result(0, "OK"), result(2, "CRITICAL")], 1757232100);
        assert!(content.starts_with("### Passive Check Result File ###\n"));
        assert!(content.contains("file_time=1757232100\n"));
        assert_eq!(
            content
                .matches("### Nagios Service Check Result ###")
                .count(),
            2
        );
        assert!(content.contains("host_name=srv1\nservice_description=Disk C\ncheck_type=1\n"));
        assert!(content.contains("start_time=1757232000.0\nfinish_time=1757232000.0\n"));
        assert!(content.contains("return_code=2\noutput=CRITICAL\n"));
    }

    #[test]
    fn write_command_file_appends_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nagios.cmd");
        std::fs::write(&path, "[1] EXISTING\n").unwrap();

        write_command_file(&path, &[result(0, "OK"), result(1, "WARN")]).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            written,
            "[1] EXISTING\n\
             [1757232000] PROCESS_SERVICE_CHECK_RESULT;srv1;Disk C;0;OK\n\
             [1757232000] PROCESS_SERVICE_CHECK_RESULT;srv1;Disk C;1;WARN\n"
        );
    }

    #[test]
    fn write_command_file_reports_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.cmd");
        let err = write_command_file(&path, &[result(0, "OK")]).unwrap_err();
        assert!(
            err.to_string().contains("Cannot open command file"),
            "{err}"
        );
        assert!(check_command_file(&path).is_err());
        assert!(
            check_command_file(dir.path()).is_err(),
            "a directory is not a command file"
        );
    }

    #[test]
    fn write_spool_file_creates_the_result_and_marker_files() {
        let dir = tempfile::tempdir().unwrap();

        let path = write_spool_file(dir.path(), &[result(0, "OK")], 1757232100)
            .unwrap()
            .expect("a file is written for a non-empty batch");

        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name.len(), 7, "{name}");
        assert!(name.starts_with('c'), "{name}");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("output=OK\n")
        );
        let marker = PathBuf::from(format!("{}.ok", path.display()));
        assert!(marker.is_file(), "marker {} missing", marker.display());

        let second = write_spool_file(dir.path(), &[result(0, "OK")], 1757232100)
            .unwrap()
            .unwrap();
        assert_ne!(path, second, "every feed gets its own file");
    }

    #[test]
    fn nothing_is_written_for_an_empty_batch() {
        let dir = tempfile::tempdir().unwrap();
        assert!(write_spool_file(dir.path(), &[], 1).unwrap().is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let missing = dir.path().join("missing.cmd");
        write_command_file(&missing, &[]).unwrap();
        assert!(!missing.exists());
    }

    #[test]
    fn spool_dir_is_validated() {
        let dir = tempfile::tempdir().unwrap();
        assert!(check_spool_dir(dir.path()).is_ok());
        assert!(check_spool_dir(&dir.path().join("nope")).is_err());
        let file = dir.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(check_spool_dir(&file).is_err());
    }
}
