#![cfg(unix)]
//! `.ips` 수집기 테스트. 실제 진단 보고서 디렉터리나 고객 보고서는 열지 않는다: 가짜 보고서를 임시 디렉터리에
//! 쓰고 [`collect_with`]에 그 디렉터리를 넘긴다.

use super::*;
use crate::error_reporting::runs::tests::{Scratch, consented, plant_marker, queued};
use crate::error_reporting::{Kind, consent, dir, event_id};
use std::fs::File;
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

const INCIDENT: &str = "8F8E5B1A-0C1D-4E2F-9A3B-1C2D3E4F5A6B";
const CAPTURE: &str = "2099-01-02 03:04:05.6789 +0900";
const LAUNCH: &str = "2099-01-02 03:00:00.1234 +0900";
const NAME: &str = "krilld-2099-01-02-030405.ips";
const THIRD_PARTY_UUID: &str = "DEADBEEF-0000-1111-2222-333344445555";

/// 가짜 309 보고서(두 JSON 문서). 실제 형식처럼 경로·심볼·서드파티 이미지·메시지 필드를 넣어 읽지 않음을 확인한다.
#[derive(Debug, Clone)]
struct Fixture {
    bug_type: String,
    body_bug_type: String,
    header_extra: String,
    proc_name: String,
    signing_id: String,
    capture: String,
    launch: Option<String>,
    pid: u32,
    incident: String,
    body_incident: Option<String>,
    exception: String,
    signal: Option<String>,
    exception_extra: String,
    bundle: Option<(String, String)>,
    body_extra: String,
}

impl Fixture {
    fn daemon() -> Self {
        Self {
            bug_type: "\"309\"".to_owned(),
            body_bug_type: "\"309\"".to_owned(),
            header_extra: String::new(),
            proc_name: "krilld".to_owned(),
            signing_id: "dev.krill.app.krilld".to_owned(),
            capture: CAPTURE.to_owned(),
            launch: Some(LAUNCH.to_owned()),
            pid: 4242,
            incident: INCIDENT.to_owned(),
            body_incident: Some(INCIDENT.to_owned()),
            exception: "EXC_BAD_ACCESS".to_owned(),
            signal: Some("SIGSEGV".to_owned()),
            exception_extra: String::new(),
            bundle: Some(("dev.krill.app".to_owned(), "0.1.99".to_owned())),
            body_extra: String::new(),
        }
    }

    fn header(&self) -> String {
        format!(
            "{{\"app_name\":\"{name}\",\"timestamp\":\"2099-01-02 03:04:06.00 +0900\",\"app_version\":\"\",\
             \"slice_uuid\":\"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\",\"build_version\":\"\",\"platform\":1,\
             \"share_with_app_devs\":0,\"is_first_party\":0,\"bug_type\":{bug},\
             \"os_version\":\"macOS 26.0 (25A1)\",\"roots_installed\":0,\"name\":\"{name}\",\
             \"incident_id\":\"{incident}\"{extra}}}",
            name = self.proc_name,
            bug = self.bug_type,
            incident = self.incident,
            extra = self.header_extra,
        )
    }

    fn body(&self) -> String {
        let launch = self
            .launch
            .as_ref()
            .map(|at| format!("  \"procLaunch\" : \"{at}\",\n"))
            .unwrap_or_default();
        let incident = self
            .body_incident
            .as_ref()
            .map(|id| format!("  \"incident\" : \"{id}\",\n"))
            .unwrap_or_default();
        let signal = self
            .signal
            .as_ref()
            .map(|name| format!(",\"signal\":\"{name}\""))
            .unwrap_or_default();
        let bundle = self
            .bundle
            .as_ref()
            .map(|(id, version)| {
                format!(
                    "  \"bundleInfo\" : {{\"CFBundleShortVersionString\":\"{version}\",\
                     \"CFBundleVersion\":\"{version}\",\"CFBundleIdentifier\":\"{id}\"}},\n"
                )
            })
            .unwrap_or_default();
        format!(
            r#"{{
  "uptime" : 1000,
  "procRole" : "Unspecified",
  "version" : 2,
  "userID" : 501,
  "deployVersion" : 210,
  "modelCode" : "Mac14,3",
  "coalitionID" : 1234,
  "osVersion" : {{"train" : "macOS 26.0", "build" : "25A1", "releaseType" : "User"}},
  "captureTime" : "{capture}",
  "codeSigningMonitor" : 1,
{incident}  "pid" : {pid},
  "translated" : false,
  "cpuType" : "ARM-64",
  "roots_installed" : 0,
  "bug_type" : {body_bug},
{launch}  "procStartAbsTime" : 1,
  "procExitAbsTime" : 2,
  "procName" : "{name}",
  "procPath" : "\/Users\/USER\/Applications\/Krill.app\/Contents\/Helpers\/{name}",
{bundle}  "parentProc" : "launchd",
  "parentPid" : 1,
  "coalitionName" : "dev.krill.app",
  "crashReporterKey" : "SECRET-REPORTER-KEY",
  "codeSigningID" : "{signing}",
  "codeSigningTeamID" : "",
  "sip" : "enabled",
  "vmRegionInfo" : "0 is not in any region. \/Users\/someone\/private",
  "exception" : {{"codes":"0x0000000000000001, 0x0000000000000000","rawCodes":[1,0],"type":"{exception}"{signal},"subtype":"KERN_INVALID_ADDRESS at 0x0000000000000000"{exception_extra}}},
  "termination" : {{"flags":0,"code":11,"namespace":"SIGNAL","indicator":"Segmentation fault: 11","byProc":"exc handler","byPid":1}},
  "asi" : {{"libsystem_c.dylib":["abort() called: secret prompt text"]}},
  "faultingThread" : 0,
  "threads" : [{{"triggered":true,"id":1,"threadState":{{"x":[{{"value":1}}]}},"queue":"com.apple.main-thread","frames":[{{"imageOffset":1234,"symbol":"secret_symbol","imageIndex":0}}]}}],
  "usedImages" : [{{"source":"P","arch":"arm64","base":4294967296,"size":1000,"uuid":"{third}","path":"\/Library\/ThirdParty\/Agent.dylib","name":"Agent.dylib"}}],
  "sharedCache" : {{"base":1,"size":2,"uuid":"aaaa"}},
  "vmSummary" : "secret vm summary",
  "legacyInfo" : {{"threadTriggered":{{"queue":"com.apple.main-thread"}}}},
  "logWritingSignature" : "abc",
  "trialInfo" : {{"rollouts":[],"experiments":[]}}{body_extra}
}}"#,
            capture = self.capture,
            pid = self.pid,
            body_bug = self.body_bug_type,
            name = self.proc_name,
            signing = self.signing_id,
            exception = self.exception,
            exception_extra = self.exception_extra,
            third = THIRD_PARTY_UUID,
            body_extra = self.body_extra,
        )
    }

    fn text(&self) -> String {
        format!("{}\n{}", self.header(), self.body())
    }

    fn parse(&self) -> Option<Report> {
        Report::parse(self.text().as_bytes())
    }
}

fn capture_ms() -> u64 {
    ips_time_ms(CAPTURE).unwrap()
}

/// 보고서보다 한 시간 뒤(수집 시각).
fn an_hour_later() -> u64 {
    capture_ms().saturating_add(3_600_000)
}

fn write_report(dir: &Path, name: &str, text: &str) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    path
}

/// 동의 직후 쓰는 보고서의 생성 시각이 동의 시각(밀리초)과 겹치지 않게 잠깐 기다린다.
fn after_consent() {
    std::thread::sleep(Duration::from_millis(20));
}

#[test]
fn parses_a_krill_crash_fixture() {
    let report = Fixture::daemon().parse().unwrap();
    let utc = crate::time::parse_rfc3339_epoch("2099-01-01T18:04:05Z").unwrap();
    assert_eq!(
        report,
        Report {
            process: Process::Daemon,
            incident: INCIDENT.to_owned(),
            exception: Exception::BadAccess,
            signal: Signal::Segv,
            capture_ms: u64::try_from(utc).unwrap().checked_mul(1000).unwrap(),
            launch_ms: ips_time_ms(LAUNCH),
            pid: Some(4242),
            release: Some("0.1.99".to_owned()),
        }
    );
}

#[test]
fn typed_parse_keeps_no_free_text_and_the_event_carries_only_enums() {
    let report = Fixture::daemon().parse().unwrap();
    let id = event_id(NATIVE_DOMAIN, report.incident.as_bytes());
    let event = report.event(id.clone(), None).unwrap();
    assert_eq!(event.event_id, id);
    assert_eq!(event.process, Process::Daemon);
    assert_eq!(event.kind, Kind::NativeCrash);
    assert_eq!(event.error_code, Code::NativeCrash);
    assert_eq!(
        event.release, "0.1.99",
        "the Krill bundle version in the report"
    );
    assert_eq!(
        event.build_id, "unknown",
        "the report cannot prove the build"
    );
    assert_eq!(event.source_loc, None);
    let native = event.native.clone().unwrap();
    assert_eq!(native.exception, Exception::BadAccess);
    assert_eq!(native.signal, Signal::Segv);
    assert!(
        native.frames.is_empty(),
        "no image UUIDs until a Krill image is verified"
    );
    event.validate().unwrap();
    let wire = serde_json::to_string(&event).unwrap();
    assert!(wire.contains("\"frames\":[]"), "{wire}");
    for leak in [
        "Users",
        "USER",
        "secret",
        "SECRET",
        "Agent",
        "DEADBEEF",
        "deadbeef",
        "KERN_INVALID",
        "Segmentation",
        "launchd",
        "Mac14",
        "ARM-64",
        "8F8E5B1A",
        "8f8e5b1a",
    ] {
        assert!(!wire.contains(leak), "{leak} leaked: {wire}");
    }
}

#[test]
fn rejects_reports_that_are_not_crashes() {
    for bug in ["\"288\"", "309", "\"309 \"", "null", "\"\"", "[\"309\"]"] {
        let fixture = Fixture {
            bug_type: bug.to_owned(),
            ..Fixture::daemon()
        };
        assert!(fixture.parse().is_none(), "header bug_type {bug}");
        let fixture = Fixture {
            body_bug_type: bug.to_owned(),
            ..Fixture::daemon()
        };
        assert!(fixture.parse().is_none(), "body bug_type {bug}");
    }
}

#[test]
fn nonfatal_or_simulated_in_any_position_rejects_the_report() {
    let positions: [fn(&mut Fixture, &str); 8] = [
        |f, v| f.header_extra = format!(",\"is_non_fatal\":{v}"),
        |f, v| f.header_extra = format!(",\"isSimulated\":{v}"),
        |f, v| f.header_extra = format!(",\"is_simulated\":{v}"),
        |f, v| f.body_extra = format!(",\n  \"isNonFatal\" : {v}"),
        |f, v| f.body_extra = format!(",\n  \"is_simulated\" : {v}"),
        |f, v| f.body_extra = format!(",\n  \"isSimulated\" : {v}"),
        |f, v| f.exception_extra = format!(",\"isNonFatal\":{v}"),
        |f, v| f.exception_extra = format!(",\"isSimulated\":{v}"),
    ];
    for set in positions {
        for value in [
            "true", "1", "-1", "2", "\"true\"", "\"1\"", "\"YES\"", "\"0\"", "null", "0.0", "[]",
            "{}",
        ] {
            let mut fixture = Fixture::daemon();
            set(&mut fixture, value);
            assert!(fixture.parse().is_none(), "{value}: {fixture:?}");
        }
        for value in ["false", "0"] {
            let mut fixture = Fixture::daemon();
            set(&mut fixture, value);
            assert!(fixture.parse().is_some(), "{value}: {fixture:?}");
        }
    }
    // 두 표기가 함께 오면 같은 필드가 두 번 온 것이다.
    let fixture = Fixture {
        header_extra: ",\"is_simulated\":0,\"isSimulated\":0".to_owned(),
        ..Fixture::daemon()
    };
    assert!(fixture.parse().is_none());
}

#[test]
fn only_exact_krill_executables_and_signing_ids_are_accepted() {
    for (name, signing, process) in [
        ("krill-app", "dev.krill.app", Process::App),
        ("krilld", "dev.krill.app.krilld", Process::Daemon),
        ("krill-holder", "dev.krill.app.holder", Process::Holder),
        ("krill", "dev.krill.app.cli", Process::Cli),
    ] {
        let fixture = Fixture {
            proc_name: name.to_owned(),
            signing_id: signing.to_owned(),
            ..Fixture::daemon()
        };
        assert_eq!(fixture.parse().unwrap().process, process, "{name}");
    }
    for (name, signing) in [
        ("krilld", "dev.krill.app.holder"),
        ("krilld", "krilld-55d7b8a9"),
        ("krilld", ""),
        ("krill-link", "dev.krill.app.link"),
        ("krill_proto-1a2b3c", "krill_proto-1a2b3c"),
        ("Safari", "com.apple.Safari"),
        ("KRILLD", "dev.krill.app.krilld"),
        ("krilld ", "dev.krill.app.krilld"),
    ] {
        let fixture = Fixture {
            proc_name: name.to_owned(),
            signing_id: signing.to_owned(),
            ..Fixture::daemon()
        };
        assert!(fixture.parse().is_none(), "{name} {signing}");
    }
    for (incident, body) in [
        ("not-a-uuid", Some("not-a-uuid")),
        ("8F8E5B1A0C1D4E2F9A3B1C2D3E4F5A6B0000", None),
        ("8F8E5B1A-0C1D-4E2F-9A3B-1C2D3E4F5A6G", None),
        (INCIDENT, Some("8F8E5B1A-0C1D-4E2F-9A3B-000000000000")),
    ] {
        let fixture = Fixture {
            incident: incident.to_owned(),
            body_incident: body.map(str::to_owned),
            ..Fixture::daemon()
        };
        assert!(fixture.parse().is_none(), "{incident} {body:?}");
    }
    let fixture = Fixture {
        body_incident: None,
        ..Fixture::daemon()
    };
    assert!(fixture.parse().is_some(), "the body incident is optional");
}

#[test]
fn exactly_two_json_documents_are_required() {
    let fixture = Fixture::daemon();
    let (header, body, text) = (fixture.header(), fixture.body(), fixture.text());
    assert!(Report::parse(text.as_bytes()).is_some());
    for broken in [
        format!("{text}\n{{}}"),
        format!("{text}\ntrailing"),
        format!("{header} {body}"),
        header.clone(),
        format!("{header}\n"),
        format!("\n{header}\n{body}"),
        format!("{body}\n{header}"),
        text.get(..200).unwrap().to_owned(),
        String::new(),
    ] {
        assert!(Report::parse(broken.as_bytes()).is_none(), "{broken:?}");
    }
    let long_header = Fixture {
        header_extra: format!(",\"pad\":\"{}\"", "x".repeat(MAX_HEADER_BYTES)),
        ..Fixture::daemon()
    };
    assert!(long_header.parse().is_none(), "first line is bounded");
    let huge = Fixture {
        body_extra: format!(
            ",\n  \"pad\" : \"{}\"",
            "x".repeat(usize::try_from(MAX_REPORT_BYTES).unwrap())
        ),
        ..Fixture::daemon()
    };
    assert!(huge.parse().is_none(), "whole report is bounded");
}

#[test]
fn duplicate_or_mistyped_fields_reject_the_document() {
    for fixture in [
        Fixture {
            header_extra: ",\"bug_type\":\"309\"".to_owned(),
            ..Fixture::daemon()
        },
        Fixture {
            body_extra: ",\n  \"procName\" : \"krilld\"".to_owned(),
            ..Fixture::daemon()
        },
        Fixture {
            body_extra: ",\n  \"captureTime\" : \"2099-01-02 03:04:05 +0900\"".to_owned(),
            ..Fixture::daemon()
        },
        Fixture {
            pid: u32::MAX,
            body_extra: ",\n  \"pid\" : -1".to_owned(),
            ..Fixture::daemon()
        },
        Fixture {
            exception: "EXC_BAD_ACCESS\",\"type\":\"EXC_CRASH".to_owned(),
            ..Fixture::daemon()
        },
    ] {
        assert!(fixture.parse().is_none(), "{fixture:?}");
    }
}

/// 무시하는 값은 `serde_json`이 재귀 없이(명시적 스택, 입력 크기 이내) 건너뛴다. 스택 넘침 없이 끝나야 한다.
#[test]
fn deeply_nested_ignored_values_are_skipped_without_recursion() {
    let nested = |depth: usize| {
        Fixture {
            body_extra: format!(
                ",\n  \"extra\" : {}{}",
                "[".repeat(depth),
                "]".repeat(depth)
            ),
            ..Fixture::daemon()
        }
        .parse()
    };
    assert!(nested(100).is_some());
    assert!(nested(500_000).is_some());
    // 읽는 필드 자리의 깊은 값은 첫 원소에서 형식 오류로 끝난다.
    let typed = Fixture {
        body_extra: format!(
            ",\n  \"bundleInfo\" : {}{}",
            "[".repeat(500_000),
            "]".repeat(500_000)
        ),
        bundle: None,
        ..Fixture::daemon()
    };
    assert!(typed.parse().is_none());
}

#[test]
fn ips_times_parse_only_in_the_report_form() {
    let utc = crate::time::parse_rfc3339_epoch("2099-01-01T18:04:05Z").unwrap();
    let expected = u64::try_from(utc).unwrap().checked_mul(1000);
    assert_eq!(ips_time_ms(CAPTURE), expected);
    assert_eq!(ips_time_ms("2099-01-01 18:04:05 +0000"), expected);
    assert_eq!(ips_time_ms("2099-01-01 18:04:05.999999 -0000"), expected);
    assert_eq!(ips_time_ms("2099-01-01 20:04:05.5 +02:00"), expected);
    let long = format!("2099-01-01 18:04:05.{} +0000", "9".repeat(64));
    for bad in [
        "2099-01-01T18:04:05Z",
        "2099-01-01 18:04:05",
        "2099-01-01 18:04:05 Z",
        "2099-01-01  18:04:05 +0000",
        "2099-01-01 18:04:05 +0000 x",
        "2099-13-01 18:04:05 +0000",
        "1969-12-31 23:59:59 +0000",
        "",
        long.as_str(),
    ] {
        assert_eq!(ips_time_ms(bad), None, "{bad}");
    }
}

#[test]
fn unknown_exception_and_signal_names_map_to_unknown() {
    let cases = [
        (
            "EXC_CORPSE_NOTIFY",
            None,
            Exception::Unknown,
            Signal::Unknown,
        ),
        ("EXC_CRASH", Some("SIGKILL"), Exception::Crash, Signal::Kill),
        (
            "EXC_BREAKPOINT",
            Some("SIGTRAP"),
            Exception::Breakpoint,
            Signal::Trap,
        ),
        (
            "EXC_BAD_ACCESS",
            Some("SIGWINCH"),
            Exception::BadAccess,
            Signal::Unknown,
        ),
        (
            "exc_bad_access",
            Some("sigsegv"),
            Exception::Unknown,
            Signal::Unknown,
        ),
    ];
    for (exception, signal, want_exception, want_signal) in cases {
        let report = Fixture {
            exception: exception.to_owned(),
            signal: signal.map(str::to_owned),
            ..Fixture::daemon()
        }
        .parse()
        .unwrap();
        assert_eq!(report.exception, want_exception, "{exception}");
        assert_eq!(report.signal, want_signal, "{signal:?}");
    }
}

#[test]
fn capture_and_creation_time_must_both_follow_consent() {
    let report = Fixture::daemon().parse().unwrap();
    let at = report.capture_ms;
    let consent = |enabled_at_ms| Consent {
        generation: "0".repeat(32),
        enabled_at_ms,
    };
    let now = at.saturating_add(60_000);
    assert_eq!(report.timing(at, &consent(at), now), Timing::Accept);
    assert_eq!(
        report.timing(at, &consent(at.saturating_add(1)), now),
        Timing::Reject,
        "captured before consent"
    );
    let captured_late = Report {
        capture_ms: at.saturating_add(10_000),
        ..report.clone()
    };
    assert_eq!(
        captured_late.timing(at, &consent(at.saturating_add(5_000)), now),
        Timing::Reject,
        "file born before consent: captureTime alone is not enough"
    );
    let early = at.saturating_sub(FUTURE_SLACK_MS).saturating_sub(1);
    assert_eq!(report.timing(at, &consent(0), early), Timing::Later);
    let stale = at.saturating_add(MAX_REPORT_AGE_MS).saturating_add(1);
    assert_eq!(report.timing(at, &consent(0), stale), Timing::Reject);
}

#[test]
fn event_ids_are_deterministic_per_incident() {
    let report = Fixture::daemon().parse().unwrap();
    let id = event_id(NATIVE_DOMAIN, report.incident.as_bytes());
    assert_eq!(id, event_id(NATIVE_DOMAIN, INCIDENT.as_bytes()));
    assert_ne!(id, event_id(FILE_DOMAIN, INCIDENT.as_bytes()));
    let other = Fixture {
        incident: "00000000-0C1D-4E2F-9A3B-1C2D3E4F5A6B".to_owned(),
        body_incident: None,
        ..Fixture::daemon()
    }
    .parse()
    .unwrap();
    assert_ne!(id, event_id(NATIVE_DOMAIN, other.incident.as_bytes()));
}

#[test]
fn release_comes_from_the_matching_run_or_the_krill_bundle() {
    let report = Fixture::daemon().parse().unwrap();
    let id = "0".repeat(32);
    let event = report
        .event(
            id.clone(),
            Some(("0.1.40".to_owned(), "0.1.40+dev.test".to_owned())),
        )
        .unwrap();
    assert_eq!(
        (event.release.as_str(), event.build_id.as_str()),
        ("0.1.40", "0.1.40+dev.test")
    );
    for bundle in [
        None,
        Some(("com.example.other".to_owned(), "9.9.9".to_owned())),
        Some((BUNDLE_ID.to_owned(), "1.0".to_owned())),
        Some((BUNDLE_ID.to_owned(), "1.0.0+meta".to_owned())),
    ] {
        let report = Fixture {
            bundle: bundle.clone(),
            ..Fixture::daemon()
        }
        .parse()
        .unwrap();
        assert!(
            report.event(id.clone(), None).is_none(),
            "unverified release is skipped: {bundle:?}"
        );
    }
}

#[test]
fn candidate_names_select_only_krill_executables() {
    assert_eq!(
        candidate("krilld-2099-01-02-030405.ips"),
        Some((Process::Daemon, "2099-01-02-030405"))
    );
    for (name, process) in [
        ("krill-app-2099-01-02-030405.ips", Process::App),
        ("krill-holder-2099-01-02-030405.ips", Process::Holder),
        ("krill-2099-01-02-030405.ips", Process::Cli),
        ("krilld-2099-01-02-030405-1.ips", Process::Daemon),
    ] {
        assert_eq!(candidate(name).map(|c| c.0), Some(process), "{name}");
    }
    let long = format!("krilld-2{}.ips", "0".repeat(MAX_NAME_BYTES));
    for name in [
        "krill_proto-0123abcd-2026-10-21-101010.ips",
        "krill-link-2099-01-02-030405.ips",
        "krilld-2099-01-02-030405.diag",
        "krilld-2099-01-02-030405.ips.synced",
        "krilld.ips",
        "krilld-.ips",
        "krilld-x.ips",
        "Safari-2099-01-02-030405.ips",
        "KRILLD-2099-01-02-030405.ips",
        long.as_str(),
    ] {
        assert_eq!(candidate(name), None, "{name}");
    }
}

#[test]
fn seen_cache_is_bounded_and_rejects_corruption() {
    let mut seen = Seen::default();
    for n in 0..600u32 {
        seen.insert(event_id("test", &n.to_le_bytes()));
    }
    assert_eq!(seen.tokens.len(), MAX_SEEN);
    assert!(
        !seen.contains(&event_id("test", &0u32.to_le_bytes())),
        "oldest out"
    );
    assert!(seen.contains(&event_id("test", &599u32.to_le_bytes())));
    let encoded = seen.encode();
    assert_eq!(encoded.len(), MAX_SEEN_BYTES);
    assert_eq!(Seen::decode(encoded.as_bytes()), Some(seen.tokens.clone()));
    let unterminated = format!("KSEEN1\n{}", "0".repeat(32));
    let overfull = format!("{encoded}{}\n", "1".repeat(32));
    for bad in [
        "",
        "KSEEN1",
        "KSEEN2\n",
        "KSEEN1\nnot-a-token\n",
        "KSEEN1\nAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\n",
        unterminated.as_str(),
        overfull.as_str(),
    ] {
        assert_eq!(Seen::decode(bad.as_bytes()), None, "{bad:?}");
    }
}

#[test]
fn collect_without_consent_reads_nothing() {
    let scratch = Scratch::new("native-none");
    assert_eq!(collect(&scratch.0).unwrap(), 0);
    assert!(!dir(&scratch.0).exists());
}

#[test]
fn collect_queues_a_fixture_report_once() {
    let (scratch, consent) = consented("native-once");
    let reports = scratch.0.join("reports");
    after_consent();
    write_report(&reports, NAME, &Fixture::daemon().text());
    let now = an_hour_later();
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), now), 1);
    let events = queued(&scratch.0);
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(
        event["event_id"],
        event_id(NATIVE_DOMAIN, INCIDENT.as_bytes())
    );
    assert_eq!(event["process"], "krilld");
    assert_eq!(event["kind"], "native_crash");
    assert_eq!(event["error_code"], "native_crash");
    assert_eq!(event["release"], "0.1.99");
    assert_eq!(event["build_id"], "unknown");
    assert_eq!(
        event["native"],
        serde_json::json!({"exception": "EXC_BAD_ACCESS", "signal": "SIGSEGV", "frames": []})
    );

    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), now), 0);
    assert_eq!(
        Seen::load(&scratch.0).tokens.len(),
        2,
        "file and incident tokens"
    );
    // 업로드로 대기열에서 빠진 뒤에도 같은 보고서를 다시 넣지 않는다.
    fs::remove_dir_all(dir(&scratch.0).join("outbox")).unwrap();
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), now), 0);
    assert!(queued(&scratch.0).is_empty());
}

#[test]
fn reports_from_before_consent_are_never_queued() {
    let scratch = Scratch::new("native-before");
    let reports = scratch.0.join("reports");
    // 동의 전에 생긴 파일: captureTime이 뒤라도 넣지 않는다.
    write_report(&reports, NAME, &Fixture::daemon().text());
    after_consent();
    crate::error_reporting::enable(&scratch.0).unwrap();
    let consent = consent::read(&scratch.0).unwrap().unwrap();
    let now = an_hour_later();
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), now), 0);
    // 동의 뒤에 생겼지만 동의 전 시각을 담은 보고서.
    let old = Fixture {
        capture: "2020-01-01 00:00:00.0000 +0000".to_owned(),
        launch: None,
        ..Fixture::daemon()
    };
    after_consent();
    write_report(&reports, "krilld-2020-01-01-000000.ips", &old.text());
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), now), 0);
    assert!(queued(&scratch.0).is_empty());
}

#[test]
fn reports_are_opened_without_following_links_or_blocking() {
    let (scratch, consent) = consented("native-open");
    let reports = scratch.0.join("reports");
    after_consent();
    let real = write_report(
        &scratch.0.join("elsewhere"),
        "real.ips",
        &Fixture::daemon().text(),
    );
    fs::create_dir_all(&reports).unwrap();
    let link = reports.join(NAME);
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let fifo = reports.join("krilld-2099-01-02-030406.ips");
    assert!(
        Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    fs::create_dir(reports.join("krilld-2099-01-02-030407.ips")).unwrap();
    let big = reports.join("krilld-2099-01-02-030408.ips");
    File::create(&big)
        .unwrap()
        .set_len(MAX_REPORT_BYTES.saturating_add(1))
        .unwrap();

    // 목록을 본 뒤 바뀐 자리여도 링크는 따라가지 않고 FIFO에서 기다리지 않는다.
    let (tx, rx) = mpsc::channel();
    let (link_path, fifo_path) = (link.clone(), fifo.clone());
    let regular = fs::symlink_metadata(&real).unwrap();
    let listed = regular.clone();
    std::thread::spawn(move || {
        tx.send((
            read_report(&link_path, &listed).is_none(),
            read_report(&fifo_path, &listed).is_none(),
        ))
        .ok();
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(10)).unwrap(),
        (true, true)
    );
    assert!(
        read_report(&real, &fs::symlink_metadata(&big).unwrap()).is_none(),
        "a different inode than the listed one is not read"
    );
    assert!(read_report(&real, &regular).is_some());
    let listed = fs::symlink_metadata(&big).unwrap();
    assert!(read_report(&big, &listed).is_none(), "oversized");

    assert_eq!(
        collect_with(&scratch.0, &consent, Some(&reports), an_hour_later()),
        0
    );
    assert!(queued(&scratch.0).is_empty());
}

#[test]
fn each_pass_opens_a_bounded_number_of_reports() {
    let (scratch, consent) = consented("native-bound");
    let reports = scratch.0.join("reports");
    after_consent();
    for n in 0..12u32 {
        write_report(
            &reports,
            &format!("krilld-2099-01-02-0300{n:02}.ips"),
            "{}\n{}",
        );
    }
    let now = an_hour_later();
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), now), 0);
    assert_eq!(Seen::load(&scratch.0).tokens.len(), MAX_OPENED);
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), now), 0);
    assert_eq!(Seen::load(&scratch.0).tokens.len(), 12);
}

#[test]
fn native_report_for_a_dead_run_wins_over_unclean_exit() {
    let (scratch, consent) = consented("native-run");
    let reports = scratch.0.join("reports");
    let launch = ips_time_ms(LAUNCH).unwrap();
    let marker = plant_marker(
        &scratch.0,
        &consent,
        Process::Daemon,
        4242,
        launch.saturating_add(1_000),
    );
    after_consent();
    write_report(&reports, NAME, &Fixture::daemon().text());
    let now = an_hour_later();
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), now), 1);
    assert!(!marker.exists(), "the native report settles the run");
    let events = queued(&scratch.0);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["error_code"], "native_crash");
    assert_eq!(events[0]["release"], "0.1.40", "the run's own release");
    assert_eq!(events[0]["build_id"], "0.1.40+dev.test");
    let after = now
        .saturating_add(runs::GRACE_MS)
        .saturating_add(runs::GRACE_MS);
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), after), 0);
    assert_eq!(queued(&scratch.0).len(), 1, "no extra unclean_exit");
}

#[test]
fn unmatched_dead_run_still_waits_for_grace() {
    let (scratch, consent) = consented("native-other-run");
    let reports = scratch.0.join("reports");
    let marker = plant_marker(&scratch.0, &consent, Process::Daemon, 9_999, 1_000);
    after_consent();
    write_report(&reports, NAME, &Fixture::daemon().text());
    let now = an_hour_later();
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), now), 1);
    assert!(marker.exists(), "another run: grace starts");
    let after = now.saturating_add(runs::GRACE_MS);
    assert_eq!(collect_with(&scratch.0, &consent, Some(&reports), after), 1);
    let mut codes: Vec<String> = queued(&scratch.0)
        .iter()
        .map(|event| event["error_code"].as_str().unwrap().to_owned())
        .collect();
    codes.sort();
    assert_eq!(codes, ["native_crash", "unclean_exit"]);
}

#[test]
fn cache_written_across_a_consent_change_is_removed() {
    let (scratch, consent) = consented("native-revoke");
    let mut seen = Seen::default();
    seen.insert("0".repeat(32));
    seen.store(&scratch.0, &consent);
    assert!(Seen::path(&scratch.0).exists());
    let stale = Consent {
        generation: "1".repeat(32),
        ..consent
    };
    seen.insert("1".repeat(32));
    seen.store(&scratch.0, &stale);
    assert!(
        !Seen::path(&scratch.0).exists(),
        "a write that raced a revocation or re-consent is undone"
    );
}

proptest::proptest! {
    #[test]
    fn report_parser_never_panics(
        bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..4096)
    ) {
        core::hint::black_box(Report::parse(&bytes));
    }

    #[test]
    fn mutated_reports_never_panic_and_stay_closed(
        edits in proptest::collection::vec(
            (proptest::prelude::any::<proptest::sample::Index>(), proptest::prelude::any::<u8>(), 0u8..3),
            1..16,
        )
    ) {
        let mut bytes = Fixture::daemon().text().into_bytes();
        for (at, byte, op) in edits {
            if bytes.is_empty() {
                bytes.push(byte);
                continue;
            }
            let i = at.index(bytes.len());
            match op {
                0 => bytes[i] = byte,
                1 => bytes.insert(i, byte),
                _ => {
                    bytes.remove(i);
                }
            }
        }
        if let Some(report) = Report::parse(&bytes) {
            proptest::prop_assert!(KNOWN.iter().any(|(_, process, _)| *process == report.process));
            proptest::prop_assert!(valid_incident(&report.incident));
        }
    }

    #[test]
    fn integer_flags_accept_only_zero(value in proptest::prelude::any::<i64>()) {
        let fixture = Fixture {
            body_extra: format!(",\n  \"isNonFatal\" : {value}"),
            ..Fixture::daemon()
        };
        proptest::prop_assert_eq!(fixture.parse().is_some(), value == 0);
    }

    #[test]
    fn ips_time_agrees_with_rfc3339(
        year in 1970i64..2200,
        month in 1i64..=12,
        day in 1i64..=28,
        hour in 0i64..24,
        minute in 0i64..60,
        second in 0i64..60,
        frac in "[0-9]{0,6}",
        east in proptest::prelude::any::<bool>(),
        zone_hours in 0i64..24,
        zone_minutes in 0i64..60,
    ) {
        let frac = if frac.is_empty() { String::new() } else { format!(".{frac}") };
        let zone = format!("{}{zone_hours:02}{zone_minutes:02}", if east { '+' } else { '-' });
        let clock = format!("{hour:02}:{minute:02}:{second:02}{frac}");
        let date = format!("{year:04}-{month:02}-{day:02}");
        let expected = crate::time::parse_rfc3339_epoch(&format!("{date}T{clock}{zone}"))
            .and_then(|secs| u64::try_from(secs).ok())
            .and_then(|secs| secs.checked_mul(1000));
        proptest::prop_assert_eq!(ips_time_ms(&format!("{date} {clock} {zone}")), expected);
    }

    #[test]
    fn seen_decode_never_panics(
        bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..20_000)
    ) {
        core::hint::black_box(Seen::decode(&bytes));
    }
}
