//! `check_nsclient fleet` — talking to an NSClient++ fleet server rather than to one agent.
//!
//! The fleet server keeps each host's latest metrics snapshot (sent by the agent when a check
//! fails) and its inventory facts. `fleet explain` hands it an error from a monitoring system
//! and gets back an explanation of that error against that evidence — so it can be run from a
//! Nagios notification command, a ticketing hook, or by hand.

use crate::cli::{FleetCommandOptions, FleetCommands};
use crate::config;
use crate::rendering::Rendering;
use anyhow::Context;
use indexmap::IndexMap;
use reqwest::Certificate;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Where to send requests and how to authenticate them, after flags, environment and the
/// stored profile have been reconciled.
#[derive(Debug, Clone, PartialEq)]
struct Connection {
    url: String,
    api_key: String,
    insecure: bool,
    ca: Option<String>,
}

#[derive(Serialize, Debug)]
struct ExplainRequest<'a> {
    host: &'a str,
    error: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    check: Option<&'a str>,
}

/// The server's answer. Mirrors `ExplainResponse` in the fleet server.
#[derive(Serialize, Deserialize, Debug)]
struct ExplainResponse {
    host_id: String,
    hostname: Option<String>,
    explanation: Explanation,
    check: Option<EvidenceRef>,
    snapshot: Option<EvidenceRef>,
    #[serde(default)]
    facts_sources: Vec<String>,
    provider: String,
    model: String,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Explanation {
    summary: String,
    #[serde(default)]
    relevant_evidence: Vec<String>,
    #[serde(default)]
    likely_causes: Vec<String>,
    #[serde(default)]
    suggested_checks: Vec<String>,
    confidence: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct EvidenceRef {
    id: String,
    command: String,
    alias: String,
    status_word: String,
    context_state: String,
    metrics_count: i64,
    reported_at: i64,
}

impl EvidenceRef {
    fn name(&self) -> String {
        if self.alias.is_empty() || self.alias == self.command {
            self.command.clone()
        } else {
            format!("{} ({})", self.command, self.alias)
        }
    }
}

/// The signed-in identity, as `GET /api/me` reports it. Only what `login` shows.
#[derive(Deserialize, Debug)]
struct Me {
    email: String,
    tenant_name: String,
}

pub async fn route_fleet(output: Rendering, args: &FleetCommandOptions) -> anyhow::Result<()> {
    match &args.command {
        FleetCommands::Login {
            id,
            url,
            api_key,
            insecure,
            ca,
        } => {
            let api_key = match api_key {
                Some(k) => k.clone(),
                None => rpassword::prompt_password("API key: ")?,
            };
            let conn = Connection {
                url: url.trim_end_matches('/').to_string(),
                api_key: api_key.trim().to_string(),
                insecure: *insecure,
                ca: ca.clone(),
            };
            // Checked before storing: a typo in the URL or a revoked key should fail here,
            // not at three in the morning inside a notification command.
            let me = whoami(&http_client(&conn, args.timeout_s)?, &conn).await?;
            config::add_fleet_profile(id, &conn.url, conn.insecure, &conn.api_key, conn.ca)?;
            output.print(&format!(
                "Signed in to {} as {} ({}); stored as fleet profile '{id}'",
                conn.url, me.email, me.tenant_name
            ));
        }
        FleetCommands::Logout { id } => {
            config::remove_fleet_profile(id)?;
            output.print(&format!("Fleet profile '{id}' removed"));
        }
        FleetCommands::Explain { host, check, error } => {
            let conn = resolve_connection(args)?;
            let error = read_error(error, std::io::stdin())?;
            let http = http_client(&conn, args.timeout_s)?;
            let answer = explain(
                &http,
                &conn,
                &ExplainRequest {
                    host,
                    error: &error,
                    check: check.as_deref(),
                },
            )
            .await?;
            render_explanation(&output, &answer, now_unix())?;
        }
    }
    Ok(())
}

/// Flags (and their environment variables) win over the stored profile, field by field, so
/// a profile can supply the URL while `FLEET_API_KEY` supplies the key.
fn resolve_connection(args: &FleetCommandOptions) -> anyhow::Result<Connection> {
    let profile = config::get_fleet_profile(args.profile.as_deref())?;
    if let (Some(id), None) = (&args.profile, &profile) {
        anyhow::bail!("Fleet profile '{id}' not found.");
    }
    let url = match (&args.url, &profile) {
        (Some(u), _) => u.clone(),
        (None, Some(p)) => p.url.clone(),
        (None, None) => anyhow::bail!(
            "No fleet server given: pass --url (or set FLEET_URL), or store one with `check_nsclient fleet login`."
        ),
    };
    let api_key = match (&args.api_key, &profile) {
        (Some(k), _) => k.clone(),
        (None, Some(p)) => config::get_fleet_api_key(&p.id)?,
        (None, None) => anyhow::bail!(
            "No fleet API key given: pass --api-key (or set FLEET_API_KEY), or store one with `check_nsclient fleet login`."
        ),
    };
    Ok(Connection {
        url: url.trim_end_matches('/').to_string(),
        api_key,
        insecure: profile.as_ref().is_some_and(|p| p.insecure),
        ca: profile.and_then(|p| p.ca),
    })
}

/// The error to explain: the arguments joined by spaces, or standard input when there are
/// none (or the only one is `-`), so a notification command can pipe a long alert in.
fn read_error(args: &[String], mut stdin: impl Read) -> anyhow::Result<String> {
    let text = if args.is_empty() || (args.len() == 1 && args[0] == "-") {
        let mut buf = String::new();
        stdin
            .read_to_string(&mut buf)
            .context("Failed to read the error from standard input")?;
        buf
    } else {
        args.join(" ")
    };
    let text = text.trim();
    if text.is_empty() {
        anyhow::bail!("No error given: pass it as arguments or on standard input.");
    }
    Ok(text.to_string())
}

fn http_client(conn: &Connection, timeout_s: u64) -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_s))
        .user_agent(concat!("check_nsclient/", env!("CARGO_PKG_VERSION")))
        .danger_accept_invalid_certs(conn.insecure);
    if let Some(ca_file) = &conn.ca {
        let ca_pem = std::fs::read(ca_file)
            .with_context(|| format!("Failed to read CA certificate {ca_file}"))?;
        builder = builder.tls_certs_merge(vec![Certificate::from_pem(&ca_pem)?]);
    }
    Ok(builder.build()?)
}

/// Turn a refused request into an error that says what to do, carrying the server's own
/// message: it is written to be read (`no host named 'web01'`, `explanations are not
/// enabled for this tenant…`).
async fn check_response(res: reqwest::Response) -> anyhow::Result<reqwest::Response> {
    let status = res.status();
    if status.is_success() {
        return Ok(res);
    }
    let body = res.text().await.unwrap_or_default();
    let body = body.trim();
    match status.as_u16() {
        401 => anyhow::bail!(
            "The fleet server did not accept the API key (401). Check it, or sign in again with `check_nsclient fleet login`."
        ),
        _ if body.is_empty() => anyhow::bail!("The fleet server answered {status}"),
        _ => anyhow::bail!("{body} ({status})"),
    }
}

async fn whoami(http: &reqwest::Client, conn: &Connection) -> anyhow::Result<Me> {
    let res = http
        .get(format!("{}/api/me", conn.url))
        .bearer_auth(&conn.api_key)
        .send()
        .await
        .with_context(|| format!("Failed to reach the fleet server at {}", conn.url))?;
    Ok(check_response(res).await?.json().await?)
}

async fn explain(
    http: &reqwest::Client,
    conn: &Connection,
    req: &ExplainRequest<'_>,
) -> anyhow::Result<ExplainResponse> {
    let res = http
        .post(format!("{}/api/explain", conn.url))
        .bearer_auth(&conn.api_key)
        .json(req)
        .send()
        .await
        .with_context(|| format!("Failed to reach the fleet server at {}", conn.url))?;
    check_response(res)
        .await?
        .json()
        .await
        .context("The fleet server's answer could not be read")
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// How long ago `ts` was, the way a person says it.
fn fmt_age(ts: i64, now: i64) -> String {
    let age = (now - ts).max(0);
    match age {
        0..=90 => "just now".to_string(),
        91..=5_400 => format!("{} minutes ago", age / 60),
        5_401..=172_800 => format!("{} hours ago", age / 3600),
        _ => format!("{} days ago", age / 86_400),
    }
}

fn render_explanation(output: &Rendering, r: &ExplainResponse, now: i64) -> anyhow::Result<()> {
    if !output.is_flat() {
        return output.render_nested_single(r);
    }
    let x = &r.explanation;
    let host = r.hostname.as_deref().unwrap_or(&r.host_id);
    let check = match &r.check {
        Some(c) => format!(
            "{}, {} {}",
            c.name(),
            c.status_word,
            fmt_age(c.reported_at, now)
        ),
        None => "no reported check matched the error".to_string(),
    };
    let snapshot = match &r.snapshot {
        Some(s) => format!(
            "{} metrics, reported {} with {}",
            s.metrics_count,
            fmt_age(s.reported_at, now),
            s.name()
        ),
        None => "none on file for this host".to_string(),
    };
    let facts = if r.facts_sources.is_empty() {
        "none on file for this host".to_string()
    } else {
        r.facts_sources.join(", ")
    };

    if output.is_text() {
        let mut s = format!("{host}: {}\n", x.summary);
        for (title, items) in [
            ("Relevant evidence", &x.relevant_evidence),
            ("Likely causes", &x.likely_causes),
            ("Suggested checks", &x.suggested_checks),
        ] {
            if !items.is_empty() {
                s.push_str(&format!("\n{title}:\n"));
                for item in items {
                    s.push_str(&format!("  - {item}\n"));
                }
            }
        }
        s.push_str(&format!(
            "\nBased on:\n  - check: {check}\n  - snapshot: {snapshot}\n  - facts: {facts}\n\
             \nConfidence: {}. Written by {}/{}; check it against the evidence before acting on it.",
            x.confidence, r.provider, r.model
        ));
        output.print(&s);
        return Ok(());
    }

    let mut map = IndexMap::new();
    map.insert("host".to_string(), host.to_string());
    map.insert("summary".to_string(), x.summary.clone());
    map.insert(
        "relevant_evidence".to_string(),
        x.relevant_evidence.join("; "),
    );
    map.insert("likely_causes".to_string(), x.likely_causes.join("; "));
    map.insert(
        "suggested_checks".to_string(),
        x.suggested_checks.join("; "),
    );
    map.insert("confidence".to_string(), x.confidence.clone());
    map.insert("check".to_string(), check);
    map.insert("snapshot".to_string(), snapshot);
    map.insert("facts".to_string(), facts);
    map.insert("model".to_string(), format!("{}/{}", r.provider, r.model));
    output.render_flat_single(&map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::rendering::StringRender;
    use std::cell::RefCell;
    use std::rc::Rc;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const NOW: i64 = 1_800_000_000;

    fn rendering(format: OutputFormat) -> (Rendering, Rc<RefCell<String>>) {
        let sink = Box::new(StringRender::new());
        let out = sink.string.clone();
        (
            Rendering::new(format, OutputStyle::Markdown, false, sink),
            out,
        )
    }

    fn answer() -> serde_json::Value {
        serde_json::json!({
            "host_id": "01JB",
            "hostname": "web01",
            "explanation": {
                "summary": "The system drive on web01 is essentially full.",
                "relevant_evidence": ["C:\\ used = 95.2% against a critical threshold of 90%"],
                "likely_causes": ["IIS logs have not been rotated"],
                "suggested_checks": ["Check the age of files under C:\\inetpub\\logs"],
                "confidence": "high"
            },
            "check": {
                "id": "c1", "command": "check_drivesize", "alias": "disk_c",
                "status_word": "CRITICAL", "context_state": "captured",
                "metrics_count": 521, "reported_at": NOW - 600
            },
            "snapshot": {
                "id": "c1", "command": "check_drivesize", "alias": "disk_c",
                "status_word": "CRITICAL", "context_state": "captured",
                "metrics_count": 521, "reported_at": NOW - 600
            },
            "facts_sources": ["agent", "import:cmdb"],
            "provider": "anthropic",
            "model": "claude-opus-5",
            "input_tokens": 6120,
            "output_tokens": 410
        })
    }

    fn conn(url: &str) -> Connection {
        Connection {
            url: url.to_string(),
            api_key: "nsk_test".to_string(),
            insecure: false,
            ca: None,
        }
    }

    #[tokio::test]
    async fn explain_sends_the_error_with_the_api_key_and_reads_the_answer() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/explain"))
            .and(header("authorization", "Bearer nsk_test"))
            .and(body_json(serde_json::json!({
                "host": "web01",
                "error": "disk_c is CRITICAL",
                "check": "disk_c"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(answer()))
            .expect(1)
            .mount(&server)
            .await;

        let c = conn(&server.uri());
        let r = explain(
            &http_client(&c, 5).unwrap(),
            &c,
            &ExplainRequest {
                host: "web01",
                error: "disk_c is CRITICAL",
                check: Some("disk_c"),
            },
        )
        .await
        .unwrap();
        assert_eq!(r.explanation.confidence, "high");
        assert_eq!(r.check.unwrap().alias, "disk_c");
    }

    #[tokio::test]
    async fn a_refusal_carries_the_servers_reason() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/explain"))
            .respond_with(ResponseTemplate::new(409).set_body_string(
                "explanations are not enabled for this tenant; an admin can turn them on under Settings",
            ))
            .mount(&server)
            .await;
        let c = conn(&server.uri());
        let req = ExplainRequest {
            host: "web01",
            error: "x",
            check: None,
        };
        let err = explain(&http_client(&c, 5).unwrap(), &c, &req)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not enabled for this tenant"), "{err}");
        assert!(err.contains("409"), "{err}");
    }

    #[tokio::test]
    async fn a_rejected_key_says_how_to_fix_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/explain"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let c = conn(&server.uri());
        let req = ExplainRequest {
            host: "web01",
            error: "x",
            check: None,
        };
        let err = explain(&http_client(&c, 5).unwrap(), &c, &req)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("fleet login"), "{err}");
    }

    #[test]
    fn text_output_reads_as_an_explanation_with_its_evidence() {
        let r: ExplainResponse = serde_json::from_value(answer()).unwrap();
        let (out, s) = rendering(OutputFormat::Text);
        render_explanation(&out, &r, NOW).unwrap();
        let text = s.borrow();
        for expected in [
            "web01: The system drive on web01 is essentially full.",
            "Relevant evidence:\n  - C:\\ used = 95.2%",
            "Likely causes:\n  - IIS logs",
            "Suggested checks:\n  - Check the age",
            "check: check_drivesize (disk_c), CRITICAL 10 minutes ago",
            "snapshot: 521 metrics, reported 10 minutes ago with check_drivesize (disk_c)",
            "facts: agent, import:cmdb",
            "Confidence: high. Written by anthropic/claude-opus-5",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn text_output_says_what_evidence_was_missing() {
        let mut v = answer();
        v["check"] = serde_json::Value::Null;
        v["snapshot"] = serde_json::Value::Null;
        v["facts_sources"] = serde_json::json!([]);
        let r: ExplainResponse = serde_json::from_value(v).unwrap();
        let (out, s) = rendering(OutputFormat::Text);
        render_explanation(&out, &r, NOW).unwrap();
        let text = s.borrow();
        assert!(
            text.contains("check: no reported check matched the error"),
            "{text}"
        );
        assert!(
            text.contains("snapshot: none on file for this host"),
            "{text}"
        );
        assert!(text.contains("facts: none on file for this host"), "{text}");
    }

    #[test]
    fn json_output_is_the_servers_answer() {
        let r: ExplainResponse = serde_json::from_value(answer()).unwrap();
        let (out, s) = rendering(OutputFormat::Json);
        render_explanation(&out, &r, NOW).unwrap();
        let v: serde_json::Value = serde_json::from_str(&s.borrow()).unwrap();
        assert_eq!(v, answer());
    }

    #[test]
    fn csv_output_is_one_flat_record() {
        let r: ExplainResponse = serde_json::from_value(answer()).unwrap();
        let (out, s) = rendering(OutputFormat::Csv);
        render_explanation(&out, &r, NOW).unwrap();
        let text = s.borrow();
        assert!(text.contains("summary,The system drive on web01"), "{text}");
        assert!(text.contains("confidence,high"), "{text}");
    }

    #[test]
    fn the_error_comes_from_the_arguments_or_from_stdin() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            read_error(&args(&["disk_c", "is", "CRITICAL"]), &b"ignored"[..]).unwrap(),
            "disk_c is CRITICAL"
        );
        assert_eq!(
            read_error(&args(&[]), &b"  piped alert\n"[..]).unwrap(),
            "piped alert"
        );
        assert_eq!(
            read_error(&args(&["-"]), &b"piped alert"[..]).unwrap(),
            "piped alert"
        );
        assert!(read_error(&args(&[]), &b"   "[..]).is_err());
    }

    fn options(profile: Option<&str>, url: Option<&str>, key: Option<&str>) -> FleetCommandOptions {
        FleetCommandOptions {
            timeout_s: 5,
            profile: profile.map(str::to_string),
            url: url.map(str::to_string),
            api_key: key.map(str::to_string),
            command: FleetCommands::Logout {
                id: "unused".to_string(),
            },
        }
    }

    #[test]
    #[serial_test::serial(config)]
    fn flags_override_the_stored_profile_field_by_field() {
        let tmp = config::mock_test_config();
        config::add_fleet_profile("prod", "https://fleet.example/", true, "nsk_stored", None)
            .unwrap();

        let c = resolve_connection(&options(None, None, None)).unwrap();
        assert_eq!(
            c.url, "https://fleet.example",
            "a trailing slash is not part of the base"
        );
        assert_eq!(c.api_key, "nsk_stored");
        assert!(c.insecure);

        let c = resolve_connection(&options(None, None, Some("nsk_flag"))).unwrap();
        assert_eq!(c.url, "https://fleet.example");
        assert_eq!(c.api_key, "nsk_flag");

        let err = resolve_connection(&options(Some("nope"), None, None)).unwrap_err();
        assert!(err.to_string().contains("'nope' not found"), "{err}");
        drop(tmp);
    }

    #[test]
    #[serial_test::serial(config)]
    fn without_a_profile_both_flags_are_needed() {
        let tmp = config::mock_test_config();
        let err = resolve_connection(&options(None, None, Some("k"))).unwrap_err();
        assert!(err.to_string().contains("--url"), "{err}");
        let err = resolve_connection(&options(None, Some("https://f"), None)).unwrap_err();
        assert!(err.to_string().contains("--api-key"), "{err}");
        let c = resolve_connection(&options(None, Some("https://f"), Some("k"))).unwrap();
        assert_eq!(c.url, "https://f");
        drop(tmp);
    }

    #[test]
    fn ages_read_the_way_a_person_says_them() {
        assert_eq!(fmt_age(NOW, NOW), "just now");
        assert_eq!(fmt_age(NOW - 600, NOW), "10 minutes ago");
        assert_eq!(fmt_age(NOW - 7200, NOW), "2 hours ago");
        assert_eq!(fmt_age(NOW - 864_000, NOW), "10 days ago");
        assert_eq!(
            fmt_age(NOW + 60, NOW),
            "just now",
            "a skewed clock is not negative"
        );
    }
}
