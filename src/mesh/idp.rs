//! Tier-2 OIDC provider preflight and ceremony decisions (T7.4 / T7.5).
//!
//! Production device-flow HTTP and signature verification are not in this
//! module. This decides whether a discovery document is a secretless public
//! client, how RFC 8628 polling must wait, and whether a compact JWT is
//! structurally eligible for later verification.

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
        return IdentityRevalidationPosture::Due;
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
        return DevicePollDisposition::Expired {
            reason: "deadline",
        };
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
        Some(kid) if kid.contains(',') || kid.contains('\0') => {
            CompactJwtDisposition::AmbiguousKid
        }
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
    object.keys().any(|key| raw.matches(&format!("\"{key}\"")).count() > 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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

    fn encode_unpadded_base64url(bytes: &[u8]) -> String {
        const TABLE: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
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
}
