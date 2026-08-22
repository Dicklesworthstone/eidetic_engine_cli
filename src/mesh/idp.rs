//! Tier-2 OIDC provider preflight, constrained HTTPS, and ID-token verify
//! (T7.4 / T7.5 / T7.6).
//!
//! Device-flow HTTP is a fail-closed `curl` plan with an empty environment.
//! Signature verification uses already-declared `ring` (RS256 / ES256) and
//! `ed25519-dalek` (EdDSA). Raw tokens never enter an origin event.

use std::io::Write;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const TEAM_IDP_PROVIDER_UNSUPPORTED_CODE: &str = "team_idp_provider_unsupported";
pub const TEAM_IDP_DEVICE_FLOW_EXPIRED_CODE: &str = "team_idp_device_flow_expired";
pub const TEAM_IDP_TOKEN_INVALID_CODE: &str = "team_idp_token_invalid";
pub const DEVICE_FLOW_DEFAULT_INTERVAL_SECS: u64 = 5;
pub const DEVICE_FLOW_SLOW_DOWN_SECS: u64 = 5;
pub const DEVICE_FLOW_LOCAL_DEADLINE_SECS: u64 = 1800;
pub const DEVICE_FLOW_MAX_REQUESTS: u32 = 300;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdpProviderCapability {
    SecretlessPublic,
    ClientSecretRequired,
    MissingDeviceEndpoint,
    MissingTokenEndpoint,
    Unsupported,
}

impl IdpProviderCapability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SecretlessPublic => "secretless_public",
            Self::ClientSecretRequired => "client_secret_required",
            Self::MissingDeviceEndpoint => "missing_device_endpoint",
            Self::MissingTokenEndpoint => "missing_token_endpoint",
            Self::Unsupported => "unsupported",
        }
    }

    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::SecretlessPublic)
    }
}

#[must_use]
pub fn classify_oidc_provider(discovery: &Value) -> IdpProviderCapability {
    let Some(object) = discovery.as_object() else {
        return IdpProviderCapability::Unsupported;
    };
    let token = object
        .get("token_endpoint")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.starts_with("https://"));
    if token.is_none() {
        return IdpProviderCapability::MissingTokenEndpoint;
    }
    let device = object
        .get("device_authorization_endpoint")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.starts_with("https://"));
    if device.is_none() {
        return IdpProviderCapability::MissingDeviceEndpoint;
    }
    let methods = object
        .get("token_endpoint_auth_methods_supported")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if methods.iter().any(|method| *method == "none") {
        return IdpProviderCapability::SecretlessPublic;
    }
    if methods
        .iter()
        .any(|method| *method == "client_secret_basic" || *method == "client_secret_post")
    {
        return IdpProviderCapability::ClientSecretRequired;
    }
    IdpProviderCapability::Unsupported
}

pub const FORBIDDEN_CURL_ENV: &[&str] = &[
    "http_proxy",
    "https_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
    "CURL_CA_BUNDLE",
    "SSL_CERT_FILE",
    "SSLKEYLOGFILE",
    "NETRC",
    "CURL_HOME",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConstrainedCurlPlan {
    pub binary: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub stdin_body: bool,
    pub ca_bundle: Option<String>,
}

#[must_use]
pub fn plan_constrained_https_post(
    curl_binary: &str,
    url: &str,
    timeout_secs: u64,
) -> Option<ConstrainedCurlPlan> {
    let binary = curl_binary.trim();
    let url = url.trim();
    if !binary.starts_with('/') || binary.contains("..") || !url.starts_with("https://") {
        return None;
    }
    Some(ConstrainedCurlPlan {
        binary: binary.to_owned(),
        argv: vec![
            binary.to_owned(),
            "--proto".to_owned(),
            "=https".to_owned(),
            "--tlsv1.2".to_owned(),
            "--http1.1".to_owned(),
            "--silent".to_owned(),
            "--show-error".to_owned(),
            "--max-time".to_owned(),
            timeout_secs.max(1).to_string(),
            "-X".to_owned(),
            "POST".to_owned(),
            "--data-binary".to_owned(),
            "@-".to_owned(),
            url.to_owned(),
        ],
        env: Vec::new(),
        stdin_body: true,
        ca_bundle: None,
    })
}

#[must_use]
pub fn plan_constrained_https_get(
    curl_binary: &str,
    url: &str,
    timeout_secs: u64,
) -> Option<ConstrainedCurlPlan> {
    let binary = curl_binary.trim();
    let url = url.trim();
    if !binary.starts_with('/') || binary.contains("..") || !url.starts_with("https://") {
        return None;
    }
    Some(ConstrainedCurlPlan {
        binary: binary.to_owned(),
        argv: vec![
            binary.to_owned(),
            "--proto".to_owned(),
            "=https".to_owned(),
            "--tlsv1.2".to_owned(),
            "--http1.1".to_owned(),
            "--silent".to_owned(),
            "--show-error".to_owned(),
            "--max-time".to_owned(),
            timeout_secs.max(1).to_string(),
            "-X".to_owned(),
            "GET".to_owned(),
            url.to_owned(),
        ],
        env: Vec::new(),
        stdin_body: false,
        ca_bundle: None,
    })
}

/// Pin an absolute CA bundle. Never `--insecure`.
#[must_use]
pub fn pin_constrained_https_ca(
    mut plan: ConstrainedCurlPlan,
    ca_bundle: &str,
) -> Option<ConstrainedCurlPlan> {
    let ca_bundle = ca_bundle.trim();
    if !ca_bundle.starts_with('/') || ca_bundle.contains("..") {
        return None;
    }
    if !std::path::Path::new(ca_bundle).is_file() {
        return None;
    }
    let url = plan.argv.pop()?;
    if !url.starts_with("https://") {
        return None;
    }
    plan.argv.push("--cacert".to_owned());
    plan.argv.push(ca_bundle.to_owned());
    plan.argv.push(url);
    plan.ca_bundle = Some(ca_bundle.to_owned());
    Some(plan)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConstrainedHttpsResult {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// Run a previously planned HTTPS GET/POST with a cleared environment.
pub fn execute_constrained_https(
    plan: &ConstrainedCurlPlan,
    body: Option<&[u8]>,
) -> Result<ConstrainedHttpsResult, String> {
    if !plan.binary.starts_with('/') || plan.binary.contains("..") {
        return Err("curl binary must be an absolute path".to_owned());
    }
    if plan.argv.first() != Some(&plan.binary) {
        return Err("curl argv must start with the absolute binary".to_owned());
    }
    if !plan.argv.iter().any(|arg| arg == "=https") {
        return Err("curl plan must pin --proto =https".to_owned());
    }
    if plan
        .argv
        .iter()
        .any(|arg| arg == "-k" || arg == "--insecure")
    {
        return Err("constrained curl must not disable TLS verification".to_owned());
    }
    if plan.ca_bundle.as_ref().is_some_and(|ca| {
        !plan
            .argv
            .windows(2)
            .any(|pair| pair == ["--cacert", ca.as_str()])
    }) {
        return Err("pinned CA bundle is missing from curl argv".to_owned());
    }
    if plan.stdin_body != body.is_some() {
        return Err("curl stdin body presence must match the plan".to_owned());
    }
    let mut command = Command::new(&plan.binary);
    command.args(plan.argv.iter().skip(1));
    command.env_clear();
    command.env("PATH", "/usr/bin:/bin");
    command.stdin(if plan.stdin_body {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("spawn constrained curl: {error}"))?;
    if let Some(body) = body {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "constrained curl stdin is unavailable".to_owned())?;
        stdin
            .write_all(body)
            .map_err(|error| format!("write constrained curl body: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait constrained curl: {error}"))?;
    Ok(ConstrainedHttpsResult {
        exit_code: output.status.code().unwrap_or(1),
        stdout: output.stdout,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Fetch a JWKS document over constrained HTTPS. Raw keys stay off disk.
pub fn fetch_jwks_document(
    curl_binary: &str,
    jwks_url: &str,
    ca_bundle: Option<&str>,
) -> Result<Value, String> {
    let mut plan = plan_constrained_https_get(curl_binary, jwks_url, 15)
        .ok_or_else(|| "JWKS fetch requires an absolute curl binary and https URL".to_owned())?;
    if let Some(ca_bundle) = ca_bundle {
        plan = pin_constrained_https_ca(plan, ca_bundle).ok_or_else(|| {
            "JWKS fetch CA pin requires an absolute existing CA bundle".to_owned()
        })?;
    }
    let executed = execute_constrained_https(&plan, None)?;
    if executed.exit_code != 0 {
        return Err(format!(
            "JWKS curl exited {} ({})",
            executed.exit_code,
            executed.stderr.trim()
        ));
    }
    let value: Value = serde_json::from_slice(&executed.stdout)
        .map_err(|error| format!("JWKS response is not JSON: {error}"))?;
    if value.get("keys").and_then(Value::as_array).is_none() {
        return Err("JWKS document is missing a keys array".to_owned());
    }
    Ok(value)
}

#[must_use]
pub fn form_urlencoded(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(input: &str) -> String {
    let mut out = String::new();
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[must_use]
pub fn discovery_https_endpoint(discovery: &Value, key: &str) -> Option<String> {
    discovery
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.starts_with("https://"))
        .map(str::to_owned)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdentityRevalidationPosture {
    Current,
    Due,
    Overdue,
    Suspended,
}

impl IdentityRevalidationPosture {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Due => "due",
            Self::Overdue => "overdue",
            Self::Suspended => "suspended",
        }
    }
}

/// Timer-only revalidation posture. No IdP HTTP.
#[must_use]
pub fn classify_identity_revalidation(
    checked_at_unix: i64,
    now_unix: i64,
    due_after_secs: i64,
    grace_secs: i64,
) -> IdentityRevalidationPosture {
    if now_unix < checked_at_unix {
        return IdentityRevalidationPosture::Current;
    }
    let age = now_unix.saturating_sub(checked_at_unix);
    if age < due_after_secs {
        return IdentityRevalidationPosture::Current;
    }
    if age < due_after_secs.saturating_add(grace_secs) {
        return IdentityRevalidationPosture::Due;
    }
    if age < due_after_secs.saturating_add(grace_secs.saturating_mul(2)) {
        return IdentityRevalidationPosture::Overdue;
    }
    IdentityRevalidationPosture::Suspended
}

/// Nondecreasing identity-authorization floor. Returns the later timestamp.
#[must_use]
pub fn advance_identity_auth_floor<'a>(current: Option<&'a str>, candidate: &'a str) -> &'a str {
    match current {
        Some(current) if current > candidate => current,
        _ => candidate,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceAuthorizationGrant {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DevicePollDisposition {
    Wait { delay_secs: u64 },
    Expired { reason: &'static str },
    Terminal { reason: &'static str },
}

#[must_use]
pub fn parse_device_authorization(value: &Value) -> Result<DeviceAuthorizationGrant, &'static str> {
    let object = value.as_object().ok_or("device_authorization_not_object")?;
    let device_code = object
        .get("device_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("missing_device_code")?
        .to_owned();
    let user_code = object
        .get("user_code")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("missing_user_code")?
        .to_owned();
    let verification_uri = object
        .get("verification_uri")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.starts_with("https://"))
        .ok_or("missing_verification_uri")?
        .to_owned();
    let verification_uri_complete = object
        .get("verification_uri_complete")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.starts_with("https://"))
        .map(str::to_owned);
    let expires_in = positive_u64(object.get("expires_in")).ok_or("invalid_expires_in")?;
    let interval = match object.get("interval") {
        None => DEVICE_FLOW_DEFAULT_INTERVAL_SECS,
        Some(value) => positive_u64(Some(value)).ok_or("invalid_interval")?,
    };
    Ok(DeviceAuthorizationGrant {
        device_code,
        user_code,
        verification_uri,
        verification_uri_complete,
        expires_in,
        interval,
    })
}

#[must_use]
pub fn device_poll_deadline_secs(provider_expires_in: u64) -> u64 {
    provider_expires_in.min(DEVICE_FLOW_LOCAL_DEADLINE_SECS)
}

#[must_use]
pub fn decide_device_poll(
    elapsed_secs: u64,
    deadline_secs: u64,
    interval_secs: u64,
    request_count: u32,
    token_error: Option<&str>,
) -> DevicePollDisposition {
    if request_count >= DEVICE_FLOW_MAX_REQUESTS {
        return DevicePollDisposition::Expired {
            reason: "poll_budget",
        };
    }
    if elapsed_secs >= deadline_secs {
        return DevicePollDisposition::Expired { reason: "deadline" };
    }
    let remaining = deadline_secs.saturating_sub(elapsed_secs);
    match token_error {
        None | Some("authorization_pending") => {
            if interval_secs > remaining {
                return DevicePollDisposition::Expired {
                    reason: "interval_exceeds_remaining",
                };
            }
            DevicePollDisposition::Wait {
                delay_secs: interval_secs,
            }
        }
        Some("slow_down") => {
            let delay = interval_secs.saturating_add(DEVICE_FLOW_SLOW_DOWN_SECS);
            if delay > remaining {
                return DevicePollDisposition::Expired {
                    reason: "interval_exceeds_remaining",
                };
            }
            DevicePollDisposition::Wait { delay_secs: delay }
        }
        Some("expired_token") => DevicePollDisposition::Terminal {
            reason: "expired_token",
        },
        Some("access_denied") => DevicePollDisposition::Terminal {
            reason: "access_denied",
        },
        Some(_) => DevicePollDisposition::Terminal { reason: "error" },
    }
}

fn positive_u64(value: Option<&Value>) -> Option<u64> {
    let value = value?;
    let parsed = value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| value.as_str()?.trim().parse::<u64>().ok())?;
    (parsed > 0).then_some(parsed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompactJwtDisposition {
    Eligible,
    Malformed,
    NonCanonical,
    UnsupportedCrit,
    EmbeddedKey,
    MissingKid,
    AmbiguousKid,
}

impl CompactJwtDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::Malformed => "malformed",
            Self::NonCanonical => "noncanonical",
            Self::UnsupportedCrit => "unsupported_crit",
            Self::EmbeddedKey => "embedded_key",
            Self::MissingKid => "missing_kid",
            Self::AmbiguousKid => "ambiguous_kid",
        }
    }

    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Eligible)
    }
}

/// Structural compact-JWT gate. Does not verify signatures.
#[must_use]
pub fn classify_compact_jwt(token: &str) -> CompactJwtDisposition {
    let trimmed = token.trim();
    if trimmed.is_empty() || trimmed.starts_with('.') || trimmed.ends_with('.') {
        return CompactJwtDisposition::Malformed;
    }
    let parts: Vec<&str> = trimmed.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return CompactJwtDisposition::Malformed;
    }
    if parts.iter().any(|part| {
        part.bytes()
            .any(|byte| matches!(byte, b'+' | b'/' | b'=' | b'\n' | b'\r' | b' '))
    }) {
        return CompactJwtDisposition::NonCanonical;
    }
    let Ok(header_bytes) = decode_unpadded_base64url(parts[0]) else {
        return CompactJwtDisposition::NonCanonical;
    };
    let Ok(payload_bytes) = decode_unpadded_base64url(parts[1]) else {
        return CompactJwtDisposition::NonCanonical;
    };
    if decode_unpadded_base64url(parts[2]).is_err() {
        return CompactJwtDisposition::NonCanonical;
    }
    if json_has_duplicate_keys(&header_bytes) || json_has_duplicate_keys(&payload_bytes) {
        return CompactJwtDisposition::Malformed;
    }
    let Ok(header) = serde_json::from_slice::<Value>(&header_bytes) else {
        return CompactJwtDisposition::Malformed;
    };
    let Some(object) = header.as_object() else {
        return CompactJwtDisposition::Malformed;
    };
    if object.contains_key("crit") {
        return CompactJwtDisposition::UnsupportedCrit;
    }
    if ["jku", "x5u", "jwk", "x5c"]
        .iter()
        .any(|key| object.contains_key(*key))
    {
        return CompactJwtDisposition::EmbeddedKey;
    }
    match object.get("kid").and_then(Value::as_str).map(str::trim) {
        None | Some("") => CompactJwtDisposition::MissingKid,
        Some(kid) if kid.contains(',') || kid.contains('\0') => CompactJwtDisposition::AmbiguousKid,
        Some(_) => CompactJwtDisposition::Eligible,
    }
}

fn decode_unpadded_base64url(input: &str) -> Result<Vec<u8>, ()> {
    if !input
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(());
    }
    let mut padded = input.to_owned();
    match padded.len() % 4 {
        0 => {}
        2 => padded.push_str("=="),
        3 => padded.push('='),
        _ => return Err(()),
    }
    let mut output = Vec::new();
    let mut buf = 0_u32;
    let mut bits = 0_u32;
    for byte in padded.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => return Err(()),
        };
        buf = (buf << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buf >> bits) as u8);
        }
    }
    Ok(output)
}

#[cfg(test)]
#[must_use]
pub(crate) fn encode_unpadded_base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        let b0 = bytes[i];
        let b1 = bytes.get(i + 1).copied();
        let b2 = bytes.get(i + 2).copied();
        out.push(char::from(TABLE[(b0 >> 2) as usize]));
        match (b1, b2) {
            (Some(b1), Some(b2)) => {
                out.push(char::from(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize]));
                out.push(char::from(TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize]));
                out.push(char::from(TABLE[(b2 & 0x3f) as usize]));
                i += 3;
            }
            (Some(b1), None) => {
                out.push(char::from(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize]));
                out.push(char::from(TABLE[((b1 & 0x0f) << 2) as usize]));
                i += 2;
            }
            (None, _) => {
                out.push(char::from(TABLE[((b0 & 0x03) << 4) as usize]));
                i += 1;
            }
        }
    }
    out
}

fn json_has_duplicate_keys(bytes: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    let Ok(raw) = std::str::from_utf8(bytes) else {
        return true;
    };
    object
        .keys()
        .any(|key| raw.matches(&format!("\"{key}\"")).count() > 1)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JwksKeyDisposition {
    Eligible,
    MissingKid,
    DuplicateKid,
    AmbiguousKid,
    UnsupportedKty,
    EmbeddedOrRemote,
}

impl JwksKeyDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Eligible => "eligible",
            Self::MissingKid => "missing_kid",
            Self::DuplicateKid => "duplicate_kid",
            Self::AmbiguousKid => "ambiguous_kid",
            Self::UnsupportedKty => "unsupported_kty",
            Self::EmbeddedOrRemote => "embedded_or_remote",
        }
    }
}

#[must_use]
pub fn classify_jwks_kid(jwks: &Value, kid: &str) -> JwksKeyDisposition {
    let kid = kid.trim();
    if kid.is_empty() || kid.contains(',') {
        return JwksKeyDisposition::AmbiguousKid;
    }
    let Some(keys) = jwks.get("keys").and_then(Value::as_array) else {
        return JwksKeyDisposition::MissingKid;
    };
    let matches: Vec<&Value> = keys
        .iter()
        .filter(|key| key.get("kid").and_then(Value::as_str) == Some(kid))
        .collect();
    match matches.as_slice() {
        [] => JwksKeyDisposition::MissingKid,
        [_, _, ..] => JwksKeyDisposition::DuplicateKid,
        [key] => {
            if ["jku", "x5u", "x5c"]
                .iter()
                .any(|field| key.get(*field).is_some())
            {
                return JwksKeyDisposition::EmbeddedOrRemote;
            }
            match key.get("kty").and_then(Value::as_str) {
                Some("RSA" | "EC" | "OKP") => JwksKeyDisposition::Eligible,
                _ => JwksKeyDisposition::UnsupportedKty,
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TokenResponseGate {
    pub jwt: CompactJwtDisposition,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
}

#[must_use]
pub fn classify_token_response(value: &Value) -> Result<TokenResponseGate, &'static str> {
    let object = value.as_object().ok_or("token_response_not_object")?;
    let id_token = object
        .get("id_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or("missing_id_token")?;
    Ok(TokenResponseGate {
        jwt: classify_compact_jwt(id_token),
        has_access_token: object
            .get("access_token")
            .and_then(Value::as_str)
            .is_some_and(|token| !token.trim().is_empty()),
        has_refresh_token: object
            .get("refresh_token")
            .and_then(Value::as_str)
            .is_some_and(|token| !token.trim().is_empty()),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReducedIdTokenClaims {
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub matched_groups: Vec<String>,
}

/// Allowlisted claim reduction. Full group lists and unrelated claims are dropped.
#[must_use]
pub fn reduce_id_token_claims(
    token: &str,
    configured_groups: &[&str],
) -> Result<ReducedIdTokenClaims, CompactJwtDisposition> {
    let disposition = classify_compact_jwt(token);
    if !disposition.accepted() {
        return Err(disposition);
    }
    let payload = token
        .split('.')
        .nth(1)
        .ok_or(CompactJwtDisposition::Malformed)?;
    let bytes =
        decode_unpadded_base64url(payload).map_err(|()| CompactJwtDisposition::NonCanonical)?;
    if json_has_duplicate_keys(&bytes) {
        return Err(CompactJwtDisposition::Malformed);
    }
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| CompactJwtDisposition::Malformed)?;
    let subject = value
        .get("sub")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(CompactJwtDisposition::Malformed)?
        .to_owned();
    let email = value
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| value.contains('@'))
        .map(str::to_owned);
    let email_verified = value
        .get("email_verified")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let presented = value
        .get("groups")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let mut matched_groups = configured_groups
        .iter()
        .filter(|group| {
            presented
                .iter()
                .any(|item| item.eq_ignore_ascii_case(group))
        })
        .map(|group| (*group).to_owned())
        .collect::<Vec<_>>();
    matched_groups.sort();
    matched_groups.dedup();
    Ok(ReducedIdTokenClaims {
        subject,
        email,
        email_verified,
        matched_groups,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IdTokenClaimDisposition {
    Accepted,
    IssuerMismatch,
    AudienceMismatch,
    Expired,
    NotYetValid,
    MissingIssuer,
}

impl IdTokenClaimDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::IssuerMismatch => "issuer_mismatch",
            Self::AudienceMismatch => "audience_mismatch",
            Self::Expired => "expired",
            Self::NotYetValid => "not_yet_valid",
            Self::MissingIssuer => "missing_issuer",
        }
    }

    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Accepted)
    }
}

/// Issuer/audience/time checks on a structurally eligible ID token. No signature verify.
#[must_use]
pub fn classify_id_token_claims(
    token: &str,
    expected_issuer: &str,
    expected_audience: &str,
    now_unix: i64,
) -> IdTokenClaimDisposition {
    let Ok(claims) = id_token_payload(token) else {
        return IdTokenClaimDisposition::MissingIssuer;
    };
    let Some(iss) = claims
        .get("iss")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return IdTokenClaimDisposition::MissingIssuer;
    };
    if iss != expected_issuer.trim() {
        return IdTokenClaimDisposition::IssuerMismatch;
    }
    let audience_ok = match claims.get("aud") {
        Some(Value::String(aud)) => aud.trim() == expected_audience.trim(),
        Some(Value::Array(items)) => items.iter().any(|item| {
            item.as_str()
                .is_some_and(|aud| aud.trim() == expected_audience.trim())
        }),
        _ => false,
    };
    let azp_ok = claims
        .get("azp")
        .and_then(Value::as_str)
        .is_none_or(|azp| azp.trim() == expected_audience.trim());
    if !audience_ok || !azp_ok {
        return IdTokenClaimDisposition::AudienceMismatch;
    }
    if let Some(nbf) = claims.get("nbf").and_then(Value::as_i64)
        && now_unix < nbf
    {
        return IdTokenClaimDisposition::NotYetValid;
    }
    if let Some(exp) = claims.get("exp").and_then(Value::as_i64)
        && now_unix >= exp
    {
        return IdTokenClaimDisposition::Expired;
    }
    IdTokenClaimDisposition::Accepted
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JwtSignatureDisposition {
    Verified,
    StructuralRejected,
    JwksRejected,
    UnsupportedAlg,
    SignatureInvalid,
}

impl JwtSignatureDisposition {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::StructuralRejected => "structural_rejected",
            Self::JwksRejected => "jwks_rejected",
            Self::UnsupportedAlg => "unsupported_alg",
            Self::SignatureInvalid => "signature_invalid",
        }
    }

    #[must_use]
    pub const fn accepted(self) -> bool {
        matches!(self, Self::Verified)
    }
}

/// Verify a compact JWT against a local JWKS document. No network.
#[must_use]
pub fn verify_compact_jwt_with_jwks(token: &str, jwks: &Value) -> JwtSignatureDisposition {
    if !classify_compact_jwt(token).accepted() {
        return JwtSignatureDisposition::StructuralRejected;
    }
    let mut parts = token.trim().split('.');
    let (Some(header_b64), Some(payload_b64), Some(sig_b64)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return JwtSignatureDisposition::StructuralRejected;
    };
    if parts.next().is_some() {
        return JwtSignatureDisposition::StructuralRejected;
    }
    let Ok(header_bytes) = decode_unpadded_base64url(header_b64) else {
        return JwtSignatureDisposition::StructuralRejected;
    };
    let Ok(header) = serde_json::from_slice::<Value>(&header_bytes) else {
        return JwtSignatureDisposition::StructuralRejected;
    };
    let Some(alg) = header.get("alg").and_then(Value::as_str).map(str::trim) else {
        return JwtSignatureDisposition::UnsupportedAlg;
    };
    let Some(kid) = header.get("kid").and_then(Value::as_str).map(str::trim) else {
        return JwtSignatureDisposition::StructuralRejected;
    };
    if classify_jwks_kid(jwks, kid) != JwksKeyDisposition::Eligible {
        return JwtSignatureDisposition::JwksRejected;
    }
    let Some(key) = jwks
        .get("keys")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| item.get("kid").and_then(Value::as_str) == Some(kid))
    else {
        return JwtSignatureDisposition::JwksRejected;
    };
    if let Some(key_use) = key.get("use").and_then(Value::as_str)
        && key_use != "sig"
    {
        return JwtSignatureDisposition::UnsupportedAlg;
    }
    if let Some(key_alg) = key.get("alg").and_then(Value::as_str)
        && key_alg != alg
    {
        return JwtSignatureDisposition::UnsupportedAlg;
    }
    let Ok(signature) = decode_unpadded_base64url(sig_b64) else {
        return JwtSignatureDisposition::StructuralRejected;
    };
    let signing_input = format!("{header_b64}.{payload_b64}");
    let ok = match (alg, key.get("kty").and_then(Value::as_str)) {
        ("RS256", Some("RSA")) => verify_rs256(key, signing_input.as_bytes(), &signature),
        ("ES256", Some("EC")) => verify_es256(key, signing_input.as_bytes(), &signature),
        ("EdDSA", Some("OKP")) => verify_eddsa(key, signing_input.as_bytes(), &signature),
        _ => return JwtSignatureDisposition::UnsupportedAlg,
    };
    if ok {
        JwtSignatureDisposition::Verified
    } else {
        JwtSignatureDisposition::SignatureInvalid
    }
}

fn jwk_b64_field(key: &Value, field: &str) -> Option<Vec<u8>> {
    decode_unpadded_base64url(key.get(field).and_then(Value::as_str)?.trim()).ok()
}

fn verify_rs256(key: &Value, signing_input: &[u8], signature: &[u8]) -> bool {
    let (Some(n), Some(e)) = (jwk_b64_field(key, "n"), jwk_b64_field(key, "e")) else {
        return false;
    };
    ring::signature::RsaPublicKeyComponents { n, e }
        .verify(
            &ring::signature::RSA_PKCS1_2048_8192_SHA256,
            signing_input,
            signature,
        )
        .is_ok()
}

fn verify_es256(key: &Value, signing_input: &[u8], signature: &[u8]) -> bool {
    if key.get("crv").and_then(Value::as_str) != Some("P-256") {
        return false;
    }
    let (Some(x), Some(y)) = (jwk_b64_field(key, "x"), jwk_b64_field(key, "y")) else {
        return false;
    };
    if x.len() != 32 || y.len() != 32 || signature.len() != 64 {
        return false;
    }
    let mut point = Vec::with_capacity(65);
    point.push(0x04);
    point.extend_from_slice(&x);
    point.extend_from_slice(&y);
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, &point)
        .verify(signing_input, signature)
        .is_ok()
}

fn verify_eddsa(key: &Value, signing_input: &[u8], signature: &[u8]) -> bool {
    if key.get("crv").and_then(Value::as_str) != Some("Ed25519") {
        return false;
    }
    let Some(x) = jwk_b64_field(key, "x") else {
        return false;
    };
    let Ok(x): Result<[u8; 32], _> = x.try_into() else {
        return false;
    };
    let Ok(sig_bytes): Result<[u8; 64], _> = signature.to_vec().try_into() else {
        return false;
    };
    let Ok(verifying) = ed25519_dalek::VerifyingKey::from_bytes(&x) else {
        return false;
    };
    let signature = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    verifying.verify_strict(signing_input, &signature).is_ok()
}

#[must_use]
pub fn json_carries_bearer_fields(value: &Value) -> bool {
    const KEYS: &[&str] = &[
        "id_token",
        "idToken",
        "access_token",
        "accessToken",
        "refresh_token",
        "refreshToken",
        "client_secret",
        "clientSecret",
    ];
    KEYS.iter().any(|key| value.get(*key).is_some())
}

fn id_token_payload(token: &str) -> Result<Value, CompactJwtDisposition> {
    if !classify_compact_jwt(token).accepted() {
        return Err(CompactJwtDisposition::Malformed);
    }
    let payload = token
        .split('.')
        .nth(1)
        .ok_or(CompactJwtDisposition::Malformed)?;
    let bytes =
        decode_unpadded_base64url(payload).map_err(|()| CompactJwtDisposition::NonCanonical)?;
    serde_json::from_slice(&bytes).map_err(|_| CompactJwtDisposition::Malformed)
}

/// Authenticated identity-attest payload. Raw tokens are forbidden on this frame.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityAttestFrameV1 {
    pub schema: String,
    pub team_id: String,
    pub member_id: String,
    pub subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub matched_groups: Vec<String>,
    pub token_hash: String,
    pub checked_at: String,
}

pub const IDENTITY_ATTEST_FRAME_SCHEMA_V1: &str = "ee.team.identity_attest.v1";

#[must_use]
pub fn identity_attest_frame_leaks_bearer(frame: &IdentityAttestFrameV1) -> bool {
    let Ok(json) = serde_json::to_string(frame) else {
        return true;
    };
    ["access_token", "refresh_token", "id_token", "client_secret"]
        .iter()
        .any(|needle| json.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[cfg(unix)]
    use std::process::{Command, Stdio};

    #[test]
    fn secretless_public_client_is_accepted() {
        let discovery = json!({
            "token_endpoint": "https://idp.example/token",
            "device_authorization_endpoint": "https://idp.example/device",
            "token_endpoint_auth_methods_supported": ["none"]
        });
        assert_eq!(
            classify_oidc_provider(&discovery),
            IdpProviderCapability::SecretlessPublic
        );
        assert!(classify_oidc_provider(&discovery).accepted());
    }

    #[test]
    fn client_secret_required_is_rejected() {
        let discovery = json!({
            "token_endpoint": "https://idp.example/token",
            "device_authorization_endpoint": "https://idp.example/device",
            "token_endpoint_auth_methods_supported": ["client_secret_basic"]
        });
        assert_eq!(
            classify_oidc_provider(&discovery),
            IdpProviderCapability::ClientSecretRequired
        );
        assert!(!classify_oidc_provider(&discovery).accepted());
    }

    #[test]
    fn missing_device_endpoint_is_rejected() {
        let discovery = json!({
            "token_endpoint": "https://idp.example/token",
            "token_endpoint_auth_methods_supported": ["none"]
        });
        assert_eq!(
            classify_oidc_provider(&discovery),
            IdpProviderCapability::MissingDeviceEndpoint
        );
    }

    #[test]
    fn http_endpoints_are_not_accepted() {
        let discovery = json!({
            "token_endpoint": "http://idp.example/token",
            "device_authorization_endpoint": "https://idp.example/device",
            "token_endpoint_auth_methods_supported": ["none"]
        });
        assert_eq!(
            classify_oidc_provider(&discovery),
            IdpProviderCapability::MissingTokenEndpoint
        );
    }

    #[test]
    fn device_authorization_defaults_interval_and_rejects_zero_expiry() {
        let grant = parse_device_authorization(&json!({
            "device_code": "dc",
            "user_code": "WDJB-MJHT",
            "verification_uri": "https://idp.example/device",
            "expires_in": 600
        }))
        .expect("grant");
        assert_eq!(grant.interval, 5);
        assert_eq!(grant.expires_in, 600);
        assert!(
            parse_device_authorization(&json!({
                "device_code": "dc",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://idp.example/device",
                "expires_in": 0
            }))
            .is_err()
        );
        assert_eq!(device_poll_deadline_secs(10_000), 1800);
    }

    #[test]
    fn device_poll_slow_down_adds_five_and_expiry_is_terminal() {
        assert_eq!(
            decide_device_poll(0, 1800, 5, 0, Some("authorization_pending")),
            DevicePollDisposition::Wait { delay_secs: 5 }
        );
        assert_eq!(
            decide_device_poll(0, 1800, 5, 0, Some("slow_down")),
            DevicePollDisposition::Wait { delay_secs: 10 }
        );
        assert_eq!(
            decide_device_poll(1796, 1800, 5, 1, Some("authorization_pending")),
            DevicePollDisposition::Expired {
                reason: "interval_exceeds_remaining"
            }
        );
        assert_eq!(
            decide_device_poll(0, 1800, 5, 300, Some("authorization_pending")),
            DevicePollDisposition::Expired {
                reason: "poll_budget"
            }
        );
        assert_eq!(
            decide_device_poll(10, 1800, 5, 1, Some("access_denied")),
            DevicePollDisposition::Terminal {
                reason: "access_denied"
            }
        );
    }

    #[test]
    fn compact_jwt_rejects_padding_embedded_keys_and_missing_kid() {
        const ELIGIBLE: &str = "eyJhbGciOiJSUzI1NiIsImtpZCI6ImsxIn0.eyJzdWIiOiJhIn0.c2ln";
        assert_eq!(
            classify_compact_jwt(ELIGIBLE),
            CompactJwtDisposition::Eligible
        );
        assert_eq!(
            classify_compact_jwt("abc.def"),
            CompactJwtDisposition::Malformed
        );
        assert_eq!(
            classify_compact_jwt("eyJhbGciOiJSUzI1NiIsImtpZCI6ImsxIn0=.eyJzdWIiOiJhIn0.c2ln"),
            CompactJwtDisposition::NonCanonical
        );
        const NO_KID: &str = "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJhIn0.c2ln";
        assert_eq!(
            classify_compact_jwt(NO_KID),
            CompactJwtDisposition::MissingKid
        );
        let embedded = compact_jwt(br#"{"alg":"RS256","kid":"k1","jku":"https://evil"}"#);
        assert_eq!(
            classify_compact_jwt(&embedded),
            CompactJwtDisposition::EmbeddedKey
        );
        let crit = compact_jwt(br#"{"alg":"RS256","kid":"k1","crit":["bork"]}"#);
        assert_eq!(
            classify_compact_jwt(&crit),
            CompactJwtDisposition::UnsupportedCrit
        );
    }

    fn compact_jwt(header: &[u8]) -> String {
        format!(
            "{}.{}.{}",
            encode_unpadded_base64url(header),
            encode_unpadded_base64url(br#"{"sub":"a"}"#),
            encode_unpadded_base64url(b"sig")
        )
    }

    #[test]
    fn identity_revalidation_is_timer_only_and_floor_never_moves_back() {
        assert_eq!(
            classify_identity_revalidation(200, 110, 60, 60),
            IdentityRevalidationPosture::Current
        );
        assert_eq!(
            classify_identity_revalidation(100, 110, 60, 60),
            IdentityRevalidationPosture::Current
        );
        assert_eq!(
            classify_identity_revalidation(100, 170, 60, 60),
            IdentityRevalidationPosture::Due
        );
        assert_eq!(
            classify_identity_revalidation(100, 230, 60, 60),
            IdentityRevalidationPosture::Overdue
        );
        assert_eq!(
            classify_identity_revalidation(100, 300, 60, 60),
            IdentityRevalidationPosture::Suspended
        );
        assert_eq!(
            advance_identity_auth_floor(Some("2026-08-13T19:00:00Z"), "2026-08-13T18:00:00Z"),
            "2026-08-13T19:00:00Z"
        );
        assert_eq!(
            advance_identity_auth_floor(Some("2026-08-13T18:00:00Z"), "2026-08-13T19:00:00Z"),
            "2026-08-13T19:00:00Z"
        );
    }

    #[test]
    fn jwks_kid_must_be_unique_and_local() {
        let jwks = json!({
            "keys": [
                {"kty":"RSA","kid":"k1","n":"n","e":"AQAB"},
                {"kty":"EC","kid":"k2","crv":"P-256","x":"x","y":"y"}
            ]
        });
        assert_eq!(classify_jwks_kid(&jwks, "k1"), JwksKeyDisposition::Eligible);
        assert_eq!(
            classify_jwks_kid(&jwks, "missing"),
            JwksKeyDisposition::MissingKid
        );
        let dup = json!({"keys":[{"kty":"RSA","kid":"k1"},{"kty":"RSA","kid":"k1"}]});
        assert_eq!(
            classify_jwks_kid(&dup, "k1"),
            JwksKeyDisposition::DuplicateKid
        );
        let remote = json!({"keys":[{"kty":"RSA","kid":"k1","x5u":"https://evil"}]});
        assert_eq!(
            classify_jwks_kid(&remote, "k1"),
            JwksKeyDisposition::EmbeddedOrRemote
        );
    }

    #[test]
    fn constrained_curl_plan_is_https_only_and_has_empty_env() {
        let plan = plan_constrained_https_post("/usr/bin/curl", "https://idp.example/token", 15)
            .expect("plan");
        assert!(plan.stdin_body);
        assert!(plan.env.is_empty());
        assert!(plan.argv.contains(&"=https".to_owned()));
        assert!(plan.argv.contains(&"@-".to_owned()));
        assert!(!plan.argv.iter().any(|arg| arg.contains("client_secret")));
        assert!(FORBIDDEN_CURL_ENV.contains(&"HTTPS_PROXY"));
        assert!(FORBIDDEN_CURL_ENV.contains(&"SSLKEYLOGFILE"));
        assert!(plan_constrained_https_post("curl", "https://idp.example/token", 15).is_none());
        assert!(
            plan_constrained_https_post("/usr/bin/curl", "http://idp.example/token", 15).is_none()
        );
        let get = plan_constrained_https_get("/usr/bin/curl", "https://idp.example/jwks", 15)
            .expect("get plan");
        assert!(!get.stdin_body);
        assert!(get.argv.contains(&"GET".to_owned()));
        assert!(plan_constrained_https_get("curl", "https://idp.example/jwks", 15).is_none());
        let ca = std::env::temp_dir().join("ee-team-idp-ca.pem");
        std::fs::write(
            &ca,
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n",
        )
        .expect("write ca");
        let pinned = pin_constrained_https_ca(plan, ca.to_str().expect("utf8")).expect("pin");
        assert_eq!(pinned.ca_bundle.as_deref(), ca.to_str());
        assert!(pinned.argv.contains(&"--cacert".to_owned()));
        assert!(pin_constrained_https_ca(get, "ca.pem").is_none());
    }

    #[test]
    fn token_response_gate_requires_id_token_and_flags_bearer_material() {
        const ID: &str = "eyJhbGciOiJSUzI1NiIsImtpZCI6ImsxIn0.eyJzdWIiOiJhIn0.c2ln";
        let gate = classify_token_response(&json!({
            "id_token": ID,
            "access_token": "atk",
            "refresh_token": "rtk"
        }))
        .expect("gate");
        assert_eq!(gate.jwt, CompactJwtDisposition::Eligible);
        assert!(gate.has_access_token);
        assert!(gate.has_refresh_token);
        assert!(classify_token_response(&json!({"access_token":"atk"})).is_err());
    }

    #[test]
    fn id_token_claim_reduction_keeps_subject_and_configured_groups_only() {
        let token = format!(
            "{}.{}.{}",
            encode_unpadded_base64url(br#"{"alg":"RS256","kid":"k1"}"#),
            encode_unpadded_base64url(
                br#"{"sub":"user-1","email":"alice@acme.com","email_verified":true,"groups":["eng","secret-admin","staff"],"iss":"https://idp.example"}"#,
            ),
            encode_unpadded_base64url(b"sig"),
        );
        let claims = reduce_id_token_claims(&token, &["eng", "staff"]).expect("claims");
        assert_eq!(claims.subject, "user-1");
        assert_eq!(claims.email.as_deref(), Some("alice@acme.com"));
        assert!(claims.email_verified);
        assert_eq!(
            claims.matched_groups,
            vec!["eng".to_owned(), "staff".to_owned()]
        );
        let json = serde_json::to_string(&json!({
            "subject": claims.subject,
            "email": claims.email,
            "matchedGroups": claims.matched_groups
        }))
        .expect("json");
        assert!(!json.contains("secret-admin"));
        assert!(!json.contains("https://idp.example"));
    }

    #[test]
    fn id_token_claim_checks_issuer_audience_and_expiry() {
        let token = format!(
            "{}.{}.{}",
            encode_unpadded_base64url(br#"{"alg":"RS256","kid":"k1"}"#),
            encode_unpadded_base64url(
                br#"{"sub":"user-1","iss":"https://idp.example","aud":"ee-public","exp":200,"nbf":50}"#,
            ),
            encode_unpadded_base64url(b"sig"),
        );
        assert_eq!(
            classify_id_token_claims(&token, "https://idp.example", "ee-public", 100),
            IdTokenClaimDisposition::Accepted
        );
        assert_eq!(
            classify_id_token_claims(&token, "https://other.example", "ee-public", 100),
            IdTokenClaimDisposition::IssuerMismatch
        );
        assert_eq!(
            classify_id_token_claims(&token, "https://idp.example", "other-client", 100),
            IdTokenClaimDisposition::AudienceMismatch
        );
        assert_eq!(
            classify_id_token_claims(&token, "https://idp.example", "ee-public", 200),
            IdTokenClaimDisposition::Expired
        );
        assert_eq!(
            classify_id_token_claims(&token, "https://idp.example", "ee-public", 40),
            IdTokenClaimDisposition::NotYetValid
        );
    }

    #[test]
    fn json_bearer_fields_catch_camel_and_snake_case() {
        assert!(json_carries_bearer_fields(&json!({"idToken":"x"})));
        assert!(json_carries_bearer_fields(&json!({"id_token":"x"})));
        assert!(json_carries_bearer_fields(&json!({"clientSecret":"x"})));
        assert!(!json_carries_bearer_fields(&json!({
            "tokenHash": "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        })));
    }

    #[test]
    fn eddsa_jwt_verifies_against_local_okp_jwks() {
        let signing = ed25519_dalek::SigningKey::from_bytes(&[0x11; 32]);
        let public = signing.verifying_key();
        let header = encode_unpadded_base64url(br#"{"alg":"EdDSA","kid":"ed1"}"#);
        let payload = encode_unpadded_base64url(br#"{"sub":"user-1","iss":"https://idp.example"}"#);
        let signing_input = format!("{header}.{payload}");
        let signature = ed25519_dalek::Signer::sign(&signing, signing_input.as_bytes());
        let token = format!(
            "{signing_input}.{}",
            encode_unpadded_base64url(&signature.to_bytes())
        );
        let jwks = json!({
            "keys": [{
                "kty": "OKP",
                "crv": "Ed25519",
                "kid": "ed1",
                "use": "sig",
                "x": encode_unpadded_base64url(public.as_bytes())
            }]
        });
        assert_eq!(
            verify_compact_jwt_with_jwks(&token, &jwks),
            JwtSignatureDisposition::Verified
        );
        let mut bad = token;
        bad.replace_range(bad.len() - 2.., "AA");
        assert_eq!(
            verify_compact_jwt_with_jwks(&bad, &jwks),
            JwtSignatureDisposition::SignatureInvalid
        );
    }

    #[test]
    fn es256_jwt_verifies_against_generated_p256_jwks() {
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &rng,
        )
        .expect("p256 generate");
        let pair = ring::signature::EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8.as_ref(),
            &rng,
        )
        .expect("p256 parse");
        let public = ring::signature::KeyPair::public_key(&pair);
        let point = public.as_ref();
        assert_eq!(point.first().copied(), Some(0x04));
        assert_eq!(point.len(), 65);
        let header = encode_unpadded_base64url(br#"{"alg":"ES256","kid":"ec1"}"#);
        let payload = encode_unpadded_base64url(br#"{"sub":"user-1"}"#);
        let signing_input = format!("{header}.{payload}");
        let signature = pair
            .sign(&rng, signing_input.as_bytes())
            .expect("p256 sign");
        let token = format!(
            "{signing_input}.{}",
            encode_unpadded_base64url(signature.as_ref())
        );
        let jwks = json!({
            "keys": [{
                "kty": "EC",
                "crv": "P-256",
                "kid": "ec1",
                "x": encode_unpadded_base64url(&point[1..33]),
                "y": encode_unpadded_base64url(&point[33..65])
            }]
        });
        assert_eq!(
            verify_compact_jwt_with_jwks(&token, &jwks),
            JwtSignatureDisposition::Verified
        );
    }

    #[test]
    fn identity_attest_frame_never_serializes_bearer_material() {
        let frame = IdentityAttestFrameV1 {
            schema: IDENTITY_ATTEST_FRAME_SCHEMA_V1.to_owned(),
            team_id: "team_aaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned(),
            member_id: "mbr_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_owned(),
            subject: "user-1".to_owned(),
            email: Some("alice@acme.com".to_owned()),
            matched_groups: vec!["eng".to_owned()],
            token_hash: "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                .to_owned(),
            checked_at: "2026-08-13T21:00:00Z".to_owned(),
        };
        assert!(!identity_attest_frame_leaks_bearer(&frame));
        let json = serde_json::to_string(&frame).expect("json");
        assert!(json.contains("user-1"));
        assert!(!json.contains("eyJ"));
    }

    #[cfg(unix)]
    struct LiveFakeIdp {
        child: std::process::Child,
        _dir: tempfile::TempDir,
        base: String,
        ca: std::path::PathBuf,
    }

    #[cfg(unix)]
    impl Drop for LiveFakeIdp {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[cfg(unix)]
    fn spawn_live_fake_idp() -> LiveFakeIdp {
        let script = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("scripts/e2e_overhaul/lib/fake_idp.py");
        assert!(
            script.is_file(),
            "fake_idp.py missing: {}",
            script.display()
        );
        let dir = tempfile::tempdir().expect("fake idp dir");
        let mut child = Command::new("python3")
            .arg(&script)
            .arg("--dir")
            .arg(dir.path())
            .arg("--port")
            .arg("0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fake_idp.py");
        let ready = dir.path().join("ready");
        for _ in 0..100 {
            if !matches!(child.try_wait(), Ok(None)) {
                let stderr = child.stderr.take().map_or_else(String::new, |mut pipe| {
                    let mut out = String::new();
                    let _ = std::io::Read::read_to_string(&mut pipe, &mut out);
                    out
                });
                panic!("fake_idp.py exited before ready: {stderr}");
            }
            if ready.is_file()
                && let Ok(text) = std::fs::read_to_string(&ready)
                && let Some(port) = text.split_whitespace().next()
            {
                let ca = dir.path().join("ca.pem");
                if ca.is_file() {
                    return LiveFakeIdp {
                        child,
                        _dir: dir,
                        base: format!("https://127.0.0.1:{port}"),
                        ca,
                    };
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = child.kill();
        panic!("fake_idp.py did not become ready");
    }

    #[cfg(unix)]
    fn live_https_get(url: &str, ca: &std::path::Path) -> Value {
        let plan = pin_constrained_https_ca(
            plan_constrained_https_get("/usr/bin/curl", url, 10).expect("get plan"),
            ca.to_str().expect("ca utf8"),
        )
        .expect("pin ca");
        let result = execute_constrained_https(&plan, None).expect("curl get");
        assert_eq!(result.exit_code, 0, "GET {url} stderr={}", result.stderr);
        serde_json::from_slice(&result.stdout).expect("json get")
    }

    #[cfg(unix)]
    fn live_https_post(url: &str, ca: &std::path::Path, body: &str) -> ConstrainedHttpsResult {
        let plan = pin_constrained_https_ca(
            plan_constrained_https_post("/usr/bin/curl", url, 10).expect("post plan"),
            ca.to_str().expect("ca utf8"),
        )
        .expect("pin ca");
        execute_constrained_https(&plan, Some(body.as_bytes())).expect("curl post")
    }

    #[cfg(unix)]
    #[test]
    fn constrained_https_fetches_fake_idp_jwks_and_verifies_rs256() {
        assert!(std::path::Path::new("/usr/bin/curl").is_file());
        let idp = spawn_live_fake_idp();
        let discovery = live_https_get(
            &format!("{}/.well-known/openid-configuration", idp.base),
            &idp.ca,
        );
        assert_eq!(
            classify_oidc_provider(&discovery),
            IdpProviderCapability::SecretlessPublic
        );
        let device = live_https_post(&format!("{}/device", idp.base), &idp.ca, "");
        let device_json: Value = serde_json::from_slice(&device.stdout).expect("device json");
        let grant = parse_device_authorization(&device_json).expect("device grant");
        let pending = live_https_post(
            &format!("{}/token", idp.base),
            &idp.ca,
            &form_urlencoded(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &grant.device_code),
            ]),
        );
        let pending_json: Value = serde_json::from_slice(&pending.stdout).expect("pending json");
        assert_eq!(
            pending_json.get("error").and_then(Value::as_str),
            Some("authorization_pending")
        );
        let _granted_status = live_https_post(
            &format!("{}/_control", idp.base),
            &idp.ca,
            &serde_json::to_string(&json!({
                "action": "set_status",
                "status": "granted",
                "user_code": grant.user_code
            }))
            .expect("control json"),
        );
        let token_resp = live_https_post(
            &format!("{}/token", idp.base),
            &idp.ca,
            &form_urlencoded(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &grant.device_code),
            ]),
        );
        let token_json: Value = serde_json::from_slice(&token_resp.stdout).expect("token json");
        let gate = classify_token_response(&token_json).expect("token gate");
        assert_eq!(gate.jwt, CompactJwtDisposition::Eligible);
        let id_token = token_json
            .get("id_token")
            .and_then(Value::as_str)
            .expect("id_token");
        let jwks = live_https_get(&format!("{}/jwks", idp.base), &idp.ca);
        assert_eq!(
            verify_compact_jwt_with_jwks(id_token, &jwks),
            JwtSignatureDisposition::Verified
        );
        assert!(!id_token.is_empty());
    }
}
