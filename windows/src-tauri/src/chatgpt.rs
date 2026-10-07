//! Sign in with ChatGPT for local/open-source clients. OAuth credentials never
//! cross the Tauri boundary: the UI receives only connection metadata.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use jsonwebtoken::{decode, decode_header, jwk::JwkSet, DecodingKey, Validation};
use rand::{thread_rng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::{platform, secrets};

const AUTHORIZE_ENDPOINT: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/api/accounts/oauth/token";
const JWKS_ENDPOINT: &str = "https://auth.openai.com/.well-known/jwks.json";
const ISSUER: &str = "https://auth.openai.com";
const RESOURCE: &str = "https://api.openai.com/v1";
const META_KEY: &str = "openai-chatgpt-meta";
const CLIENT_KEY: &str = "openai-chatgpt-client-id";
const HOST_KEY: &str = "openai-chatgpt-host-id";
const ACCESS_KEY: &str = "openai-chatgpt-access-token";
const REFRESH_KEY: &str = "openai-chatgpt-refresh-token";
const ID_KEY: &str = "openai-chatgpt-id-token";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthRecord {
    client_id: String,
    host_id: String,
    subject: String,
    email: Option<String>,
    name: Option<String>,
    id_token: String,
    access_token: String,
    refresh_token: String,
    scopes: Vec<String>,
    expires_in: u64,
    saved_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AuthMeta {
    subject: String,
    email: Option<String>,
    name: Option<String>,
    scopes: Vec<String>,
    expires_in: u64,
    saved_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptStatus {
    pub connected: bool,
    pub sharing: bool,
    pub email: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptModel {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: u64,
    scope: String,
}

#[derive(Debug, Deserialize)]
struct IdentityClaims {
    iss: String,
    #[serde(rename = "aud")]
    _aud: Value,
    sub: String,
    exp: u64,
    iat: u64,
    nonce: String,
    email: Option<String>,
    name: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn load() -> Option<AuthRecord> {
    let meta: AuthMeta = serde_json::from_str(&secrets::get(META_KEY)?).ok()?;
    Some(AuthRecord {
        client_id: secrets::get(CLIENT_KEY)?,
        host_id: secrets::get(HOST_KEY)?,
        subject: meta.subject,
        email: meta.email,
        name: meta.name,
        id_token: secrets::get_large(ID_KEY)?,
        access_token: secrets::get_large(ACCESS_KEY)?,
        refresh_token: secrets::get_large(REFRESH_KEY)?,
        scopes: meta.scopes,
        expires_in: meta.expires_in,
        saved_at: meta.saved_at,
    })
}

fn save(record: &AuthRecord) -> Result<(), String> {
    let meta = serde_json::to_string(&AuthMeta {
        subject: record.subject.clone(),
        email: record.email.clone(),
        name: record.name.clone(),
        scopes: record.scopes.clone(),
        expires_in: record.expires_in,
        saved_at: record.saved_at,
    })
    .map_err(|e| e.to_string())?;
    // Windows generic credentials have a small per-entry payload limit. Keep
    // each OAuth token in its own protected entry instead of one large JSON.
    secrets::set(CLIENT_KEY, &record.client_id)?;
    secrets::set(HOST_KEY, &record.host_id)?;
    secrets::set_large(ID_KEY, &record.id_token)?;
    secrets::set_large(ACCESS_KEY, &record.access_token)?;
    secrets::set_large(REFRESH_KEY, &record.refresh_token)?;
    secrets::set(META_KEY, &meta)
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    thread_rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn host_id() -> Result<String, String> {
    if let Some(value) = secrets::get(HOST_KEY) {
        return Ok(value);
    }
    let mut bytes = [0_u8; 16];
    thread_rng().fill_bytes(&mut bytes);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let value = format!(
        "urn:uuid:{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes(bytes[0..4].try_into().unwrap()),
        u16::from_be_bytes(bytes[4..6].try_into().unwrap()),
        u16::from_be_bytes(bytes[6..8].try_into().unwrap()),
        u16::from_be_bytes(bytes[8..10].try_into().unwrap()),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
        ]),
    );
    secrets::set(HOST_KEY, &value)?;
    Ok(value)
}

pub fn status() -> ChatGptStatus {
    let record = load();
    ChatGptStatus {
        connected: record.is_some(),
        sharing: record.as_ref().is_some_and(|value| {
            value
                .scopes
                .iter()
                .any(|scope| scope == "chatgpt.tokens.use.direct")
        }),
        email: record.as_ref().and_then(|value| value.email.clone()),
        name: record.and_then(|value| value.name),
    }
}

pub fn sign_out() -> Result<(), String> {
    for key in [META_KEY, CLIENT_KEY] {
        secrets::clear(key)?;
    }
    for key in [ACCESS_KEY, REFRESH_KEY, ID_KEY] {
        secrets::clear_large(key)?;
    }
    Ok(())
}

pub async fn sign_in() -> Result<ChatGptStatus, String> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("Could not start the local sign-in callback: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
    let host_id = host_id()?;
    let existing = load();
    let saved_client = existing
        .as_ref()
        .map(|value| value.client_id.clone())
        .or_else(|| secrets::get(CLIENT_KEY));
    let client_id = saved_client.as_deref().unwrap_or("dynamic_agent_client");

    let state = random_urlsafe(32);
    let nonce = random_urlsafe(32);
    let verifier = random_urlsafe(64);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut authorize = Url::parse(AUTHORIZE_ENDPOINT).map_err(|e| e.to_string())?;
    {
        let mut query = authorize.query_pairs_mut();
        query
            .append_pair("client_id", client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair(
                "scope",
                "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
            )
            .append_pair("resource", RESOURCE)
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge_method", "S256")
            .append_pair("code_challenge", &challenge)
            .append_pair("ext_agent_host_id", &host_id);
        if saved_client.is_none() {
            query.append_pair("agent_name_hint", "Coucou");
        } else if let Some(record) = &existing {
            query.append_pair("id_token_hint", &record.id_token);
            if let Some(email) = &record.email {
                query.append_pair("login_hint", email);
            }
        }
    }
    platform::open_url(authorize.as_str());

    let (mut stream, _) =
        tokio::time::timeout(std::time::Duration::from_secs(300), listener.accept())
            .await
            .map_err(|_| "ChatGPT sign-in timed out.".to_string())?
            .map_err(|e| format!("Sign-in callback failed: {e}"))?;
    let mut request = vec![0_u8; 16 * 1024];
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        stream.read(&mut request),
    )
    .await
    .map_err(|_| "The sign-in callback timed out.".to_string())?
    .map_err(|e| e.to_string())?;
    let request = String::from_utf8_lossy(&request[..read]);
    let target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| "Invalid sign-in callback.".to_string())?;
    let callback = Url::parse(&format!("http://127.0.0.1:{port}{target}"))
        .map_err(|_| "Invalid sign-in callback URL.".to_string())?;
    if callback.path() != "/auth/callback" {
        return Err("Unexpected sign-in callback path.".into());
    }
    let params: std::collections::HashMap<_, _> = callback.query_pairs().into_owned().collect();
    if params.get("state") != Some(&state) {
        return Err("The sign-in state did not match. Please try again.".into());
    }
    if let Some(error) = params.get("error") {
        return Err(format!("ChatGPT sign-in was not completed: {error}"));
    }
    let code = params
        .get("code")
        .ok_or_else(|| "The sign-in callback did not contain a code.".to_string())?;
    let issued_client = match saved_client {
        Some(value) => {
            if params
                .get("client_id")
                .is_some_and(|returned| returned != &value)
            {
                return Err(
                    "The returned ChatGPT client did not match the saved connection.".into(),
                );
            }
            value
        }
        None => params
            .get("client_id")
            .filter(|value| !value.is_empty() && value.as_str() != "dynamic_agent_client")
            .cloned()
            .ok_or_else(|| "ChatGPT did not finish registering Coucou.".to_string())?,
    };
    // Retain the issued registration even if the one-time code exchange fails.
    secrets::set(CLIENT_KEY, &issued_client)?;

    let result: Result<ChatGptStatus, String> = async {
        let tokens = exchange_code(&issued_client, code, &verifier, &redirect_uri).await?;
        let id_token = tokens
            .id_token
            .as_deref()
            .ok_or_else(|| "OpenAI did not return an identity token.".to_string())?;
        let identity = verify_identity(id_token, &issued_client, &nonce).await?;
        let scopes: Vec<String> = tokens
            .scope
            .split_whitespace()
            .map(str::to_string)
            .collect();
        if !scopes
            .iter()
            .any(|scope| scope == "chatgpt.tokens.use.direct")
        {
            return Err("ChatGPT plan usage was not enabled for Coucou.".into());
        }
        let refresh_token = tokens
            .refresh_token
            .ok_or_else(|| "OpenAI did not return a refresh token.".to_string())?;
        save(&AuthRecord {
            client_id: issued_client,
            host_id,
            subject: identity.sub,
            email: identity.email,
            name: identity.name,
            id_token: id_token.to_string(),
            access_token: tokens.access_token,
            refresh_token,
            scopes,
            expires_in: tokens.expires_in,
            saved_at: now(),
        })?;
        Ok(status())
    }
    .await;

    let (title, message) = if result.is_ok() {
        (
            "Coucou connected",
            "ChatGPT is connected. You can return to Coucou.",
        )
    } else {
        ("Coucou connection failed", "Coucou could not finish saving the connection. Return to the app for details and try again.")
    };
    let page = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nConnection: close\r\n\r\n<!doctype html><meta charset=utf-8><title>{title}</title><body style='font:16px system-ui;background:#0b0c0e;color:#f5f6f8;padding:40px'>{message}</body>"
    );
    let _ = stream.write_all(page.as_bytes()).await;
    result
}

async fn exchange_code(
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, String> {
    let response = reqwest::Client::new()
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
            ("resource", RESOURCE),
        ])
        .send()
        .await
        .map_err(|e| format!("Could not exchange the sign-in code: {e}"))?;
    token_response(response).await
}

async fn token_response(response: reqwest::Response) -> Result<TokenResponse, String> {
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        let detail = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("error_description")
                    .or_else(|| value.get("error"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "OpenAI rejected the authorization request.".into());
        return Err(detail);
    }
    serde_json::from_str(&text).map_err(|e| format!("Invalid OpenAI token response: {e}"))
}

async fn verify_identity(
    token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<IdentityClaims, String> {
    let header = decode_header(token).map_err(|e| format!("Invalid identity token: {e}"))?;
    let kid = header
        .kid
        .ok_or_else(|| "The identity token has no key id.".to_string())?;
    let jwks = reqwest::get(JWKS_ENDPOINT)
        .await
        .map_err(|e| format!("Could not load OpenAI signing keys: {e}"))?
        .json::<JwkSet>()
        .await
        .map_err(|e| format!("Invalid OpenAI signing keys: {e}"))?;
    let jwk = jwks
        .find(&kid)
        .ok_or_else(|| "OpenAI's signing key was not found.".to_string())?;
    let key = DecodingKey::from_jwk(jwk).map_err(|e| e.to_string())?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    validation.leeway = 5;
    validation
        .required_spec_claims
        .extend(["sub".into(), "iat".into()]);
    let claims = decode::<IdentityClaims>(token, &key, &validation)
        .map_err(|e| format!("Could not verify the OpenAI identity: {e}"))?
        .claims;
    if claims.nonce != nonce {
        return Err("The identity token nonce did not match.".into());
    }
    if claims.iss != ISSUER
        || claims.sub.is_empty()
        || claims.exp <= now()
        || claims.iat > now() + 5
    {
        return Err("The OpenAI identity token contained invalid claims.".into());
    }
    Ok(claims)
}

pub async fn access_token() -> Result<String, String> {
    let mut record = load().ok_or_else(|| "Connect your ChatGPT plan in Settings.".to_string())?;
    if record
        .saved_at
        .saturating_add(record.expires_in)
        .saturating_sub(60)
        > now()
    {
        return Ok(record.access_token);
    }
    let response = reqwest::Client::new()
        .post(TOKEN_ENDPOINT)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", record.client_id.as_str()),
            ("refresh_token", record.refresh_token.as_str()),
            ("resource", RESOURCE),
        ])
        .send()
        .await
        .map_err(|e| format!("Could not refresh the ChatGPT connection: {e}"))?;
    let tokens = token_response(response).await?;
    record.access_token = tokens.access_token;
    record.refresh_token = tokens
        .refresh_token
        .ok_or_else(|| "OpenAI did not rotate the refresh token.".to_string())?;
    if let Some(id_token) = tokens.id_token {
        record.id_token = id_token;
    }
    record.scopes = tokens
        .scope
        .split_whitespace()
        .map(str::to_string)
        .collect();
    record.expires_in = tokens.expires_in;
    record.saved_at = now();
    save(&record)?;
    Ok(record.access_token)
}

pub async fn models() -> Result<Vec<ChatGptModel>, String> {
    let token = access_token().await?;
    let response = reqwest::Client::new()
        .get("https://api.openai.com/v1/models")
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Could not load ChatGPT models: {e}"))?;
    let status = response.status();
    let value = response.json::<Value>().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err("OpenAI could not list models for this ChatGPT connection.".into());
    }
    let rows = value
        .get("models")
        .or_else(|| value.get("data"))
        .and_then(Value::as_array)
        .ok_or_else(|| "OpenAI returned an invalid model list.".to_string())?;
    let mut result = Vec::new();
    for row in rows {
        if row
            .get("visibility")
            .and_then(Value::as_str)
            .is_some_and(|value| value != "list")
        {
            continue;
        }
        let Some(id) = row
            .get("slug")
            .or_else(|| row.get("id"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let label = row
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or(id);
        result.push(ChatGptModel {
            id: id.into(),
            label: label.into(),
        });
    }
    Ok(result)
}
