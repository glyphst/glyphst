// Without GPUI nothing signs in or loads saved credentials, so the sign-in
// and persistence machinery is unused and requests report the subscription as
// not initialized.
#![cfg_attr(not(feature = "gpui"), allow(dead_code, unused_imports))]

use std::{
    str::FromStr as _,
    sync::{Arc, Condvar, LazyLock, Mutex, RwLock, Weak},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow};
use base64::{
    Engine as _,
    engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD},
};
use futures::{
    StreamExt as _,
    channel::{mpsc, oneshot},
};
#[cfg(feature = "gpui")]
use gpui::{App, AppContext as _, Global};
use rand::Rng as _;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use url::{Url, form_urlencoded};

#[cfg(feature = "gpui")]
use crate::settings::{AiProvider, SettingsStore};

pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";

const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CREDENTIALS_KEY: &str = "https://glyphst.app/auth/chatgpt-subscription";
const CALLBACK_PATH: &str = "/auth/callback";
const CALLBACK_PORTS: [u16; 2] = [1455, 1457];
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const LOAD_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_REFRESH_BUFFER_MS: u64 = 5 * 60 * 1000;

static ACTIVE_AUTH: LazyLock<RwLock<Weak<AuthInner>>> = LazyLock::new(|| RwLock::new(Weak::new()));
static REQUEST_LIMITER: LazyLock<RequestLimiter> = LazyLock::new(|| RequestLimiter::new(4));

#[derive(Serialize, Deserialize, Clone)]
struct CodexCredentials {
    access_token: String,
    refresh_token: String,
    expires_at_ms: u64,
    account_id: Option<String>,
    email: Option<String>,
}

impl CodexCredentials {
    fn is_expired(&self) -> bool {
        now_ms().saturating_add(TOKEN_REFRESH_BUFFER_MS) >= self.expires_at_ms
    }

    fn request_credentials(&self) -> RequestCredentials {
        RequestCredentials {
            access_token: self.access_token.clone(),
            account_id: self.account_id.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthSnapshot {
    pub loading: bool,
    pub signing_in: bool,
    pub email: Option<String>,
    pub error: Option<String>,
}

impl AuthSnapshot {
    pub fn is_authenticated(&self) -> bool {
        self.email.is_some()
    }
}

struct AuthState {
    credentials: Option<CodexCredentials>,
    loading: bool,
    signing_in: bool,
    refreshing: bool,
    generation: u64,
    last_error: Option<String>,
}

enum PersistenceCommand {
    Write {
        generation: u64,
        bytes: Vec<u8>,
        completion: Option<oneshot::Sender<std::result::Result<(), String>>>,
    },
    Delete,
}

struct AuthInner {
    state: Mutex<AuthState>,
    changed: Condvar,
    persistence_tx: mpsc::UnboundedSender<PersistenceCommand>,
}

#[derive(Clone)]
pub struct ChatGptSubscription {
    inner: Arc<AuthInner>,
}

#[cfg(feature = "gpui")]
impl Global for ChatGptSubscription {}

impl ChatGptSubscription {
    #[cfg(feature = "gpui")]
    pub fn global(cx: &App) -> Option<Self> {
        cx.try_global::<Self>().cloned()
    }

    pub fn snapshot(&self) -> AuthSnapshot {
        let state = self.inner.lock_state();
        AuthSnapshot {
            loading: state.loading,
            signing_in: state.signing_in,
            email: state.credentials.as_ref().map(|credentials| {
                credentials
                    .email
                    .clone()
                    .unwrap_or_else(|| "ChatGPT account".to_string())
            }),
            error: state.last_error.clone(),
        }
    }

    pub fn is_authenticated(&self) -> bool {
        self.inner.lock_state().credentials.is_some()
    }

    #[cfg(feature = "gpui")]
    pub fn sign_in(&self, cx: &mut App) {
        let generation = {
            let mut state = self.inner.lock_state();
            if state.signing_in {
                return;
            }
            state.generation = state.generation.wrapping_add(1);
            state.signing_in = true;
            state.last_error = None;
            state.generation
        };
        notify(cx);

        let (redirect_uri, callback_rx) = match start_callback_server() {
            Ok(callback) => callback,
            Err(error) => {
                self.inner
                    .finish_sign_in_error(generation, format!("{error:#}"));
                notify(cx);
                return;
            }
        };
        let (verifier, challenge, oauth_state) = oauth_secrets();
        let authorize_url = match authorize_url(&redirect_uri, &challenge, &oauth_state) {
            Ok(url) => url,
            Err(error) => {
                self.inner
                    .finish_sign_in_error(generation, format!("{error:#}"));
                notify(cx);
                return;
            }
        };

        cx.open_url(authorize_url.as_str());
        let inner = self.inner.clone();
        cx.spawn(async move |cx| {
            let result = async {
                let callback = callback_rx
                    .await
                    .map_err(|_| anyhow!("OAuth callback was cancelled"))??;
                if callback.state != oauth_state {
                    return Err(anyhow!("OAuth state mismatch"));
                }
                cx.background_spawn(async move {
                    exchange_code(&callback.code, &verifier, &redirect_uri)
                })
                .await
            }
            .await;

            match result {
                Ok(credentials) => {
                    let json = match serde_json::to_vec(&credentials) {
                        Ok(json) => json,
                        Err(error) => {
                            inner.finish_sign_in_error(
                                generation,
                                format!("failed to encode credentials: {error}"),
                            );
                            cx.update(notify);
                            return;
                        }
                    };
                    let write_result = inner.persist_and_wait(generation, json).await;
                    let write_result = match write_result {
                        Ok(result) => result,
                        Err(_) => Err("credential persistence worker stopped".to_string()),
                    };
                    if let Err(error) = write_result {
                        inner.finish_sign_in_error(generation, error);
                        cx.update(notify);
                        return;
                    }

                    if inner.finish_sign_in(generation, credentials) {
                        cx.update(|cx| {
                            enable_provider(cx);
                            notify(cx);
                        });
                    }
                }
                Err(error) => {
                    inner.finish_sign_in_error(generation, format!("{error:#}"));
                    cx.update(notify);
                }
            }
        })
        .detach();
    }

    #[cfg(feature = "gpui")]
    pub fn sign_out(&self, cx: &mut App) {
        {
            let mut state = self.inner.lock_state();
            state.generation = state.generation.wrapping_add(1);
            state.credentials = None;
            state.signing_in = false;
            state.refreshing = false;
            state.last_error = None;
            self.inner.changed.notify_all();
        }
        notify(cx);
        self.inner.delete_persisted();
    }
}

#[cfg(feature = "gpui")]
pub fn init(cx: &mut App) {
    if cx.has_global::<ChatGptSubscription>() {
        return;
    }

    let (persistence_tx, persistence_rx) = mpsc::unbounded();
    let inner = Arc::new(AuthInner {
        state: Mutex::new(AuthState {
            credentials: None,
            loading: true,
            signing_in: false,
            refreshing: false,
            generation: 0,
            last_error: None,
        }),
        changed: Condvar::new(),
        persistence_tx,
    });
    *ACTIVE_AUTH
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::downgrade(&inner);
    cx.set_global(ChatGptSubscription {
        inner: inner.clone(),
    });

    spawn_persistence_worker(persistence_rx, inner.clone(), cx);
    let load_generation = inner.lock_state().generation;
    let read_task = cx.read_credentials(CREDENTIALS_KEY);
    cx.spawn(async move |cx| {
        match read_task.await {
            Ok(Some((_, bytes))) => match serde_json::from_slice::<CodexCredentials>(&bytes) {
                Ok(credentials) => inner.finish_load(load_generation, Some(credentials), None),
                Err(error) => inner.finish_load(
                    load_generation,
                    None,
                    Some(format!("saved ChatGPT credentials are invalid: {error}")),
                ),
            },
            Ok(None) => inner.finish_load(load_generation, None, None),
            Err(error) => inner.finish_load(
                load_generation,
                None,
                Some(format!("failed to read ChatGPT credentials: {error:#}")),
            ),
        }
        cx.update(notify);
    })
    .detach();
}

#[cfg(feature = "gpui")]
fn spawn_persistence_worker(
    mut receiver: mpsc::UnboundedReceiver<PersistenceCommand>,
    inner: Arc<AuthInner>,
    cx: &mut App,
) {
    cx.spawn(async move |cx| {
        while let Some(command) = receiver.next().await {
            let (task, completion) = match command {
                PersistenceCommand::Write {
                    generation,
                    bytes,
                    completion,
                } => {
                    if inner.lock_state().generation != generation {
                        if let Some(completion) = completion {
                            let _ = completion
                                .send(Err("ChatGPT sign-in changed before credentials were saved"
                                    .to_string()));
                        }
                        continue;
                    }
                    (
                        cx.update(move |cx| {
                            cx.write_credentials(CREDENTIALS_KEY, "Bearer", &bytes)
                        }),
                        completion,
                    )
                }
                PersistenceCommand::Delete => {
                    (cx.update(|cx| cx.delete_credentials(CREDENTIALS_KEY)), None)
                }
            };
            let result = task.await.map_err(|error| format!("{error:#}"));
            if let Err(error) = &result {
                eprintln!("failed to update ChatGPT credentials in the keychain: {error}");
            }
            if let Some(completion) = completion {
                let _ = completion.send(result);
            }
            cx.update(notify);
        }
    })
    .detach();
}

#[cfg(feature = "gpui")]
fn notify(cx: &mut App) {
    if cx.has_global::<ChatGptSubscription>() {
        let _ = cx.global_mut::<ChatGptSubscription>();
    }
    cx.refresh_windows();
}

#[cfg(feature = "gpui")]
fn enable_provider(cx: &mut App) {
    if !cx.has_global::<SettingsStore>() {
        return;
    }
    let mut settings = cx.global::<SettingsStore>().settings.clone();
    let config = settings
        .ai
        .provider_config_mut(AiProvider::ChatGptSubscription);
    if config.enabled {
        return;
    }
    config.enabled = true;
    if let Err(error) = cx
        .global_mut::<SettingsStore>()
        .set_settings(settings.clone())
    {
        cx.global_mut::<SettingsStore>()
            .set_settings_in_memory(settings);
        eprintln!("failed to enable the ChatGPT Subscription provider: {error:#}");
    }
}

impl AuthInner {
    fn lock_state(&self) -> std::sync::MutexGuard<'_, AuthState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn finish_load(
        &self,
        generation: u64,
        credentials: Option<CodexCredentials>,
        error: Option<String>,
    ) {
        let mut state = self.lock_state();
        if state.generation == generation {
            state.credentials = credentials;
            state.last_error = error;
        }
        state.loading = false;
        self.changed.notify_all();
    }

    fn finish_sign_in(&self, generation: u64, credentials: CodexCredentials) -> bool {
        let mut state = self.lock_state();
        if state.generation != generation || !state.signing_in {
            return false;
        }
        state.credentials = Some(credentials);
        state.signing_in = false;
        state.last_error = None;
        self.changed.notify_all();
        true
    }

    fn finish_sign_in_error(&self, generation: u64, error: String) {
        let mut state = self.lock_state();
        if state.generation != generation {
            return;
        }
        state.signing_in = false;
        state.last_error = Some(error);
        self.changed.notify_all();
    }

    fn persist(&self, generation: u64, credentials: &CodexCredentials) {
        if let Ok(bytes) = serde_json::to_vec(credentials) {
            let _ = self
                .persistence_tx
                .unbounded_send(PersistenceCommand::Write {
                    generation,
                    bytes,
                    completion: None,
                });
        }
    }

    fn persist_and_wait(
        &self,
        generation: u64,
        bytes: Vec<u8>,
    ) -> oneshot::Receiver<std::result::Result<(), String>> {
        let (completion, receiver) = oneshot::channel();
        let _ = self
            .persistence_tx
            .unbounded_send(PersistenceCommand::Write {
                generation,
                bytes,
                completion: Some(completion),
            });
        receiver
    }

    fn delete_persisted(&self) {
        let _ = self
            .persistence_tx
            .unbounded_send(PersistenceCommand::Delete);
    }
}

#[derive(Clone)]
struct RequestCredentials {
    access_token: String,
    account_id: Option<String>,
}

pub struct RequestAccess {
    credentials: RequestCredentials,
    _permit: RequestPermit,
}

impl RequestAccess {
    pub fn authorize(
        &self,
        request: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        let request = request
            .bearer_auth(&self.credentials.access_token)
            .header("originator", "glyphst")
            .header("openai-beta", "responses=experimental");
        if let Some(account_id) = self
            .credentials
            .account_id
            .as_deref()
            .filter(|account_id| !account_id.is_empty())
        {
            request.header("chatgpt-account-id", account_id)
        } else {
            request
        }
    }
}

pub fn acquire_request_access() -> Result<RequestAccess> {
    let inner = ACTIVE_AUTH
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .upgrade()
        .ok_or_else(|| {
            anyhow!(
                "ChatGPT Subscription is not initialized; restart Glyphst and sign in from Settings > AI"
            )
        })?;
    let credentials = fresh_credentials(&inner)?;
    Ok(RequestAccess {
        credentials,
        _permit: REQUEST_LIMITER.acquire(),
    })
}

pub fn invalidate_credentials(message: impl Into<String>) {
    let Some(inner) = ACTIVE_AUTH
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .upgrade()
    else {
        return;
    };
    let mut state = inner.lock_state();
    state.generation = state.generation.wrapping_add(1);
    state.credentials = None;
    state.refreshing = false;
    state.last_error = Some(message.into());
    inner.changed.notify_all();
    drop(state);
    inner.delete_persisted();
}

fn fresh_credentials(inner: &Arc<AuthInner>) -> Result<RequestCredentials> {
    let load_deadline = Instant::now() + LOAD_WAIT_TIMEOUT;
    loop {
        let mut state = inner.lock_state();
        while state.loading {
            let remaining = load_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!(
                    "timed out while loading ChatGPT credentials from the system keychain"
                ));
            }
            let (next_state, timeout) = inner
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next_state;
            if timeout.timed_out() && state.loading {
                return Err(anyhow!(
                    "timed out while loading ChatGPT credentials from the system keychain"
                ));
            }
        }

        let credentials = state.credentials.clone().ok_or_else(|| {
            anyhow!(
                "You are not signed in to ChatGPT. Sign in from Settings > AI to use ChatGPT Subscription."
            )
        })?;
        if !credentials.is_expired() {
            return Ok(credentials.request_credentials());
        }

        if state.refreshing {
            state = inner
                .changed
                .wait(state)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            drop(state);
            continue;
        }

        state.refreshing = true;
        let generation = state.generation;
        let refresh_token_value = credentials.refresh_token.clone();
        drop(state);

        let refresh_result = refresh_token(&refresh_token_value);
        let mut state = inner.lock_state();
        if state.generation != generation {
            state.refreshing = false;
            inner.changed.notify_all();
            return Err(anyhow!(
                "ChatGPT sign-in changed while refreshing credentials"
            ));
        }

        match refresh_result {
            Ok(credentials) => {
                let request_credentials = credentials.request_credentials();
                state.credentials = Some(credentials.clone());
                state.refreshing = false;
                state.last_error = None;
                inner.changed.notify_all();
                drop(state);
                inner.persist(generation, &credentials);
                return Ok(request_credentials);
            }
            Err(RefreshError::Fatal(error)) => {
                state.generation = state.generation.wrapping_add(1);
                state.credentials = None;
                state.refreshing = false;
                state.last_error = Some("Your ChatGPT session expired. Sign in again.".to_string());
                inner.changed.notify_all();
                drop(state);
                inner.delete_persisted();
                return Err(error);
            }
            Err(RefreshError::Transient(error)) => {
                state.refreshing = false;
                inner.changed.notify_all();
                return Err(error);
            }
        }
    }
}

struct RequestLimiter {
    available: Mutex<usize>,
    changed: Condvar,
}

impl RequestLimiter {
    fn new(limit: usize) -> Self {
        Self {
            available: Mutex::new(limit),
            changed: Condvar::new(),
        }
    }

    fn acquire(&'static self) -> RequestPermit {
        let mut available = self
            .available
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while *available == 0 {
            available = self
                .changed
                .wait(available)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        *available -= 1;
        RequestPermit { limiter: self }
    }
}

struct RequestPermit {
    limiter: &'static RequestLimiter,
}

impl Drop for RequestPermit {
    fn drop(&mut self) {
        let mut available = self
            .limiter
            .available
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *available += 1;
        self.limiter.changed.notify_one();
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    email: Option<String>,
}

fn exchange_code(code: &str, verifier: &str, redirect_uri: &str) -> Result<CodexCredentials> {
    exchange_code_at(TOKEN_URL, code, verifier, redirect_uri)
}

fn exchange_code_at(
    token_url: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<CodexCredentials> {
    let body = form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code", code)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_verifier", verifier)
        .finish();
    let response = reqwest::blocking::Client::builder()
        .timeout(TOKEN_REQUEST_TIMEOUT)
        .build()
        .context("failed to create the ChatGPT token client")?
        .post(token_url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .context("failed to exchange the ChatGPT authorization code")?;
    let status = response.status();
    let body = response
        .text()
        .context("failed to read the ChatGPT token response")?;
    if !status.is_success() {
        return Err(anyhow!(
            "ChatGPT token exchange failed with status {status}: {body}"
        ));
    }
    let tokens: TokenResponse =
        serde_json::from_str(&body).context("failed to parse the ChatGPT token response")?;
    credentials_from_tokens(tokens, None)
}

enum RefreshError {
    Fatal(anyhow::Error),
    Transient(anyhow::Error),
}

fn refresh_token(refresh_token_value: &str) -> std::result::Result<CodexCredentials, RefreshError> {
    let body = form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("refresh_token", refresh_token_value)
        .finish();
    let response = reqwest::blocking::Client::builder()
        .timeout(TOKEN_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| RefreshError::Transient(error.into()))?
        .post(TOKEN_URL)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .map_err(|error| RefreshError::Transient(error.into()))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| RefreshError::Transient(error.into()))?;
    if !status.is_success() {
        let error = anyhow!("ChatGPT token refresh failed with status {status}: {body}");
        if matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return Err(RefreshError::Fatal(error));
        }
        return Err(RefreshError::Transient(error));
    }
    let tokens: TokenResponse =
        serde_json::from_str(&body).map_err(|error| RefreshError::Transient(error.into()))?;
    credentials_from_tokens(tokens, Some(refresh_token_value)).map_err(RefreshError::Transient)
}

fn credentials_from_tokens(
    tokens: TokenResponse,
    fallback_refresh_token: Option<&str>,
) -> Result<CodexCredentials> {
    let refresh_token = tokens
        .refresh_token
        .or_else(|| fallback_refresh_token.map(str::to_string))
        .ok_or_else(|| anyhow!("ChatGPT token response omitted the refresh token"))?;
    let jwt = tokens
        .id_token
        .as_deref()
        .unwrap_or(tokens.access_token.as_str());
    let claims = extract_jwt_claims(jwt);
    Ok(CodexCredentials {
        access_token: tokens.access_token,
        refresh_token,
        expires_at_ms: now_ms().saturating_add(tokens.expires_in.saturating_mul(1000)),
        account_id: claims.account_id,
        email: claims.email.or(tokens.email),
    })
}

struct JwtClaims {
    account_id: Option<String>,
    email: Option<String>,
}

fn extract_jwt_claims(jwt: &str) -> JwtClaims {
    let Some(payload) = jwt.split('.').nth(1) else {
        return JwtClaims {
            account_id: None,
            email: None,
        };
    };
    let decoded = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| URL_SAFE.decode(payload));
    let Ok(payload) = decoded else {
        return JwtClaims {
            account_id: None,
            email: None,
        };
    };
    let Ok(claims) = serde_json::from_slice::<Value>(&payload) else {
        return JwtClaims {
            account_id: None,
            email: None,
        };
    };
    let account_id = claims
        .get("chatgpt_account_id")
        .and_then(Value::as_str)
        .or_else(|| {
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(Value::as_array)
                .and_then(|organizations| organizations.first())
                .and_then(|organization| organization.get("id"))
                .and_then(Value::as_str)
        })
        .map(str::to_string);
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string);
    JwtClaims { account_id, email }
}

struct OAuthCallback {
    code: String,
    state: String,
}

fn start_callback_server() -> Result<(String, oneshot::Receiver<Result<OAuthCallback>>)> {
    let mut bound = None;
    let mut last_error = None;
    for port in CALLBACK_PORTS {
        match tiny_http::Server::http(format!("127.0.0.1:{port}")) {
            Ok(server) => {
                bound = Some((server, port));
                break;
            }
            Err(error) => last_error = Some(error.to_string()),
        }
    }
    let (server, port) = bound.ok_or_else(|| {
        anyhow!(
            "could not start the ChatGPT sign-in callback server on ports 1455 or 1457: {}",
            last_error.unwrap_or_else(|| "unknown error".to_string())
        )
    })?;
    let redirect_uri = format!("http://localhost:{port}{CALLBACK_PATH}");
    let (sender, receiver) = oneshot::channel();

    std::thread::spawn(move || {
        let deadline = Instant::now() + CALLBACK_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let _ = sender.send(Err(anyhow!("ChatGPT sign-in timed out")));
                return;
            }
            let request = match server.recv_timeout(remaining.min(Duration::from_millis(500))) {
                Ok(Some(request)) => request,
                Ok(None) => continue,
                Err(error) => {
                    let _ = sender.send(Err(anyhow!("ChatGPT callback server failed: {error}")));
                    return;
                }
            };
            let parsed = parse_callback(request.url());
            let (status, body) = match &parsed {
                Ok(_) => (
                    200,
                    callback_page(
                        "Authorization successful",
                        "You can close this tab and return to Glyphst.",
                        false,
                    ),
                ),
                Err(error) => (
                    400,
                    callback_page("Authorization failed", &error.to_string(), true),
                ),
            };
            let mut response = tiny_http::Response::from_string(body).with_status_code(status);
            if let Ok(header) =
                tiny_http::Header::from_str("Content-Type: text/html; charset=utf-8")
            {
                response = response.with_header(header);
            }
            let _ = request.respond(response);
            let _ = sender.send(parsed);
            return;
        }
    });

    Ok((redirect_uri, receiver))
}

fn parse_callback(raw_url: &str) -> Result<OAuthCallback> {
    let url = Url::parse(&format!("http://localhost{raw_url}"))
        .context("failed to parse the ChatGPT OAuth callback")?;
    if url.path() != CALLBACK_PATH {
        return Err(anyhow!("unexpected OAuth callback path"));
    }
    let mut code = None;
    let mut state = None;
    let mut oauth_error = None;
    let mut oauth_error_description = None;
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" if !value.is_empty() => code = Some(value.into_owned()),
            "state" if !value.is_empty() => state = Some(value.into_owned()),
            "error" if !value.is_empty() => oauth_error = Some(value.into_owned()),
            "error_description" if !value.is_empty() => {
                oauth_error_description = Some(value.into_owned())
            }
            _ => {}
        }
    }
    if let Some(error) = oauth_error {
        return Err(anyhow!(
            "ChatGPT authorization failed: {error} ({})",
            oauth_error_description.unwrap_or_else(|| "no description".to_string())
        ));
    }
    Ok(OAuthCallback {
        code: code.ok_or_else(|| anyhow!("OAuth callback omitted the authorization code"))?,
        state: state.ok_or_else(|| anyhow!("OAuth callback omitted the state"))?,
    })
}

fn oauth_secrets() -> (String, String, String) {
    let mut verifier_bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut verifier_bytes);
    let verifier = URL_SAFE_NO_PAD.encode(verifier_bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut state_bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut state_bytes);
    let state = state_bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    (verifier, challenge, state)
}

fn authorize_url(redirect_uri: &str, challenge: &str, state: &str) -> Result<Url> {
    let mut url = Url::parse(AUTHORIZE_URL).context("invalid ChatGPT authorization URL")?;
    url.query_pairs_mut()
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("scope", "openid profile email offline_access")
        .append_pair("response_type", "code")
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("id_token_add_organizations", "true")
        .append_pair("state", state)
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "glyphst");
    Ok(url)
}

fn callback_page(title: &str, message: &str, error: bool) -> String {
    let color = if error { "#f38ba8" } else { "#a6e3a1" };
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
content=\"width=device-width\"><title>{}</title><style>body{{margin:0;min-height:100vh;display:flex;\
align-items:center;justify-content:center;background:#1e1e2e;color:#cdd6f4;font-family:system-ui,\
sans-serif}}main{{max-width:420px;padding:40px;border-radius:12px;background:#313244;text-align:center;\
box-shadow:0 4px 24px #0006}}i{{display:block;width:48px;height:48px;margin:0 auto 20px;\
border-radius:50%;background:{color}}}p{{color:#a6adc8}}</style></head><body><main><i></i><h1>{}</h1>\
<p>{}</p><small>Glyphst</small></main></body></html>",
        html_escape(title),
        html_escape(title),
        html_escape(message),
    )
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

pub fn responses_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/responses") {
        base_url.to_string()
    } else {
        format!("{base_url}/responses")
    }
}

pub fn response_text(value: &Value) -> Option<String> {
    if let Some(text) = value.get("output_text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    let mut text = String::new();
    for content in value
        .get("output")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|output| output.get("content").and_then(Value::as_array))
        .flatten()
    {
        if content.get("type").and_then(Value::as_str) == Some("output_text")
            && let Some(part) = content.get("text").and_then(Value::as_str)
        {
            text.push_str(part);
        }
    }
    (!text.is_empty()).then_some(text)
}

pub fn response_error(value: &Value) -> Option<String> {
    value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .pointer("/response/error/message")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .pointer("/response/incomplete_details/reason")
                .and_then(Value::as_str)
        })
        .map(str::to_string)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{collections::HashMap, thread::JoinHandle};

    struct CapturedTokenRequest {
        method: String,
        path: String,
        body: String,
    }

    fn start_token_server(
        status: u16,
        response_body: String,
    ) -> (String, JoinHandle<CapturedTokenRequest>) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let token_url = format!("http://{address}/oauth/token");
        let task = std::thread::spawn(move || {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .expect("token exchange request did not arrive");
            let method = request.method().to_string();
            let path = request.url().to_string();
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            request
                .respond(tiny_http::Response::from_string(response_body).with_status_code(status))
                .unwrap();
            CapturedTokenRequest { method, path, body }
        });
        (token_url, task)
    }

    #[test]
    fn responses_endpoint_is_appended_once() {
        assert_eq!(
            responses_url("https://chatgpt.com/backend-api/codex"),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            responses_url("https://example.test/responses"),
            "https://example.test/responses"
        );
    }

    #[test]
    fn extracts_responses_output_text() {
        assert_eq!(
            response_text(&json!({
                "output": [{
                    "content": [
                        {"type": "reasoning_text", "text": "hidden"},
                        {"type": "output_text", "text": "hello"},
                        {"type": "output_text", "text": " world"}
                    ]
                }]
            }))
            .as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn parses_callback_and_rejects_errors() {
        let callback = parse_callback("/auth/callback?code=abc&state=xyz").unwrap();
        assert_eq!(callback.code, "abc");
        assert_eq!(callback.state, "xyz");
        assert!(parse_callback("/auth/callback?error=access_denied&error_description=no").is_err());
    }

    #[test]
    fn extracts_account_and_email_from_chatgpt_jwt() {
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": "account-123"
                },
                "email": "reader@example.com"
            }))
            .unwrap(),
        );
        let claims = extract_jwt_claims(&format!("header.{payload}.signature"));
        assert_eq!(claims.account_id.as_deref(), Some("account-123"));
        assert_eq!(claims.email.as_deref(), Some("reader@example.com"));
    }

    #[test]
    fn authorization_url_uses_pkce_and_codex_flags() {
        let url =
            authorize_url("http://localhost:1455/auth/callback", "challenge", "state").unwrap();
        let parameters = url
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            parameters.get("client_id").map(|value| value.as_ref()),
            Some(CLIENT_ID)
        );
        assert_eq!(
            parameters
                .get("code_challenge_method")
                .map(|value| value.as_ref()),
            Some("S256")
        );
        assert_eq!(
            parameters
                .get("codex_cli_simplified_flow")
                .map(|value| value.as_ref()),
            Some("true")
        );
        assert_eq!(
            parameters.get("originator").map(|value| value.as_ref()),
            Some("glyphst")
        );
    }

    #[test]
    fn authorization_code_exchange_does_not_require_a_tokio_runtime() {
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({
                "https://api.openai.com/auth": {
                    "chatgpt_account_id": "account-123"
                },
                "email": "reader@example.com"
            }))
            .unwrap(),
        );
        let (token_url, server_task) = start_token_server(
            200,
            json!({
                "access_token": "access-token",
                "refresh_token": "refresh-token",
                "id_token": format!("header.{payload}.signature"),
                "expires_in": 3600
            })
            .to_string(),
        );

        let credentials = exchange_code_at(
            &token_url,
            "authorization-code",
            "pkce-verifier",
            "http://localhost:1455/auth/callback",
        )
        .unwrap();
        let request = server_task.join().unwrap();

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/oauth/token");
        let parameters = form_urlencoded::parse(request.body.as_bytes())
            .into_owned()
            .collect::<HashMap<_, _>>();
        assert_eq!(
            parameters.get("grant_type").map(String::as_str),
            Some("authorization_code")
        );
        assert_eq!(
            parameters.get("client_id").map(String::as_str),
            Some(CLIENT_ID)
        );
        assert_eq!(
            parameters.get("code").map(String::as_str),
            Some("authorization-code")
        );
        assert_eq!(
            parameters.get("redirect_uri").map(String::as_str),
            Some("http://localhost:1455/auth/callback")
        );
        assert_eq!(
            parameters.get("code_verifier").map(String::as_str),
            Some("pkce-verifier")
        );
        assert_eq!(credentials.access_token, "access-token");
        assert_eq!(credentials.refresh_token, "refresh-token");
        assert_eq!(credentials.account_id.as_deref(), Some("account-123"));
        assert_eq!(credentials.email.as_deref(), Some("reader@example.com"));
        assert!(credentials.expires_at_ms > now_ms());
    }

    #[test]
    fn authorization_code_exchange_reports_http_and_json_errors() {
        let (token_url, http_server_task) =
            start_token_server(400, r#"{"error":"invalid_grant"}"#.to_string());
        let error = exchange_code_at(
            &token_url,
            "expired-code",
            "pkce-verifier",
            "http://localhost:1455/auth/callback",
        )
        .err()
        .expect("HTTP error response should fail the exchange");
        http_server_task.join().unwrap();
        let error = format!("{error:#}");
        assert!(error.contains("400 Bad Request"));
        assert!(error.contains("invalid_grant"));

        let (token_url, json_server_task) = start_token_server(200, "not valid JSON".to_string());
        let error = exchange_code_at(
            &token_url,
            "authorization-code",
            "pkce-verifier",
            "http://localhost:1455/auth/callback",
        )
        .err()
        .expect("invalid JSON response should fail the exchange");
        json_server_task.join().unwrap();
        assert!(format!("{error:#}").contains("failed to parse the ChatGPT token response"));
    }

    #[test]
    fn stale_keychain_load_cannot_restore_signed_out_credentials() {
        let (persistence_tx, _persistence_rx) = mpsc::unbounded();
        let inner = AuthInner {
            state: Mutex::new(AuthState {
                credentials: None,
                loading: true,
                signing_in: false,
                refreshing: false,
                generation: 1,
                last_error: None,
            }),
            changed: Condvar::new(),
            persistence_tx,
        };
        inner.finish_load(
            0,
            Some(CodexCredentials {
                access_token: "stale-access-token".to_string(),
                refresh_token: "stale-refresh-token".to_string(),
                expires_at_ms: u64::MAX,
                account_id: Some("stale-account".to_string()),
                email: Some("stale@example.com".to_string()),
            }),
            Some("stale load error".to_string()),
        );

        let state = inner.lock_state();
        assert!(!state.loading);
        assert!(state.credentials.is_none());
        assert!(state.last_error.is_none());
    }
}
