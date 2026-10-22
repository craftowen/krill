//! `krill account`: 프로바이더 다계정 조회·등록·삭제·원클릭 전환·자동전환 설정(MK-107·MK-108).

use std::ffi::OsString;
use std::io::Write as _;
use std::process::ExitCode;
use std::time::Duration;

use krill_proto::daemon::{self, Paths};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::screen::Msg as _;
use krill_proto::usage::{
    AccountsSnapshot, AutoSwitchMetric, AutoSwitchPolicy, AutoSwitchRule, FEAT_USAGE, ProviderKind,
    ResetConsumeOutcome, UsageReply, UsageRequest,
};

const USAGE: &str = "usage: krill account [list [--json] | history [--json] | direct-usage <on|off> | provisioning <on|off|status> [--json] | policy <predict|reset-first|cooldown> <on|off|minutes> | switch <provider> <id> | login <provider> <id> | add <provider> <label> [profile_dir] | rm <provider> <id> | consume-reset <provider> <id> [--json] | auto-switch <on|off> [threshold] | rule <provider> <from-id-or-label> <on|off> [threshold%] [max|5h|7d] [target-id-or-label|auto] | rule-rm <provider> <from-id-or-label>]";

pub fn run(raw: &[OsString]) -> ExitCode {
    // 서브커맨드 자리만 도움말로 본다. `add <label> help`의 `help`는 라벨이다.
    let help_here = matches!(
        raw.first().and_then(|a| a.to_str()),
        Some("-h" | "--help" | "help")
    ) || matches!(raw.get(1).and_then(|a| a.to_str()), Some("-h" | "--help"));
    if help_here {
        println!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let Ok(args) = raw
        .iter()
        .cloned()
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
    else {
        eprintln!("account: arguments must be UTF-8");
        return ExitCode::from(2);
    };
    let strs: Vec<&str> = args.iter().map(String::as_str).collect();
    if strs.first() == Some(&"provisioning") {
        return run_provisioning(&strs);
    }
    if matches!(strs.as_slice(), ["history"] | ["history", "--json"]) {
        return match request(&UsageRequest::ListAccounts).and_then(|snap| {
            if !snap.advanced.supported {
                return Err("daemon does not support account history; restart daemon".into());
            }
            let rendered = if strs.len() == 2 {
                serde_json::to_string_pretty(&snap.advanced.history).map_err(|e| e.to_string())?
            } else {
                format_history(&snap.advanced.history)
            };
            println!("{rendered}");
            Ok(())
        }) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("account: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if let ["login", provider, id] = strs.as_slice() {
        return match login(provider, id) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("account: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let consume = match strs.as_slice() {
        ["consume-reset", prov, id] => Some((*prov, *id, false)),
        ["consume-reset", prov, id, "--json"] => Some((*prov, *id, true)),
        _ => None,
    };
    if let Some((prov, id, json)) = consume {
        return run_consume_reset(prov, id, json);
    }
    let (req, json) = match strs.as_slice() {
        [] | ["list" | "ls"] => (UsageRequest::ListAccounts, false),
        ["list" | "ls", "--json"] => (UsageRequest::ListAccounts, true),
        ["direct-usage", value @ ("on" | "off")] => {
            (UsageRequest::SetDirectUsage(*value == "on"), false)
        }
        ["policy", key, value] => {
            let parsed = request(&UsageRequest::ListAccounts)
                .and_then(|snap| parse_advanced_policy(key, value, snap.advanced.policy));
            match parsed {
                Ok(req) => (req, false),
                Err(e) => {
                    eprintln!("account: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        ["rule" | "rule-rm", ..] => {
            let parsed = request(&UsageRequest::ListAccounts)
                .and_then(|snap| parse_rule_command(&strs, &snap));
            match parsed {
                Ok(req) => (req, false),
                Err(e) => {
                    eprintln!("account: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        ["switch", prov, id] => {
            let Some(provider) = ProviderKind::parse(prov) else {
                eprintln!(
                    "account: unknown provider '{prov}' (expected claude|codex|agy|opencode)"
                );
                return ExitCode::from(2);
            };
            (
                UsageRequest::SwitchAccount {
                    provider,
                    account_id: (*id).to_owned(),
                },
                false,
            )
        }
        ["add", prov, label] => {
            let Some(provider) = ProviderKind::parse(prov) else {
                eprintln!(
                    "account: unknown provider '{prov}' (expected claude|codex|agy|opencode)"
                );
                return ExitCode::from(2);
            };
            (
                UsageRequest::AddAccount {
                    provider,
                    label: (*label).to_owned(),
                    email: None,
                    profile_dir: String::new(),
                },
                false,
            )
        }
        ["add", prov, label, dir] => {
            let Some(provider) = ProviderKind::parse(prov) else {
                eprintln!(
                    "account: unknown provider '{prov}' (expected claude|codex|agy|opencode)"
                );
                return ExitCode::from(2);
            };
            (
                UsageRequest::AddAccount {
                    provider,
                    label: (*label).to_owned(),
                    email: None,
                    profile_dir: (*dir).to_owned(),
                },
                false,
            )
        }
        ["rm" | "remove", prov, id] => {
            let Some(provider) = ProviderKind::parse(prov) else {
                eprintln!(
                    "account: unknown provider '{prov}' (expected claude|codex|agy|opencode)"
                );
                return ExitCode::from(2);
            };
            (
                UsageRequest::RemoveAccount {
                    provider,
                    account_id: (*id).to_owned(),
                },
                false,
            )
        }
        ["auto-switch", ..] => {
            let snap = match request(&UsageRequest::ListAccounts) {
                Ok(snap) => snap,
                Err(e) => {
                    eprintln!("account: {e}");
                    return ExitCode::from(2);
                }
            };
            match parse_auto_switch_command(&strs, &snap) {
                Ok(req) => (req, false),
                Err(e) => {
                    eprintln!("account: {e}");
                    return ExitCode::from(2);
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match request(&req) {
        Ok(snap) => {
            if json {
                match snapshot_to_json(&snap) {
                    Ok(rendered) => println!("{rendered}"),
                    Err(e) => {
                        eprintln!("account: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print_snapshot(&snap);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("account: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 현재 터미널에서 등록 계정의 CLI 로그인을 실행한다. 활성 계정은 바꾸지 않는다.
fn login(provider: &str, id: &str) -> Result<(), String> {
    let provider = ProviderKind::parse(provider).ok_or("unknown provider")?;
    let snap = request(&UsageRequest::ListAccounts)?;
    let id = resolve_account(&snap, provider, id)?;
    let account = snap
        .accounts
        .iter()
        .find(|a| a.id == id)
        .ok_or("account not found")?;
    // 호스트 기본 계정은 env 없이 다룬다(Claude는 env가 있으면 다른 키체인 항목을 쓴다).
    let dir = (!account.host_default).then(|| std::path::Path::new(&account.profile_dir));
    let path = std::env::var_os("PATH").unwrap_or_default();
    let status = login_command(provider, dir, &path)?
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err("login failed or was cancelled".to_owned());
    }
    let exe = krill_proto::agent::which(provider.as_str(), &path)
        .ok_or("provider executable not found")?;
    let auth = if provider == ProviderKind::Antigravity && account.host_default {
        std::fs::write(
            std::path::Path::new(&account.profile_dir).join(".krill-login-completed"),
            b"",
        )
        .ok();
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        loop {
            if request(&UsageRequest::ListAccounts).is_ok_and(|s| {
                s.connection_for(&account.id) == krill_proto::usage::ConnectionStatus::Connected
            }) {
                break Some(krill_proto::account_login::AuthStatus {
                    logged_in: true,
                    identity: None,
                });
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    } else {
        krill_proto::account_login::probe(&exe, provider, dir)
    }
    .filter(|s| s.logged_in)
    .ok_or("login could not be verified")?;
    if let Some(dir) = dir {
        std::fs::write(dir.join(".krill-login-completed"), b"").ok();
    }
    if let Some(identity) = auth.identity {
        let label = if account
            .label
            .strip_prefix(provider.display_name())
            .and_then(|s| s.strip_prefix(" Profile #"))
            .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        {
            identity.org_name.unwrap_or_else(|| identity.email.clone())
        } else {
            account.label.clone()
        };
        // N-1 데몬은 메타데이터 변경 확장이 없을 수 있다. 인증 성공은 그대로 인정한다.
        request(&UsageRequest::RenameAccount {
            provider,
            account_id: account.id.clone(),
            label,
            email: Some(identity.email.clone()),
        })
        .ok();
        println!("{}", identity.email);
    }
    println!("account: login verified");
    Ok(())
}

fn login_command(
    provider: ProviderKind,
    dir: Option<&std::path::Path>,
    path: &std::ffi::OsStr,
) -> Result<std::process::Command, String> {
    let exe = krill_proto::agent::which(provider.as_str(), path)
        .ok_or("provider executable not found")?;
    krill_proto::account_login::login_command(&exe, provider, dir)
        .ok_or_else(|| "provider does not support interactive account login".to_owned())
}

/// OWE-709: `auto-switch on|off [threshold]`를 [`UsageRequest::SetAutoSwitch`]로 푼다.
/// on/off만으로는 기존 임계치·한도·규칙을 그대로 두어야 한다(덮어쓰기 금지).
fn parse_auto_switch_command(
    args: &[&str],
    snap: &AccountsSnapshot,
) -> Result<UsageRequest, String> {
    let (state, rest) = match args {
        ["auto-switch", state, rest @ ..] => (*state, rest),
        _ => return Err(USAGE.to_owned()),
    };
    let enabled = match state {
        "on" | "enable" | "true" | "1" => true,
        "off" | "disable" | "false" | "0" => false,
        _ => return Err(USAGE.to_owned()),
    };
    if rest.len() > 1 {
        return Err(USAGE.to_owned());
    }
    // 임계치를 함께 주면 그 값만 바꾸고, 생략하면 기존 값을 보존한다.
    // 공유 상수 범위로 자른다(OWE-673·675 유지).
    let threshold_percent = match rest.first() {
        None => snap.auto_switch.threshold_percent.clamp(
            krill_proto::usage::AUTO_SWITCH_THRESHOLD_MIN,
            krill_proto::usage::AUTO_SWITCH_THRESHOLD_MAX,
        ),
        Some(s) => s
            .trim_end_matches('%')
            .parse::<u8>()
            .map_err(|_| format!("invalid threshold: {s}"))?
            .clamp(
                krill_proto::usage::AUTO_SWITCH_THRESHOLD_MIN,
                krill_proto::usage::AUTO_SWITCH_THRESHOLD_MAX,
            ),
    };
    Ok(UsageRequest::SetAutoSwitch(AutoSwitchPolicy {
        enabled,
        threshold_percent,
        switch_on_rate_limit: snap.auto_switch.switch_on_rate_limit,
        rules: snap.auto_switch.rules.clone(),
    }))
}

fn resolve_account(
    snap: &AccountsSnapshot,
    provider: ProviderKind,
    value: &str,
) -> Result<String, String> {
    if let Some(account) = snap
        .accounts
        .iter()
        .find(|a| a.provider == provider && a.id == value)
    {
        return Ok(account.id.clone());
    }
    let mut matching = snap
        .accounts
        .iter()
        .filter(|a| a.provider == provider && a.label.eq_ignore_ascii_case(value));
    let account = matching
        .next()
        .ok_or_else(|| format!("account not found: {value}"))?;
    if matching.next().is_some() {
        return Err(format!(
            "ambiguous account label: {value}; use an account id"
        ));
    }
    Ok(account.id.clone())
}

fn parse_rule_command(args: &[&str], snap: &AccountsSnapshot) -> Result<UsageRequest, String> {
    let (command, provider, from, rest) = match args {
        [command @ ("rule" | "rule-rm"), provider, from, rest @ ..] => {
            (*command, *provider, *from, rest)
        }
        _ => return Err(USAGE.to_owned()),
    };
    let provider =
        ProviderKind::parse(provider).ok_or_else(|| format!("unknown provider: {provider}"))?;
    let from_account_id = resolve_account(snap, provider, from)?;
    if command == "rule-rm" {
        return if rest.is_empty() {
            Ok(UsageRequest::RemoveAutoSwitchRule {
                provider,
                from_account_id,
            })
        } else {
            Err(USAGE.to_owned())
        };
    }
    let (state, options) = rest.split_first().ok_or_else(|| USAGE.to_owned())?;
    if options.len() > 3 {
        return Err(USAGE.to_owned());
    }
    let enabled = match *state {
        "on" => true,
        "off" => false,
        _ => return Err("rule state must be on or off".to_owned()),
    };
    let threshold_percent = options
        .first()
        .map_or(Ok(90), |s| {
            s.trim_end_matches('%')
                .parse::<u8>()
                .map_err(|_| format!("invalid threshold: {s}"))
        })?
        .clamp(
            krill_proto::usage::AUTO_SWITCH_THRESHOLD_MIN,
            krill_proto::usage::AUTO_SWITCH_THRESHOLD_MAX,
        );
    let metric = options.get(1).map_or(Ok(AutoSwitchMetric::Max), |s| {
        AutoSwitchMetric::parse(s).ok_or_else(|| format!("invalid metric: {s}; expected max|5h|7d"))
    })?;
    let target_account_id = match options.get(2) {
        None | Some(&"auto") => None,
        Some(target) => Some(resolve_account(snap, provider, target)?),
    };
    if target_account_id.as_deref() == Some(from_account_id.as_str()) {
        return Err("target account must differ from source account".to_owned());
    }
    Ok(UsageRequest::UpsertAutoSwitchRule(AutoSwitchRule {
        provider,
        from_account_id,
        enabled,
        threshold_percent,
        metric,
        target_account_id,
    }))
}

/// OWE-722: 설정의 유일한 쓰기 주체는 krilld다(AS-03).
fn run_provisioning(args: &[&str]) -> ExitCode {
    let (action, json) = match args {
        ["provisioning", action @ ("on" | "off" | "status")] => (*action, false),
        ["provisioning", action @ ("on" | "off" | "status"), "--json"] => (*action, true),
        _ => {
            eprintln!("usage: krill account provisioning <on|off|status> [--json]");
            return ExitCode::from(2);
        }
    };
    let req = match action {
        "on" => UsageRequest::SetProvisioning(true),
        "off" => UsageRequest::SetProvisioning(false),
        _ => UsageRequest::GetProvisioning,
    };
    match exchange(&req) {
        Ok(UsageReply::Provisioning(enabled)) => {
            if json {
                println!("{}", serde_json::json!({ "provisioning": enabled }));
            } else {
                println!("provisioning: {}", if enabled { "on" } else { "off" });
            }
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!("account: unexpected provisioning reply");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("account: {e}");
            ExitCode::FAILURE
        }
    }
}

fn request(req: &UsageRequest) -> Result<AccountsSnapshot, String> {
    match exchange(req)? {
        UsageReply::Accounts(snap) => Ok(snap),
        UsageReply::Current(_) | UsageReply::Orphans(_) => Ok(AccountsSnapshot::default()),
        UsageReply::ResetConsumed { snapshot, .. } => Ok(snapshot),
        UsageReply::Provisioning(_) => Err("unexpected account reply".to_owned()),
        UsageReply::Failed(e) => Err(e.to_string()),
    }
}

/// 리셋 크레딧 1개를 소진한다. 소진 결과와 소진 직후 계정 스냅샷을 함께 보여준다
/// (데몬 `ConsumeResetCredit` 패리티). Codex banked reset와 Claude 주간 세션 리셋·프로모션 grant를 지원한다.
fn run_consume_reset(provider: &str, id: &str, json: bool) -> ExitCode {
    let Some(provider) = ProviderKind::parse(provider) else {
        eprintln!("account: unknown provider '{provider}' (expected claude|codex|agy|opencode)");
        return ExitCode::from(2);
    };
    if provider != ProviderKind::Codex && provider != ProviderKind::Claude {
        eprintln!("account: reset credits are only available for codex and claude");
        return ExitCode::from(2);
    }
    let req = UsageRequest::ConsumeResetCredit {
        provider,
        account_id: id.to_owned(),
    };
    let outcome_label = |outcome: ResetConsumeOutcome| match outcome {
        ResetConsumeOutcome::Reset => "reset",
        ResetConsumeOutcome::NothingToReset => "nothing_to_reset",
        ResetConsumeOutcome::NoCredit => "no_credit",
        ResetConsumeOutcome::AlreadyRedeemed => "already_redeemed",
    };
    match exchange(&req) {
        Ok(UsageReply::ResetConsumed {
            snapshot,
            outcome,
            windows_reset,
        }) => {
            if json {
                let accounts = serde_json::to_value(&snapshot).unwrap_or(serde_json::Value::Null);
                println!(
                    "{}",
                    serde_json::json!({
                        "outcome": outcome_label(outcome),
                        "windows_reset": windows_reset,
                        "accounts": accounts,
                    })
                );
            } else {
                println!(
                    "reset credit: {} (windows reset: {windows_reset})",
                    outcome_label(outcome)
                );
                print_snapshot(&snapshot);
            }
            ExitCode::SUCCESS
        }
        Ok(_) => {
            eprintln!("account: unexpected reset reply");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("account: {e}");
            ExitCode::FAILURE
        }
    }
}

fn exchange(req: &UsageRequest) -> Result<UsageReply, String> {
    let paths = Paths::dev().map_err(|e| e.to_string())?;
    let mut hello = daemon::client_hello(ClientRole::Cli, "krill account");
    hello.features = FEAT_USAGE
        | krill_proto::usage::FEAT_PROVIDER_DETAILS
        | krill_proto::usage::FEAT_USAGE_AGY
        | krill_proto::usage::FEAT_USAGE_RULES
        | krill_proto::usage::FEAT_USAGE_STATUS
        | krill_proto::usage::FEAT_USAGE_RATELIMIT
        | krill_proto::usage::FEAT_USAGE_MANAGE
        | krill_proto::usage::FEAT_ACCOUNT_CONNECTION
        | krill_proto::usage::FEAT_ACCOUNT_POLICY
        | krill_proto::usage::FEAT_USAGE_RESET;
    let mut conn = if matches!(
        req,
        UsageRequest::SetProvisioning(_) | UsageRequest::GetProvisioning
    ) {
        // 데몬이 없으면 오류다. 로컬 파일 쓰기나 데몬 자동 기동으로 우회하지 않는다.
        daemon::handshake(&paths.socket, &hello, Duration::from_secs(30))
            .map_err(|e| format!("cannot talk to krilld: {e}"))?
    } else {
        daemon::connect(&paths, &hello).map_err(|e| e.to_string())?
    };
    if conn.negotiated.features & FEAT_USAGE == 0 {
        return Err("krilld does not support FEAT_USAGE; restart daemon".to_owned());
    }
    let required = req.required_features()
        | if matches!(req, UsageRequest::SwitchAccount { .. }) {
            krill_proto::usage::FEAT_PROVIDER_DETAILS
        } else {
            0
        };
    if conn.negotiated.features & required != required {
        return Err("daemon does not support this account extension; restart daemon".to_owned());
    }
    let timeout = Some(Duration::from_secs(15));
    conn.stream
        .set_read_timeout(timeout)
        .map_err(|e| e.to_string())?;
    conn.stream
        .set_write_timeout(timeout)
        .map_err(|e| e.to_string())?;
    conn.stream
        .write_all(&req.encode().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let (kind, body) = ipc::read_frame(&mut conn.stream)
        .map_err(|e| e.to_string())?
        .ok_or("krilld disconnected")?;
    match UsageReply::decode(kind, &body).map_err(|e| e.to_string())? {
        UsageReply::Failed(e) => Err(e.to_string()),
        reply => Ok(reply),
    }
}

/// OWE-709: `list --json` 출력. 스냅샷 그대로 직렬화한다.
fn snapshot_to_json(snap: &AccountsSnapshot) -> Result<String, String> {
    serde_json::to_string_pretty(snap).map_err(|e| e.to_string())
}

fn format_window(w: &krill_proto::usage::Window) -> String {
    match w.resets_at.as_deref().map(str::trim) {
        Some(r) if !r.is_empty() && r != "0" => format!("{}% (resets_at={r})", w.percent),
        _ => format!("{}%", w.percent),
    }
}

fn print_snapshot(snap: &AccountsSnapshot) {
    println!(
        "auto-switch: {} (threshold: {}%, rate-limit switch: {})",
        if snap.auto_switch.enabled {
            "ON"
        } else {
            "OFF"
        },
        snap.auto_switch.threshold_percent,
        if snap.auto_switch.switch_on_rate_limit {
            "ON"
        } else {
            "OFF"
        }
    );
    for rule in &snap.auto_switch.rules {
        println!(
            "rule: [{}] {} {} at {}% ({}) -> {}",
            rule.provider.as_str(),
            rule.from_account_id,
            if rule.enabled { "ON" } else { "OFF" },
            rule.threshold_percent,
            rule.metric.short_code(),
            rule.target_account_id.as_deref().unwrap_or("auto")
        );
    }
    for ev in &snap.switched {
        println!(
            "switched:    [{}] {} -> {} (at {}%)",
            ev.provider.as_str(),
            ev.from_label,
            ev.to_label,
            ev.reason_percent
        );
    }
    for a in &snap.accounts {
        let mark = if a.active { "*" } else { " " };
        let h5 = a
            .usage
            .five_hour
            .as_ref()
            .map_or_else(|| "-".to_owned(), format_window);
        let d7 = a
            .usage
            .seven_day
            .as_ref()
            .map_or_else(|| "-".to_owned(), format_window);
        println!(
            "{mark} {:<8} {:<22} {:<18} 5h:{:<5} 7d:{:<5} {} [{}]",
            a.provider.as_str(),
            a.id,
            a.label,
            h5,
            d7,
            a.profile_dir,
            snap.connection_for(&a.id).label_ko()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::usage::{AccountInfo, Snapshot};

    #[test]
    fn help_returns_zero() {
        assert_eq!(run(&[OsString::from("--help")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("-h")]), ExitCode::SUCCESS);
        assert_eq!(run(&[OsString::from("help")]), ExitCode::SUCCESS);
    }

    #[test]
    #[cfg(unix)]
    fn login_command_runs_fake_claude_in_selected_profile() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("owe700-cli-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let profile = root.join("profile with spaces");
        std::fs::create_dir_all(&profile).unwrap();
        let exe = root.join("claude");
        std::fs::write(&exe, "#!/bin/sh\n[ \"$1 $2 $3\" = 'auth login --claudeai' ] || exit 8\nprintf '%s' \"$HOME\" > \"$CLAUDE_CONFIG_DIR/login-called\"\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let status = login_command(ProviderKind::Claude, Some(&profile), root.as_os_str())
            .unwrap()
            .env("HOME", &root)
            .env("PATH", &root)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            std::fs::read_to_string(profile.join("login-called")).unwrap(),
            root.display().to_string()
        );
        assert!(!root.join("wrong/login-called").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn accounts() -> AccountsSnapshot {
        AccountsSnapshot {
            accounts: [
                (ProviderKind::Claude, "claude-host", "Personal"),
                (ProviderKind::Claude, "claude-work", "Work Max"),
                (ProviderKind::Codex, "codex-host", "Personal"),
                (ProviderKind::Antigravity, "agy-host", "Personal"),
                (ProviderKind::OpenCode, "opencode-host", "Personal"),
            ]
            .into_iter()
            .map(|(provider, id, label)| AccountInfo {
                provider,
                id: id.into(),
                label: label.into(),
                email: None,
                profile_dir: String::new(),
                active: false,
                host_default: false,
                stale: false,
                usage: Snapshot::default(),
            })
            .collect(),
            ..AccountsSnapshot::default()
        }
    }

    #[test]
    fn rule_command_resolves_ids_and_labels_and_parses_options() {
        let snap = accounts();
        assert_eq!(
            parse_rule_command(
                &["rule", "claude", "Personal", "on", "85%", "7d", "work max"],
                &snap
            )
            .unwrap(),
            UsageRequest::UpsertAutoSwitchRule(AutoSwitchRule {
                provider: ProviderKind::Claude,
                from_account_id: "claude-host".into(),
                enabled: true,
                threshold_percent: 85,
                metric: AutoSwitchMetric::SevenDay,
                target_account_id: Some("claude-work".into()),
            })
        );
        for provider in ["claude", "codex", "agy", "opencode"] {
            let UsageRequest::UpsertAutoSwitchRule(rule) =
                parse_rule_command(&["rule", provider, "Personal", "off"], &snap).unwrap()
            else {
                panic!("expected rule");
            };
            assert!(!rule.enabled);
            assert_eq!(rule.threshold_percent, 90);
            assert_eq!(rule.metric, AutoSwitchMetric::Max);
            assert_eq!(rule.target_account_id, None);
        }
        let UsageRequest::UpsertAutoSwitchRule(rule) = parse_rule_command(
            &["rule", "claude", "claude-host", "on", "100", "5h", "auto"],
            &snap,
        )
        .unwrap() else {
            panic!("expected rule");
        };
        // 규칙 임계값은 문서·데몬과 같은 1~100(K-27): 100은 그대로, 30은 50으로 올리지 않는다.
        assert_eq!(rule.threshold_percent, 100);
        assert_eq!(rule.metric, AutoSwitchMetric::FiveHour);
        let UsageRequest::UpsertAutoSwitchRule(rule) =
            parse_rule_command(&["rule", "claude", "claude-host", "on", "30"], &snap).unwrap()
        else {
            panic!("expected rule");
        };
        assert_eq!(rule.threshold_percent, 30);
        assert_eq!(
            parse_rule_command(&["rule-rm", "claude", "Work Max"], &snap).unwrap(),
            UsageRequest::RemoveAutoSwitchRule {
                provider: ProviderKind::Claude,
                from_account_id: "claude-work".into()
            }
        );
    }

    #[test]
    fn rule_command_rejects_invalid_or_ambiguous_arguments() {
        let mut snap = accounts();
        for args in [
            vec!["rule"],
            vec!["rule", "unknown", "Personal", "on"],
            vec!["rule", "claude", "missing", "on"],
            vec!["rule", "claude", "Personal", "maybe"],
            vec!["rule", "claude", "Personal", "on", "abc"],
            vec!["rule", "claude", "Personal", "on", "90", "daily"],
            vec!["rule", "claude", "Personal", "on", "90", "max", "missing"],
            vec!["rule", "claude", "Personal", "on", "90", "max", "Personal"],
            vec![
                "rule", "claude", "Personal", "on", "90", "max", "auto", "extra",
            ],
            vec!["rule-rm", "claude", "Personal", "extra"],
        ] {
            assert!(parse_rule_command(&args, &snap).is_err(), "{args:?}");
        }
        snap.accounts[1].label = "Personal".into();
        assert!(parse_rule_command(&["rule-rm", "claude", "Personal"], &snap).is_err());
        assert!(parse_rule_command(&["rule-rm", "claude", "claude-host"], &snap).is_ok());
    }
    #[test]
    fn usage_window_includes_only_known_reset_times() {
        for (reset, expected) in [
            (
                Some("2026-10-16T12:00:00Z"),
                "42% (resets_at=2026-10-16T12:00:00Z)",
            ),
            (None, "42%"),
            (Some("0"), "42%"),
            (Some("  "), "42%"),
        ] {
            let window = krill_proto::usage::Window {
                percent: 42,
                resets_at: reset.map(str::to_owned),
            };
            assert_eq!(format_window(&window), expected);
        }
    }

    fn customized_snapshot() -> AccountsSnapshot {
        let mut snap = accounts();
        snap.auto_switch.enabled = false;
        snap.auto_switch.threshold_percent = 77;
        snap.auto_switch.switch_on_rate_limit = false;
        snap.auto_switch.rules.push(AutoSwitchRule {
            provider: ProviderKind::Claude,
            from_account_id: "claude-host".into(),
            enabled: true,
            threshold_percent: 60,
            metric: AutoSwitchMetric::SevenDay,
            target_account_id: Some("claude-work".into()),
        });
        snap
    }

    /// OWE-709: on/off만으로는 기존 임계치·한도·규칙을 덮어쓰지 않는다.
    #[test]
    fn auto_switch_on_off_preserves_threshold_limit_and_rules() {
        let snap = customized_snapshot();
        for (state, enabled) in [("on", true), ("off", false)] {
            let UsageRequest::SetAutoSwitch(policy) =
                parse_auto_switch_command(&["auto-switch", state], &snap).unwrap()
            else {
                panic!("expected SetAutoSwitch");
            };
            assert_eq!(policy.enabled, enabled);
            assert_eq!(policy.threshold_percent, 77);
            assert!(!policy.switch_on_rate_limit);
            assert_eq!(policy.rules, snap.auto_switch.rules);
        }
        // 임계치를 함께 주면 그 값만 바뀐다(공유 상수 범위로 자름).
        let UsageRequest::SetAutoSwitch(policy) =
            parse_auto_switch_command(&["auto-switch", "on", "85%"], &snap).unwrap()
        else {
            panic!("expected SetAutoSwitch");
        };
        assert!(policy.enabled);
        assert_eq!(policy.threshold_percent, 85);
        assert!(!policy.switch_on_rate_limit);
        assert_eq!(policy.rules, snap.auto_switch.rules);
    }

    #[test]
    fn auto_switch_command_rejects_invalid_arguments() {
        let snap = customized_snapshot();
        for args in [
            vec!["auto-switch"],
            vec!["auto-switch", "maybe"],
            vec!["auto-switch", "on", "abc"],
            vec!["auto-switch", "on", "90", "extra"],
            vec!["auto-switch", "off", "75%", "extra"],
        ] {
            assert!(parse_auto_switch_command(&args, &snap).is_err(), "{args:?}");
        }
    }

    /// OWE-709: `--json` 출력은 스냅샷과 같은 내용이다.
    #[test]
    fn list_json_roundtrips_snapshot() {
        let snap = customized_snapshot();
        let rendered = snapshot_to_json(&snap).unwrap();
        let back: AccountsSnapshot = serde_json::from_str(&rendered).unwrap();
        assert_eq!(back, snap);
    }
}

fn parse_advanced_policy(
    key: &str,
    value: &str,
    mut policy: krill_proto::usage::AdvancedPolicy,
) -> Result<UsageRequest, String> {
    match (key, value) {
        ("predict", "on" | "off") => policy.predict_exhaustion = value == "on",
        ("reset-first", "on" | "off") => policy.prefer_soonest_reset = value == "on",
        ("cooldown", value) => {
            policy.cooldown_secs = value
                .parse::<u64>()
                .ok()
                .and_then(|v| v.checked_mul(60))
                .ok_or("cooldown expects minutes")?;
        }
        _ => return Err("policy expects predict/reset-first on|off, or cooldown minutes".into()),
    }
    Ok(UsageRequest::SetAdvancedPolicy(policy))
}

fn format_history(history: &[krill_proto::usage::SwitchRecord]) -> String {
    if history.is_empty() {
        return "자동전환 이력 없음".into();
    }
    history
        .iter()
        .map(|r| {
            format!(
                "{} [{}] {} -> {} ({}%, {})",
                r.at,
                r.event.provider.as_str(),
                r.event.from_label,
                r.event.to_label,
                r.event.reason_percent,
                if r.predicted {
                    "소진 예측"
                } else {
                    "임계치/한도"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    #[test]
    fn advanced_policy_changes_only_requested_setting_and_rejects_overflow() {
        let old = krill_proto::usage::AdvancedPolicy {
            predict_exhaustion: true,
            prefer_soonest_reset: true,
            cooldown_secs: 900,
        };
        let UsageRequest::SetAdvancedPolicy(p) =
            parse_advanced_policy("predict", "off", old.clone()).unwrap()
        else {
            panic!("policy")
        };
        assert!(!p.predict_exhaustion);
        assert!(p.prefer_soonest_reset);
        assert_eq!(p.cooldown_secs, 900);
        let UsageRequest::SetAdvancedPolicy(p) =
            parse_advanced_policy("cooldown", "5", old.clone()).unwrap()
        else {
            panic!("policy")
        };
        assert_eq!(p.cooldown_secs, 300);
        assert!(p.predict_exhaustion);
        assert!(parse_advanced_policy("cooldown", &u64::MAX.to_string(), old.clone()).is_err());
        assert!(parse_advanced_policy("predict", "maybe", old).is_err());
        assert!(format_history(&[]).contains("없음"));
    }
}
