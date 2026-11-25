// Adapted from Orca `src/main/antigravity/native-credential-codec.ts`
// (Copyright (c) 2026 Lovecast Inc., MIT License).
//! Antigravity (`agy`) native credential codec & identity projection.
//!
//! Handles `go-keyring-base64:`, `go-keyring-encoded:`, plain JSON, both modern (`StoredToken`)
//! and legacy OAuth token envelopes, Google OIDC `id_token` claims, and `id_token_email`.

use serde_json::Value;
use zeroize::Zeroizing;

pub(crate) const MAX_CREDENTIAL_BYTES: usize = 64 * 1024;
pub(crate) const GOOGLE_ISSUER: &str = "https://accounts.google.com";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AntigravityCredentialIdentity {
    pub issuer: &'static str,
    pub subject: String,
    pub email: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AntigravityNativeCredential {
    pub contents: Zeroizing<String>,
    pub auth_method: String,
    pub identity: Option<AntigravityCredentialIdentity>,
    /// Unverified display hint (`id_token_email`) when `agy` drops `id_token` on refresh.
    pub email_hint: Option<String>,
    pub access_token: Zeroizing<String>,
    pub refresh_token: Option<Zeroizing<String>>,
}

impl std::fmt::Debug for AntigravityNativeCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AntigravityNativeCredential")
            .field("auth_method", &self.auth_method)
            .field("identity", &self.identity)
            .field("has_refresh_token", &self.refresh_token.is_some())
            .finish_non_exhaustive()
    }
}

pub(crate) fn invalid_credential() -> String {
    "Antigravity credentials could not be decoded.".to_owned()
}

pub(crate) fn decode_antigravity_keychain_value(value: &str) -> Result<Zeroizing<String>, String> {
    if value.len() > MAX_CREDENTIAL_BYTES {
        return Err(invalid_credential());
    }
    let trimmed = value.trim();
    if let Some(encoded) = trimmed.strip_prefix("go-keyring-base64:") {
        if encoded.is_empty() {
            return Err(invalid_credential());
        }
        let bytes = Zeroizing::new(base64_decode_strict(encoded).ok_or_else(invalid_credential)?);
        if base64_encode(&bytes) != encoded {
            return Err(invalid_credential());
        }
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| invalid_credential())?;
        return Ok(Zeroizing::new(text));
    }
    if let Some(encoded) = trimmed.strip_prefix("go-keyring-encoded:") {
        if encoded.is_empty() || !encoded.len().is_multiple_of(2) {
            return Err(invalid_credential());
        }
        let mut bytes = Zeroizing::new(Vec::with_capacity(encoded.len() / 2));
        let (pairs, rem) = encoded.as_bytes().as_chunks::<2>();
        if !rem.is_empty() {
            return Err(invalid_credential());
        }
        for pair in pairs {
            let s = std::str::from_utf8(pair).map_err(|_| invalid_credential())?;
            if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(invalid_credential());
            }
            let b = u8::from_str_radix(s, 16).map_err(|_| invalid_credential())?;
            bytes.push(b);
        }
        let text = String::from_utf8(bytes.to_vec()).map_err(|_| invalid_credential())?;
        return Ok(Zeroizing::new(text));
    }
    Ok(Zeroizing::new(trimmed.to_owned()))
}

#[cfg(any(target_os = "macos", test))]
pub(crate) fn encode_antigravity_keychain_value(
    contents: &str,
) -> Result<Zeroizing<String>, String> {
    if contents.len() > MAX_CREDENTIAL_BYTES {
        return Err(invalid_credential());
    }
    Ok(Zeroizing::new(format!(
        "go-keyring-base64:{}",
        base64_encode(contents.as_bytes())
    )))
}

pub(crate) fn read_google_identity_claims(claims: &Value) -> Option<AntigravityCredentialIdentity> {
    let obj = claims.as_object()?;
    let subject = obj
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())?
        .to_owned();
    let email = if obj.get("email_verified").and_then(Value::as_bool) == Some(true) {
        obj.get("email")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 320 && !s.chars().any(char::is_control))
            .map(str::to_owned)
    } else {
        None
    };
    Some(AntigravityCredentialIdentity {
        issuer: GOOGLE_ISSUER,
        subject,
        email,
    })
}

/// Unverified JWT claims label local accounts; they never authorize operations.
fn read_identity(id_token: Option<&Value>) -> Option<AntigravityCredentialIdentity> {
    let token = id_token?.as_str()?;
    let claims = krill_proto::account_login::jwt_claims(token)?;
    let iss = claims.get("iss").and_then(Value::as_str)?;
    if matches!(iss, "accounts.google.com" | "https://accounts.google.com") {
        read_google_identity_claims(&claims)
    } else {
        None
    }
}

fn read_email_hint(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?;
    if raw.len() > 320 || raw.chars().any(char::is_control) {
        return None;
    }
    let email = raw.trim();
    let (local, domain) = email.split_once('@')?;
    if local.is_empty()
        || domain.is_empty()
        || domain.contains('@')
        || email.chars().any(char::is_whitespace)
    {
        return None;
    }
    Some(email.to_owned())
}

pub(crate) fn parse_antigravity_native_credential(
    contents: &str,
) -> Result<AntigravityNativeCredential, String> {
    if contents.len() > MAX_CREDENTIAL_BYTES {
        return Err(invalid_credential());
    }
    let value: Value = serde_json::from_str(contents).map_err(|_| invalid_credential())?;
    let obj = value.as_object().ok_or_else(invalid_credential)?;
    // agy's legacy OAuth grants retain an empty auth method when rewritten as StoredToken.
    let legacy = !obj.contains_key("token") && !obj.contains_key("auth_method");
    let token = if legacy {
        obj
    } else {
        obj.get("token")
            .and_then(Value::as_object)
            .ok_or_else(invalid_credential)?
    };
    let auth_method = match obj.get("auth_method") {
        None => String::new(),
        Some(v) => v.as_str().ok_or_else(invalid_credential)?.to_owned(),
    };
    let access_token = token
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(invalid_credential)?
        .to_owned();
    let refresh_token = token
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| Zeroizing::new(s.to_owned()));
    let id_token = obj.get("id_token").or_else(|| token.get("id_token"));
    let identity = read_identity(id_token);
    let email_hint = identity
        .as_ref()
        .and_then(|id| id.email.clone())
        .or_else(|| {
            if obj.get("email_verified").and_then(Value::as_bool) == Some(false) {
                None
            } else {
                read_email_hint(obj.get("id_token_email"))
            }
        });
    Ok(AntigravityNativeCredential {
        contents: Zeroizing::new(contents.to_owned()),
        auth_method,
        identity,
        email_hint,
        access_token: Zeroizing::new(access_token),
        refresh_token,
    })
}

pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let char_at = |idx: u8| char::from(TABLE.get(usize::from(idx & 0x3f)).copied().unwrap_or(b'A'));
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let Some(&b0) = chunk.first() else {
            continue;
        };
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(char_at(b0 >> 2));
        out.push(char_at(((b0 & 0x03) << 4) | (b1 >> 4)));
        if chunk.len() > 1 {
            out.push(char_at(((b1 & 0x0f) << 2) | (b2 >> 6)));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(char_at(b2 & 0x3f));
        } else {
            out.push('=');
        }
    }
    out
}

pub(crate) fn base64_decode_strict(input: &str) -> Option<Vec<u8>> {
    if !input.len().is_multiple_of(4) {
        return None;
    }
    let trimmed = input.trim_end_matches('=');
    let pad = input.len() - trimmed.len();
    if pad > 2 {
        return None;
    }
    let mut decoded = Vec::with_capacity(input.len() / 4 * 3);
    let (mut bits, mut count) = (0_u32, 0_u32);
    for byte in trimmed.bytes() {
        let n = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(n);
        count += 6;
        if count >= 8 {
            count -= 8;
            decoded.push(u8::try_from((bits >> count) & 255).ok()?);
        }
    }
    if count >= 6 || bits & ((1 << count) - 1) != 0 {
        return None;
    }
    Some(decoded)
}
