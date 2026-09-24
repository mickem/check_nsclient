use crate::config;
use crate::debug;
use crate::nsclient::ConnectionOptions;
use crate::nsclient::login_helper::login_and_fetch_key;
use crate::nsclient::messages::{
    AliasResult, CachedResult, DescribedMetrics, EventRecord, ExecuteNagiosResult, ExecuteResult,
    FactsResponse, ListModulesResult, ListQueriesResult, LogClearResult, LogRecord, LogStatus,
    LoginResponse, MetadataChannel, MetadataResource, Metrics, ModulesResult, NewLogRecord,
    PaginatedResponse, PingResult, QueryHelp, QueryResult, ResultFilter, ResultsRemoved,
    ScriptRuntimes, SettingsCommandAction, SettingsCommandRequest, SettingsDeleteResult,
    SettingsDescription, SettingsDiff, SettingsEntry, SettingsStatus, Tags,
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

/// The query parameter the two listings that no longer expose `--all` still pin.
///
/// No caller can ask for anything else, yet the parameter is sent, because an
/// agent that still honours it defaults it to *true* when it is absent. On
/// `queries` that means running every registered command with `help-pb` to
/// collect its parameters -- 6.6s against 0.083s on the pinned 0.18.0, holding a
/// WEB server thread for all of it and answering nothing else meanwhile. On
/// `aliases` it means a scan of the module directory (0.124s against 0.110s) for
/// an answer that is identical either way. Newer agents ignore the parameter, so
/// pinning it costs them nothing.
const NO_FETCH_ALL: [(&str, &str); 1] = [("all", "false")];

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

    /// GET `path` with `query` appended, e.g. `&[("all", "false")]`; any
    /// `Serialize` shape reqwest accepts as a query string will do.
    async fn get_with_query<T: DeserializeOwned, Q: Serialize + ?Sized>(
        &self,
        path: &str,
        query: &Q,
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
        let response = self.send_unchecked(method, path, &configure).await?;
        Self::check_status(response, path).await
    }

    /// `GET path`, with `404` answered as `Ok(None)`.
    ///
    /// For an endpoint where "there is nothing here" is an answer rather than a
    /// failure, so a caller can tell it apart from a timeout without reading it
    /// back out of an error message.
    async fn get_json_optional<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> anyhow::Result<Option<T>> {
        let response = self.send_unchecked(Method::GET, path, &|b| b).await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = Self::check_status(response, path).await?;
        Self::parse_json(response, path).await.map(Some)
    }

    /// The transport half of [`ApiClient::send`]: refresh the token and retry
    /// once if the credentials are rejected, then hand the response back with
    /// its status still unexamined.
    async fn send_unchecked<F>(
        &self,
        method: Method,
        path: &str,
        configure: &F,
    ) -> anyhow::Result<Response>
    where
        F: Fn(RequestBuilder) -> RequestBuilder,
    {
        debug::log(1, format!("{method} {}", self.url_for(path)));
        let response = configure(self.authed_request(method.clone(), path)?)
            .send()
            .await?;
        debug::log(1, format!("{} from {path}", response.status()));
        if !Self::is_auth_failure(response.status()) {
            return Ok(response);
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
        Ok(response)
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
    /// Every check command the agent has registered.
    ///
    /// There is no `all` for the caller to pass. Asking for it made the agent
    /// run every registered command with `help-pb` to collect its parameters --
    /// seconds of work holding a WEB server thread, during which it answered
    /// nothing else -- and the listing does not report parameters anyway.
    /// NSClient++ now ignores the parameter for that reason.
    ///
    /// Implementations must still pin [`NO_FETCH_ALL`] on the wire: an agent
    /// that reads the parameter defaults it to *true*, so omitting it asks for
    /// exactly that inventory.
    async fn list_queries(&self) -> anyhow::Result<Vec<ListQueriesResult>>;
    /// Every query alias the agent has registered.
    ///
    /// Like [`ApiClientApi::list_queries`] this takes no `all` from the caller:
    /// the alias inventory never read the flag, so asking for it changed nothing
    /// except the promise made to whoever typed it. Implementations pin
    /// [`NO_FETCH_ALL`] here too, for the reason given there.
    async fn list_aliases(&self) -> anyhow::Result<Vec<AliasResult>>;
    async fn get_query(&self, id: &str) -> anyhow::Result<QueryResult>;
    /// Everything a check accepts: every option with its default and its
    /// description, and every filter keyword it offers.
    ///
    /// `Ok(None)` is the agent answering 404 -- an unknown query, or an agent
    /// from before the endpoint existed. That is a fact about the query and
    /// worth remembering; an `Err` is a request that went wrong and may not go
    /// wrong again.
    async fn get_query_help(&self, id: &str) -> anyhow::Result<Option<QueryHelp>>;
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
    /// Read the full inventory when `path` is empty, otherwise a dotted subtree.
    async fn get_facts(&self, path: &str) -> anyhow::Result<FactsResponse>;
    async fn refresh_facts(&self) -> anyhow::Result<FactsResponse>;
    async fn get_metrics(&self) -> anyhow::Result<Metrics>;
    /// The same readings as [`ApiClientApi::get_metrics`] plus what each one
    /// means, taken from a single snapshot (`?meta=1`).
    async fn get_metrics_described(&self) -> anyhow::Result<DescribedMetrics>;
    /// Metrics in the OpenMetrics/Prometheus text exposition format.
    async fn get_openmetrics(&self) -> anyhow::Result<String>;
    /// The passive result cache, optionally filtered. With the server's default
    /// `clear on poll = true` this *drains* the results it returns.
    async fn list_results(&self, filter: &ResultFilter) -> anyhow::Result<Vec<CachedResult>>;
    /// One cached result by key (a lookup: never drains).
    async fn get_result(&self, key: &str) -> anyhow::Result<CachedResult>;
    /// Drop one cached result.
    async fn delete_result(&self, key: &str) -> anyhow::Result<ResultsRemoved>;
    /// Empty the result cache.
    async fn clear_results(&self) -> anyhow::Result<ResultsRemoved>;
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
        self.get_with_query("api/v2/modules", &[("all", all)]).await
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

    async fn list_queries(&self) -> anyhow::Result<Vec<ListQueriesResult>> {
        self.get_with_query("api/v2/queries", &NO_FETCH_ALL).await
    }

    async fn list_aliases(&self) -> anyhow::Result<Vec<AliasResult>> {
        self.get_with_query("api/v2/aliases", &NO_FETCH_ALL).await
    }

    async fn get_query(&self, id: &str) -> anyhow::Result<QueryResult> {
        self.get_json(&format!("api/v2/queries/{id}")).await
    }

    async fn get_query_help(&self, id: &str) -> anyhow::Result<Option<QueryHelp>> {
        self.get_json_optional(&format!("api/v2/queries/{id}/help"))
            .await
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
        self.get_with_query(&format!("api/v2/scripts/{runtime}"), &[("all", all)])
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
        self.get_with_query(
            &format!("api/v2/settings/descriptions{path}"),
            &[("samples", samples)],
        )
        .await
    }

    async fn get_settings_diff(&self, path: &str) -> anyhow::Result<SettingsDiff> {
        let params: &[(&str, &str)] = if path.is_empty() {
            &[]
        } else {
            &[("path", path)]
        };
        self.get_with_query("api/v2/settings/diff", params).await
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

    async fn get_facts(&self, path: &str) -> anyhow::Result<FactsResponse> {
        if path.is_empty() {
            return self.get_json("api/v2/facts").await;
        }
        let params = [("path".to_string(), path.to_string())];
        self.get_with_query("api/v2/facts", &params).await
    }

    async fn refresh_facts(&self) -> anyhow::Result<FactsResponse> {
        let path = "api/v2/facts/commands/refresh";
        let response = self.send(Method::POST, path, |b| b).await?;
        Self::parse_json(response, path).await
    }

    async fn get_metrics(&self) -> anyhow::Result<Metrics> {
        self.get_json("api/v2/metrics").await
    }

    async fn get_metrics_described(&self) -> anyhow::Result<DescribedMetrics> {
        // The agent reads `1`, `true` and `yes` as asking for the described
        // document; anything else -- `0` and a bare `meta` included -- gets the
        // flat map, which would not deserialize into this shape.
        let params = [("meta".to_string(), "1".to_string())];
        self.get_with_query("api/v2/metrics", &params).await
    }

    async fn get_openmetrics(&self) -> anyhow::Result<String> {
        self.get_text("api/v2/openmetrics").await
    }

    async fn list_results(&self, filter: &ResultFilter) -> anyhow::Result<Vec<CachedResult>> {
        self.get_with_query("api/v2/results", &filter.to_query())
            .await
    }

    async fn get_result(&self, key: &str) -> anyhow::Result<CachedResult> {
        // Keys contain `/` by default (`${host}/${alias-or-command}`) and the
        // server matches the rest of the path verbatim, so the key is not
        // percent-encoded.
        self.get_json(&format!("api/v2/results/{key}")).await
    }

    async fn delete_result(&self, key: &str) -> anyhow::Result<ResultsRemoved> {
        let path = format!("api/v2/results/{key}");
        let response = self.send(Method::DELETE, &path, |b| b).await?;
        Self::parse_json(response, &path).await
    }

    async fn clear_results(&self) -> anyhow::Result<ResultsRemoved> {
        let path = "api/v2/results";
        let response = self.send(Method::DELETE, path, |b| b).await?;
        Self::parse_json(response, path).await
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
            async fn list_queries(&self) -> anyhow::Result<Vec<ListQueriesResult>>;
            async fn list_aliases(&self) -> anyhow::Result<Vec<AliasResult>>;
            async fn get_query(&self, id: &str) -> anyhow::Result<QueryResult>;
            async fn get_query_help(&self, id: &str) -> anyhow::Result<Option<QueryHelp>>;
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
            async fn get_facts(&self, path: &str) -> anyhow::Result<FactsResponse>;
            async fn refresh_facts(&self) -> anyhow::Result<FactsResponse>;
            async fn get_metrics(&self) -> anyhow::Result<Metrics>;
            async fn get_metrics_described(&self) -> anyhow::Result<DescribedMetrics>;
            async fn get_openmetrics(&self) -> anyhow::Result<String>;
            async fn list_results(&self, filter: &ResultFilter) -> anyhow::Result<Vec<CachedResult>>;
            async fn get_result(&self, key: &str) -> anyhow::Result<CachedResult>;
            async fn delete_result(&self, key: &str) -> anyhow::Result<ResultsRemoved>;
            async fn clear_results(&self) -> anyhow::Result<ResultsRemoved>;
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
    async fn list_results_sends_only_the_set_filters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/results"))
            .and(query_param("host", "srv1"))
            .and(query_param("status", "warning,critical"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "key": "srv1/check_cpu",
                    "host": "srv1",
                    "command": "check_cpu",
                    "status": 2,
                    "result": "CRITICAL",
                    "message": "CRITICAL: load 99%",
                    "perf": "'load'=99%;80;90",
                    "result_seen": 1757145900,
                    "age": 5
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let filter = ResultFilter {
            host: Some("srv1".into()),
            status: Some("warning,critical".into()),
            ..Default::default()
        };
        let results = api.list_results(&filter).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].key, "srv1/check_cpu");
        assert_eq!(results[0].status, 2);
        assert_eq!(
            results[0].nagios_output(),
            "CRITICAL: load 99%|'load'=99%;80;90"
        );
        // Verified by the mock's `expect(1)` and the query matchers: unset
        // filters must not travel as empty parameters.
        let received = server.received_requests().await.unwrap();
        assert!(!received[0].url.query().unwrap_or("").contains("channel="));
    }

    #[tokio::test]
    async fn get_result_keeps_the_slash_in_the_key() {
        let server = MockServer::start().await;
        // The slash must stay a path separator (the server matches the rest of
        // the path verbatim); other characters are percent-encoded as usual.
        Mock::given(method("GET"))
            .and(path("/api/v2/results/srv1/Disk%20C"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "key": "srv1/Disk C",
                "alias": "Disk C",
                "status": 0
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let result = api.get_result("srv1/Disk C").await.unwrap();
        assert_eq!(result.alias_or_command(), "Disk C");
    }

    #[tokio::test]
    async fn delete_and_clear_results_return_the_removed_count() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/results/srv1/check_cpu"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"removed": 1})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/api/v2/results"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"removed": 12})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        assert_eq!(
            api.delete_result("srv1/check_cpu").await.unwrap().removed,
            1
        );
        assert_eq!(api.clear_results().await.unwrap().removed, 12);
    }

    #[tokio::test]
    async fn disabled_result_cache_is_reported_with_the_server_hint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/results"))
            .respond_with(ResponseTemplate::new(503).set_body_string(
                "Passive result cache is disabled. Set enabled=true under /settings/WEB/server/results to turn it on.",
            ))
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let err = api
            .list_results(&ResultFilter::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("503"), "{err}");
        assert!(err.contains("Set enabled=true"), "{err}");
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

    fn facts_envelope(
        selected_path: &str,
        found: bool,
        facts: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "revision": 7, "collected": "2026-09-23T10:00:00Z",
            "path": selected_path, "found": found, "enabled": ["os", "hardware"],
            "errors": {"hardware": "WMI query timed out"},
            "gathered": {"os": "2026-09-23T06:12:41Z"}, "facts": facts
        })
    }

    #[tokio::test]
    async fn facts_get_and_refresh_use_authenticated_endpoints_and_keep_the_envelope() {
        let server = MockServer::start().await;
        let body = facts_envelope(
            "",
            true,
            serde_json::json!({
                "os": {"family": "windows"}, "hardware": {"cpu_cores": 20, "memory_gb": 32.5}
            }),
        );
        for (verb, endpoint) in [
            ("GET", "/api/v2/facts"),
            ("POST", "/api/v2/facts/commands/refresh"),
        ] {
            Mock::given(method(verb))
                .and(path(endpoint))
                .and(header("authorization", "Bearer secret"))
                .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                .expect(1)
                .mount(&server)
                .await;
        }
        let api = token_client(&server.uri(), "secret", None);
        assert_eq!(
            serde_json::to_value(api.get_facts("").await.unwrap()).unwrap(),
            body
        );
        assert_eq!(
            serde_json::to_value(api.refresh_facts().await.unwrap()).unwrap(),
            body
        );
        let requests = server.received_requests().await.unwrap();
        assert!(requests[0].url.query().unwrap_or_default().is_empty());
        assert!(requests[1].body.is_empty(), "refresh takes no payload");
    }

    #[tokio::test]
    async fn facts_paths_are_query_encoded_and_can_select_any_json_node() {
        let server = MockServer::start().await;
        let api = token_client(&server.uri(), "secret", None);
        for (selected_path, found, value) in [
            ("os", true, serde_json::json!({"family": "linux"})),
            ("os.family", true, serde_json::json!("linux")),
            ("hardware.cpu_cores", true, serde_json::json!(20)),
            (
                "storage.volumes",
                true,
                serde_json::json!([{"id": "/", "size": 42}]),
            ),
            ("os.features", true, serde_json::json!(["a", "b"])),
            ("os.secure_boot", true, serde_json::json!(false)),
            ("missing &path=os#?", false, serde_json::json!({})),
        ] {
            let body = facts_envelope(selected_path, found, value);
            Mock::given(method("GET"))
                .and(path("/api/v2/facts"))
                .and(query_param("path", selected_path))
                .respond_with(ResponseTemplate::new(200).set_body_json(&body))
                .expect(1)
                .mount(&server)
                .await;
            let response = api.get_facts(selected_path).await.unwrap();
            assert_eq!(serde_json::to_value(response).unwrap(), body);
        }
        for request in server.received_requests().await.unwrap() {
            assert_eq!(request.url.query_pairs().count(), 1);
        }
    }

    #[tokio::test]
    async fn facts_empty_fallback_without_path_is_supported() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/facts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "revision": 0, "collected": "", "enabled": [], "errors": {},
                "gathered": {}, "found": false, "facts": {}
            })))
            .mount(&server)
            .await;
        let api = token_client(&server.uri(), "secret", None);
        let response = api.get_facts("").await.unwrap();
        assert_eq!(response.path, "");
        assert_eq!(response.revision, 0);
        assert!(!response.found);
    }

    #[tokio::test]
    async fn facts_http_and_decoding_failures_are_not_empty_inventories() {
        for (status, body) in [
            (403, "Forbidden"),
            (404, "Document not found"),
            (500, "Failed to refresh facts"),
            (200, "not json"),
            (200, "{}"),
            (200, ""),
        ] {
            let server = MockServer::start().await;
            for (verb, endpoint) in [
                ("GET", "/api/v2/facts"),
                ("POST", "/api/v2/facts/commands/refresh"),
            ] {
                Mock::given(method(verb))
                    .and(path(endpoint))
                    .respond_with(ResponseTemplate::new(status).set_body_string(body))
                    .expect(1)
                    .mount(&server)
                    .await;
            }
            let api = token_client(&server.uri(), "secret", None);
            let read_error = api.get_facts("").await.unwrap_err().to_string();
            let refresh_error = api.refresh_facts().await.unwrap_err().to_string();
            for error in [read_error, refresh_error] {
                assert!(error.contains("api/v2/facts"), "{error}");
                if status != 200 {
                    assert!(error.contains(&status.to_string()), "{error}");
                }
            }
        }
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
    async fn list_queries_never_asks_for_the_parameter_inventory() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries"))
            // `all=true` here makes the agent run every registered command to
            // collect its parameters, and an agent that reads the parameter
            // assumes `true` when it is absent -- so the explicit `false` is
            // what keeps the listing cheap. See `NO_FETCH_ALL`.
            .and(query_param("all", "false"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{
                    "name": "check_cpu",
                    "title": "check_cpu",
                    "description": "Check the CPU load",
                    "plugin": "CheckSystem"
                }])),
            )
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let queries = api.list_queries().await.unwrap();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].name, "check_cpu");
        assert_eq!(queries[0].plugin, "CheckSystem");
    }

    #[tokio::test]
    async fn list_aliases_never_asks_for_the_disk_scan() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/aliases"))
            .and(query_param("all", "false"))
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
        let aliases = api.list_aliases().await.unwrap();
        assert_eq!(aliases.len(), 1);
        assert_eq!(aliases[0].name, "alias_cpu");
        assert_eq!(
            aliases[0].query_url,
            "https://localhost:8443/api/v2/queries/alias_cpu/"
        );
    }

    #[tokio::test]
    async fn described_metrics_pair_values_with_what_they_mean() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/metrics"))
            // The agent only reads `1`, `true` and `yes` as asking for the
            // described document; a bare `meta` would return the flat map.
            .and(query_param("meta", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "metrics": {
                    "system.mem.physical.used": 5123456789u64,
                    "system.cpu.core 0.idle": 93,
                    "workers.jobs": 1847
                },
                "metadata": {
                    "system.mem.physical.used": {
                        "type": "gauge",
                        "help": "Physical memory in use",
                        "unit": "bytes"
                    },
                    "system.cpu.core 0.idle": {
                        "type": "gauge",
                        "unit": "percent",
                        "labels": {"core": "0"}
                    },
                    "workers.jobs": {"type": "counter"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let described = api.get_metrics_described().await.unwrap();
        assert_eq!(described.metrics["workers.jobs"], 1847);

        let rows = described.to_rows();
        // Sorted by metric name, so the order does not depend on the map.
        let names: Vec<&str> = rows.iter().map(|r| r.metric.as_str()).collect();
        assert_eq!(
            names,
            [
                "system.cpu.core 0.idle",
                "system.mem.physical.used",
                "workers.jobs"
            ]
        );

        let memory = &rows[1];
        assert_eq!(memory.value, "5123456789");
        assert_eq!(memory.unit, "bytes");
        assert_eq!(memory.metric_type, "gauge");
        assert_eq!(memory.help, "Physical memory in use");
        assert_eq!(memory.labels, "");

        // A per-instance metric carries its labels; one the producer described
        // only partly leaves the rest empty rather than rendering `null`.
        assert_eq!(rows[0].labels, "core=0");
        assert_eq!(rows[0].help, "");
        assert_eq!(rows[2].metric_type, "counter");
        assert_eq!(rows[2].unit, "");
    }

    #[tokio::test]
    async fn query_help_reports_options_and_filter_keywords() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/check_drivesize/help"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "check_drivesize",
                "keyword_source": "check_drivesize",
                "parameters": [
                    {
                        "name": "filter",
                        "default_value": "none",
                        "required": false,
                        "repeatable": false,
                        "content_type": "string",
                        "short_description": "Filter which marks interesting items.",
                        "short_description_long": "ignored",
                        "long_description": "Filter which marks interesting items.
Common option for all filter checks."
                    },
                    {
                        "name": "show-all",
                        "default_value": "",
                        "required": false,
                        "repeatable": false,
                        "content_type": "bool",
                        "short_description": "Show all items.",
                        "long_description": "Show all items."
                    }
                ],
                "fields": [
                    {"name": "free", "short_description": "", "long_description": "Free disk space"},
                    {"name": "convert_bytes()", "short_description": "", "long_description": "Convert a byte value."}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let help = api
            .get_query_help("check_drivesize")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(help.name, "check_drivesize");
        assert_eq!(help.keyword_source, "check_drivesize");
        assert_eq!(help.parameters.len(), 2);
        assert_eq!(help.parameters[0].default_value, "none");
        assert_eq!(help.parameters[0].content_type, "string");
        // A bool with an empty default is a plain switch that takes no value.
        assert_eq!(help.parameters[1].content_type, "bool");
        assert_eq!(help.parameters[1].default_value, "");
        // A filter function keeps the "()" the registry marks it with.
        assert_eq!(help.fields[1].name, "convert_bytes()");
    }

    #[tokio::test]
    async fn query_help_of_a_check_that_is_not_filter_based_has_no_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/check_ok/help"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "alias_ok",
                "keyword_source": "check_ok",
                "parameters": [],
                "fields": []
            })))
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        let help = api.get_query_help("check_ok").await.unwrap().unwrap();
        assert!(help.fields.is_empty());
        // An alias reports the command its keywords would have come from.
        assert_eq!(help.keyword_source, "check_ok");
        assert_eq!(help.name, "alias_ok");
    }

    #[tokio::test]
    async fn query_help_reports_a_404_as_nothing_rather_than_as_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/nope/help"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Document not found"))
            .expect(1)
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        // An unknown query, or an agent without the endpoint: a fact about the
        // query, and a caller is meant to be able to tell it from a timeout.
        assert!(api.get_query_help("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn query_help_still_reports_a_real_failure_as_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v2/queries/check_cpu/help"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;

        let api = token_client(&server.uri(), "secret", None);
        assert!(api.get_query_help("check_cpu").await.is_err());
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
