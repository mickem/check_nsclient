use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

/// Define the available output formats
#[derive(ValueEnum, Clone, Debug)]
pub enum OutputFormat {
    Text,
    Json,
    Yaml,
    Csv,
}

#[derive(ValueEnum, Clone, Debug)]
pub enum OutputStyle {
    Rounded,
    Blank,
    Markdown,
}

fn parse_kv_option(raw: &str) -> Result<(String, String), String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("expected KEY[=VALUE]".to_string());
    }
    let (raw_key, value_part) = match trimmed.split_once('=') {
        Some((key, value)) => (key, Some(value)),
        None => (trimmed, None),
    };
    let key = raw_key.trim_start_matches('-').trim();
    if key.is_empty() {
        return Err("option name cannot be empty".to_string());
    }
    let value = value_part
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| "".to_string());
    Ok((key.to_string(), value))
}

#[derive(Parser)]
#[command(name = "check_nsclient")]
#[command(author)]
#[command(version)]
#[command(about = "NSClient command line client", long_about = None)]
pub struct Cli {
    /// Print debug information (HTTP requests/responses) to stderr; repeat for more detail
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub(crate) debug: u8,

    /// Show more information
    #[arg(long, value_enum)]
    pub output_long: bool,

    /// Set the output style (if format is text)
    #[arg(long, value_enum, default_value_t = OutputStyle::Rounded)]
    pub output_style: OutputStyle,

    /// Set the output format (text, json, yaml or csv)
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub output: OutputFormat,

    /// Use WSL workarounds for keyring (token storage)
    #[arg(long)]
    pub(crate) wsl: bool,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub enum NSClientCommands {
    /// Send a ping to ensure NSClient++ can be reached
    Ping {},
    /// Show version
    Version {},
    /// Manage modules
    Modules {
        #[command(subcommand)]
        command: ModulesCommand,
    },
    /// Execute and show queries
    Queries {
        #[command(subcommand)]
        command: QueriesCommand,
    },
    /// List query aliases
    Aliases {
        #[command(subcommand)]
        command: AliasesCommand,
    },
    /// Inspect the event store
    Events {
        #[command(subcommand)]
        command: EventsCommand,
    },
    /// Show the agent tags
    Tags {
        #[command(subcommand)]
        command: TagsCommand,
    },
    /// Discover what this agent can check and submit to
    Metadata {
        #[command(subcommand)]
        command: MetadataCommand,
    },
    /// Inspect/acknowledge logs
    Logs {
        #[command(subcommand)]
        command: LogsCommand,
    },
    /// Manage scripts
    Scripts {
        #[command(subcommand)]
        command: ScriptsCommand,
    },
    /// Inspect settings
    Settings {
        #[command(subcommand)]
        command: SettingsCommand,
    },
    /// Metrics & health surfaces
    Metrics {
        #[command(subcommand)]
        command: MetricsCommand,
    },
    /// Auth / session helpers
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Legacy command (same as client)
    Test {},
    /// Connect to and interact with NSClient
    Client {},
}
#[derive(Args)]
pub struct NSClientCommandOptions {
    /// The timeout in seconds
    #[arg(short, long, default_value_t = 30)]
    pub(crate) timeout_s: u64,
    /// The user agent to use
    #[arg(short = 'A', long, default_value = "nscp-client")]
    pub(crate) user_agent: String,
    /// The profile to connect to
    #[arg(short = 'p', long)]
    pub(crate) profile: Option<String>,
    /// The subcommand to run
    #[command(subcommand)]
    pub(crate) command: NSClientCommands,
}

#[derive(Subcommand)]
pub enum ProfileCommands {
    /// List all profiles
    List {},
    /// Show details about a profile
    Show { id: String },
    /// Set the default profile
    SetDefault { id: String },
    /// Remove a profile
    Remove { id: String },
}

#[derive(Subcommand)]
pub enum Commands {
    /// Communicate with NSClient
    #[command(name = "nsclient")]
    NSClient(NSClientCommandOptions),
    /// Show version
    Version {},
    /// Manage profiles
    Profile {
        #[command(subcommand)]
        command: ProfileCommands,
    },
}

#[derive(Subcommand)]
pub enum ModulesCommand {
    /// List modules
    List {
        /// List all modules (not just loaded modules)
        #[arg(short, long)]
        all: bool,
        /// Show all information (same as --output-long)
        #[arg(short, long)]
        long: bool,
    },
    /// Show details about a module
    Show { id: String },
    /// Load a module so it can be used.
    Load { id: String },
    /// Unload a module so it cannot be used.
    Unload { id: String },
    /// Enable a module so it will be loaded on startup.
    Enable { id: String },
    /// Disable a module so it will no longer be loaded on startup.
    Disable { id: String },
    /// Load and enable a module so it can be used.
    Use { id: String },
    /// Upload a module archive and load it.
    ///
    /// The archive is stored as <module-path>/<ID>.zip on the server and loaded
    /// immediately, so its code runs as the user the service runs as. Only
    /// upload archives you trust.
    Upload {
        /// Name to store the module under
        id: String,
        /// Module archive (.zip) to upload
        #[arg(long)]
        file: String,
    },
}

#[derive(Subcommand)]
pub enum QueriesCommand {
    /// List queries
    List {
        /// List all queries (not just loaded queries)
        #[arg(short, long)]
        all: bool,
        /// Show all information (same as --output-long)
        #[arg(short, long)]
        long: bool,
    },
    /// Show details about a query
    Show { id: String },
    /// Execute a query (show output)
    #[command(trailing_var_arg = true)]
    Execute {
        id: String,
        /// Arguments to pass to the query (use KEY=VALUE, --KEY=VALUE or bare KEY for boolean flags)
        #[arg(value_name = "KEY=VALUE", value_parser = parse_kv_option, allow_hyphen_values = true)]
        args: Vec<(String, String)>,
    },
    /// Execute a query (Nagios compatible output)
    #[command(trailing_var_arg = true)]
    ExecuteNagios {
        id: String,
        /// Additional query options (use KEY=VALUE or --KEY=VALUE, values keep order specified)
        #[arg(value_name = "KEY=VALUE", value_parser = parse_kv_option, allow_hyphen_values = true)]
        args: Vec<(String, String)>,
    },
}

#[derive(Subcommand)]
pub enum AliasesCommand {
    /// List aliases
    List {
        /// List all aliases (including modules that are not loaded)
        #[arg(short, long)]
        all: bool,
        /// Show all information (same as --output-long)
        #[arg(short, long)]
        long: bool,
    },
}

#[derive(Subcommand)]
pub enum MetadataCommand {
    /// List the available metadata resources
    List {},
    /// List the performance counters this host exposes (Windows)
    Counters {},
    /// List the registered submission channels and their modules
    Channels {},
}

#[derive(Subcommand)]
pub enum TagsCommand {
    /// Show all tags set on this agent
    Show {},
}

#[derive(Subcommand)]
pub enum EventsCommand {
    /// List buffered events
    List {},
    /// Drain the event store (shows the events that were removed)
    Clear {},
}

#[derive(Subcommand)]
pub enum LogsCommand {
    /// List log records (paginated)
    List {
        /// Page number (starts at 1)
        #[arg(long, default_value_t = 1u64)]
        page: u64,
        /// Page size
        #[arg(long, default_value_t = 50u64)]
        size: u64,
        /// Filter by level (INFO/WARNING/ERROR/...)
        #[arg(long)]
        level: Option<String>,
        /// Show file/line columns
        #[arg(short, long)]
        long: bool,
    },
    /// Show current log counter status
    Status {},
    /// Reset aggregated log status counters
    Reset {},
    /// Drop every buffered log record
    Clear {},
    /// Append a record to the agent log
    Add {
        /// Message to log
        #[arg(long)]
        message: String,
        /// Level to log at (debug/info/warning/error)
        #[arg(long, default_value = "info")]
        level: String,
        /// File to attribute the record to
        #[arg(long, default_value = "check_nsclient")]
        file: String,
        /// Line to attribute the record to
        #[arg(long, default_value_t = 0)]
        line: u64,
    },
}

#[derive(Subcommand)]
pub enum ScriptsCommand {
    /// List the available script runtimes
    ListRuntimes {},
    /// List the scripts of a runtime
    List {
        #[arg(long)]
        runtime: String,
    },
    /// Show a script definition (or the script itself)
    Show {
        /// Runtime the script belongs to (ext, lua, py)
        #[arg(long)]
        runtime: String,
        /// Name of the script (or a path such as scripts/check_x.sh)
        script: String,
    },
    /// Upload a script, replacing any existing definition
    Add {
        /// Runtime to add the script to (ext, lua, py)
        #[arg(long)]
        runtime: String,
        /// Name to store the script under
        script: String,
        /// File to read the script from
        #[arg(long)]
        file: String,
    },
    /// Delete a script definition (or the script file)
    Delete {
        /// Runtime the script belongs to (ext, lua, py)
        #[arg(long)]
        runtime: String,
        /// Name of the script (or a path such as scripts/check_x.sh)
        script: String,
    },
}

#[derive(Subcommand)]
pub enum SettingsCommand {
    /// Summary if settings are dirty
    Status {},
    /// List settings entries
    List {
        /// Only list keys under this path (default: the whole store)
        #[arg(long, default_value = "")]
        path: String,
    },
    /// Show setting descriptions
    Descriptions {
        /// Show all information (same as --output-long)
        #[arg(short, long)]
        long: bool,
    },
    /// Update a setting value
    Set {
        /// Path of the setting (section)
        #[arg(long)]
        path: String,
        /// Key of the setting
        #[arg(long)]
        key: String,
        /// New value
        #[arg(long)]
        value: String,
    },
    /// Show the changes made since the last save
    Diff {
        /// Only show changes under this path
        #[arg(long, default_value = "")]
        path: String,
    },
    /// Remove a setting key, or a whole section
    // Removing a whole section is destructive, so it has to be asked for
    // explicitly rather than by leaving --key out.
    #[command(group = ArgGroup::new("target").required(true).args(["key", "all_keys"]))]
    Delete {
        /// Path of the setting (section)
        #[arg(long)]
        path: String,
        /// Key to remove
        #[arg(long)]
        key: Option<String>,
        /// Remove every key under the path instead of a single key
        #[arg(long)]
        all_keys: bool,
    },
    /// Issue settings command (load/save/reload)
    Command {
        #[arg(value_enum)]
        action: SettingsCommandActionCli,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SettingsCommandActionCli {
    Load,
    Save,
    Reload,
}

#[derive(Subcommand)]
pub enum MetricsCommand {
    /// Show all metrics as a table (or json/yaml/csv)
    Show {},
    /// Dump metrics in the OpenMetrics/Prometheus text exposition format
    Openmetrics {},
}

#[derive(Subcommand)]
pub enum AuthCommand {
    /// Login and store token
    Login {
        /// Profile ID to store the token under
        #[arg(default_value = "default")]
        id: String,
        /// NSClient++ URL
        #[arg(long, default_value = "https://localhost:8443")]
        url: String,
        /// Username to login with
        #[arg(long, default_value = "admin")]
        username: String,
        /// Password to login with (prompted for when omitted; prefer the environment
        /// variable or the prompt over the flag to keep it out of the shell history)
        #[arg(long, env = "CHECK_NSCLIENT_PASSWORD", hide_env_values = true)]
        password: Option<String>,
        /// Allow insecure TLS connections (i.e. dont validate certificate)
        #[arg(long)]
        insecure: bool,
        /// CA File to use for TLS connections
        #[arg(long)]
        ca: Option<String>,
    },
    /// Logout and forget stored token
    Logout { id: String },
    /// Show who the stored credentials authenticate as
    Status {},
    /// Refresh the api key (using the stored password)
    Refresh {
        /// Profile ID of profile to refresh token
        #[arg(default_value = "default")]
        id: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("check_nsclient").chain(args.iter().copied()))
            .unwrap_or_else(|e| panic!("failed to parse {args:?}: {e}"))
    }

    fn query_args(cli: &Cli) -> (&str, &[(String, String)]) {
        match &cli.command {
            Commands::NSClient(opts) => match &opts.command {
                NSClientCommands::Queries {
                    command: QueriesCommand::Execute { id, args },
                } => (id, args),
                NSClientCommands::Queries {
                    command: QueriesCommand::ExecuteNagios { id, args },
                } => (id, args),
                _ => panic!("not a query execute command"),
            },
            _ => panic!("not an nsclient command"),
        }
    }

    #[test]
    fn execute_accepts_hyphenated_key_value_arguments() {
        let cli = parse(&[
            "nsclient",
            "queries",
            "execute",
            "check_cpu",
            "--warning=load>80",
            "-c",
            "time=5m",
            "show-all",
        ]);
        let (id, args) = query_args(&cli);
        assert_eq!(id, "check_cpu");
        assert_eq!(
            args,
            &[
                ("warning".to_string(), "load>80".to_string()),
                ("c".to_string(), String::new()),
                ("time".to_string(), "5m".to_string()),
                ("show-all".to_string(), String::new()),
            ]
        );
    }

    #[test]
    fn execute_nagios_accepts_hyphenated_key_value_arguments() {
        let cli = parse(&[
            "nsclient",
            "queries",
            "execute-nagios",
            "check_memory",
            "--critical=used>90%",
        ]);
        let (id, args) = query_args(&cli);
        assert_eq!(id, "check_memory");
        assert_eq!(args, &[("critical".to_string(), "used>90%".to_string())]);
    }

    #[test]
    fn global_options_are_parsed_before_subcommand() {
        let cli = parse(&[
            "--output",
            "json",
            "--output-long",
            "-dd",
            "--wsl",
            "nsclient",
            "-p",
            "prod",
            "-t",
            "5",
            "ping",
        ]);
        assert!(matches!(cli.output, OutputFormat::Json));
        assert!(cli.output_long);
        assert_eq!(cli.debug, 2);
        assert!(cli.wsl);
        match &cli.command {
            Commands::NSClient(opts) => {
                assert_eq!(opts.profile.as_deref(), Some("prod"));
                assert_eq!(opts.timeout_s, 5);
                assert_eq!(opts.user_agent, "nscp-client");
                assert!(matches!(opts.command, NSClientCommands::Ping {}));
            }
            _ => panic!("not an nsclient command"),
        }
    }

    #[test]
    fn auth_login_defaults() {
        let cli = parse(&["nsclient", "auth", "login", "--password", "pw"]);
        match &cli.command {
            Commands::NSClient(opts) => match &opts.command {
                NSClientCommands::Auth {
                    command:
                        AuthCommand::Login {
                            id,
                            url,
                            username,
                            password,
                            insecure,
                            ca,
                        },
                } => {
                    assert_eq!(id, "default");
                    assert_eq!(url, "https://localhost:8443");
                    assert_eq!(username, "admin");
                    assert_eq!(password.as_deref(), Some("pw"));
                    assert!(!insecure);
                    assert!(ca.is_none());
                }
                _ => panic!("not a login command"),
            },
            _ => panic!("not an nsclient command"),
        }
    }

    fn login_password(cli: &Cli) -> Option<String> {
        match &cli.command {
            Commands::NSClient(opts) => match &opts.command {
                NSClientCommands::Auth {
                    command: AuthCommand::Login { password, .. },
                } => password.clone(),
                _ => panic!("not a login command"),
            },
            _ => panic!("not an nsclient command"),
        }
    }

    #[test]
    #[serial_test::serial(env)]
    fn auth_login_password_is_optional_and_read_from_environment() {
        unsafe { std::env::remove_var("CHECK_NSCLIENT_PASSWORD") };
        let cli = parse(&["nsclient", "auth", "login"]);
        assert_eq!(login_password(&cli), None);

        unsafe { std::env::set_var("CHECK_NSCLIENT_PASSWORD", "from-env") };
        let cli = parse(&["nsclient", "auth", "login"]);
        assert_eq!(login_password(&cli).as_deref(), Some("from-env"));

        // An explicit flag wins over the environment.
        let cli = parse(&["nsclient", "auth", "login", "--password", "flag"]);
        assert_eq!(login_password(&cli).as_deref(), Some("flag"));
        unsafe { std::env::remove_var("CHECK_NSCLIENT_PASSWORD") };
    }

    #[test]
    fn settings_set_requires_all_parts() {
        let result = Cli::try_parse_from([
            "check_nsclient",
            "nsclient",
            "settings",
            "set",
            "--path",
            "/settings/default",
            "--key",
            "allowed hosts",
        ]);
        assert!(result.is_err());
        let cli = parse(&[
            "nsclient",
            "settings",
            "set",
            "--path",
            "/settings/default",
            "--key",
            "allowed hosts",
            "--value",
            "127.0.0.1",
        ]);
        match &cli.command {
            Commands::NSClient(opts) => match &opts.command {
                NSClientCommands::Settings {
                    command: SettingsCommand::Set { path, key, value },
                } => {
                    assert_eq!(path, "/settings/default");
                    assert_eq!(key, "allowed hosts");
                    assert_eq!(value, "127.0.0.1");
                }
                _ => panic!("not a settings set command"),
            },
            _ => panic!("not an nsclient command"),
        }
    }

    #[test]
    fn profile_commands_parse() {
        let cli = parse(&["profile", "set-default", "prod"]);
        assert!(matches!(
            cli.command,
            Commands::Profile {
                command: ProfileCommands::SetDefault { ref id }
            } if id == "prod"
        ));
        assert!(Cli::try_parse_from(["check_nsclient", "profile", "bogus"]).is_err());
    }

    #[test]
    fn parse_kv_option_supports_key_value_pairs() {
        let parsed = parse_kv_option("foo=bar").unwrap();
        assert_eq!(parsed, ("foo".to_string(), "bar".to_string()));
    }

    #[test]
    fn parse_kv_option_supports_bare_flags() {
        let parsed = parse_kv_option("--help").unwrap();
        assert_eq!(parsed, ("help".to_string(), "".to_string()));
    }

    #[test]
    fn parse_kv_option_rejects_empty_input() {
        assert!(parse_kv_option("   ").is_err());
    }

    #[test]
    fn parse_kv_option_rejects_missing_key_before_equals() {
        assert!(parse_kv_option("=value").is_err());
    }
}
