use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use tabled::Tabled;

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct PingResult {
    #[tabled()]
    pub name: String,
    #[tabled()]
    pub version: String,
}

impl PingResult {
    pub(crate) fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("name".to_string(), self.name.clone());
        map.insert("version".to_string(), self.version.clone());
        map
    }
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct LogRecord {
    #[tabled()]
    pub level: String,
    #[tabled()]
    pub date: String,
    #[tabled()]
    pub file: String,
    #[tabled()]
    pub line: u64,
    #[tabled()]
    pub message: String,
}

/// A log record submitted to the agent.
#[derive(Debug, Serialize, Deserialize)]
pub struct NewLogRecord {
    pub level: String,
    pub message: String,
    pub file: String,
    pub line: u64,
}

/// How many buffered log records the server dropped.
#[derive(Debug, Serialize, Deserialize)]
pub struct LogClearResult {
    #[serde(default)]
    pub count: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogStatus {
    pub errors: u64,
    pub last_error: Option<String>,
}

impl LogStatus {
    pub(crate) fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("errors".to_string(), self.errors.to_string());
        map.insert(
            "last_error".to_string(),
            self.last_error.clone().unwrap_or_default(),
        );
        map
    }
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct ScriptRuntimes {
    #[tabled()]
    pub module: String,
    #[tabled()]
    pub name: String,
    #[tabled()]
    pub title: String,
}

pub type Metrics = HashMap<String, Value>;

/// What one metric means, from `/api/v2/metrics?meta=1`.
///
/// Only `type` is always there. `help`, `unit` and `labels` appear only where
/// the producing module declared them, so a metric published through the bare
/// `add_metric()` shorthand carries its type and nothing it never said.
#[derive(Debug, Serialize, Deserialize)]
pub struct MetricDescription {
    /// `gauge`, `counter`, `unknown`, `info`, `summary` or `histogram`.
    #[serde(rename = "type")]
    pub metric_type: String,
    // Left out again when the agent left them out. json and yaml are supposed
    // to echo the document as it arrived, and serializing `null` would invent
    // three keys the agent never sent -- for a metric published through the
    // bare `add_metric()` shorthand, on every one of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Set where the metric is measured per instance, e.g. `{"core": "0"}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<HashMap<String, String>>,
}

/// `/api/v2/metrics?meta=1`: the readings and what they mean, from one
/// snapshot.
///
/// The two halves come from the same tick on purpose -- pairing a value with a
/// unit fetched separately would be a guess -- and `metadata` is keyed by the
/// same keys as `metrics`.
#[derive(Debug, Serialize, Deserialize)]
pub struct DescribedMetrics {
    pub metrics: Metrics,
    #[serde(default)]
    pub metadata: HashMap<String, MetricDescription>,
}

/// One described metric as a table row.
#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct DescribedMetricRow {
    #[tabled()]
    pub metric: String,
    #[tabled()]
    pub value: String,
    #[tabled()]
    pub unit: String,
    #[tabled(rename = "type")]
    pub metric_type: String,
    #[tabled()]
    pub labels: String,
    #[tabled()]
    pub help: String,
}

impl DescribedMetrics {
    /// The document as table rows, sorted by metric name.
    ///
    /// A metric the agent described nothing about still gets a row: the value
    /// is the point, and the description is what may be missing. Fields the
    /// producer never declared render empty rather than as `null`.
    pub fn to_rows(&self) -> Vec<DescribedMetricRow> {
        let mut keys: Vec<&String> = self.metrics.keys().collect();
        keys.sort();
        keys.into_iter()
            .map(|key| {
                let described = self.metadata.get(key);
                DescribedMetricRow {
                    metric: key.clone(),
                    value: match &self.metrics[key] {
                        Value::String(text) => text.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    },
                    unit: described.and_then(|d| d.unit.clone()).unwrap_or_default(),
                    metric_type: described.map(|d| d.metric_type.clone()).unwrap_or_default(),
                    labels: described
                        .and_then(|d| d.labels.as_ref())
                        .map(render_labels)
                        .unwrap_or_default(),
                    help: described.and_then(|d| d.help.clone()).unwrap_or_default(),
                }
            })
            .collect()
    }
}

/// `{"core": "0", "die": "1"}` as `core=0, die=1`, ordered so the same labels
/// always render the same way.
fn render_labels(labels: &HashMap<String, String>) -> String {
    let mut pairs: Vec<String> = labels
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    pairs.sort();
    pairs.join(", ")
}

/// Agent tags: free-form `name -> value` labels attached to this host.
pub type Tags = HashMap<String, String>;

#[derive(Debug, Serialize, Deserialize)]
pub struct PaginatedResponse<T> {
    pub content: T,
    pub page: u64,
    pub pages: u64,
    pub limit: u64,
    pub count: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SettingsStatus {
    pub context: String,
    #[serde(rename = "type")]
    pub status_type: String,
    #[serde(rename = "has_changed")]
    pub has_changed: bool,
}

impl SettingsStatus {
    pub(crate) fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("context".to_string(), self.context.clone());
        map.insert("type".to_string(), self.status_type.clone());
        map.insert("has_changed".to_string(), self.has_changed.to_string());
        map
    }
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct SettingsEntry {
    pub key: String,
    pub path: String,
    pub value: String,
}

/// One configuration change since the last save.
#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct SettingsDiffEntry {
    /// added / removed / modified / path_added / path_removed
    #[tabled()]
    pub change_type: String,
    #[tabled()]
    pub path: String,
    #[tabled()]
    pub key: String,
    #[tabled()]
    pub old_value: String,
    #[tabled()]
    pub new_value: String,
    /// Values of sensitive keys are redacted to `***` by the server.
    #[tabled()]
    #[serde(default)]
    pub is_sensitive: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SettingsDiff {
    #[serde(default)]
    pub entries: Vec<SettingsDiffEntry>,
    #[serde(default)]
    pub count: u64,
}

/// What the server removed in response to a settings DELETE.
#[derive(Debug, Serialize, Deserialize)]
pub struct SettingsDeleteResult {
    pub status: String,
    pub keys: u64,
    #[serde(default)]
    pub recursive: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SettingsDescription {
    pub default_value: String,
    pub description: String,
    pub icon: String,
    pub is_advanced_key: bool,
    pub is_object: bool,
    pub is_sample_key: bool,
    pub is_template_key: bool,
    pub key: String,
    pub path: String,
    #[serde(rename = "type")]
    pub value_type: String,
    pub plugins: Vec<String>,
    pub sample_usage: String,
    pub title: String,
    pub value: String,
}
impl SettingsDescription {
    pub fn to_flat(&self) -> FlatSettingsDescription {
        FlatSettingsDescription {
            default_value: self.default_value.clone(),
            description: self.description.clone(),
            icon: self.icon.clone(),
            is_advanced_key: self.is_advanced_key,
            is_object: self.is_object,
            is_sample_key: self.is_sample_key,
            is_template_key: self.is_template_key,
            key: self.key.clone(),
            path: self.path.clone(),
            value_type: self.value_type.clone(),
            plugins: self.plugins.join(", "),
            sample_usage: self.sample_usage.clone(),
            title: self.title.clone(),
            value: self.value.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct FlatSettingsDescription {
    #[tabled()]
    pub key: String,
    #[tabled()]
    pub path: String,
    #[tabled()]
    pub value: String,
    #[tabled()]
    pub default_value: String,
    #[tabled()]
    pub description: String,
    #[tabled()]
    pub icon: String,
    #[tabled()]
    pub is_advanced_key: bool,
    #[tabled()]
    pub is_object: bool,
    #[tabled()]
    pub is_sample_key: bool,
    #[tabled()]
    pub is_template_key: bool,
    #[tabled(rename = "type")]
    #[serde(rename = "type")]
    pub value_type: String,
    #[tabled()]
    pub plugins: String,
    #[tabled()]
    pub sample_usage: String,
    #[tabled()]
    pub title: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SettingsCommandAction {
    Load,
    Save,
    Reload,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SettingsCommandRequest {
    pub command: SettingsCommandAction,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LoginResponse {
    pub key: String,
    /// The user the credentials belong to. Older servers omit it.
    #[serde(default)]
    pub user: String,
}

/// Render the `experimental` flag for a table or csv cell.
///
/// A settled command is the overwhelmingly common case, so it renders as an
/// empty cell rather than a column of `false` the eye has to filter out; only a
/// command that is still moving says so. The serialized forms (json, yaml) keep
/// the plain boolean, which is what a machine wants.
fn mark_experimental(experimental: &bool) -> String {
    if *experimental { "experimental" } else { "" }.to_string()
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct ListModulesMetadata {
    pub alias: String,
    pub plugin_id: String,
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct ListModulesResult {
    #[tabled()]
    pub id: String,
    #[tabled()]
    pub name: String,
    #[tabled()]
    pub title: String,
    #[tabled()]
    pub description: String,
    #[tabled()]
    pub enabled: bool,
    #[tabled()]
    pub loaded: bool,
    /// Set when the module or command says it is still moving: its options,
    /// filter keywords and output may change in a coming release. Absent from
    /// agents older than the flag, which is read as "not experimental".
    #[serde(default)]
    #[tabled(display = "mark_experimental")]
    pub experimental: bool,
    #[tabled(inline)]
    pub metadata: ListModulesMetadata,
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct FlatListModulesResult {
    #[tabled()]
    pub id: String,
    #[tabled()]
    pub name: String,
    #[tabled()]
    pub title: String,
    #[tabled()]
    pub description: String,
    #[tabled()]
    pub enabled: bool,
    #[tabled()]
    pub loaded: bool,
    /// Set when the module or command says it is still moving: its options,
    /// filter keywords and output may change in a coming release. Absent from
    /// agents older than the flag, which is read as "not experimental".
    #[serde(default)]
    #[tabled(display = "mark_experimental")]
    pub experimental: bool,
    #[tabled()]
    pub alias: String,
    #[tabled()]
    pub plugin_id: String,
}
impl ListModulesResult {
    pub fn to_flat(&self) -> FlatListModulesResult {
        FlatListModulesResult {
            id: self.id.clone(),
            name: self.name.clone(),
            title: self.title.clone(),
            description: self.description.clone(),
            enabled: self.enabled,
            loaded: self.loaded,
            experimental: self.experimental,
            alias: self.metadata.alias.clone(),
            plugin_id: self.metadata.plugin_id.clone(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ModulesResult {
    pub id: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub enabled: bool,
    pub loaded: bool,
    /// See [`ListQueriesResult::experimental`].
    #[serde(default)]
    pub experimental: bool,
    pub metadata: ListModulesMetadata,
}

impl ModulesResult {
    pub(crate) fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("id".to_string(), self.id.clone());
        map.insert("name".to_string(), self.name.clone());
        map.insert("title".to_string(), self.title.clone());
        map.insert("description".to_string(), self.description.clone());
        map.insert("enabled".to_string(), self.enabled.to_string());
        map.insert("loaded".to_string(), self.loaded.to_string());
        map.insert("experimental".to_string(), self.experimental.to_string());
        map.insert("alias".to_string(), self.metadata.alias.clone());
        map.insert("plugin_id".to_string(), self.metadata.plugin_id.clone());
        map
    }
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct ListQueriesResult {
    #[tabled()]
    pub name: String,
    #[tabled()]
    pub title: String,
    #[tabled()]
    pub description: String,
    #[tabled()]
    pub plugin: String,
    /// Set when the module or command says it is still moving: its options,
    /// filter keywords and output may change in a coming release. Absent from
    /// agents older than the flag, which is read as "not experimental".
    #[serde(default)]
    #[tabled(display = "mark_experimental")]
    pub experimental: bool,
}

/// One metadata resource advertised by `/api/v2/metadata`.
#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct MetadataResource {
    #[tabled()]
    pub name: String,
    #[tabled()]
    pub title: String,
    #[tabled()]
    pub url: String,
}

/// A submission channel and the modules listening on it.
#[derive(Debug, Serialize, Deserialize)]
pub struct MetadataChannel {
    pub name: String,
    #[serde(default)]
    pub plugins: Vec<String>,
}

impl MetadataChannel {
    pub fn to_flat(&self) -> FlatMetadataChannel {
        FlatMetadataChannel {
            name: self.name.clone(),
            plugins: self.plugins.join(", "),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct FlatMetadataChannel {
    #[tabled()]
    pub name: String,
    #[tabled()]
    pub plugins: String,
}

/// One entry from the event store (event log hits, real-time filter matches, ...).
#[derive(Debug, Serialize, Deserialize)]
pub struct EventRecord {
    pub index: i64,
    pub event: String,
    pub date: String,
    #[serde(default)]
    pub data: HashMap<String, String>,
}

impl EventRecord {
    pub fn to_flat(&self) -> FlatEventRecord {
        let mut data: Vec<String> = self
            .data
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        data.sort();
        FlatEventRecord {
            index: self.index,
            date: self.date.clone(),
            event: self.event.clone(),
            data: data.join(", "),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct FlatEventRecord {
    #[tabled()]
    pub index: i64,
    #[tabled()]
    pub date: String,
    #[tabled()]
    pub event: String,
    #[tabled()]
    pub data: String,
}

/// A query alias: an admin defined wrapper around a real check command.
#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct AliasResult {
    #[tabled()]
    pub name: String,
    #[tabled()]
    pub title: String,
    #[tabled()]
    pub description: String,
    #[tabled()]
    pub plugin: String,
    /// See [`ListQueriesResult::experimental`].
    #[serde(default)]
    #[tabled(display = "mark_experimental")]
    pub experimental: bool,
    /// Aliases are executed through the regular queries endpoint.
    #[tabled(skip)]
    #[serde(default)]
    pub query_url: String,
    #[tabled(skip)]
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}

#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct QueryResult {
    #[tabled()]
    pub name: String,
    #[tabled()]
    pub title: String,
    #[tabled()]
    pub description: String,
    #[tabled()]
    pub plugin: String,
    /// See [`ListQueriesResult::experimental`].
    #[serde(default)]
    #[tabled(display = "mark_experimental")]
    pub experimental: bool,
    #[tabled(skip)]
    pub metadata: HashMap<String, String>,
}

impl QueryResult {
    pub(crate) fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("name".to_string(), self.name.clone());
        map.insert("title".to_string(), self.title.clone());
        map.insert("description".to_string(), self.description.clone());
        map.insert("plugin".to_string(), self.plugin.clone());
        map.insert("experimental".to_string(), self.experimental.to_string());
        map
    }
}

/// One option a check accepts, from `/api/v2/queries/{query}/help`.
#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct QueryParameter {
    #[tabled()]
    pub name: String,
    #[tabled(rename = "default")]
    #[serde(default)]
    pub default_value: String,
    #[tabled()]
    #[serde(default)]
    pub required: bool,
    #[tabled()]
    #[serde(default)]
    pub repeatable: bool,
    /// `bool` for an option that takes a boolean, `string` for everything else.
    ///
    /// A boolean option still takes a value on the wire (`show-all=true`), so
    /// this is what tells a caller which of the two to send. A `bool` with an
    /// empty `default` is a plain switch that takes no value at all.
    #[tabled(rename = "type")]
    #[serde(default)]
    pub content_type: String,
    #[tabled(rename = "description")]
    #[serde(default)]
    pub short_description: String,
    /// Hidden unless `--long` is asked for: it runs to several lines.
    #[tabled(rename = "details")]
    #[serde(default)]
    pub long_description: String,
}

/// One filter keyword a check offers.
///
/// A filter *function* keeps the trailing `()` the registry marks it with,
/// since that is the only thing that tells it from a variable; the suffix is
/// not part of the name.
#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct QueryField {
    #[tabled()]
    pub name: String,
    #[tabled(rename = "description")]
    #[serde(default)]
    pub short_description: String,
    /// Hidden unless `--long` is asked for: it runs to several lines.
    #[tabled(rename = "details")]
    #[serde(default)]
    pub long_description: String,
}

/// Everything a check accepts: `/api/v2/queries/{query}/help`.
///
/// A check that is not filter based answers with an empty `fields` list rather
/// than with a pretence.
#[derive(Debug, Serialize, Deserialize)]
pub struct QueryHelp {
    pub name: String,
    /// The command the keywords belong to. It differs from `name` only for an
    /// alias, which declares no keywords of its own -- its filter expressions
    /// are written in the keywords of the command it stands for.
    #[serde(default)]
    pub keyword_source: String,
    #[serde(default)]
    pub parameters: Vec<QueryParameter>,
    #[serde(default)]
    pub fields: Vec<QueryField>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PerfData {
    pub value: Option<f64>,
    pub unit: Option<String>,
    pub warning: Option<f64>,
    pub critical: Option<f64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteLine {
    pub message: String,
    pub perf: HashMap<String, PerfData>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteResult {
    pub command: String,
    pub lines: Vec<ExecuteLine>,
    pub result: i32,
}
impl ExecuteResult {
    /// Flatten the result into an ordered key/value list for table/csv output.
    ///
    /// A single line is rendered as `output` plus one entry per performance counter.
    /// Multiple lines are numbered (`output 1`, `output 2`, ...) and performance counters
    /// that repeat across lines get a `(line N)` suffix so nothing is overwritten.
    pub(crate) fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("command".to_string(), self.command.clone());
        let multi_line = self.lines.len() > 1;
        for (index, line) in self.lines.iter().enumerate() {
            let line_no = index + 1;
            let output_key = if multi_line {
                format!("output {line_no}")
            } else {
                "output".to_string()
            };
            map.insert(output_key, clean_up_line(&line.message));
            let mut perf: Vec<_> = line.perf.iter().collect();
            perf.sort_by(|a, b| a.0.cmp(b.0));
            for (key, perf) in perf {
                let key = if map.contains_key(key) {
                    format!("{key} (line {line_no})")
                } else {
                    key.clone()
                };
                map.insert(key, perf_to_simple_string(perf));
            }
        }
        map.insert("result".to_string(), result_to_string(self.result));
        map
    }
}

const OFFSET: usize = 0;
const TAB_LENGTH: usize = 8;
fn clean_up_line(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut len = OFFSET;
    for c in line.chars() {
        if c == '\t' {
            let count = TAB_LENGTH - (len % TAB_LENGTH);
            for _ in 0..count {
                result.push(' ');
            }
            len += count;
        } else if c == '\n' || c == '\r' {
            result.push(c);
            len = OFFSET;
        } else {
            result.push(c);
            len += 1;
        }
    }
    result
}

fn result_to_string(result: i32) -> String {
    match result {
        0 => "OK".to_string(),
        1 => "WARNING".to_string(),
        2 => "CRITICAL".to_string(),
        _ => "UNKNOWN".to_string(),
    }
}

fn perf_to_simple_string(perf: &PerfData) -> String {
    let mut parts = Vec::new();

    if let Some(value) = perf.value {
        let mut value_part = value.to_string();
        if let Some(unit) = &perf.unit {
            value_part.push_str(unit);
        }
        parts.push(value_part);
    }

    if let Some(warning) = perf.warning {
        parts.push(format!("warning: {}", warning));
    }

    if let Some(critical) = perf.critical {
        parts.push(format!("critical: {}", critical));
    }

    if let Some(minimum) = perf.minimum {
        parts.push(format!("minimum: {}", minimum));
    }

    if let Some(maximum) = perf.maximum {
        parts.push(format!("maximum: {}", maximum));
    }

    parts.join(", ")
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteNagiosLine {
    pub message: String,
    pub perf: String,
}

impl ExecuteNagiosLine {
    /// Human friendly rendering (used by the interactive client).
    pub(crate) fn render(&self) -> String {
        if self.perf.is_empty() {
            return self.message.clone();
        }
        format!("{} | {}", self.message, self.perf)
    }

    /// Nagios plugin output format: `message|perfdata`.
    pub(crate) fn render_nagios(&self) -> String {
        if self.perf.is_empty() {
            return self.message.clone();
        }
        format!("{}|{}", self.message, self.perf)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecuteNagiosResult {
    pub command: String,
    pub lines: Vec<ExecuteNagiosLine>,
    pub result: String,
}

impl ExecuteNagiosResult {
    pub(crate) fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("command".to_string(), self.command.clone());
        map.insert(
            "lines".to_string(),
            serde_json::to_string(&self.lines).unwrap(),
        );
        map.insert("result".to_string(), self.result.clone());
        map
    }

    pub(crate) fn get_exit_code(&self) -> i32 {
        match self.result.to_uppercase().as_str() {
            "OK" | "0" => 0,
            "WARNING" | "1" => 1,
            "CRITICAL" | "2" => 2,
            "UNKNOWN" | "3" => 3,
            _ => 3,
        }
    }
}

/// One entry of the WEB server's passive result cache (`/api/v2/results`).
///
/// Every field is defaulted so a newer (or older) server that adds or lacks a
/// field still deserializes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CachedResult {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub index: i64,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub alias: String,
    /// Nagios status: 0 OK, 1 WARNING, 2 CRITICAL, 3 UNKNOWN.
    #[serde(default = "unknown_status")]
    pub status: i32,
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub perf: String,
    #[serde(default)]
    pub count: i64,
    #[serde(default)]
    pub first_seen: i64,
    #[serde(default)]
    pub last_seen: i64,
    #[serde(default)]
    pub result_seen: i64,
    #[serde(default)]
    pub first_seen_date: String,
    #[serde(default)]
    pub last_seen_date: String,
    #[serde(default)]
    pub result_seen_date: String,
    /// Seconds since the key last reported.
    #[serde(default)]
    pub age: i64,
    #[serde(default)]
    pub result_url: String,
}

fn unknown_status() -> i32 {
    3
}

impl CachedResult {
    /// The status as a word, from the number (the server's `result` field says
    /// the same thing, but the number is what the exit code is built from).
    pub fn status_name(&self) -> String {
        result_to_string(self.status)
    }

    /// The `alias` when set, otherwise the `command` (mirrors the server's
    /// `${alias-or-command}` index variable).
    pub fn alias_or_command(&self) -> &str {
        if self.alias.is_empty() {
            &self.command
        } else {
            &self.alias
        }
    }

    /// Plugin output in Nagios format: `message|perf`.
    pub fn nagios_output(&self) -> String {
        if self.perf.is_empty() {
            self.message.clone()
        } else {
            format!("{}|{}", self.message, self.perf)
        }
    }

    pub fn to_flat(&self) -> FlatCachedResult {
        FlatCachedResult {
            key: self.key.clone(),
            result: self.status_name(),
            age: self.age,
            message: clean_up_line(&self.message),
            perf: self.perf.clone(),
            host: self.host.clone(),
            command: self.command.clone(),
            alias: self.alias.clone(),
            channel: self.channel.clone(),
            count: self.count,
            first_seen: self.first_seen_date.clone(),
            last_seen: self.last_seen_date.clone(),
            result_seen: self.result_seen_date.clone(),
        }
    }

    pub(crate) fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("key".to_string(), self.key.clone());
        map.insert("host".to_string(), self.host.clone());
        map.insert("source".to_string(), self.source.clone());
        map.insert("channel".to_string(), self.channel.clone());
        map.insert("command".to_string(), self.command.clone());
        map.insert("alias".to_string(), self.alias.clone());
        map.insert("result".to_string(), self.status_name());
        map.insert("message".to_string(), clean_up_line(&self.message));
        map.insert("perf".to_string(), self.perf.clone());
        map.insert("count".to_string(), self.count.to_string());
        map.insert("age".to_string(), self.age.to_string());
        map.insert("first seen".to_string(), self.first_seen_date.clone());
        map.insert("last seen".to_string(), self.last_seen_date.clone());
        map.insert("result seen".to_string(), self.result_seen_date.clone());
        map
    }
}

/// Table row for a cached result (see [`CachedResult::to_flat`]).
#[derive(Debug, Serialize, Deserialize, Tabled)]
pub struct FlatCachedResult {
    #[tabled()]
    pub key: String,
    #[tabled()]
    pub result: String,
    #[tabled()]
    pub age: i64,
    #[tabled()]
    pub message: String,
    #[tabled()]
    pub perf: String,
    #[tabled()]
    pub host: String,
    #[tabled()]
    pub command: String,
    #[tabled()]
    pub alias: String,
    #[tabled()]
    pub channel: String,
    #[tabled()]
    pub count: i64,
    #[tabled(rename = "first seen")]
    pub first_seen: String,
    #[tabled(rename = "last seen")]
    pub last_seen: String,
    #[tabled(rename = "result seen")]
    pub result_seen: String,
}

/// Columns of [`FlatCachedResult`] that are only shown with `--long`.
pub const CACHED_RESULT_LONG_COLUMNS: &[&str] = &[
    "perf",
    "host",
    "command",
    "alias",
    "channel",
    "count",
    "first seen",
    "last seen",
    "result seen",
];

/// Server side filter for `GET /api/v2/results`. Empty fields match everything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResultFilter {
    pub channel: Option<String>,
    pub host: Option<String>,
    pub command: Option<String>,
    pub alias: Option<String>,
    /// Comma separated list of `ok`, `warning`, `critical`, `unknown` (or `0`-`3`).
    pub status: Option<String>,
}

impl ResultFilter {
    /// The filter as query parameters, only naming the fields that are set.
    pub fn to_query(&self) -> Vec<(String, String)> {
        let mut query = Vec::new();
        for (key, value) in [
            ("channel", &self.channel),
            ("host", &self.host),
            ("command", &self.command),
            ("alias", &self.alias),
            ("status", &self.status),
        ] {
            if let Some(value) = value
                && !value.trim().is_empty()
            {
                query.push((key.to_string(), value.trim().to_string()));
            }
        }
        query
    }
}

/// Response of `DELETE /api/v2/results[/{key}]`.
#[derive(Debug, Serialize, Deserialize)]
pub struct ResultsRemoved {
    pub removed: i64,
}

impl ResultsRemoved {
    pub(crate) fn to_dict(&self) -> IndexMap<String, String> {
        let mut map = IndexMap::new();
        map.insert("removed".to_string(), self.removed.to_string());
        map
    }
}

#[cfg(test)]
mod cached_result_tests {
    use super::*;

    #[test]
    fn deserializes_the_documented_shape_and_tolerates_missing_fields() {
        let full: CachedResult = serde_json::from_str(
            r#"{"key":"srv1/check_drivesize","index":42,"channel":"WEB","host":"srv1",
                "source":"srv1","command":"check_drivesize","alias":"","status":1,
                "result":"WARNING","message":"WARNING C: 85%","perf":"'C:'=85%;80;90",
                "count":17,"first_seen":1757145000,"last_seen":1757145900,
                "result_seen":1757145900,"first_seen_date":"2026-09-06 10:30:00",
                "last_seen_date":"2026-09-06 10:45:00","result_seen_date":"2026-09-06 10:45:00",
                "age":12,"result_url":"https://localhost:8443/api/v2/results/srv1/check_drivesize"}"#,
        )
        .unwrap();
        assert_eq!(full.key, "srv1/check_drivesize");
        assert_eq!(full.status, 1);
        assert_eq!(full.status_name(), "WARNING");
        assert_eq!(full.alias_or_command(), "check_drivesize");
        assert_eq!(full.nagios_output(), "WARNING C: 85%|'C:'=85%;80;90");

        let sparse: CachedResult = serde_json::from_str(r#"{"key":"k","alias":"Disk C"}"#).unwrap();
        assert_eq!(sparse.status, 3, "a missing status must not read as OK");
        assert_eq!(sparse.alias_or_command(), "Disk C");
        assert_eq!(sparse.nagios_output(), "");
    }

    #[test]
    fn filter_only_sends_set_fields() {
        assert!(ResultFilter::default().to_query().is_empty());
        let filter = ResultFilter {
            host: Some(" srv1 ".into()),
            status: Some("warning,critical".into()),
            alias: Some("".into()),
            ..Default::default()
        };
        assert_eq!(
            filter.to_query(),
            vec![
                ("host".to_string(), "srv1".to_string()),
                ("status".to_string(), "warning,critical".to_string()),
            ]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perf(value: Option<f64>, unit: Option<&str>) -> PerfData {
        PerfData {
            value,
            unit: unit.map(str::to_string),
            warning: None,
            critical: None,
            minimum: None,
            maximum: None,
        }
    }

    #[test]
    fn clean_up_line_expands_tabs_to_next_tab_stop() {
        assert_eq!(clean_up_line("a\tb"), "a       b");
        assert_eq!(clean_up_line("abcdefgh\tb"), "abcdefgh        b");
        assert_eq!(clean_up_line("ab\ncd\te"), "ab\ncd      e");
        assert_eq!(clean_up_line("plain"), "plain");
    }

    #[test]
    fn result_to_string_maps_nagios_codes() {
        assert_eq!(result_to_string(0), "OK");
        assert_eq!(result_to_string(1), "WARNING");
        assert_eq!(result_to_string(2), "CRITICAL");
        assert_eq!(result_to_string(3), "UNKNOWN");
        assert_eq!(result_to_string(42), "UNKNOWN");
        assert_eq!(result_to_string(-1), "UNKNOWN");
    }

    #[test]
    fn perf_to_simple_string_includes_only_present_fields() {
        assert_eq!(perf_to_simple_string(&perf(None, None)), "");
        assert_eq!(perf_to_simple_string(&perf(Some(3.5), None)), "3.5");
        assert_eq!(perf_to_simple_string(&perf(Some(3.0), Some("%"))), "3%");
        let full = PerfData {
            value: Some(10.0),
            unit: Some("MB".into()),
            warning: Some(80.0),
            critical: Some(90.0),
            minimum: Some(0.0),
            maximum: Some(100.0),
        };
        assert_eq!(
            perf_to_simple_string(&full),
            "10MB, warning: 80, critical: 90, minimum: 0, maximum: 100"
        );
    }

    #[test]
    fn execute_result_to_dict_single_line() {
        let result = ExecuteResult {
            command: "check_cpu".into(),
            lines: vec![ExecuteLine {
                message: "OK".into(),
                perf: HashMap::from([
                    ("b".to_string(), perf(Some(2.0), None)),
                    ("a".to_string(), perf(Some(1.0), None)),
                ]),
            }],
            result: 0,
        };
        let dict = result.to_dict();
        let keys: Vec<&str> = dict.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["command", "output", "a", "b", "result"]);
        assert_eq!(dict["output"], "OK");
        assert_eq!(dict["result"], "OK");
    }

    #[test]
    fn execute_result_to_dict_keeps_every_line_and_counter() {
        let result = ExecuteResult {
            command: "check_drivesize".into(),
            lines: vec![
                ExecuteLine {
                    message: "C: ok".into(),
                    perf: HashMap::from([("used".to_string(), perf(Some(1.0), Some("GB")))]),
                },
                ExecuteLine {
                    message: "D: ok".into(),
                    perf: HashMap::from([("used".to_string(), perf(Some(2.0), Some("GB")))]),
                },
            ],
            result: 1,
        };
        let dict = result.to_dict();
        let keys: Vec<&str> = dict.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "command",
                "output 1",
                "used",
                "output 2",
                "used (line 2)",
                "result"
            ]
        );
        assert_eq!(dict["output 1"], "C: ok");
        assert_eq!(dict["output 2"], "D: ok");
        assert_eq!(dict["used"], "1GB");
        assert_eq!(dict["used (line 2)"], "2GB");
        assert_eq!(dict["result"], "WARNING");
    }

    #[test]
    fn nagios_exit_code_accepts_names_and_numbers() {
        let with = |result: &str| ExecuteNagiosResult {
            command: "x".into(),
            lines: vec![],
            result: result.into(),
        };
        assert_eq!(with("OK").get_exit_code(), 0);
        assert_eq!(with("ok").get_exit_code(), 0);
        assert_eq!(with("0").get_exit_code(), 0);
        assert_eq!(with("Warning").get_exit_code(), 1);
        assert_eq!(with("1").get_exit_code(), 1);
        assert_eq!(with("CRITICAL").get_exit_code(), 2);
        assert_eq!(with("2").get_exit_code(), 2);
        assert_eq!(with("UNKNOWN").get_exit_code(), 3);
        assert_eq!(with("3").get_exit_code(), 3);
        assert_eq!(with("garbage").get_exit_code(), 3);
    }

    #[test]
    fn nagios_line_rendering() {
        let line = ExecuteNagiosLine {
            message: "OK: fine".into(),
            perf: "'load'=1;2;3".into(),
        };
        assert_eq!(line.render(), "OK: fine | 'load'=1;2;3");
        assert_eq!(line.render_nagios(), "OK: fine|'load'=1;2;3");

        let no_perf = ExecuteNagiosLine {
            message: "OK: fine".into(),
            perf: String::new(),
        };
        assert_eq!(no_perf.render(), "OK: fine");
        assert_eq!(no_perf.render_nagios(), "OK: fine");
    }

    #[test]
    fn nagios_result_to_dict_serializes_lines_as_json() {
        let result = ExecuteNagiosResult {
            command: "check".into(),
            lines: vec![ExecuteNagiosLine {
                message: "m".into(),
                perf: "p".into(),
            }],
            result: "OK".into(),
        };
        let dict = result.to_dict();
        assert_eq!(dict["command"], "check");
        assert_eq!(dict["lines"], r#"[{"message":"m","perf":"p"}]"#);
        assert_eq!(dict["result"], "OK");
    }

    #[test]
    fn log_status_to_dict_renders_missing_error_as_empty() {
        let status = LogStatus {
            errors: 3,
            last_error: None,
        };
        let dict = status.to_dict();
        assert_eq!(dict["errors"], "3");
        assert_eq!(dict["last_error"], "");
    }

    #[test]
    fn settings_command_action_serializes_lowercase() {
        let request = SettingsCommandRequest {
            command: SettingsCommandAction::Reload,
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"command":"reload"}"#
        );
    }

    #[test]
    fn settings_description_to_flat_joins_plugins() {
        let description = SettingsDescription {
            default_value: "d".into(),
            description: "desc".into(),
            icon: "i".into(),
            is_advanced_key: false,
            is_object: false,
            is_sample_key: false,
            is_template_key: false,
            key: "k".into(),
            path: "/p".into(),
            value_type: "string".into(),
            plugins: vec!["A".into(), "B".into()],
            sample_usage: "s".into(),
            title: "t".into(),
            value: "v".into(),
        };
        assert_eq!(description.to_flat().plugins, "A, B");
    }
}
