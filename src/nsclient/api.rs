use crate::config;
use crate::nsclient::login_helper::login_and_fetch_key;
use crate::nsclient::messages::{
    ExecuteNagiosResult, ExecuteResult, ListModulesResult, ListQueriesResult, LogRecord, LogStatus,
    LoginResponse, Metrics, ModulesResult, PaginatedResponse, PingResult, QueryResult,
    ScriptRuntimes, SettingsCommandAction, SettingsCommandRequest, SettingsDescription,
    SettingsEntry, SettingsStatus,
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
}

impl ApiClient {
    pub(crate) fn new(
        builder: ClientBuilder,
        base_url: &str,
        auth: Auth,
        id: Option<String>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            client: builder.build()?,
            base_url: base_url.to_owned(),
            auth: RwLock::new(auth),
            id,
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> anyhow::Result<T> {
        let response = self.send(Method::GET, path, |b| b).await?;
        Ok(response.json::<T>().await?)
    }

    async fn get_empty(&self, path: &str) -> anyhow::Result<()> {
        self.send(Method::GET, path, |b| b).await.map(|_| ())
    }

    async fn get_with_query<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(String, String)],
    ) -> anyhow::Result<T> {
        let response = self.send(Method::GET, path, |b| b.query(query)).await?;
        Ok(response.json::<T>().await?)
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
        let response = configure(self.authed_request(method.clone(), path)?)
            .send()
            .await?;
        if !Self::is_auth_failure(response.status()) {
            return Self::check_status(response, path).await;
        }
        let status = response.status();
        if !self.refresh_token().await? {
            anyhow::bail!("Authentication failed for {path}: {status}");
        }
        let response = configure(self.authed_request(method, path)?).send().await?;
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
    async fn reset_log_status(&self) -> anyhow::Result<()>;
    async fn list_modules(&self, all: &bool) -> anyhow::Result<Vec<ListModulesResult>>;
    async fn get_module(&self, id: &str) -> anyhow::Result<ModulesResult>;
    async fn module_command(&self, id: &str, command: &str) -> anyhow::Result<()>;
    async fn list_queries(&self, all: &bool) -> anyhow::Result<Vec<ListQueriesResult>>;
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
    async fn list_scripts(&self, runtime: &str) -> anyhow::Result<Vec<String>>;
    async fn get_settings_status(&self) -> anyhow::Result<SettingsStatus>;
    async fn get_settings(&self) -> anyhow::Result<Vec<SettingsEntry>>;
    async fn get_settings_descriptions(&self) -> anyhow::Result<Vec<SettingsDescription>>;
    async fn update_settings(&self, settings: &SettingsEntry) -> anyhow::Result<()>;
    async fn settings_command(&self, command: SettingsCommandAction) -> anyhow::Result<()>;
    async fn login(&self) -> anyhow::Result<LoginResponse>;
    async fn get_metrics(&self) -> anyhow::Result<Metrics>;
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

    async fn list_queries(&self, all: &bool) -> anyhow::Result<Vec<ListQueriesResult>> {
        let params = [("all".to_string(), all.to_string())];
        self.get_with_query("api/v2/queries", &params).await
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
    async fn list_scripts(&self, runtime: &str) -> anyhow::Result<Vec<String>> {
        self.get_json(&format!("api/v2/scripts/{runtime}")).await
    }

    async fn get_settings_status(&self) -> anyhow::Result<SettingsStatus> {
        self.get_json("api/v2/settings/status").await
    }

    async fn get_settings(&self) -> anyhow::Result<Vec<SettingsEntry>> {
        self.get_json("api/v2/settings").await
    }

    async fn get_settings_descriptions(&self) -> anyhow::Result<Vec<SettingsDescription>> {
        self.get_json("api/v2/settings/descriptions").await
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

    async fn get_metrics(&self) -> anyhow::Result<Metrics> {
        self.get_json("api/v2/metrics").await
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
            async fn list_modules(&self, all: &bool) -> anyhow::Result<Vec<ListModulesResult>>;
            async fn get_module(&self, id: &str) -> anyhow::Result<ModulesResult>;
            async fn module_command(&self, id: &str, command: &str) -> anyhow::Result<()>;
            async fn list_queries(&self, all: &bool) -> anyhow::Result<Vec<ListQueriesResult>>;
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
            async fn list_scripts(&self, runtime: &str) -> anyhow::Result<Vec<String>>;
            async fn get_settings_status(&self) -> anyhow::Result<SettingsStatus>;
            async fn get_settings(&self) -> anyhow::Result<Vec<SettingsEntry>>;
            async fn get_settings_descriptions(&self) -> anyhow::Result<Vec<SettingsDescription>>;
            async fn update_settings(&self, settings: &SettingsEntry) -> anyhow::Result<()>;
            async fn settings_command(
                &self,
                command: SettingsCommandAction,
            ) -> anyhow::Result<()>;
            async fn login(&self) -> anyhow::Result<LoginResponse>;
            async fn get_metrics(&self) -> anyhow::Result<Metrics>;
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

    fn token_client(url: &str, token: &str, id: Option<&str>) -> Box<dyn ApiClientApi> {
        build_client(
            url,
            5,
            "test-agent",
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
            5,
            "test-agent",
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
