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
        map
    }
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
