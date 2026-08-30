use crate::config;
use crate::debug;
use crate::nsclient::ConnectionOptions;
use crate::nsclient::login_helper::login_and_fetch_key;
use crate::nsclient::messages::{
    AliasResult, EventRecord, ExecuteNagiosResult, ExecuteResult, ListModulesResult,
    ListQueriesResult, LogClearResult, LogRecord, LogStatus, LoginResponse, MetadataChannel,
    MetadataResource, Metrics, ModulesResult, NewLogRecord, PaginatedResponse, PingResult,
    QueryResult, ScriptRuntimes, SettingsCommandAction, SettingsCommandRequest,
    SettingsDeleteResult, SettingsDescription, SettingsDiff, SettingsEntry, SettingsStatus, Tags,
};
use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use reqwest::header::{AUTHORIZATION, HeaderMap};
use reqwest::{ClientBuilder, Method, RequestBuilder, Response, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::sync::RwLock;

/// Maximum number of bytes of a response body to include in an error message.
const MAX_ERROR_BODY_LEN: usize = 512;

fn header_or_zero(headers: &HeaderMap, key: &str) -> u64 {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

#[derive(Clone)]
pub enum Auth {
    Password(String, String),
    Token(String),
}

pub struct ApiClient {
    client: reqwest::Client,
    base_url: String,
    auth: RwLock<Auth>,
    id: Option<String>,
    options: ConnectionOptions,
}

impl ApiClient {
    pub(crate) fn new(
        builder: ClientBuilder,
        base_url: &str,
        auth: Auth,
        id: Option<String>,
        options: ConnectionOptions,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            client: builder.build()?,
            base_url: base_url.to_owned(),
            auth: RwLock::new(auth),
            id,
            options,
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let response = self.send(Method::GET, path, |b| b).await?;
        Self::parse_json(response, path).await
    }

    /// Decode a JSON body, turning an empty body (which NSClient++ sends for e.g.
    /// `/api/v2/metrics` before the first collection cycle) into a readable error
    /// instead of serde's "EOF while parsing a value".
    async fn parse_json<T: DeserializeOwned>(response: Response, path: &str) -> anyhow::Result<T> {
        let body = response.text().await?;
        if body.trim().is_empty() {
            anyhow::bail!(
                "Empty response from {path} (the server may still be starting up, try again shortly)"
            );
        }
        serde_json::from_str(&body)
            .map_err(|e| anyhow::anyhow!("Invalid JSON response from {path}: {e}"))
    }

    async fn get_empty(&self, path: &str) -> anyhow::Result<()> {
        self.send(Method::GET, path, |b| b).await.map(|_| ())
    }

    /// Fetch a response body verbatim (for endpoints that are not JSON).
    async fn get_text(&self, path: &str) -> anyhow::Result<String> {
        self.text(Method::GET, path, |b| b).await
    }

    /// Send a request whose response is plain text rather than JSON.
    async fn text<F>(&self, method: Method, path: &str, configure: F) -> anyhow::Result<String>
    where
        F: Fn(RequestBuilder) -> RequestBuilder,
    {
        let response = self.send(method, path, configure).await?;
        Ok(response.text().await?)
    }

    async fn get_with_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> anyhow::Result<T> {
        let response = self.send(Method::GET, path, |b| b.query(query)).await?;
        Self::parse_json(response, path).await
    }

    async fn delete(&self, path: &str) -> anyhow::Result<()> {
        self.send(Method::DELETE, path, |b| b).await.map(|_| ())
    }

    async fn send_json<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> anyhow::Result<()> {
        self.send(method, path, |b| b.json(body)).await.map(|_| ())
    }

    /// Fetch a fresh API key using the stored password of the profile this client was
    /// created from, persist it and switch this client over to using it.
    ///
    /// Returns `Ok(false)` when this client is not bound to a profile (and hence cannot refresh).
    async fn refresh_token(&self) -> anyhow::Result<bool> {
        let Some(id) = &self.id else {
            return Ok(false);
        };
        let profile = match config::get_nsclient_profile(id)? {
            Some(profile) => profile,
            None => anyhow::bail!("Failed to refresh token because profile {id} does not exist"),
        };
        let password = config::get_password(id)?;

        let token = login_and_fetch_key(
            &profile.url,
            &profile.username,
            &password,
            profile.insecure,
            profile.ca,
            &self.options,
        )
        .await?;
        config::update_token(id, &token)?;
        match self.auth.write() {
            Ok(mut auth) => *auth = Auth::Token(token),
            Err(e) => anyhow::bail!("Failed to update in-memory token: {e}"),
        }
        Ok(true)
    }

    fn authed_request(&self, method: Method, path: &str) -> anyhow::Result<RequestBuilder> {
        let url = self.url_for(path);
        let auth = match self.auth.read() {
            Ok(auth) => auth.clone(),
            Err(e) => anyhow::bail!("Failed to read auth state: {e}"),
        };
        Ok(match auth {
            Auth::Password(username, password) => self
                .client
                .request(method, url)
                .basic_auth(username, Some(password)),
            Auth::Token(token) => self
                .client
                .request(method, url)
                .header(AUTHORIZATION, format!("Bearer {token}")),
        })
    }

    fn is_auth_failure(status: StatusCode) -> bool {
        status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN
    }

    /// Send a request, transparently refreshing the API token and retrying once when the
    /// server rejects the current credentials.
    ///
    /// `configure` is applied to a freshly built (authenticated) request builder each time the
    /// request is (re)built so that a retry always carries the *current* credentials.
    async fn send<F>(&self, method: Method, path: &str, configure: F) -> anyhow::Result<Response>
    where
        F: Fn(RequestBuilder) -> RequestBuilder,
    {
        debug::log(1, format!("{method} {}", self.url_for(path)));
        let response = configure(self.authed_request(method.clone(), path)?)
            .send()
            .await?;
        debug::log(1, format!("{} from {path}", response.status()));
        if !Self::is_auth_failure(response.status()) {
            return Self::check_status(response, path).await;
        }
        let status = response.status();
        debug::log(1, "Credentials rejected, trying to refresh the token");
        if !self.refresh_token().await? {
            anyhow::bail!("Authentication failed for {path}: {status}");
        }
        debug::log(1, format!("{method} {} (retry)", self.url_for(path)));
        let response = configure(self.authed_request(method, path)?).send().await?;
        debug::log(1, format!("{} from {path}", response.status()));
        if Self::is_auth_failure(response.status()) {
            anyhow::bail!(
                "Authentication failed for {path} even after refreshing the token: {}",
                response.status()
            );
        }
        Self::check_status(response, path).await
    }

    async fn check_status(response: Response, path: &str) -> anyhow::Result<Response> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let body = response.text().await.unwrap_or_default();
        debug::log(2, format!("Response body from {path}: {body}"));
        let body = body.trim();
        if body.is_empty() {
            anyhow::bail!("Invalid response status from {path}: {status}");
        }
        let body: String = body.chars().take(MAX_ERROR_BODY_LEN).collect();
        anyhow::bail!("Invalid response status from {path}: {status}: {body}");
    }

    fn url_for(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn build_page<T>(content: T, headers: &HeaderMap) -> PaginatedResponse<T> {
        let count = header_or_zero(headers, "X-Pagination-Count");
        let page = header_or_zero(headers, "X-Pagination-Page");
        let limit = header_or_zero(headers, "X-Pagination-Limit");
        let pages = if limit == 0 { 0 } else { count.div_ceil(limit) };
        PaginatedResponse {
            content,
            page,
            pages,
            limit,
            count,
        }
    }
}

#[cfg_attr(test, automock)]
#[async_trait]
pub trait ApiClientApi: Send + Sync {
    async fn ping(&self) -> anyhow::Result<PingResult>;
    async fn get_logs(
        &self,
        page: u64,
        size: u64,
        level: Option<String>,
    ) -> anyhow::Result<PaginatedResponse<Vec<LogRecord>>>;
    async fn get_logs_since(
        &self,
        page: u64,
        size: u64,
        since: usize,
    ) -> anyhow::Result<(PaginatedResponse<Vec<LogRecord>>, usize)>;
    async fn get_log_status(&self) -> anyhow::Result<LogStatus>;
    /// Reset the aggregated error counters (keeps the buffered records).
    async fn reset_log_status(&self) -> anyhow::Result<()>;
    /// Drop every buffered log record.
    async fn clear_logs(&self) -> anyhow::Result<LogClearResult>;
    /// Append a record to the agent log.
    async fn add_log(&self, record: &NewLogRecord) -> anyhow::Result<()>;
    async fn list_modules(&self, all: &bool) -> anyhow::Result<Vec<ListModulesResult>>;
    async fn get_module(&self, id: &str) -> anyhow::Result<ModulesResult>;
    async fn module_command(&self, id: &str, command: &str) -> anyhow::Result<()>;
    /// Upload a module archive and load it. The server stores it as
    /// `${module-path}/<id>.zip` and then loads it, so the archive runs as the
    /// service user.
    async fn upload_module(&self, id: &str, archive: Vec<u8>) -> anyhow::Result<()>;
    async fn list_queries(&self, all: &bool) -> anyhow::Result<Vec<ListQueriesResult>>;
    async fn list_aliases(&self, all: &bool) -> anyhow::Result<Vec<AliasResult>>;
    async fn get_query(&self, id: &str) -> anyhow::Result<QueryResult>;
    async fn execute_query(
        &self,
        id: &str,
        args: &[(String, String)],
    ) -> anyhow::Result<ExecuteResult>;
    async fn execute_query_nagios(
        &self,
        id: &str,
        args: &[(String, String)],
    ) -> anyhow::Result<ExecuteNagiosResult>;
    async fn list_script_runtimes(&self) -> anyhow::Result<Vec<ScriptRuntimes>>;
    /// The scripts of `runtime`; `all` also lists files that are not wired up
    /// as a command yet.
    async fn list_scripts(&self, runtime: &str, all: &bool) -> anyhow::Result<Vec<String>>;
    /// The definition (or content) of a single script.
    async fn get_script(&self, runtime: &str, script: &str) -> anyhow::Result<String>;
    /// Upload `content` as `script`, replacing an existing definition.
    async fn add_script(
        &self,
        runtime: &str,
        script: &str,
        content: String,
    ) -> anyhow::Result<String>;
    async fn delete_script(&self, runtime: &str, script: &str) -> anyhow::Result<String>;
    async fn get_settings_status(&self) -> anyhow::Result<SettingsStatus>;
    /// List the keys under `path` (the whole store when it is empty).
    async fn get_settings(&self, path: &str) -> anyhow::Result<Vec<SettingsEntry>>;
    /// Remove a single key, or the whole `path` when `key` is `None`.
    async fn delete_settings(
        &self,
        path: &str,
        key: Option<String>,
    ) -> anyhow::Result<SettingsDeleteResult>;
    /// Setting descriptions under `path`; `samples` also returns sample keys.
    async fn get_settings_descriptions(
        &self,
        path: &str,
        samples: &bool,
    ) -> anyhow::Result<Vec<SettingsDescription>>;
    /// The changes made since the last save, optionally limited to `path`.
    async fn get_settings_diff(&self, path: &str) -> anyhow::Result<SettingsDiff>;
    async fn update_settings(&self, settings: &SettingsEntry) -> anyhow::Result<()>;
    async fn settings_command(&self, command: SettingsCommandAction) -> anyhow::Result<()>;
    async fn login(&self) -> anyhow::Result<LoginResponse>;
    /// Revoke the API token this client authenticates with (server side).
    async fn logout(&self) -> anyhow::Result<()>;
    async fn list_events(&self) -> anyhow::Result<Vec<EventRecord>>;
    /// Drain the event store: the returned events are removed from the server.
    async fn clear_events(&self) -> anyhow::Result<Vec<EventRecord>>;
    async fn list_metadata(&self) -> anyhow::Result<Vec<MetadataResource>>;
    /// Performance counters, forwarded verbatim from `CheckSystem pdh --list`.
    async fn get_metadata_counters(&self) -> anyhow::Result<Vec<serde_json::Value>>;
    async fn get_metadata_channels(&self) -> anyhow::Result<Vec<MetadataChannel>>;
    async fn get_tags(&self) -> anyhow::Result<Tags>;
    async fn get_metrics(&self) -> anyhow::Result<Metrics>;
    /// Metrics in the OpenMetrics/Prometheus text exposition format.
    async fn get_openmetrics(&self) -> anyhow::Result<String>;
}

#[async_trait::async_trait]
impl ApiClientApi for ApiClient {
    async fn ping(&self) -> anyhow::Result<PingResult> {
        self.get_json("api/v2/info").await
    }

    async fn get_logs(
        &self,
        page: u64,
        size: u64,
        level: Option<String>,
    ) -> anyhow::Result<PaginatedResponse<Vec<LogRecord>>> {
        let mut params: Vec<(String, String)> = vec![
            ("page".to_string(), page.to_string()),
            ("per_page".to_string(), size.to_string()),
        ];
        if let Some(level) = level {
            params.push(("level".to_string(), level));
        }
        let path = "api/v2/logs";
        let response = self.send(Method::GET, path, |b| b.query(&params)).await?;
        let headers = response.headers().clone();
        let content = response.json::<Vec<LogRecord>>().await?;
        Ok(Self::build_page(content, &headers))
    }

    async fn get_logs_since(
        &self,
        page: u64,
        size: u64,
        since: usize,
    ) -> anyhow::Result<(PaginatedResponse<Vec<LogRecord>>, usize)> {
        let params: Vec<(String, String)> = vec![
            ("page".to_string(), page.to_string()),
            ("per_page".to_string(), size.to_string()),
            ("since".to_string(), since.to_string()),
        ];
        let path = "api/v2/logs/since";
        let response = self.send(Method::GET, path, |b| b.query(&params)).await?;
        let headers = response.headers().clone();
        let content = response.json::<Vec<LogRecord>>().await?;
        let last_index = header_or_zero(&headers, "X-Log-Index") as usize;
        Ok((Self::build_page(content, &headers), last_index))
    }

    async fn get_log_status(&self) -> anyhow::Result<LogStatus> {
        self.get_json("api/v2/logs/status").await
    }

    async fn reset_log_status(&self) -> anyhow::Result<()> {
        self.delete("api/v2/logs/status").await
    }

    async fn clear_logs(&self) -> anyhow::Result<LogClearResult> {
        let path = "api/v2/logs";
        let response = self.send(Method::DELETE, path, |b| b).await?;
        Self::parse_json(response, path).await
    }

    async fn add_log(&self, record: &NewLogRecord) -> anyhow::Result<()> {
        self.send_json(Method::POST, "api/v2/logs", record).await
    }

    async fn list_modules(&self, all: &bool) -> anyhow::Result<Vec<ListModulesResult>> {
        let params = [("all".to_string(), all.to_string())];
        self.get_with_query("api/v2/modules", &params).await
    }

    async fn get_module(&self, id: &str) -> anyhow::Result<ModulesResult> {
        self.get_json(&format!("api/v2/modules/{id}")).await
    }

    async fn module_command(&self, id: &str, command: &str) -> anyhow::Result<()> {
        let path = format!("api/v2/modules/{id}/commands/{command}");
        self.get_empty(&path).await
    }

    async fn upload_module(&self, id: &str, archive: Vec<u8>) -> anyhow::Result<()> {
        let path = format!("api/v2/modules/{id}");
        // Cloned per attempt so a token refresh can rebuild and resend it.
        self.send(Method::POST, &path, |b| b.body(archive.clone()))
            .await
            .map(|_| ())
    }

    async fn list_queries(&self, all: &bool) -> anyhow::Result<Vec<ListQueriesResult>> {
        let params = [("all".to_string(), all.to_string())];
        self.get_with_query("api/v2/queries", &params).await
    }

    async fn list_aliases(&self, all: &bool) -> anyhow::Result<Vec<AliasResult>> {
        let params = [("all".to_string(), all.to_string())];
        self.get_with_query("api/v2/aliases", &params).await
    }

    async fn get_query(&self, id: &str) -> anyhow::Result<QueryResult> {
        self.get_json(&format!("api/v2/queries/{id}")).await
    }

    async fn execute_query(
        &self,
        id: &str,
        args: &[(String, String)],
    ) -> anyhow::Result<ExecuteResult> {
        let path = format!("api/v2/queries/{id}/commands/execute");
        self.get_with_query(&path, args).await
    }

    async fn execute_query_nagios(
        &self,
        id: &str,
        args: &[(String, String)],
    ) -> anyhow::Result<ExecuteNagiosResult> {
        let path = format!("api/v2/queries/{id}/commands/execute_nagios");
        self.get_with_query(&path, args).await
    }

    async fn list_script_runtimes(&self) -> anyhow::Result<Vec<ScriptRuntimes>> {
        self.get_json("api/v2/scripts").await
    }
    async fn list_scripts(&self, runtime: &str, all: &bool) -> anyhow::Result<Vec<String>> {
        let params = [("all".to_string(), all.to_string())];
        self.get_with_query(&format!("api/v2/scripts/{runtime}"), &params)
            .await
    }

    async fn get_script(&self, runtime: &str, script: &str) -> anyhow::Result<String> {
        self.get_text(&format!("api/v2/scripts/{runtime}/{script}"))
            .await
    }

    async fn add_script(
        &self,
        runtime: &str,
        script: &str,
        content: String,
    ) -> anyhow::Result<String> {
        let path = format!("api/v2/scripts/{runtime}/{script}");
        // The body is the script itself; clone per attempt so a token refresh
        // can rebuild and resend the request.
        self.text(Method::PUT, &path, |b| b.body(content.clone()))
            .await
    }

    async fn delete_script(&self, runtime: &str, script: &str) -> anyhow::Result<String> {
        let path = format!("api/v2/scripts/{runtime}/{script}");
        self.text(Method::DELETE, &path, |b| b).await
    }

    async fn get_settings_status(&self) -> anyhow::Result<SettingsStatus> {
        self.get_json("api/v2/settings/status").await
    }

    async fn get_settings(&self, path: &str) -> anyhow::Result<Vec<SettingsEntry>> {
        self.get_json(&format!("api/v2/settings{path}")).await
    }

    async fn delete_settings(
        &self,
        path: &str,
        key: Option<String>,
    ) -> anyhow::Result<SettingsDeleteResult> {
        let url = format!("api/v2/settings{path}");
        let query: Vec<(String, String)> = key
            .map(|key| vec![("key".to_string(), key)])
            .unwrap_or_default();
        let response = self.send(Method::DELETE, &url, |b| b.query(&query)).await?;
        Self::parse_json(response, &url).await
    }

    async fn get_settings_descriptions(
        &self,
        path: &str,
        samples: &bool,
    ) -> anyhow::Result<Vec<SettingsDescription>> {
        let params = [("samples".to_string(), samples.to_string())];
        self.get_with_query(&format!("api/v2/settings/descriptions{path}"), &params)
            .await
    }

    async fn get_settings_diff(&self, path: &str) -> anyhow::Result<SettingsDiff> {
        let params: Vec<(String, String)> = if path.is_empty() {
            Vec::new()
        } else {
            vec![("path".to_string(), path.to_string())]
        };
        self.get_with_query("api/v2/settings/diff", &params).await
    }

    async fn update_settings(&self, settings: &SettingsEntry) -> anyhow::Result<()> {
        self.send_json(Method::PUT, "api/v2/settings", settings)
            .await
    }

    async fn settings_command(&self, command: SettingsCommandAction) -> anyhow::Result<()> {
        let payload = SettingsCommandRequest { command };
        self.send_json(Method::POST, "api/v2/settings/command", &payload)
            .await
    }

    async fn login(&self) -> anyhow::Result<LoginResponse> {
        self.get_json("api/v2/login").await
    }

    async fn logout(&self) -> anyhow::Result<()> {
        self.delete("api/v2/login").await
    }

    async fn list_events(&self) -> anyhow::Result<Vec<EventRecord>> {
        self.get_json("api/v2/events").await
    }

    async fn clear_events(&self) -> anyhow::Result<Vec<EventRecord>> {
        let path = "api/v2/events";
        let response = self.send(Method::DELETE, path, |b| b).await?;
        Self::parse_json(response, path).await
    }

    async fn list_metadata(&self) -> anyhow::Result<Vec<MetadataResource>> {
        self.get_json("api/v2/metadata").await
    }

    async fn get_metadata_counters(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        self.get_json("api/v2/metadata/counters").await
    }

    async fn get_metadata_channels(&self) -> anyhow::Result<Vec<MetadataChannel>> {
        self.get_json("api/v2/metadata/channels").await
    }

    async fn get_tags(&self) -> anyhow::Result<Tags> {
        self.get_json("api/v2/tags").await
    }

    async fn get_metrics(&self) -> anyhow::Result<Metrics> {
        self.get_json("api/v2/metrics").await
    }

    async fn get_openmetrics(&self) -> anyhow::Result<String> {
        self.get_text("api/v2/openmetrics").await
    }
}

#[cfg(test)]
pub mod mocks {
    use super::*;
    use mockall::mock;

    mock! {
        pub ApiClientApiImpl {}

        #[async_trait::async_trait]
        impl ApiClientApi for ApiClientApiImpl {
            async fn ping(&self) -> anyhow::Result<PingResult>;
            async fn get_logs(
                &self,
                page: u64,
                size: u64,
                level: Option<String>,
            ) -> anyhow::Result<PaginatedResponse<Vec<LogRecord>>>;
            async fn get_logs_since(&self, page: u64, size: u64, since: usize) -> anyhow::Result<(PaginatedResponse<Vec<LogRecord>>, usize)>;
            async fn get_log_status(&self) -> anyhow::Result<LogStatus>;
            async fn reset_log_status(&self) -> anyhow::Result<()>;
            async fn clear_logs(&self) -> anyhow::Result<LogClearResult>;
            async fn add_log(&self, record: &NewLogRecord) -> anyhow::Result<()>;
            async fn list_modules(&self, all: &bool) -> anyhow::Result<Vec<ListModulesResult>>;
            async fn get_module(&self, id: &str) -> anyhow::Result<ModulesResult>;
            async fn module_command(&self, id: &str, command: &str) -> anyhow::Result<()>;
            async fn upload_module(&self, id: &str, archive: Vec<u8>) -> anyhow::Result<()>;
            async fn list_queries(&self, all: &bool) -> anyhow::Result<Vec<ListQueriesResult>>;
            async fn list_aliases(&self, all: &bool) -> anyhow::Result<Vec<AliasResult>>;
            async fn get_query(&self, id: &str) -> anyhow::Result<QueryResult>;
            async fn execute_query(
                &self,
                id: &str,
                args: &[(String, String)],
            ) -> anyhow::Result<ExecuteResult>;
            async fn execute_query_nagios(
                &self,
                id: &str,
                args: &[(String, String)],
            ) -> anyhow::Result<ExecuteNagiosResult>;
            async fn list_script_runtimes(&self) -> anyhow::Result<Vec<ScriptRuntimes>>;
            async fn list_scripts(&self, runtime: &str, all: &bool) -> anyhow::Result<Vec<String>>;
            async fn get_script(&self, runtime: &str, script: &str) -> anyhow::Result<String>;
            async fn add_script(&self, runtime: &str, script: &str, content: String) -> anyhow::Result<String>;
            async fn delete_script(&self, runtime: &str, script: &str) -> anyhow::Result<String>;
            async fn get_settings_status(&self) -> anyhow::Result<SettingsStatus>;
            async fn get_settings(&self, path: &str) -> anyhow::Result<Vec<SettingsEntry>>;
            async fn delete_settings(&self, path: &str, key: Option<String>) -> anyhow::Result<SettingsDeleteResult>;
            async fn get_settings_descriptions(&self, path: &str, samples: &bool) -> anyhow::Result<Vec<SettingsDescription>>;
            async fn get_settings_diff(&self, path: &str) -> anyhow::Result<SettingsDiff>;
            async fn update_settings(&self, settings: &SettingsEntry) -> anyhow::Result<()>;
            async fn settings_command(
                &self,
                command: SettingsCommandAction,
            ) -> anyhow::Result<()>;
            async fn login(&self) -> anyhow::Result<LoginResponse>;
            async fn logout(&self) -> anyhow::Result<()>;
            async fn list_events(&self) -> anyhow::Result<Vec<EventRecord>>;
            async fn clear_events(&self) -> anyhow::Result<Vec<EventRecord>>;
            async fn list_metadata(&self) -> anyhow::Result<Vec<MetadataResource>>;
            async fn get_metadata_counters(&self) -> anyhow::Result<Vec<serde_json::Value>>;
            async fn get_metadata_channels(&self) -> anyhow::Result<Vec<MetadataChannel>>;
            async fn get_tags(&self) -> anyhow::Result<Tags>;
            async fn get_metrics(&self) -> anyhow::Result<Metrics>;
            async fn get_openmetrics(&self) -> anyhow::Result<String>;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{add_nsclient_profile, mock_test_config};
    use crate::nsclient::build_client;
    use reqwest::header::HeaderValue;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (key, value) in pairs {
            map.insert(*key, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn header_or_zero_parses_numeric_headers() {
        let map = headers(&[("X-Pagination-Count", "42")]);
        assert_eq!(header_or_zero(&map, "X-Pagination-Count"), 42);
    }

    #[test]
    fn header_or_zero_defaults_to_zero_for_missing_or_invalid_headers() {
        let map = headers(&[("X-Pagination-Count", "not-a-number")]);
        assert_eq!(header_or_zero(&map, "X-Pagination-Count"), 0);
        assert_eq!(header_or_zero(&map, "X-Missing"), 0);
    }

    #[test]
    fn build_page_computes_page_count_from_headers() {
        let map = headers(&[
            ("X-Pagination-Count", "101"),
            ("X-Pagination-Page", "2"),
            ("X-Pagination-Limit", "50"),
        ]);
        let page = ApiClient::build_page(vec![1, 2, 3], &map);
        assert_eq!(page.content, vec![1, 2, 3]);
        assert_eq!(page.count, 101);
        assert_eq!(page.page, 2);
        assert_eq!(page.limit, 50);
        assert_eq!(page.pages, 3);
    }

    #[test]
    fn build_page_handles_missing_headers() {
        let page = ApiClient::build_page((), &HeaderMap::new());
        assert_eq!(page.count, 0);
        assert_eq!(page.limit, 0);
        assert_eq!(page.pages, 0);
    }

    fn options() -> ConnectionOptions {
        ConnectionOptions {
            timeout_s: 5,
            user_agent: "test-agent".into(),
        }
    }

    fn token_client(url: &str, token: &str, id: Option<&str>) -> Box<dyn ApiClientApi> {
        build_client(
            url,
            &options(),
            Auth::Token(token.to_string()),
            false,
            id.map(|s| s.to_string()),
            None,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn sends_bearer_token_and_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/info"))
            .and(header("authorization", "Bearer secret"))
            .and(header("user-agent", "test-agent"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "NSClient++",
                "version": "1.0"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let result = api.ping().await.unwrap();
        assert_eq!(result.version, "1.0");
    }

    #[tokio::test]
    async fn sends_basic_auth_for_password_auth() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/login"))
            .and(header("authorization", "Basic YWRtaW46aHVudGVyMg=="))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"key": "the-key"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = build_client(
            &server.uri(),
            &options(),
            Auth::Password("admin".into(), "hunter2".into()),
            false,
            None,
            None,
        )
        .unwrap();
        assert_eq!(api.login().await.unwrap().key, "the-key");
    }

    #[tokio::test]
    async fn error_responses_include_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/info"))
            .respond_with(ResponseTemplate::new(500).set_body_string("kaboom"))
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let err = api.ping().await.unwrap_err().to_string();
        assert!(err.contains("500"), "{err}");
        assert!(err.contains("kaboom"), "{err}");
    }

    #[tokio::test]
    async fn empty_body_is_reported_clearly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/metrics"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let err = api.get_metrics().await.unwrap_err().to_string();
        assert!(err.contains("Empty response from api/v2/metrics"), "{err}");
    }

    #[tokio::test]
    async fn invalid_json_is_reported_with_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/info"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>oops</html>"))
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let err = api.ping().await.unwrap_err().to_string();
        assert!(
            err.contains("Invalid JSON response from api/v2/info"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn unauthorized_without_profile_fails_without_retry() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/info"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let err = api.ping().await.unwrap_err().to_string();
        assert!(err.contains("Authentication failed"), "{err}");
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn unauthorized_refreshes_token_and_retries_with_new_token() {
        let tmp = mock_test_config();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/info"))
            .and(header("authorization", "Bearer stale"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/login"))
            .and(header("authorization", "Basic YWRtaW46aHVudGVyMg=="))
            .and(header("user-agent", "test-agent"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"key": "fresh"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/info"))
            .and(header("authorization", "Bearer fresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "NSClient++",
                "version": "2.0"
            })))
            .expect(2)
            .mount(&server)
            .await;
        add_nsclient_profile(
            "refresh-profile",
            &server.uri(),
            false,
            "admin",
            "hunter2",
            "stale",
            None,
        )
        .unwrap();

        let api = token_client(&server.uri(), "stale", Some("refresh-profile"));
        assert_eq!(api.ping().await.unwrap().version, "2.0");
        // The refreshed token must be used for subsequent calls without another refresh.
        assert_eq!(api.ping().await.unwrap().version, "2.0");
        assert_eq!(config::get_api_key("refresh-profile").unwrap(), "fresh");
        drop(tmp);
    }

    #[tokio::test]
    #[serial_test::serial(config)]
    async fn unauthorized_after_refresh_reports_error() {
        let tmp = mock_test_config();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/info"))
            .respond_with(ResponseTemplate::new(401))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/login"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"key": "fresh"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        add_nsclient_profile(
            "refresh-fail",
            &server.uri(),
            false,
            "admin",
            "hunter2",
            "stale",
            None,
        )
        .unwrap();

        let api = token_client(&server.uri(), "stale", Some("refresh-fail"));
        let err = api.ping().await.unwrap_err().to_string();
        assert!(err.contains("even after refreshing"), "{err}");
        drop(tmp);
    }

    #[tokio::test]
    async fn execute_query_encodes_arguments_as_query_parameters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/check_cpu/commands/execute"))
            .and(query_param("warning", "load > 80"))
            .and(query_param("show-all", ""))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "command": "check_cpu",
                "lines": [],
                "result": 0
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let args = vec![
            ("warning".to_string(), "load > 80".to_string()),
            ("show-all".to_string(), String::new()),
        ];
        let result = api.execute_query("check_cpu", &args).await.unwrap();
        assert_eq!(result.command, "check_cpu");
    }

    #[tokio::test]
    async fn get_logs_parses_pagination_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/logs"))
            .and(query_param("page", "2"))
            .and(query_param("per_page", "10"))
            .and(query_param("level", "error"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-Pagination-Count", "25")
                    .insert_header("X-Pagination-Page", "2")
                    .insert_header("X-Pagination-Limit", "10")
                    .set_body_json(serde_json::json!([{
                        "level": "error",
                        "date": "2024-01-01",
                        "file": "main.cpp",
                        "line": 12,
                        "message": "boom"
                    }])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let page = api.get_logs(2, 10, Some("error".into())).await.unwrap();
        assert_eq!(page.content.len(), 1);
        assert_eq!(page.content[0].message, "boom");
        assert_eq!(page.count, 25);
        assert_eq!(page.pages, 3);
    }

    #[tokio::test]
    async fn upload_module_posts_the_archive_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/modules/MyModule"))
            .and(wiremock::matchers::body_bytes(vec![0x50, 0x4b, 0x05, 0x06]))
            // The server answers with an empty body on success.
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        api.upload_module("MyModule", vec![0x50, 0x4b, 0x05, 0x06])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn upload_module_surfaces_a_rejected_name() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/modules/bad"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid module name"))
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let err = api.upload_module("bad", vec![1, 2, 3]).await.unwrap_err();
        assert!(err.to_string().contains("Invalid module name"), "{err}");
    }

    #[tokio::test]
    async fn logs_are_cleared_and_appended() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/logs"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"count": 12})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v2/logs"))
            .and(body_json(serde_json::json!({
                "level": "info",
                "message": "hello",
                "file": "cli",
                "line": 1
            })))
            // The endpoint answers with an empty body, which must not be
            // treated as a decoding failure.
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        assert_eq!(api.clear_logs().await.unwrap().count, 12);
        api.add_log(&NewLogRecord {
            level: "info".into(),
            message: "hello".into(),
            file: "cli".into(),
            line: 1,
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn settings_descriptions_take_a_path_and_samples_flag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/settings/descriptions/settings/WEB/server"))
            .and(query_param("samples", "true"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "default_value": "8443",
                    "description": "The port to listen on",
                    "icon": "icon",
                    "is_advanced_key": false,
                    "is_object": false,
                    "is_sample_key": false,
                    "is_template_key": false,
                    "key": "port",
                    "path": "/settings/WEB/server",
                    "type": "string",
                    "plugins": ["WEBServer"],
                    "sample_usage": "",
                    "title": "Port",
                    "value": "8443"
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let descriptions = api
            .get_settings_descriptions("/settings/WEB/server", &true)
            .await
            .unwrap();
        assert_eq!(descriptions[0].key, "port");
    }

    #[tokio::test]
    async fn settings_diff_passes_the_path_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/settings/diff"))
            .and(query_param("path", "/settings/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "entries": [{
                    "path": "/settings/probe",
                    "key": "k1",
                    "old_value": "",
                    "new_value": "v1",
                    "change_type": "added",
                    "is_sensitive": false
                }],
                "count": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let diff = api.get_settings_diff("/settings/probe").await.unwrap();
        assert_eq!(diff.count, 1);
        assert_eq!(diff.entries[0].change_type, "added");
        assert_eq!(diff.entries[0].new_value, "v1");
    }

    #[tokio::test]
    async fn settings_diff_without_a_path_sends_no_filter() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/settings/diff"))
            .and(wiremock::matchers::query_param_is_missing("path"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"entries": [], "count": 0})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        assert_eq!(api.get_settings_diff("").await.unwrap().count, 0);
    }

    #[tokio::test]
    async fn settings_are_listed_and_deleted_by_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/settings/settings/probe"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "/settings/probe", "key": "k1", "value": "v1"}
            ])))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/settings/settings/probe"))
            .and(query_param("key", "k1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"status": "success", "keys": 1, "recursive": true}),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let entries = api.get_settings("/settings/probe").await.unwrap();
        assert_eq!(entries[0].key, "k1");
        let removed = api
            .delete_settings("/settings/probe", Some("k1".to_string()))
            .await
            .unwrap();
        assert_eq!(removed.keys, 1);
        assert_eq!(removed.status, "success");
    }

    #[tokio::test]
    async fn deleting_a_whole_path_sends_no_key_parameter() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/settings/settings/probe"))
            .and(wiremock::matchers::query_param_is_missing("key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                serde_json::json!({"status": "success", "keys": 2, "recursive": true}),
            ))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let removed = api.delete_settings("/settings/probe", None).await.unwrap();
        assert_eq!(removed.keys, 2);
    }

    #[tokio::test]
    async fn list_scripts_passes_the_all_flag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/scripts/ext"))
            .and(query_param("all", "true"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!(["scripts/check_ok.bat"])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        assert_eq!(
            api.list_scripts("ext", &true).await.unwrap(),
            vec!["scripts/check_ok.bat".to_string()]
        );
    }

    #[tokio::test]
    async fn scripts_are_fetched_uploaded_and_deleted_as_text() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/scripts/ext/check_probe"))
            .respond_with(ResponseTemplate::new(200).set_body_string("scripts/check_probe.sh"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/api/v2/scripts/ext/check_probe"))
            .and(wiremock::matchers::body_string("echo OK\n"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Added check_probe"))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/scripts/ext/check_probe"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Script was removed"))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        assert_eq!(
            api.get_script("ext", "check_probe").await.unwrap(),
            "scripts/check_probe.sh"
        );
        assert_eq!(
            api.add_script("ext", "check_probe", "echo OK\n".to_string())
                .await
                .unwrap(),
            "Added check_probe"
        );
        assert_eq!(
            api.delete_script("ext", "check_probe").await.unwrap(),
            "Script was removed"
        );
    }

    #[tokio::test]
    async fn metadata_index_counters_and_channels() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/metadata"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "counters", "title": "Performance counters", "url": "u1"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/metadata/counters"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "\\Memory\\Available Bytes", "type": "large"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/v2/metadata/channels"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "NSCA", "plugins": ["NSCAClient"]}
            ])))
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        assert_eq!(api.list_metadata().await.unwrap()[0].name, "counters");
        assert_eq!(
            api.get_metadata_counters().await.unwrap()[0]["type"],
            "large"
        );
        let channels = api.get_metadata_channels().await.unwrap();
        assert_eq!(channels[0].plugins, vec!["NSCAClient".to_string()]);
    }

    #[tokio::test]
    async fn tags_are_returned_as_a_map() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/tags"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"env": "prod"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        assert_eq!(api.get_tags().await.unwrap()["env"], "prod");
    }

    #[tokio::test]
    async fn events_are_listed_and_drained() {
        let server = MockServer::start().await;
        let body = serde_json::json!([{
            "index": 7,
            "event": "eventlog",
            "date": "2026-08-30 12:00:00",
            "data": {"source": "kernel"}
        }]);
        Mock::given(method("GET"))
            .and(path("/api/v2/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body.clone()))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let listed = api.list_events().await.unwrap();
        assert_eq!(listed[0].index, 7);
        assert_eq!(listed[0].data["source"], "kernel");
        // DELETE returns the events it removed.
        let drained = api.clear_events().await.unwrap();
        assert_eq!(drained[0].event, "eventlog");
    }

    #[tokio::test]
    async fn list_aliases_passes_the_all_flag() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/aliases"))
            .and(query_param("all", "true"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "name": "alias_cpu",
                    "title": "alias_cpu",
                    "description": "Alias for: check_cpu",
                    "plugin": "CheckExternalScripts",
                    "query_url": "https://localhost:8443/api/v2/queries/alias_cpu/",
                    "metadata": {}
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let aliases = api.list_aliases(&true).await.unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].name, "alias_cpu");
        assert_eq!(
            aliases[0].query_url,
            "https://localhost:8443/api/v2/queries/alias_cpu/"
        );
    }

    #[tokio::test]
    async fn openmetrics_is_returned_verbatim() {
        let server = MockServer::start().await;
        // The server labels this endpoint application/json even though the body
        // is the plain-text exposition format, so it must not be JSON decoded.
        Mock::given(method("GET"))
            .and(path("/api/v2/openmetrics"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "application/json")
                    .set_body_string("cpu_total 12\nmem_used 42\n"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        assert_eq!(
            api.get_openmetrics().await.unwrap(),
            "cpu_total 12\nmem_used 42\n"
        );
    }

    #[tokio::test]
    async fn logout_deletes_the_login_resource() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/login"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"status": "ok"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        api.logout().await.unwrap();
    }

    #[tokio::test]
    async fn settings_command_posts_json_payload() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v2/settings/command"))
            .and(body_json(serde_json::json!({"command": "reload"})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        api.settings_command(SettingsCommandAction::Reload)
            .await
            .unwrap();
    }
}
