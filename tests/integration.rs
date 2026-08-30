//! End-to-end tests that drive the built `check_nsclient` binary against a real
//! NSClient++ instance.
//!
//! The server is not started from here: `tests/integration/run.sh` (or `run.ps1`)
//! builds and starts the Docker image in `tests/integration/` and then runs this
//! suite with the connection details in the environment. The suite is skipped
//! entirely (every test passes with a notice on stderr) when
//! `CHECK_NSCLIENT_IT_URL` is unset, so a plain `cargo test` never needs Docker.
//!
//! Environment:
//! - `CHECK_NSCLIENT_IT_URL`       e.g. `https://127.0.0.1:8443` (required to run)
//! - `CHECK_NSCLIENT_IT_PASSWORD`  REST password (default `it-password`)
//! - `CHECK_NSCLIENT_IT_USERNAME`  REST user (default `admin`)
//!
//! Every client gets its own configuration directory (a temp dir passed as
//! `APPDATA` / `XDG_CONFIG_HOME` / `HOME`) so the developer's real profiles are
//! never touched. Tokens do go into the real OS keyring, under profile ids that
//! are unique to this run; `Client::drop` logs the profile out again.

use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use tempfile::TempDir;

const BIN: &str = env!("CARGO_BIN_EXE_check_nsclient");

struct Target {
    url: String,
    username: String,
    password: String,
}

fn target() -> Option<Target> {
    let url = std::env::var("CHECK_NSCLIENT_IT_URL").ok()?;
    Some(Target {
        url,
        username: std::env::var("CHECK_NSCLIENT_IT_USERNAME").unwrap_or_else(|_| "admin".into()),
        password: std::env::var("CHECK_NSCLIENT_IT_PASSWORD")
            .unwrap_or_else(|_| "it-password".into()),
    })
}

/// The tests share one NSClient++ instance and several of them mutate it
/// (loading modules, writing settings, resetting log counters). `settings
/// command load` in particular re-reads the on-disk configuration and would
/// undo another test's `modules enable` mid-flight, so tests take turns.
static SERVER: Mutex<()> = Mutex::new(());

fn lock_server() -> MutexGuard<'static, ()> {
    // A panicking test poisons the mutex; the next test should still run
    // (and report its own result) rather than fail on the poison.
    SERVER.lock().unwrap_or_else(|e| e.into_inner())
}

/// An NSClient++ target plus exclusive access to it for the duration of a test.
struct Session {
    target: Target,
    _lock: MutexGuard<'static, ()>,
}

impl std::ops::Deref for Session {
    type Target = Target;
    fn deref(&self) -> &Target {
        &self.target
    }
}

/// Skip the calling test (returning early) unless an NSClient++ target is
/// configured; otherwise wait for exclusive access to it.
macro_rules! require_target {
    () => {
        match target() {
            Some(t) => Session {
                target: t,
                _lock: lock_server(),
            },
            None => {
                eprintln!(
                    "skipped: CHECK_NSCLIENT_IT_URL is not set (see tests/integration/README.md)"
                );
                return;
            }
        }
    };
}

/// A `check_nsclient` invocation context: isolated config dir + one logged in profile.
struct Client {
    config_dir: TempDir,
    profile: String,
    /// Unused on Windows where the config dir is selected via APPDATA only.
    #[allow(dead_code)]
    home: PathBuf,
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

impl Client {
    /// Create a client with a fresh config dir and log it in under a unique profile id.
    fn login(target: &Target) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let profile = format!("it-{}-{}", std::process::id(), n);
        let config_dir = TempDir::new().expect("temp config dir");
        let client = Self {
            home: config_dir.path().to_path_buf(),
            config_dir,
            profile,
        };
        let out = client.run(&[
            "nsclient",
            "auth",
            "login",
            &client.profile,
            "--url",
            &target.url,
            "--username",
            &target.username,
            "--password",
            &target.password,
            "--insecure",
        ]);
        assert_success(&out, "auth login");
        assert_eq!(stdout(&out).trim(), "Successfully logged in");
        client
    }

    fn command(&self) -> Command {
        let mut cmd = Command::new(BIN);
        let dir = self.config_dir.path();
        cmd.env("APPDATA", dir)
            .env("XDG_CONFIG_HOME", dir)
            .env("HOME", dir)
            .env_remove("CHECK_NSCLIENT_PASSWORD");
        cmd
    }

    /// Run the binary with `args` verbatim (no profile is injected).
    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(args)
            .output()
            .expect("failed to spawn check_nsclient")
    }

    /// Run an `nsclient` sub command against this client's profile with the given
    /// global options prepended, e.g. `ns(&["--output", "json"], &["ping"])`.
    fn ns(&self, global: &[&str], args: &[&str]) -> Output {
        let mut all: Vec<&str> = global.to_vec();
        all.extend(["nsclient", "--profile", &self.profile]);
        all.extend(args);
        self.run(&all)
    }

    /// Run an `nsclient` sub command with `--output json`, assert success and parse stdout.
    fn json(&self, args: &[&str]) -> Value {
        let out = self.ns(&["--output", "json"], args);
        assert_success(&out, &args.join(" "));
        serde_json::from_str(&stdout(&out))
            .unwrap_or_else(|e| panic!("{}: invalid json ({e}):\n{}", args.join(" "), stdout(&out)))
    }

    /// Run an `nsclient` sub command with text output, assert success and return stdout.
    fn text(&self, args: &[&str]) -> String {
        let out = self.ns(&[], args);
        assert_success(&out, &args.join(" "));
        stdout(&out)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.run(&["nsclient", "auth", "logout", &self.profile]);
    }
}

/// One shared, logged-in client for the read-only tests. Its profile id is stable
/// so re-runs overwrite the same keyring entries instead of accumulating new ones.
fn shared(target: &Target) -> &'static Client {
    static SHARED: OnceLock<Client> = OnceLock::new();
    SHARED.get_or_init(|| {
        let config_dir = TempDir::new().expect("temp config dir");
        let client = Client {
            home: config_dir.path().to_path_buf(),
            config_dir,
            profile: "check_nsclient-it".into(),
        };
        let out = client.run(&[
            "nsclient",
            "auth",
            "login",
            &client.profile,
            "--url",
            &target.url,
            "--username",
            &target.username,
            "--password",
            &target.password,
            "--insecure",
        ]);
        assert_success(&out, "auth login (shared)");
        client
    })
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_success(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        stdout(out),
        stderr(out)
    );
}

fn assert_failure(out: &Output, what: &str) -> String {
    assert!(
        !out.status.success(),
        "{what} unexpectedly succeeded\nstdout:\n{}",
        stdout(out)
    );
    stderr(out)
}

/// The API token `check_nsclient` stored for `profile` in the OS keyring.
///
/// Reads the very entry the binary wrote (service `check_nsclient`, key
/// `<profile>_token`), which is what lets the logout test prove the token is
/// revoked server side rather than merely forgotten locally.
fn stored_token(profile: &str) -> String {
    keyring::Entry::new("check_nsclient", &format!("{profile}_token"))
        .expect("keyring entry")
        .get_password()
        .expect("a token was stored for the profile")
}

/// Status code of `GET <url>/api/v2/info` when authenticated with `token`.
fn info_status_with_token(url: &str, token: &str) -> u16 {
    let url = format!("{}/api/v2/info", url.trim_end_matches('/'));
    tokio::runtime::Runtime::new()
        .expect("tokio runtime")
        .block_on(async {
            reqwest::Client::builder()
                .danger_accept_invalid_certs(true)
                .build()
                .expect("http client")
                .get(&url)
                .header("Authorization", format!("Bearer {token}"))
                .send()
                .await
                .expect("request")
                .status()
                .as_u16()
        })
}

fn names(list: &Value, key: &str) -> Vec<String> {
    list.as_array()
        .unwrap_or_else(|| panic!("expected a json array, got {list}"))
        .iter()
        .map(|item| item[key].as_str().unwrap_or_default().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// auth / profile
// ---------------------------------------------------------------------------

#[test]
fn profile_list_and_show_reflect_login() {
    let target = require_target!();
    let client = Client::login(&target);

    let out = client.run(&["--output", "json", "profile", "list"]);
    assert_success(&out, "profile list");
    let list: Value = serde_json::from_str(&stdout(&out)).unwrap();
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == client.profile)
        .expect("profile is listed");
    assert_eq!(entry["url"], target.url);
    assert_eq!(entry["username"], target.username);
    assert_eq!(entry["insecure"], true);
    assert_eq!(entry["default"], true, "first profile becomes the default");
    assert_eq!(entry["has_token"], true);
    assert_eq!(entry["has_password"], true);

    let shown = client.text(&["version"]);
    assert!(shown.contains("NSClient++"), "{shown}");

    let out = client.run(&["profile", "show", &client.profile]);
    assert_success(&out, "profile show");
    let text = stdout(&out);
    assert!(text.contains("has_token"), "{text}");
    assert!(text.contains("true"), "{text}");
}

#[test]
fn default_profile_is_used_when_none_is_given() {
    let target = require_target!();
    let client = Client::login(&target);

    let out = client.run(&["nsclient", "ping"]);
    assert_success(&out, "ping via default profile");
    assert!(stdout(&out).starts_with("Successfully pinged NSClient++ version "));
}

#[test]
fn wrong_password_is_rejected_at_login() {
    let target = require_target!();
    let client = Client::login(&target);

    let out = client.run(&[
        "nsclient",
        "auth",
        "login",
        "it-wrong-password",
        "--url",
        &target.url,
        "--username",
        &target.username,
        "--password",
        "definitely-not-the-password",
        "--insecure",
    ]);
    let err = assert_failure(&out, "login with wrong password");
    assert!(err.contains("Failed to login"), "{err}");
    assert!(err.contains("Authentication failed"), "{err}");
}

#[test]
fn refresh_replaces_token_and_keeps_working() {
    let target = require_target!();
    let client = Client::login(&target);

    let out = client.ns(&[], &["auth", "refresh", &client.profile]);
    assert_success(&out, "auth refresh");
    assert_eq!(stdout(&out).trim(), "Token successfully refreshed");

    let out = client.ns(&[], &["ping"]);
    assert_success(&out, "ping after refresh");
}

#[test]
fn logout_removes_profile_and_credentials() {
    let target = require_target!();
    let client = Client::login(&target);

    let out = client.run(&["nsclient", "auth", "logout", &client.profile]);
    assert_success(&out, "auth logout");
    assert_eq!(stdout(&out).trim(), "Successfully logged out");

    let out = client.run(&["--output", "json", "profile", "list"]);
    assert_success(&out, "profile list after logout");
    assert_eq!(stdout(&out).trim(), "No profiles configured");

    let out = client.ns(&[], &["ping"]);
    let err = assert_failure(&out, "ping after logout");
    assert!(err.contains("not found"), "{err}");
}

#[test]
fn auth_status_reports_the_authenticated_user() {
    let target = require_target!();
    let client = Client::login(&target);

    let json = client.json(&["auth", "status"]);
    assert_eq!(json["profile"], client.profile);
    assert_eq!(json["url"], target.url);
    assert_eq!(json["username"], target.username);
    assert_eq!(json["user"], target.username);
    assert_eq!(json["authenticated"], true);

    let text = client.text(&["auth", "status"]);
    assert!(text.contains("│ user"), "{text}");
    assert!(text.contains(&target.username), "{text}");
}

#[test]
fn auth_status_fails_without_a_usable_profile() {
    let target = require_target!();
    let client = Client::login(&target);

    let out = client.run(&["nsclient", "--profile", "no-such-profile", "auth", "status"]);
    let err = assert_failure(&out, "auth status for an unknown profile");
    assert!(err.contains("not found"), "{err}");
}

#[test]
fn logout_revokes_the_token_on_the_server() {
    let target = require_target!();
    let client = Client::login(&target);

    let token = stored_token(&client.profile);
    assert_eq!(
        info_status_with_token(&target.url, &token),
        200,
        "the freshly issued token should be accepted"
    );

    let out = client.run(&["nsclient", "auth", "logout", &client.profile]);
    assert_success(&out, "auth logout");
    assert!(
        stdout(&out).contains("Successfully logged out"),
        "{}",
        stdout(&out)
    );
    assert!(
        !stdout(&out).contains("Warning"),
        "revoking should have succeeded: {}",
        stdout(&out)
    );

    let status = info_status_with_token(&target.url, &token);
    assert!(
        status == 401 || status == 403,
        "token is still accepted after logout (http {status})"
    );
}

#[test]
fn insecure_flag_is_required_for_self_signed_certificate() {
    let target = require_target!();
    let client = Client::login(&target);

    let out = client.run(&[
        "nsclient",
        "auth",
        "login",
        "it-strict-tls",
        "--url",
        &target.url,
        "--username",
        &target.username,
        "--password",
        &target.password,
    ]);
    let err = assert_failure(&out, "login without --insecure");
    assert!(err.contains("Failed to login"), "{err}");
}

// ---------------------------------------------------------------------------
// ping / version / output formats
// ---------------------------------------------------------------------------

#[test]
fn ping_reports_server_version() {
    let target = require_target!();
    let client = shared(&target);

    let text = client.text(&["ping"]);
    assert!(
        text.starts_with("Successfully pinged NSClient++ version "),
        "{text}"
    );
}

#[test]
fn version_in_every_output_format() {
    let target = require_target!();
    let client = shared(&target);

    let json = client.json(&["version"]);
    assert_eq!(json["name"], "NSClient++");
    let version = json["version"].as_str().expect("version string");
    assert!(!version.is_empty());

    let yaml = stdout(&client.ns(&["--output", "yaml"], &["version"]));
    assert!(yaml.contains("name: NSClient++"), "{yaml}");
    assert!(yaml.contains(&format!("version: {version}")), "{yaml}");

    let csv = stdout(&client.ns(&["--output", "csv"], &["version"]));
    assert!(csv.contains("name,NSClient++"), "{csv}");
    assert!(csv.contains(&format!("version,{version}")), "{csv}");

    let rounded = client.text(&["version"]);
    assert!(rounded.contains("│ name"), "{rounded}");
    let markdown = stdout(&client.ns(&["--output-style", "markdown"], &["version"]));
    assert!(markdown.contains("| name"), "{markdown}");
    let blank = stdout(&client.ns(&["--output-style", "blank"], &["version"]));
    assert!(blank.contains(" name"), "{blank}");
    assert!(!blank.contains('|') && !blank.contains('│'), "{blank}");
}

#[test]
fn debug_flag_logs_requests_to_stderr_only() {
    let target = require_target!();
    let client = shared(&target);

    let out = client.ns(&["-d", "--output", "json"], &["version"]);
    assert_success(&out, "version with -d");
    let err = stderr(&out);
    assert!(err.contains("[debug] GET "), "{err}");
    assert!(err.contains("api/v2/info"), "{err}");
    assert!(err.contains("200 OK from api/v2/info"), "{err}");
    let json: Value = serde_json::from_str(&stdout(&out)).expect("stdout is still clean json");
    assert_eq!(json["name"], "NSClient++");
}

#[test]
fn timeout_and_user_agent_options_are_accepted() {
    let target = require_target!();
    let client = shared(&target);

    let out = client.run(&[
        "nsclient",
        "--profile",
        &client.profile,
        "--timeout-s",
        "5",
        "--user-agent",
        "check_nsclient-it/1.0",
        "ping",
    ]);
    assert_success(&out, "ping with timeout/user-agent");
}

// ---------------------------------------------------------------------------
// modules
// ---------------------------------------------------------------------------

#[test]
fn modules_list_and_show() {
    let target = require_target!();
    let client = shared(&target);

    let loaded = client.json(&["modules", "list"]);
    let loaded_names = names(&loaded, "name");
    for expected in ["CheckSystem", "CheckHelpers", "WEBServer"] {
        assert!(
            loaded_names.contains(&expected.to_string()),
            "{loaded_names:?}"
        );
    }
    for module in loaded.as_array().unwrap() {
        assert_eq!(module["loaded"], true, "{module}");
        assert!(module["metadata"].is_object(), "{module}");
    }

    let all = client.json(&["modules", "list", "--all"]);
    assert!(all.as_array().unwrap().len() >= loaded.as_array().unwrap().len());
    assert!(
        all.as_array().unwrap().iter().any(|m| m["loaded"] == false),
        "--all should include modules that are not loaded"
    );

    let shown = client.json(&["modules", "show", "CheckSystem"]);
    assert_eq!(shown["id"], "CheckSystem");
    assert_eq!(shown["loaded"], true);
    assert!(shown["description"].as_str().unwrap().len() > 10);

    let text = client.text(&["modules", "list"]);
    assert!(text.contains("│ id "), "{text}");
    assert!(
        !text.contains("description"),
        "description hidden by default: {text}"
    );
    let long = client.text(&["modules", "list", "--long"]);
    assert!(long.contains("description"), "{long}");
}

#[test]
fn modules_load_unload_enable_disable_cycle() {
    let target = require_target!();
    let client = Client::login(&target);

    // Pick a module that is available but not currently loaded so the cycle
    // cannot disturb the modules the other tests depend on.
    let all = client.json(&["modules", "list", "--all"]);
    let candidate = all
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["loaded"] == false && m["enabled"] == false)
        .map(|m| m["id"].as_str().unwrap().to_string())
        .expect("an unloaded, disabled module to exercise");
    let state = |client: &Client| {
        let m = client.json(&["modules", "show", &candidate]);
        (m["loaded"] == true, m["enabled"] == true)
    };

    let out = client.ns(&[], &["modules", "load", &candidate]);
    assert_success(&out, "modules load");
    assert!(stdout(&out).starts_with(&format!("Successfully loaded module {candidate}")));
    assert_eq!(state(&client), (true, false));

    let out = client.ns(&[], &["modules", "enable", &candidate]);
    assert_success(&out, "modules enable");
    assert_eq!(state(&client), (true, true));

    let out = client.ns(&[], &["modules", "disable", &candidate]);
    assert_success(&out, "modules disable");
    assert_eq!(state(&client), (true, false));

    let out = client.ns(&[], &["modules", "unload", &candidate]);
    assert_success(&out, "modules unload");
    assert_eq!(
        stdout(&out).trim(),
        format!("Successfully unloaded module {candidate}")
    );
    assert_eq!(state(&client), (false, false));

    let out = client.ns(&[], &["modules", "use", &candidate]);
    assert_success(&out, "modules use");
    assert_eq!(state(&client), (true, true));

    // Restore the original state.
    assert_success(
        &client.ns(&[], &["modules", "disable", &candidate]),
        "restore disable",
    );
    assert_success(
        &client.ns(&[], &["modules", "unload", &candidate]),
        "restore unload",
    );
    assert_eq!(state(&client), (false, false));
}

#[test]
fn unknown_module_is_an_error() {
    let target = require_target!();
    let client = shared(&target);

    let out = client.ns(&["--output", "json"], &["modules", "show", "NoSuchModule"]);
    let err = assert_failure(&out, "modules show NoSuchModule");
    assert!(err.contains("Failed to fetch module NoSuchModule"), "{err}");
}

// ---------------------------------------------------------------------------
// queries
// ---------------------------------------------------------------------------

#[test]
fn queries_list_and_show() {
    let target = require_target!();
    let client = shared(&target);

    let list = client.json(&["queries", "list"]);
    let query_names = names(&list, "name");
    for expected in ["check_ok", "check_warning", "check_critical", "check_cpu"] {
        assert!(
            query_names.contains(&expected.to_string()),
            "{query_names:?}"
        );
    }

    let all = client.json(&["queries", "list", "--all"]);
    assert!(all.as_array().unwrap().len() >= list.as_array().unwrap().len());

    let shown = client.json(&["queries", "show", "check_ok"]);
    assert_eq!(shown["name"], "check_ok");
    assert_eq!(shown["plugin"], "CheckHelpers");
    assert!(shown["metadata"].is_object());

    let text = client.text(&["queries", "list"]);
    assert!(
        text.contains("│ name "),
        "the query name must be visible: {text}"
    );
    assert!(text.contains("check_ok"), "{text}");
}

#[test]
fn queries_execute_returns_structured_result() {
    let target = require_target!();
    let client = shared(&target);

    let ok = client.json(&["queries", "execute", "check_ok"]);
    assert_eq!(ok["command"], "check_ok");
    assert_eq!(ok["result"], 0);
    assert!(!ok["lines"].as_array().unwrap().is_empty());

    let warning = client.json(&["queries", "execute", "check_warning", "message=hello there"]);
    assert_eq!(warning["result"], 1);
    assert!(
        warning["lines"][0]["message"]
            .as_str()
            .unwrap()
            .contains("hello there"),
        "{warning}"
    );

    // Hyphenated key/value arguments and a real system check with performance data.
    let cpu = client.json(&[
        "queries",
        "execute",
        "check_cpu",
        "--warning=load>101",
        "--time=5m",
    ]);
    assert_eq!(cpu["command"], "check_cpu");
    assert_eq!(cpu["result"], 0, "{cpu}");
    let perf = &cpu["lines"][0]["perf"];
    assert!(
        perf.is_object() && !perf.as_object().unwrap().is_empty(),
        "{cpu}"
    );

    let text = client.text(&["queries", "execute", "check_warning", "message=in-text"]);
    assert!(text.contains("│ command │ check_warning"), "{text}");
    assert!(text.contains("in-text"), "{text}");
    assert!(text.contains("│ result  │ WARNING"), "{text}");
}

#[test]
fn execute_nagios_uses_nagios_format_and_exit_codes() {
    let target = require_target!();
    let client = shared(&target);

    for (query, expected_code, expected_word) in [
        ("check_ok", 0, "OK"),
        ("check_warning", 1, "WARNING"),
        ("check_critical", 2, "CRITICAL"),
    ] {
        let out = client.ns(&[], &["queries", "execute-nagios", query, "message=probe"]);
        assert_eq!(
            out.status.code(),
            Some(expected_code),
            "{query}: stdout={} stderr={}",
            stdout(&out),
            stderr(&out)
        );
        let text = stdout(&out);
        assert!(text.contains("probe"), "{query}: {text}");
        assert!(!text.contains('│'), "nagios output must be plain: {text}");

        let out = client.ns(&["--output", "json"], &["queries", "execute-nagios", query]);
        assert_eq!(out.status.code(), Some(expected_code));
        let json: Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(json["result"], expected_word);
        assert_eq!(json["command"], query);
    }

    let out = client.ns(
        &[],
        &[
            "queries",
            "execute-nagios",
            "check_cpu",
            "--warning=load>101",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(
        stdout(&out).contains('|'),
        "perf data separator expected: {}",
        stdout(&out)
    );
}

#[test]
fn unknown_query_yields_unknown_status() {
    let target = require_target!();
    let client = shared(&target);

    // NSClient++ answers an unknown query with a regular UNKNOWN result rather
    // than an HTTP error, so the client must surface it as such.
    let json = client.json(&["queries", "execute", "no_such_query"]);
    assert_eq!(json["result"], 3, "{json}");
    assert!(
        json["lines"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Unknown command"),
        "{json}"
    );

    let text = client.text(&["queries", "execute", "no_such_query"]);
    assert!(text.contains("│ result  │ UNKNOWN"), "{text}");

    let out = client.ns(&[], &["queries", "execute-nagios", "no_such_query"]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stdout(&out).contains("Unknown command"), "{}", stdout(&out));
}

// ---------------------------------------------------------------------------
// aliases
// ---------------------------------------------------------------------------

#[test]
fn aliases_list() {
    let target = require_target!();
    let client = shared(&target);

    let aliases = client.json(&["aliases", "list"]);
    let list = aliases.as_array().expect("an array of aliases");
    assert!(
        !list.is_empty(),
        "CheckExternalScripts ships default aliases: {aliases}"
    );
    let alias = &list[0];
    for key in ["name", "title", "description", "plugin", "query_url"] {
        assert!(alias[key].is_string(), "missing {key} in {alias}");
    }
    assert!(
        alias["query_url"].as_str().unwrap().contains("/queries/"),
        "an alias is executed through the queries endpoint: {alias}"
    );

    let names = names(&aliases, "name");
    let text = client.text(&["aliases", "list"]);
    assert!(text.contains("│ name "), "{text}");
    assert!(text.contains(&names[0]), "{text}");
    assert!(
        !text.contains("description"),
        "description is hidden by default: {text}"
    );
    let long = client.text(&["aliases", "list", "--long"]);
    assert!(long.contains("description"), "{long}");

    // An alias resolves through the regular queries flow.
    let all = client.json(&["aliases", "list", "--all"]);
    assert!(all.as_array().unwrap().len() >= list.len());
}

// ---------------------------------------------------------------------------
// metadata
// ---------------------------------------------------------------------------

#[test]
fn metadata_list_channels_and_counters() {
    let target = require_target!();
    let client = shared(&target);

    let resources = client.json(&["metadata", "list"]);
    let names = names(&resources, "name");
    for expected in ["counters", "channels"] {
        assert!(names.contains(&expected.to_string()), "{names:?}");
    }
    for resource in resources.as_array().unwrap() {
        assert!(resource["title"].is_string(), "{resource}");
        assert!(
            resource["url"].as_str().unwrap().contains("/metadata/"),
            "{resource}"
        );
    }
    let text = client.text(&["metadata", "list"]);
    assert!(text.contains("│ name "), "{text}");
    assert!(text.contains("counters"), "{text}");

    // Channels are registered by submission modules; a stock agent has none,
    // so only the shape is pinned here.
    let channels = client.json(&["metadata", "channels"]);
    for channel in channels.as_array().expect("an array of channels") {
        assert!(channel["name"].is_string(), "{channel}");
        assert!(channel["plugins"].is_array(), "{channel}");
    }
    assert_success(
        &client.ns(&[], &["metadata", "channels"]),
        "metadata channels (text)",
    );

    // Performance counters come from CheckSystem's pdh command, which only
    // exists on Windows; on Linux 0.18.0 answers 500 rather than an empty list.
    let out = client.ns(&["--output", "json"], &["metadata", "counters"]);
    if out.status.success() {
        let counters: Value = serde_json::from_str(&stdout(&out)).expect("counters json");
        for counter in counters.as_array().expect("an array of counters") {
            assert!(counter["name"].is_string(), "{counter}");
        }
    } else {
        let err = stderr(&out);
        assert!(
            err.contains("Failed to fetch performance counters"),
            "{err}"
        );
        assert!(err.contains("CheckSystem"), "{err}");
    }
}

// ---------------------------------------------------------------------------
// tags
// ---------------------------------------------------------------------------

#[test]
fn tags_show() {
    let target = require_target!();
    let client = shared(&target);

    // A stock agent has no tags configured, so json must still be a (possibly
    // empty) object while the table says so in words.
    let tags = client.json(&["tags", "show"]);
    let map = tags.as_object().expect("a tag object");
    for (name, value) in map {
        assert!(value.is_string(), "tag {name} is not a string: {value}");
    }

    let text = client.text(&["tags", "show"]);
    if map.is_empty() {
        assert_eq!(text, "No tags set\n");
    } else {
        let (name, value) = map.iter().next().unwrap();
        assert!(text.contains(name.as_str()), "{text}");
        assert!(text.contains(value.as_str().unwrap()), "{text}");
    }
}

// ---------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------

#[test]
fn events_list_and_clear() {
    let target = require_target!();
    let client = Client::login(&target);

    // The store is usually empty on a freshly started Linux agent (it fills
    // from eventlog / real-time filters), so the contract under test is the
    // shape of the response and that draining it is accepted.
    let events = client.json(&["events", "list"]);
    let list = events.as_array().expect("an array of events");
    for event in list {
        assert!(event["index"].is_number(), "{event}");
        assert!(event["event"].is_string(), "{event}");
        assert!(event["date"].is_string(), "{event}");
        assert!(event["data"].is_object(), "{event}");
    }

    let text = client.text(&["events", "list"]);
    assert!(
        text.contains("index"),
        "the header is always rendered: {text}"
    );

    // `clear` drains the store and hands back what it removed.
    let drained = client.json(&["events", "clear"]);
    assert!(drained.is_array(), "{drained}");
    assert_eq!(
        drained.as_array().unwrap().len(),
        list.len(),
        "clear should return the events that were buffered"
    );

    // Everything was drained, so a second clear finds nothing.
    let again = client.json(&["events", "clear"]);
    assert!(again.as_array().unwrap().is_empty(), "{again}");
    assert!(
        client
            .json(&["events", "list"])
            .as_array()
            .unwrap()
            .is_empty(),
        "the store should be empty after draining it"
    );
}

// ---------------------------------------------------------------------------
// logs
// ---------------------------------------------------------------------------

#[test]
fn logs_list_status_and_reset() {
    let target = require_target!();
    let client = shared(&target);

    let page = client.json(&["logs", "list", "--page", "1", "--size", "5"]);
    assert!(page["content"].is_array(), "{page}");
    assert!(page["content"].as_array().unwrap().len() <= 5);
    assert!(page["count"].is_number() && page["pages"].is_number() && page["limit"].is_number());
    if let Some(record) = page["content"].as_array().unwrap().first() {
        for key in ["level", "date", "file", "line", "message"] {
            assert!(!record[key].is_null(), "missing {key} in {record}");
        }
    }

    let text = client.text(&["logs", "list", "--size", "3"]);
    assert!(text.contains("│ level "), "{text}");
    assert!(!text.contains("│ file "), "file hidden by default: {text}");
    let long = client.text(&["logs", "list", "--size", "3", "--long"]);
    assert!(long.contains("│ file "), "{long}");

    let filtered = client.json(&["logs", "list", "--level", "error", "--size", "50"]);
    for record in filtered["content"].as_array().unwrap() {
        assert_eq!(record["level"].as_str().unwrap().to_lowercase(), "error");
    }

    let status = client.json(&["logs", "status"]);
    assert!(status["errors"].is_number(), "{status}");
    let status_text = client.text(&["logs", "status"]);
    assert!(status_text.contains("│ errors "), "{status_text}");

    let out = client.ns(&[], &["logs", "reset"]);
    assert_success(&out, "logs reset");
    assert_eq!(stdout(&out).trim(), "Successfully reset log status");
    let status = client.json(&["logs", "status"]);
    assert_eq!(status["errors"], 0);
    assert!(
        status["last_error"].is_null() || status["last_error"] == "",
        "{status}"
    );
}

// ---------------------------------------------------------------------------
// scripts
// ---------------------------------------------------------------------------

#[test]
fn scripts_runtimes_and_scripts() {
    let target = require_target!();
    let client = shared(&target);

    let runtimes = client.json(&["scripts", "list-runtimes"]);
    let runtime_names = names(&runtimes, "name");
    for expected in ["ext", "lua"] {
        assert!(
            runtime_names.contains(&expected.to_string()),
            "{runtime_names:?}"
        );
    }
    for runtime in runtimes.as_array().unwrap() {
        assert!(runtime["module"].is_string() && runtime["title"].is_string());
    }
    let text = client.text(&["scripts", "list-runtimes"]);
    assert!(text.contains("│ module "), "{text}");
    assert!(text.contains("CheckExternalScripts"), "{text}");

    let scripts = client.json(&["scripts", "list", "--runtime", "ext"]);
    assert!(scripts.is_array(), "{scripts}");
    let text = client.text(&["scripts", "list", "--runtime", "ext"]);
    assert!(
        text.trim().is_empty() || text.contains("│ script "),
        "{text}"
    );

    // NSClient++ 0.18.0 does not answer the script listing for the lua runtime
    // (HTTP 500 "No response from module"). Accept either a fixed server or
    // the failure — but the failure must carry the server's explanation.
    let out = client.ns(
        &["--output", "json"],
        &["scripts", "list", "--runtime", "lua"],
    );
    if !out.status.success() {
        let err = stderr(&out);
        assert!(err.contains("runtime lua"), "{err}");
        assert!(err.contains("500"), "{err}");
        assert!(err.contains("No response from module"), "{err}");
    }

    let out = client.ns(&[], &["scripts", "list", "--runtime", "no-such-runtime"]);
    let err = assert_failure(&out, "scripts list for unknown runtime");
    assert!(err.contains("no-such-runtime"), "{err}");
}

#[test]
fn scripts_add_show_and_delete_round_trip() {
    let target = require_target!();
    let client = Client::login(&target);

    let name = format!("check_it_{}", std::process::id());
    let dir = TempDir::new().expect("temp dir");
    let file = dir.path().join("probe.sh");
    std::fs::write(&file, "#!/bin/sh\necho \"OK: probe\"\nexit 0\n").unwrap();

    let added = client.text(&[
        "scripts",
        "add",
        "--runtime",
        "ext",
        &name,
        "--file",
        &file.to_string_lossy(),
    ]);
    assert!(added.contains(&name), "{added}");

    let listed = client.json(&["scripts", "list", "--runtime", "ext"]);
    let scripts: Vec<String> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    assert!(scripts.contains(&name), "{scripts:?}");

    // The definition points at the file the upload created.
    let shown = client.text(&["scripts", "show", "--runtime", "ext", &name]);
    assert!(shown.contains(&name), "{shown}");
    assert!(
        !shown.contains('│'),
        "the definition is plain text: {shown}"
    );

    let removed = client.text(&["scripts", "delete", "--runtime", "ext", &name]);
    assert!(removed.to_lowercase().contains("remove"), "{removed}");

    let listed = client.json(&["scripts", "list", "--runtime", "ext"]);
    let scripts: Vec<String> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    assert!(
        !scripts.contains(&name),
        "still listed after delete: {scripts:?}"
    );
}

#[test]
fn scripts_add_reports_a_missing_file() {
    let target = require_target!();
    let client = Client::login(&target);

    let out = client.ns(
        &[],
        &[
            "scripts",
            "add",
            "--runtime",
            "ext",
            "check_missing",
            "--file",
            "definitely/not/here.sh",
        ],
    );
    let err = assert_failure(&out, "scripts add with a missing file");
    assert!(
        err.contains("Failed to read definitely/not/here.sh"),
        "{err}"
    );
}

// ---------------------------------------------------------------------------
// settings
// ---------------------------------------------------------------------------

#[test]
fn settings_status_list_and_descriptions() {
    let target = require_target!();
    let client = shared(&target);

    let status = client.json(&["settings", "status"]);
    assert!(status["context"].is_string(), "{status}");
    assert!(status["type"].is_string(), "{status}");
    assert!(status["has_changed"].is_boolean(), "{status}");
    let text = client.text(&["settings", "status"]);
    assert!(text.contains("│ has_changed "), "{text}");

    let list = client.json(&["settings", "list"]);
    let entries = list.as_array().unwrap();
    assert!(!entries.is_empty());
    assert!(
        entries
            .iter()
            .any(|e| e["path"] == "/modules" && e["key"] == "WEBServer"),
        "WEBServer module setting expected in {list}"
    );

    let descriptions = client.json(&["settings", "descriptions"]);
    let descriptions = descriptions.as_array().unwrap();
    assert!(!descriptions.is_empty());
    let port = descriptions
        .iter()
        .find(|d| d["path"] == "/settings/WEB/server" && d["key"] == "port")
        .unwrap_or_else(|| panic!("web server port description expected"));
    assert!(port["plugins"].is_array(), "{port}");
    assert!(port["type"].is_string(), "{port}");

    let text = client.text(&["settings", "descriptions"]);
    assert!(text.contains("│ key "), "{text}");
    assert!(!text.contains("default_value"), "hidden by default: {text}");
    let long = stdout(&client.ns(&["--output-long"], &["settings", "descriptions"]));
    assert!(long.contains("default_value"), "{long}");
}

#[test]
fn settings_set_and_command_round_trip() {
    let target = require_target!();
    let client = Client::login(&target);

    let path = "/settings/check_nsclient-it";
    let key = format!("key-{}", std::process::id());
    let value = "integration value";

    let out = client.ns(
        &[],
        &[
            "settings", "set", "--path", path, "--key", &key, "--value", value,
        ],
    );
    assert_success(&out, "settings set");
    assert_eq!(stdout(&out).trim(), format!("Updated {path}/{key}"));

    let list = client.json(&["settings", "list"]);
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == path && e["key"] == key)
        .unwrap_or_else(|| panic!("updated setting missing from {list}"));
    assert_eq!(entry["value"], value);

    let status = client.json(&["settings", "status"]);
    assert_eq!(status["has_changed"], true, "{status}");

    // (has_changed flips back to false after `save`, but other tests toggle
    // module settings concurrently, so only the round trip is asserted here.)
    for action in ["save", "load", "reload"] {
        let out = client.ns(&[], &["settings", "command", action]);
        assert_success(&out, &format!("settings command {action}"));
        assert!(stdout(&out).starts_with("Executed "), "{}", stdout(&out));
    }

    let list = client.json(&["settings", "list"]);
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|e| e["path"] == path && e["key"] == key),
        "setting must survive save/load"
    );
}

// ---------------------------------------------------------------------------
// metrics
// ---------------------------------------------------------------------------

#[test]
fn metrics_openmetrics_is_plain_exposition_text() {
    let target = require_target!();
    let client = shared(&target);

    // Same warm-up as metrics_show: the exposition body is empty until
    // CheckSystem has collected once.
    let mut body = String::new();
    for _ in 0..30 {
        let out = client.ns(&[], &["metrics", "openmetrics"]);
        assert_success(&out, "metrics openmetrics");
        body = stdout(&out);
        if !body.trim().is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    assert!(!body.trim().is_empty(), "openmetrics stayed empty");
    assert!(
        !body.trim_start().starts_with('{'),
        "must not be json: {body}"
    );
    assert!(!body.contains('│'), "must not be a table: {body}");
    let sample = body
        .lines()
        .find(|l| l.starts_with("system."))
        .unwrap_or_else(|| body.lines().next().expect("at least one line"));
    let (name, value) = sample
        .rsplit_once(' ')
        .unwrap_or_else(|| panic!("expected `name value`, got {sample:?}"));
    assert!(!name.is_empty(), "{sample:?}");
    assert!(
        value.parse::<f64>().is_ok(),
        "expected a numeric value in {sample:?}"
    );

    // The exposition format is printed verbatim regardless of --output.
    let json_out = client.ns(&["--output", "json"], &["metrics", "openmetrics"]);
    assert_success(&json_out, "metrics openmetrics --output json");
    assert!(!stdout(&json_out).trim_start().starts_with('{'));
}

#[test]
fn metrics_show() {
    let target = require_target!();
    let client = shared(&target);

    // NSClient++ serves an empty body until CheckSystem's first collection cycle
    // (a few seconds after start), which the client reports as an error; wait it out.
    let mut metrics = Value::Null;
    for attempt in 0..30 {
        let out = client.ns(&["--output", "json"], &["metrics", "show"]);
        if out.status.success() {
            metrics = serde_json::from_str(&stdout(&out)).expect("metrics json");
            break;
        }
        let err = stderr(&out);
        assert!(
            err.contains("Empty response from api/v2/metrics"),
            "attempt {attempt}: unexpected error: {err}"
        );
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let map = metrics
        .as_object()
        .expect("metrics did not become available within 30s");
    assert!(!map.is_empty());
    assert!(
        map.keys().any(|k| k.starts_with("system.")),
        "expected system.* metrics from CheckSystem, got {:?}",
        map.keys().take(10).collect::<Vec<_>>()
    );

    let text = client.text(&["metrics", "show"]);
    assert!(text.contains("│ system."), "{text}");
    assert!(
        !text.contains("\"\""),
        "strings must not be json-quoted: {text}"
    );
}
