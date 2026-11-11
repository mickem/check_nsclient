//! Running checks over NRPE, the protocol `check_nrpe` speaks.
//!
//! Unlike the REST API this needs no login and no profile: an NRPE server
//! decides by source address (and, optionally, client certificate) whom it
//! answers. The command behaves like a Nagios plugin throughout -- whatever
//! goes wrong ends up as an UNKNOWN on stdout with exit code 3, never as an
//! error exit that Nagios would read as a WARNING.

mod client;
mod packet;

use crate::cli::{NrpeCommand, NrpeCommandOptions, NrpeProtocolVersion};
use crate::nrpe::client::{ConnectionOptions, ProtocolVersion, Response, TlsOptions};
use crate::rendering::Rendering;
use indexmap::IndexMap;
use serde::Serialize;
use std::time::Duration;

pub use packet::{DEFAULT_PAYLOAD_LENGTH, MAX_PAYLOAD_LENGTH, MIN_PAYLOAD_LENGTH};

/// The command every NRPE server answers without running anything.
const PING_COMMAND: &str = "_NRPE_CHECK";

const UNKNOWN: i32 = 3;

/// The outcome of one NRPE query, as rendered for json/yaml/csv.
#[derive(Debug, Serialize)]
struct NrpeResult {
    command: String,
    /// OK, WARNING, CRITICAL or UNKNOWN.
    result: String,
    /// Plugin output as the server sent it (`message|perfdata`).
    output: String,
}

impl NrpeResult {
    fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("command".to_string(), self.command.clone());
        map.insert("result".to_string(), self.result.clone());
        map.insert("output".to_string(), self.output.clone());
        map
    }
}

/// The Nagios exit code for a result code off the wire.
fn exit_code(result_code: i16) -> i32 {
    match result_code {
        0..=3 => i32::from(result_code),
        _ => UNKNOWN,
    }
}

fn status_name(exit_code: i32) -> &'static str {
    match exit_code {
        0 => "OK",
        1 => "WARNING",
        2 => "CRITICAL",
        _ => "UNKNOWN",
    }
}

fn connection_options(args: &NrpeCommandOptions) -> ConnectionOptions {
    ConnectionOptions {
        host: args.host.clone(),
        port: args.port,
        timeout: Duration::from_secs(args.timeout_s),
        version: match args.protocol_version {
            NrpeProtocolVersion::V2 => ProtocolVersion::V2,
            NrpeProtocolVersion::V4 => ProtocolVersion::V4,
        },
        payload_length: args.payload_length,
        tls: (!args.no_tls).then(|| TlsOptions {
            insecure: args.insecure,
            ca: args.ca.clone(),
            certificate: args.certificate.clone(),
            certificate_key: args.certificate_key.clone(),
        }),
    }
}

/// What goes on the wire: the command, then each argument after a `!`.
fn payload(command: &NrpeCommand) -> anyhow::Result<String> {
    match command {
        NrpeCommand::Ping {} => Ok(PING_COMMAND.to_string()),
        NrpeCommand::Query { command, args } => {
            // The protocol has no way to escape its separator, so a `!` in the
            // command name would silently turn its tail into an argument. In an
            // argument it merely splits that argument, which is how `check_nrpe`
            // users pass several at once, so that stays allowed.
            if command.is_empty() || command.contains('!') {
                anyhow::bail!("'{command}' is not a valid command name");
            }
            let mut payload = command.clone();
            for arg in args {
                payload.push('!');
                payload.push_str(arg);
            }
            Ok(payload)
        }
    }
}

fn render(output: &Rendering, command: &str, code: i32, text: &str) -> anyhow::Result<()> {
    if output.is_text() {
        output.print(text);
        return Ok(());
    }
    let result = NrpeResult {
        command: command.to_string(),
        result: status_name(code).to_string(),
        output: text.to_string(),
    };
    output.render_single(&result, NrpeResult::to_dict)
}

/// Route an `nrpe` sub command and return the process exit code, which is the
/// Nagios status of the check (or UNKNOWN when it could not be run).
pub async fn route_nrpe(output: Rendering, args: &NrpeCommandOptions) -> anyhow::Result<i32> {
    let outcome: anyhow::Result<(String, Response)> = async {
        let payload = payload(&args.command)?;
        let response = client::query(&connection_options(args), &payload).await?;
        Ok((payload, response))
    }
    .await;
    match outcome {
        Ok((payload, response)) => {
            let code = exit_code(response.result_code);
            render(&output, &payload, code, &response.output)?;
            Ok(code)
        }
        Err(e) => {
            let command = match &args.command {
                NrpeCommand::Ping {} => PING_COMMAND,
                NrpeCommand::Query { command, .. } => command,
            };
            render(&output, command, UNKNOWN, &format!("UNKNOWN: {e:#}"))?;
            Ok(UNKNOWN)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{OutputFormat, OutputStyle};
    use crate::nrpe::packet::{TYPE_RESPONSE, encode_v2, read_packet};
    use crate::rendering::StringRender;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    fn args(port: u16, command: NrpeCommand) -> NrpeCommandOptions {
        NrpeCommandOptions {
            host: "127.0.0.1".into(),
            port,
            timeout_s: 10,
            protocol_version: NrpeProtocolVersion::V2,
            payload_length: 1024,
            no_tls: true,
            insecure: false,
            ca: None,
            certificate: None,
            certificate_key: None,
            command,
        }
    }

    fn query(command: &str, arguments: &[&str]) -> NrpeCommand {
        NrpeCommand::Query {
            command: command.into(),
            args: arguments.iter().map(|a| a.to_string()).collect(),
        }
    }

    /// A plain text server that answers one query with `result_code` and the
    /// query it was sent.
    async fn server(result_code: i16) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let query = read_packet(&mut tcp, 1024).await.unwrap();
            let text = [b"got ".as_slice(), &query.payload, b"|'a'=1"].concat();
            let answer = encode_v2(TYPE_RESPONSE, result_code, &text, 1024).unwrap();
            tcp.write_all(&answer).await.unwrap();
        });
        port
    }

    async fn run(format: OutputFormat, args: &NrpeCommandOptions) -> (i32, String) {
        let sink = Box::new(StringRender::new());
        let text = sink.string.clone();
        let output = Rendering::new(format, OutputStyle::Rounded, false, sink);
        let code = route_nrpe(output, args).await.unwrap();
        let text = text.borrow().clone();
        (code, text)
    }

    #[test]
    fn payload_joins_command_and_arguments() {
        assert_eq!(payload(&NrpeCommand::Ping {}).unwrap(), "_NRPE_CHECK");
        assert_eq!(payload(&query("check_cpu", &[])).unwrap(), "check_cpu");
        assert_eq!(
            payload(&query("check_cpu", &["warn=load>80", "--show-all", "a!b"])).unwrap(),
            "check_cpu!warn=load>80!--show-all!a!b"
        );
        assert!(payload(&query("", &[])).is_err());
        assert!(payload(&query("check!cpu", &[])).is_err());
    }

    #[test]
    fn exit_code_is_the_nagios_status() {
        assert_eq!(exit_code(0), 0);
        assert_eq!(exit_code(1), 1);
        assert_eq!(exit_code(2), 2);
        assert_eq!(exit_code(3), 3);
        assert_eq!(exit_code(-1), 3);
        assert_eq!(exit_code(42), 3);
        assert_eq!(status_name(2), "CRITICAL");
        assert_eq!(status_name(3), "UNKNOWN");
    }

    #[test]
    fn connection_options_follow_the_arguments() {
        let mut args = args(5666, NrpeCommand::Ping {});
        assert!(connection_options(&args).tls.is_none());
        args.no_tls = false;
        args.insecure = true;
        args.ca = Some("ca.pem".into());
        args.protocol_version = NrpeProtocolVersion::V4;
        args.timeout_s = 3;
        let options = connection_options(&args);
        assert_eq!(options.version, ProtocolVersion::V4);
        assert_eq!(options.timeout, Duration::from_secs(3));
        let tls = options.tls.expect("TLS is the default");
        assert!(tls.insecure);
        assert_eq!(tls.ca.as_deref(), Some("ca.pem"));
    }

    #[tokio::test]
    async fn query_prints_the_output_and_returns_the_status() {
        let port = server(1).await;
        let (code, text) = run(
            OutputFormat::Text,
            &args(port, query("check_cpu", &["warn=80"])),
        )
        .await;
        assert_eq!(code, 1);
        assert_eq!(text, "got check_cpu!warn=80|'a'=1\n");
    }

    #[tokio::test]
    async fn ping_sends_the_nrpe_check_command() {
        let port = server(0).await;
        let (code, text) = run(OutputFormat::Text, &args(port, NrpeCommand::Ping {})).await;
        assert_eq!(code, 0);
        assert_eq!(text, "got _NRPE_CHECK|'a'=1\n");
    }

    #[tokio::test]
    async fn query_renders_json() {
        let port = server(2).await;
        let (code, text) = run(OutputFormat::Json, &args(port, query("check_cpu", &[]))).await;
        assert_eq!(code, 2);
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "command": "check_cpu",
                "result": "CRITICAL",
                "output": "got check_cpu|'a'=1",
            })
        );
    }

    #[tokio::test]
    async fn failure_is_reported_as_unknown() {
        // Bind and drop to get a port that is closed right now.
        let port = {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            listener.local_addr().unwrap().port()
        };
        let (code, text) = run(OutputFormat::Text, &args(port, query("check_cpu", &[]))).await;
        assert_eq!(code, 3);
        assert!(
            text.starts_with(&format!("UNKNOWN: failed to connect to 127.0.0.1:{port}")),
            "{text}"
        );

        let (code, text) = run(OutputFormat::Json, &args(port, query("check!cpu", &[]))).await;
        assert_eq!(code, 3);
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["result"], "UNKNOWN");
        assert_eq!(
            json["output"],
            "UNKNOWN: 'check!cpu' is not a valid command name"
        );
    }
}
