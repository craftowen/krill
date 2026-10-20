//! 로컬 로그인 계정의 표시용 이메일. JWT는 인증 판정에 쓰지 않는다.
//! 저장 형식 참고: Orca codex-auth-identity·native-credential-codec (MIT, Lovecast Inc.).
use krill_proto::account_login::Identity;
use krill_proto::usage::ProviderKind;
use serde_json::Value;

pub(crate) fn from_credentials(provider: ProviderKind, raw: &[u8]) -> Option<Identity> {
    let data: Value = serde_json::from_slice(raw).ok()?;
    let token = match provider {
        ProviderKind::Codex => {
            // API 키 로그인에 남은 OAuth 토큰을 현재 계정으로 표시하지 않는다.
            if data.get("auth_mode").and_then(Value::as_str) == Some("apikey")
                || data
                    .get("OPENAI_API_KEY")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.trim().is_empty())
            {
                return None;
            }
            data.pointer("/tokens/id_token")
                .or_else(|| data.pointer("/tokens/idToken"))?
        }
        ProviderKind::Antigravity => data
            .get("id_token")
            .or_else(|| data.pointer("/token/id_token"))?,
        _ => return None,
    }
    .as_str()?;
    let claims = claims(token)?;
    if provider == ProviderKind::Antigravity
        && (!matches!(
            claims.get("iss").and_then(Value::as_str),
            Some("accounts.google.com" | "https://accounts.google.com")
        ) || claims
            .get("sub")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
            || claims.get("email_verified").and_then(Value::as_bool) != Some(true))
    {
        return None;
    }
    let email = claims
        .get("email")
        .or_else(|| claims.pointer("/https:~1~1api.openai.com~1profile/email"))?
        .as_str()?
        .trim();
    if email.is_empty() || email.len() > 320 || email.chars().any(char::is_control) {
        return None;
    }
    Some(Identity {
        email: email.to_owned(),
        org_id: None,
        org_name: None,
    })
}

fn claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    parts.next()?;
    let payload = parts.next()?;
    parts.next()?;
    if parts.next().is_some() || payload.is_empty() || payload.len() > 16 * 1024 {
        return None;
    }
    let (mut bits, mut count) = (0_u32, 0_u32);
    let mut decoded = Vec::new();
    for byte in payload.bytes() {
        let n = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
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
    serde_json::from_slice(&decoded).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn encode(bytes: &[u8]) -> String {
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let (mut bits, mut count) = (0_u32, 0_u32);
        let mut out = String::new();
        for byte in bytes {
            bits = (bits << 8) | u32::from(*byte);
            count += 8;
            while count >= 6 {
                count -= 6;
                out.push(char::from(
                    *alphabet.get(((bits >> count) & 63) as usize).unwrap(),
                ));
            }
        }
        if count > 0 {
            out.push(char::from(
                *alphabet.get(((bits << (6 - count)) & 63) as usize).unwrap(),
            ));
        }
        out
    }
    fn credentials(provider: ProviderKind, claims: &Value) -> Vec<u8> {
        let token = format!(
            "e30.{}.synthetic",
            encode(&serde_json::to_vec(claims).unwrap())
        );
        serde_json::to_vec(&if provider == ProviderKind::Codex {
            serde_json::json!({"tokens":{"id_token":token}})
        } else {
            serde_json::json!({"token":{"id_token":token}})
        })
        .unwrap()
    }
    #[test]
    fn current_login_email_from_codex_and_agy_credentials() {
        let codex = credentials(
            ProviderKind::Codex,
            &serde_json::json!({"email":"codex@example.test","secret":"never returned"}),
        );
        let identity = from_credentials(ProviderKind::Codex, &codex).unwrap();
        assert_eq!(identity.email, "codex@example.test");
        assert_eq!(identity.org_id, None);
        let mut keyed: Value = serde_json::from_slice(&codex).unwrap();
        keyed["auth_mode"] = Value::String("apikey".into());
        assert!(
            from_credentials(ProviderKind::Codex, &serde_json::to_vec(&keyed).unwrap()).is_none()
        );
        for verified in [true, false] {
            let raw = credentials(
                ProviderKind::Antigravity,
                &serde_json::json!({"iss":"https://accounts.google.com","sub":"user","email_verified":verified,"email":"agy@example.test"}),
            );
            assert_eq!(
                from_credentials(ProviderKind::Antigravity, &raw).map(|v| v.email),
                verified.then(|| "agy@example.test".into())
            );
        }
        let raw = credentials(
            ProviderKind::Codex,
            &serde_json::json!({"https://api.openai.com/profile":{"email":"profile@example.test"}}),
        );
        assert_eq!(
            from_credentials(ProviderKind::Codex, &raw).unwrap().email,
            "profile@example.test"
        );
    }
    #[test]
    fn malformed_identity_is_unknown() {
        for raw in [
            b"bad json".as_slice(),
            br#"{"tokens":{"id_token":"bad"}}"#,
            br#"{"tokens":{"id_token":"a.!.b"}}"#,
        ] {
            assert!(from_credentials(ProviderKind::Codex, raw).is_none());
        }
        assert!(claims(&format!("a.{}.b", "A".repeat(16385))).is_none());
        for token in ["a.A.b", "a.Zh.b", "a.e30.b.extra", "a..b"] {
            assert!(claims(token).is_none());
        }
    }
    #[test]
    fn live_auth_identity_reaches_accounts_snapshot() {
        use super::super::{RefreshTrigger, host_default_dir, refresh_accounts_authenticated};
        use std::collections::BTreeMap;
        let root =
            std::env::temp_dir().join(format!("mobile-home-identity-{}", std::process::id()));
        let home = root.join("home");
        let state = root.join("state");
        std::fs::create_dir_all(&home).unwrap();
        let auth = [
            ProviderKind::Claude,
            ProviderKind::Codex,
            ProviderKind::Antigravity,
        ]
        .into_iter()
        .map(|provider| {
            (
                (provider, host_default_dir(&home, provider)),
                krill_proto::account_login::AuthStatus {
                    logged_in: true,
                    identity: Some(Identity {
                        email: format!("{}@example.test", provider.as_str()),
                        org_id: None,
                        org_name: None,
                    }),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
        let snapshot =
            refresh_accounts_authenticated(&home, Some(&state), RefreshTrigger::ReadOnly, &auth);
        for (key, value) in &auth {
            let a = snapshot
                .accounts
                .iter()
                .find(|a| a.provider == key.0 && a.host_default)
                .unwrap();
            assert_eq!(a.email.as_ref(), value.identity.as_ref().map(|v| &v.email));
            assert!(a.active);
        }
        // A cached identity from a logged-out CLI cannot become the current host identity.
        let mut auth = auth;
        for status in auth.values_mut() {
            status.logged_in = false;
        }
        let snapshot =
            refresh_accounts_authenticated(&home, Some(&state), RefreshTrigger::ReadOnly, &auth);
        assert!(
            snapshot
                .accounts
                .iter()
                .filter(|a| a.host_default)
                .all(|a| a.email.is_none())
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    proptest::proptest! {
        #[test]
        fn malformed_credentials_never_panic(raw in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)) {
            let _ = from_credentials(ProviderKind::Codex, &raw);
            let _ = from_credentials(ProviderKind::Antigravity, &raw);
            let _ = claims(&String::from_utf8_lossy(&raw));
        }
    }
}
