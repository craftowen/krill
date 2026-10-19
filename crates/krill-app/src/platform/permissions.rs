//! 앱 전용 점검(MK-160): 설치 위치·알림 권한·격리 속성·폴더/전체 디스크 접근.
//!
//! 근거: 비공개 MK-160 스파이크 기록(측정값). `objc2` 호출은 이 파일에만 둔다.
//! 조회 API는 비동기 결과를 `Check`로 바꾸는 얇은 래퍼 + 판정 순수 함수로 나누고,
//! 판정만 단위 테스트한다(`tests.rs`). 사용자에게 보이는 문구는 전부
//! `i18n` 카탈로그(`permissions` 그룹) 경유다.
//!
//! 스레드 규칙(측정 기반):
//! - 알림 조회·요청은 어떤 스레드에서 호출해도 된다.
//!   콜백이 `UN` 내부 큐로 오고(실측 7.3ms), 호출 스레드는 `recv_timeout`으로만 기다린다.
//! - 폴더 프로브는 버튼을 눌렀을 때만 워커 스레드에서 타임아웃과 함께 호출한다.
//!   미허용 상태에서 호출 스레드가 커널에서 블로킹되기 때문이다(실측 8초).
//! - `completion handler` 안에는 패닉 경로가 없다(`send`·`from_raw`·`msg_send!`만).
//!   `C`·`objc2` 콜백 밖으로 패닉이 나가지 않는다(CONVENTIONS U10).

use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

#[cfg(target_os = "macos")]
use block2::RcBlock;
use krill_proto::doctor::{Check, Fix, FixKind, Level, Status};
#[cfg(target_os = "macos")]
use objc2::msg_send;
#[cfg(target_os = "macos")]
use objc2::runtime::{AnyClass, AnyObject, Bool};
#[cfg(target_os = "macos")]
use objc2_foundation::{NSBundle, NSError, NSString, NSURL};
#[cfg(target_os = "macos")]
use objc2_user_notifications::{
    UNAuthorizationOptions, UNNotificationSettings, UNUserNotificationCenter,
};

use crate::i18n::catalog::permissions as text;

/// `UNAuthorizationStatus`의 Rust 대응. `raw` 값은 `Apple` 정의와 같다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationStatus {
    /// `0`: 아직 묻지 않음.
    NotDetermined,
    /// `1`: 거부됨.
    Denied,
    /// `2`: 허용됨.
    Authorized,
    /// `3`: 임시 허용(조용히 전달).
    Provisional,
    /// `4`: 임시 세션(App Clips).
    Ephemeral,
}

impl NotificationStatus {
    /// `authorizationStatus` 원시값을 바꾼다. 미지의 값은 허용을 유도하는
    /// `NotDetermined`(경고 경로)로 둔다.
    pub const fn from_raw(raw: isize) -> Self {
        match raw {
            1 => Self::Denied,
            2 => Self::Authorized,
            3 => Self::Provisional,
            4 => Self::Ephemeral,
            _ => Self::NotDetermined,
        }
    }
}

/// 폴더 프로브 대상.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderKind {
    Documents,
    Desktop,
    Downloads,
}

impl FolderKind {
    /// 검사할 세 폴더.
    pub const fn all() -> [Self; 3] {
        [Self::Documents, Self::Desktop, Self::Downloads]
    }

    /// 홈 디렉터리 아래 이름.
    pub const fn subdir(self) -> &'static str {
        match self {
            Self::Documents => "Documents",
            Self::Desktop => "Desktop",
            Self::Downloads => "Downloads",
        }
    }

    /// 사용자에게 보이는 이름(현재 로케일).
    pub fn label(self) -> &'static str {
        match self {
            Self::Documents => text::FOLDER_DOCUMENTS.get(),
            Self::Desktop => text::FOLDER_DESKTOP.get(),
            Self::Downloads => text::FOLDER_DOWNLOADS.get(),
        }
    }
}

/// `[알림 허용]` 버튼 요청 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthRequestOutcome {
    Granted,
    Denied,
    /// 시스템 요청 실패 원인. 거부와 API 오류를 구분한다.
    Failed(String),
    /// 비번들 실행이라 요청 자체를 못 보냄.
    Unavailable,
    /// 콜백이 시간 안에 오지 않음.
    Timeout,
}

pub const LOCATION_ID: &str = "app.location";
pub const NOTIFICATIONS_ID: &str = "app.notifications";
pub const QUARANTINE_ID: &str = "app.quarantine";
pub const FOLDERS_ID: &str = "app.folders";
pub const FULL_DISK_ID: &str = "app.full_disk";
pub const ACCESSIBILITY_ID: &str = "app.accessibility";
pub const SCREEN_RECORDING_ID: &str = "app.screen_recording";

/// 폴더 접근 거부 시 여는 설정 화면.
pub const FOLDERS_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders";
/// 전체 디스크 접근 거부 시 여는 설정 화면.
pub const FULL_DISK_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
/// 손쉬운 사용(Accessibility) 권한 설정 화면(MK-187 Computer Use).
pub const ACCESSIBILITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
/// 화면 기록(Screen Recording) 권한 설정 화면(MK-187 Computer Use).
pub const SCREEN_RECORDING_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
/// 자동화 접근이 거부됐을 때 여는 설정 화면.
pub const AUTOMATION_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation";
/// Gatekeeper 안내를 확인할 개인정보 및 보안 화면.
pub const SECURITY_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?General";
/// 설치 위치 실패 시 여는 Finder 위치(응용 프로그램 폴더).
pub const APPLICATIONS_URL: &str = "file:///Applications/";
/// 특정 설정 화면 열기가 실패했을 때의 폴백(시스템 설정 루트).
/// 루트 착륙은 미측정이라 MK-163에서 확인한다.
pub const SETTINGS_ROOT_URL: &str = "x-apple.systempreferences:";

const NOTIFICATIONS_SETTINGS_PAGE: &str = "com.apple.Notifications-Settings.extension";
#[cfg(target_os = "macos")]
const QUARANTINE_ATTR: &str = "com.apple.quarantine";
const FDA_CANARY: &str = "/Library/Application Support/com.apple.TCC/TCC.db";

/// 수정 방법이 없을 때의 `Fix`.
fn no_fix() -> Fix {
    Fix {
        kind: FixKind::None,
        value: String::new(),
    }
}

/// 알림 거부·임시 허용 시 여는 설정 `URL`. `bundle_id`가 없으면 폴백(루트) `URL`이다.
pub fn notifications_settings_url(bundle_id: Option<&str>) -> String {
    match bundle_id {
        Some(id) => format!("x-apple.systempreferences:{NOTIFICATIONS_SETTINGS_PAGE}?id={id}"),
        None => format!("x-apple.systempreferences:{NOTIFICATIONS_SETTINGS_PAGE}"),
    }
}

/// `app.location` 판정. `bundled`는 `is_bundled()` 값이다.
pub fn decide_location(exe: &Path, bundled: bool) -> Check {
    if !bundled {
        return Check {
            id: LOCATION_ID.to_owned(),
            level: Level::Required,
            status: Status::Skip,
            summary: text::LOCATION_SKIP_SUMMARY.get().to_owned(),
            detail: text::LOCATION_SKIP_DETAIL.get().to_owned(),
            fix: no_fix(),
        };
    }
    let current = exe.display().to_string();
    if current.contains("/AppTranslocation/") {
        return Check {
            id: LOCATION_ID.to_owned(),
            level: Level::Required,
            status: Status::Fail,
            summary: text::LOCATION_TRANSLOCATED_SUMMARY.get().to_owned(),
            detail: text::LOCATION_TRANSLOCATED_DETAIL.format(&[("current", current.as_str())]),
            fix: Fix {
                kind: FixKind::OpenUrl,
                value: APPLICATIONS_URL.to_owned(),
            },
        };
    }
    if current.starts_with("/Volumes/") {
        return Check {
            id: LOCATION_ID.to_owned(),
            level: Level::Required,
            status: Status::Fail,
            summary: text::LOCATION_DMG_SUMMARY.get().to_owned(),
            detail: text::LOCATION_DMG_DETAIL.format(&[("current", current.as_str())]),
            fix: Fix {
                kind: FixKind::OpenUrl,
                value: APPLICATIONS_URL.to_owned(),
            },
        };
    }
    Check {
        id: LOCATION_ID.to_owned(),
        level: Level::Required,
        status: Status::Ok,
        summary: text::LOCATION_OK_SUMMARY.get().to_owned(),
        detail: text::LOCATION_OK_DETAIL.format(&[("current", current.as_str())]),
        fix: no_fix(),
    }
}

/// `app.notifications` 판정. `None`은 조회 불가(비번들·시간 초과)다.
///
/// `NotDetermined`의 `fix`는 의도적으로 `None`이다. MK-161은 `id`가
/// `app.notifications`이고 `status`가 `warn`이며 `fix.kind`이 `none`일 때
/// `[알림 허용]` 버튼을 달고 `request_notification_authorization`을 부른다.
pub fn decide_notifications(status: Option<NotificationStatus>, bundle_id: Option<&str>) -> Check {
    let settings_url = notifications_settings_url(bundle_id);
    let base = Check {
        id: NOTIFICATIONS_ID.to_owned(),
        level: Level::Recommended,
        status: Status::Warn,
        summary: String::new(),
        detail: String::new(),
        fix: no_fix(),
    };
    match status {
        None => Check {
            status: Status::Skip,
            summary: text::NOTIFICATIONS_SKIP_SUMMARY.get().to_owned(),
            detail: text::NOTIFICATIONS_SKIP_DETAIL.get().to_owned(),
            ..base
        },
        Some(NotificationStatus::Authorized) => Check {
            status: Status::Ok,
            summary: text::NOTIFICATIONS_OK_SUMMARY.get().to_owned(),
            detail: text::NOTIFICATIONS_OK_DETAIL.get().to_owned(),
            ..base
        },
        Some(NotificationStatus::Ephemeral) => Check {
            status: Status::Ok,
            summary: text::NOTIFICATIONS_EPHEMERAL_SUMMARY.get().to_owned(),
            detail: text::NOTIFICATIONS_EPHEMERAL_DETAIL.get().to_owned(),
            ..base
        },
        Some(NotificationStatus::Provisional) => Check {
            status: Status::Warn,
            summary: text::NOTIFICATIONS_PROVISIONAL_SUMMARY.get().to_owned(),
            detail: text::NOTIFICATIONS_PROVISIONAL_DETAIL.get().to_owned(),
            fix: Fix {
                kind: FixKind::OpenUrl,
                value: settings_url,
            },
            ..base
        },
        Some(NotificationStatus::NotDetermined) => Check {
            status: Status::Warn,
            summary: text::NOTIFICATIONS_NOTDETERMINED_SUMMARY.get().to_owned(),
            detail: text::NOTIFICATIONS_NOTDETERMINED_DETAIL.get().to_owned(),
            ..base
        },
        Some(NotificationStatus::Denied) => Check {
            status: Status::Warn,
            summary: text::NOTIFICATIONS_DENIED_SUMMARY.get().to_owned(),
            detail: text::NOTIFICATIONS_DENIED_DETAIL.get().to_owned(),
            fix: Fix {
                kind: FixKind::OpenUrl,
                value: settings_url,
            },
            ..base
        },
    }
}

/// `app.quarantine` 판정. `bundled`는 `is_bundled()` 값이다.
pub fn decide_quarantine(has_quarantine: bool, bundled: bool) -> Check {
    if !bundled {
        return Check {
            id: QUARANTINE_ID.to_owned(),
            level: Level::Recommended,
            status: Status::Skip,
            summary: text::QUARANTINE_SKIP_SUMMARY.get().to_owned(),
            detail: text::QUARANTINE_SKIP_DETAIL.get().to_owned(),
            fix: no_fix(),
        };
    }
    if has_quarantine {
        Check {
            id: QUARANTINE_ID.to_owned(),
            level: Level::Recommended,
            status: Status::Warn,
            summary: text::QUARANTINE_WARN_SUMMARY.get().to_owned(),
            detail: text::QUARANTINE_WARN_DETAIL.get().to_owned(),
            fix: no_fix(),
        }
    } else {
        Check {
            id: QUARANTINE_ID.to_owned(),
            level: Level::Recommended,
            status: Status::Ok,
            summary: text::QUARANTINE_OK_SUMMARY.get().to_owned(),
            detail: text::QUARANTINE_OK_DETAIL.get().to_owned(),
            fix: no_fix(),
        }
    }
}

/// `app.folders` 대기 상태. 자동 검사는 하지 않고 버튼을 안내한다.
pub fn folders_idle_check() -> Check {
    Check {
        id: FOLDERS_ID.to_owned(),
        level: Level::Optional,
        status: Status::Skip,
        summary: text::FOLDERS_IDLE_SUMMARY.get().to_owned(),
        detail: text::FOLDERS_IDLE_DETAIL.get().to_owned(),
        fix: no_fix(),
    }
}

/// `app.folders` 프로브 결과 판정. 입력은 `(폴더, 읽힘)` 목록이다.
pub fn decide_folder_probe(results: &[(FolderKind, bool)]) -> Check {
    if results.is_empty() {
        return folders_idle_check();
    }
    let denied: Vec<&str> = results
        .iter()
        .filter(|(_, readable)| !readable)
        .map(|(kind, _)| kind.label())
        .collect();
    if denied.is_empty() {
        Check {
            id: FOLDERS_ID.to_owned(),
            level: Level::Optional,
            status: Status::Ok,
            summary: text::FOLDERS_OK_SUMMARY.get().to_owned(),
            detail: text::FOLDERS_OK_DETAIL.get().to_owned(),
            fix: no_fix(),
        }
    } else {
        Check {
            id: FOLDERS_ID.to_owned(),
            level: Level::Optional,
            status: Status::Fail,
            summary: text::FOLDERS_FAIL_SUMMARY.format(&[("denied", &denied.join("·"))]),
            detail: text::FOLDERS_FAIL_DETAIL.get().to_owned(),
            fix: Fix {
                kind: FixKind::OpenUrl,
                value: FOLDERS_SETTINGS_URL.to_owned(),
            },
        }
    }
}

/// `app.full_disk` 판정. 입력은 카나리아 읽기 성공 여부다.
pub fn decide_full_disk(can_read_canary: bool) -> Check {
    if can_read_canary {
        Check {
            id: FULL_DISK_ID.to_owned(),
            level: Level::Optional,
            status: Status::Ok,
            summary: text::FULLDISK_OK_SUMMARY.get().to_owned(),
            detail: text::FULLDISK_OK_DETAIL.get().to_owned(),
            fix: no_fix(),
        }
    } else {
        Check {
            id: FULL_DISK_ID.to_owned(),
            level: Level::Optional,
            status: Status::Warn,
            summary: text::FULLDISK_WARN_SUMMARY.get().to_owned(),
            detail: text::FULLDISK_WARN_DETAIL.get().to_owned(),
            fix: Fix {
                kind: FixKind::OpenUrl,
                value: FULL_DISK_SETTINGS_URL.to_owned(),
            },
        }
    }
}

/// 번들 식별자. 비번들 실행(`cargo run` 등)이면 `None`이다.
#[cfg(target_os = "macos")]
pub fn bundle_id() -> Option<String> {
    NSBundle::mainBundle()
        .bundleIdentifier()
        .map(|id| id.to_string())
}
#[cfg(not(target_os = "macos"))]
pub fn bundle_id() -> Option<String> {
    None
}

/// 번들로 실행 중이면 `true`다. `UN` 조회·요청의 가드다.
pub fn is_bundled() -> bool {
    bundle_id().is_some()
}

/// 알림 권한 조회(프롬프트 없음, 실측 7.3ms).
///
/// 비번들이면 `UN`을 건드리지 않고 `None`을 돌려준다(무가드 호출은 `SIGABRT`,
/// MK-016 실측). 어떤 스레드에서 호출해도 된다. 시간 안에 답이 없으면 `None`이다.
#[cfg(target_os = "macos")]
pub fn query_notification_status(timeout: Duration) -> Option<NotificationStatus> {
    if !is_bundled() {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // 패닉 경로 없음: `as_ref`·`authorizationStatus`·`from_raw`·`send`는 패닉하지 않는다.
        // SAFETY: 콜백 동안 유효한 `UNNotificationSettings` 참조다.
        let raw = unsafe { settings.as_ref() }.authorizationStatus().0;
        if tx.send(NotificationStatus::from_raw(raw)).is_err() {
            // 수신자가 타임아웃으로 떠났으면 버린다.
        }
    });
    center.getNotificationSettingsWithCompletionHandler(&block);
    // `block`을 수신까지 살려 둔다(`UN`이 복사하지만 수명 보장용).
    let _keep_alive = block;
    rx.recv_timeout(timeout).ok()
}
#[cfg(not(target_os = "macos"))]
pub fn query_notification_status(_timeout: Duration) -> Option<NotificationStatus> {
    None
}

/// `[알림 허용]` 버튼용 요청. 시스템 허용 대화상자를 띄우므로 버튼 맥락에서만 부른다.
///
/// 비번들이면 요청을 보내지 않고 `Unavailable`을 돌려준다. 어떤 스레드에서
/// 호출해도 된다. 허용 대화상자의 클릭은 사람이 하므로 결과 확인은 미측정이다.
#[cfg(target_os = "macos")]
pub fn request_notification_authorization(timeout: Duration) -> AuthRequestOutcome {
    if !is_bundled() {
        return AuthRequestOutcome::Unavailable;
    }
    let (tx, rx) = mpsc::channel();
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let block = RcBlock::new(move |granted: Bool, error: *mut NSError| {
        let outcome = if let Some(error) = NonNull::new(error) {
            // SAFETY: 완료 콜백 동안 UN이 NSError의 유효한 수명을 보장한다.
            let error = unsafe { error.as_ref() };
            AuthRequestOutcome::Failed(error.localizedDescription().to_string())
        } else if granted.as_bool() {
            AuthRequestOutcome::Granted
        } else {
            AuthRequestOutcome::Denied
        };
        if tx.send(outcome).is_err() {
            // 수신자가 타임아웃으로 떠났으면 버린다.
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(
        UNAuthorizationOptions::Alert
            | UNAuthorizationOptions::Sound
            | UNAuthorizationOptions::Badge,
        &block,
    );
    // `block`을 수신까지 살려 둔다(`UN`이 복사하지만 수명 보장용).
    let _keep_alive = block;
    match rx.recv_timeout(timeout) {
        Ok(outcome) => outcome,
        Err(_) => AuthRequestOutcome::Timeout,
    }
}
#[cfg(not(target_os = "macos"))]
pub fn request_notification_authorization(_timeout: Duration) -> AuthRequestOutcome {
    AuthRequestOutcome::Unavailable
}

/// `URL`을 순서대로 열어 처음 성공한 것을 돌려준다. 특정 설정 화면이 안
/// 열리면 시스템 설정 루트로 폴백한다. 후보가 없으면 `false`다.
pub fn open_first_available(urls: &[&str]) -> bool {
    urls.iter().any(|url| open_url(url))
}

/// `URL`을 연다(시스템 설정·`Finder`). 열기 요청을 전달하면 `true`다.
#[cfg(target_os = "macos")]
pub fn open_url(url: &str) -> bool {
    let Some(workspace_class) = AnyClass::get(c"NSWorkspace") else {
        return false;
    };
    let ns_string = NSString::from_str(url);
    let Some(ns_url) = NSURL::URLWithString(&ns_string) else {
        return false;
    };
    // SAFETY: `sharedWorkspace`는 `AppKit` 싱글턴을 돌려준다.
    let workspace: *mut AnyObject = unsafe { msg_send![workspace_class, sharedWorkspace] };
    if workspace.is_null() {
        return false;
    }
    // SAFETY: `workspace`는 살아 있는 `NSWorkspace`이고 `ns_url`은 유효한 `NSURL`이다.
    let opened: Bool = unsafe { msg_send![workspace, openURL: &*ns_url] };
    opened.as_bool()
}
/// `URL`을 연다(Windows `ShellExecuteW`). 열기 요청을 전달하면 `true`다.
#[cfg(not(target_os = "macos"))]
pub fn open_url(url: &str) -> bool {
    super::shell_open(url.as_ref()).is_ok()
}

/// 번들 경로의 `com.apple.quarantine` 유무.
#[cfg(target_os = "macos")]
pub fn has_quarantine_attr(path: &Path) -> bool {
    let mut buf = vec![0u8; 4096];
    match rustix::fs::listxattr(path, buf.as_mut_slice()) {
        Ok(len) => buf
            .get(..len)
            .unwrap_or_default()
            .split(|byte| *byte == 0)
            .any(|name| name == QUARANTINE_ATTR.as_bytes()),
        Err(_) => false,
    }
}
#[cfg(not(target_os = "macos"))]
pub fn has_quarantine_attr(_path: &Path) -> bool {
    false
}

/// 전체 디스크 접근 카나리아 읽기. 읽히면 접근 허용 상태다.
/// 읽기 시도 자체는 프롬프트를 띄우지 않는다(실측: 거부 시 즉시 `EPERM`).
pub fn probe_full_disk() -> bool {
    match std::fs::File::open(FDA_CANARY) {
        Ok(mut file) => {
            let mut buf = [0u8; 1];
            std::io::Read::read(&mut file, &mut buf).is_ok()
        }
        Err(_) => false,
    }
}

/// 폴더 읽기 프로브. 호출 스레드가 동의 대기 중 블로킹될 수 있으므로
/// 버튼 맥락의 워커 스레드에서 타임아웃과 함께 호출한다(실측).
pub fn probe_folder_readable(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none_or(|entry| entry.is_ok()),
        Err(_) => false,
    }
}

/// 폴더 프로브 묶음(버튼용). 주체를 바꾸기 쉬우라고 한 함수에 격리한다.
/// 각 폴더를 워커 스레드에서 타임아웃과 함께 읽고, 시간 안에 답이 없으면
/// 읽힘 실패로 둔다.
pub fn probe_folders(timeout: Duration) -> Vec<(FolderKind, bool)> {
    FolderKind::all()
        .into_iter()
        .map(|kind| {
            let readable = folder_path(kind).is_some_and(|path| {
                let (tx, rx) = mpsc::channel();
                std::thread::Builder::new()
                    .name("mk-folder-probe".into())
                    .spawn(move || {
                        if tx.send(probe_folder_readable(&path)).is_err() {
                            // 수신자가 타임아웃으로 떠났으면 버린다.
                        }
                    })
                    .is_ok()
                    && rx.recv_timeout(timeout).unwrap_or(false)
            });
            (kind, readable)
        })
        .collect()
}

/// `FolderKind`의 검사 경로. `HOME`이 없으면 `None`이다.
pub fn folder_path(kind: FolderKind) -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(kind.subdir()))
}

/// Computer Use TCC 권한 종류(MK-187).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputerUsePermissionKind {
    Accessibility,
    Screenshots,
}

impl ComputerUsePermissionKind {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "accessibility" => Some(Self::Accessibility),
            "screenshots" | "screen" | "screen-recording" => Some(Self::Screenshots),
            _ => None,
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility",
            Self::Screenshots => "screenshots",
        }
    }

    pub const fn settings_url(self) -> &'static str {
        match self {
            Self::Accessibility => ACCESSIBILITY_SETTINGS_URL,
            Self::Screenshots => SCREEN_RECORDING_SETTINGS_URL,
        }
    }
}

/// Computer Use TCC 권한 스냅샷(MK-187).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComputerUsePermissions {
    pub accessibility: bool,
    pub screenshots: bool,
}

impl ComputerUsePermissions {
    pub const fn is_granted(self, kind: ComputerUsePermissionKind) -> bool {
        match kind {
            ComputerUsePermissionKind::Accessibility => self.accessibility,
            ComputerUsePermissionKind::Screenshots => self.screenshots,
        }
    }

    pub const fn all_granted(self) -> bool {
        self.accessibility && self.screenshots
    }
}

/// 손쉬운 사용(Accessibility) TCC 권한 허용 여부(`AXIsProcessTrusted`, 프롬프트 없음).
#[cfg(target_os = "macos")]
pub fn probe_accessibility() -> bool {
    // SAFETY: `ApplicationServices`의 `AXIsProcessTrusted`는 인자 없는 읽기 전용 C 함수다.
    unsafe { AXIsProcessTrusted() != 0 }
}
#[cfg(not(target_os = "macos"))]
pub fn probe_accessibility() -> bool {
    false
}

/// 화면 기록(Screen Recording) TCC 권한 허용 여부(`CGPreflightScreenCaptureAccess`, 프롬프트 없음).
#[cfg(target_os = "macos")]
pub fn probe_screen_recording() -> bool {
    // SAFETY: `CoreGraphics`의 `CGPreflightScreenCaptureAccess`는 인자 없는 읽기 전용 C 함수다.
    unsafe { CGPreflightScreenCaptureAccess() }
}
#[cfg(not(target_os = "macos"))]
pub fn probe_screen_recording() -> bool {
    false
}

/// Computer Use에 필요한 두 TCC 권한(손쉬운 사용·화면 기록)을 프롬프트 없이 조회한다.
pub fn probe_computer_use_permissions() -> ComputerUsePermissions {
    ComputerUsePermissions {
        accessibility: probe_accessibility(),
        screenshots: probe_screen_recording(),
    }
}

/// 지정한 Computer Use 권한의 시스템 설정 화면을 연다.
pub fn open_computer_use_settings(kind: ComputerUsePermissionKind) -> bool {
    open_first_available(&[kind.settings_url(), SETTINGS_ROOT_URL])
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
