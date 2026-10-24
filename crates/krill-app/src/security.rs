//! 터미널 보안 강화(MK-076, T3#25):
//!
//! 1. **링크 팝오버**: 링크(OSC 8·평문 URL)를 맨 클릭하면 바로 열지 않고 클릭한 자리에 실제 대상 URL과
//!    복사·브라우저 열기 버튼을 띄운다(Orca식). Cmd+클릭은 바로 연다(`http://`·`https://`·`mailto:`·`file://`만 허용,
//!    제어·양방향 위장 문자 거부).
//! 2. **창 제목(OSC 0·2) 제어문자 필터**: 데몬 엔진(`krilld::term`)·프로토콜(`krill_proto::screen::sanitize_title`)·
//!    UI 탭 제목 모두에서 제어 문자(C0·DEL·C1)와 양방향(Bidi) 방향 전환 문자를 걸러 낸다.
//! 3. **여러 줄 붙여넣기 경고**: 프로그램이 bracketed paste(`?2004h`)를 켰으면 `ESC[200~`…`ESC[201~`로 감싸
//!    보내고(본문의 ESC·`^C` 제거), bracketed paste를 안 쓰는 프로그램에 줄바꿈(`\n`·`\r`)이 든 텍스트를 붙여넣으면
//!    바로 실행되지 않게 보류하고 경고 배너(줄 수·미리보기·붙여넣기/취소)를 띄운다.
//! 4. **Secure Keyboard Entry**: 비밀번호 입력 시 다른 프로세스의 이벤트 탭 스니핑을 막는 macOS Carbon
//!    `EnableSecureEventInput` / `DisableSecureEventInput`을 메뉴·버튼 토글로 제공한다. 창이 비활성화되거나
//!    종료되면 자동으로 해제해 다른 앱의 전역 단축키·입력기를 방해하지 않는다.

use krill_proto::term_modes::Modes;

use crate::layout::PaneId;
use crate::platform;

pub use krill_proto::screen::{is_bidi_control, sanitize_title, sanitize_title_max};

const PREVIEW_CHARS: usize = 48;

/// 사용자에게 보이는 ko/en 문자열 키(MK-094).
pub use crate::i18n::catalog::security as text;

/// 맨 클릭으로 연 링크 팝오버: 칸, 실제 대상 URL, 클릭한 창 좌표.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkMenu {
    pub pane: PaneId,
    pub uri: String,
    pub position: gpui::Point<gpui::Pixels>,
}

impl LinkMenu {
    /// 팝오버에 보일 URL(제어·양방향 위장 문자 제거).
    pub fn display_uri(&self) -> String {
        sanitize_title(&self.uri)
    }
}

/// 붙여넣을 바이트에 줄바꿈(`\n` 또는 `\r`)이 들어 있는지 판정한다.
/// 줄바꿈이 하나라도 있으면 bracketed paste 미지원 프로그램에서 즉시 명령이 실행될 수 있다.
pub fn is_multiline_paste(text: &[u8]) -> bool {
    text.contains(&b'\n') || text.contains(&b'\r')
}

/// 붙여넣을 텍스트의 줄 수(최소 1, 줄바꿈이 있으면 최소 2).
pub fn count_paste_lines(text: &[u8]) -> usize {
    if !is_multiline_paste(text) {
        return 1;
    }
    let mut breaks = 0usize;
    let mut it = text.iter().peekable();
    while let Some(&b) = it.next() {
        if b == b'\r' {
            if it.peek() == Some(&&b'\n') {
                it.next();
            }
            breaks = breaks.saturating_add(1);
        } else if b == b'\n' {
            breaks = breaks.saturating_add(1);
        }
    }
    // 끝에 줄바꿈이 하나만 있어도("rm -rf /\n") 실행 위험이 있는 여러 줄 붙여넣기로 취급해 최소 2줄로 표시한다.
    let trailing = text.last().is_some_and(|&b| b == b'\n' || b == b'\r');
    let lines = if trailing {
        breaks
    } else {
        breaks.saturating_add(1)
    };
    lines.max(2)
}

/// 여러 줄 붙여넣기 경고에서 보류 중인 페이로드.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingPaste {
    pub pane: PaneId,
    pub payload: Vec<u8>,
    pub lines: usize,
    pub preview: String,
}

impl PendingPaste {
    pub fn warning_text(&self) -> String {
        text::PASTE_WARNING.format(&[
            ("lines", &self.lines.to_string()),
            ("preview", &self.preview),
        ])
    }
}

/// 붙여넣기 판정 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PastePlan {
    /// 바로 PTY로 보낸다(bracketed paste가 켜져 있거나 한 줄짜리).
    Immediate(Vec<u8>),
    /// bracketed paste가 꺼진 상태에서 줄바꿈이 포함되어 사용자 확인이 필요하다.
    Warn(PendingPaste),
}

/// 붙여넣기를 바로 보낼지 경고 후 보류할지 결정한다(인코딩은 MK-033 `krill_term::paste::paste` 사용).
pub fn plan_paste(pane: PaneId, text: &[u8], modes: Modes) -> PastePlan {
    let payload = krill_term::paste::paste(text, modes);
    if modes.contains(Modes::BRACKETED_PASTE) || !is_multiline_paste(text) {
        return PastePlan::Immediate(payload);
    }
    PastePlan::Warn(PendingPaste {
        pane,
        payload,
        lines: count_paste_lines(text),
        preview: preview_paste(text),
    })
}

fn preview_paste(text: &[u8]) -> String {
    let s = String::from_utf8_lossy(text);
    let mut out = String::new();
    let mut chars = s.chars();
    let mut count = 0usize;
    let mut prev_cr = false;
    for c in chars.by_ref() {
        if c == '\n' && prev_cr {
            prev_cr = false;
            continue;
        }
        prev_cr = c == '\r';
        let mapped = match c {
            '\r' | '\n' => '⏎',
            c if c.is_control() || is_bidi_control(c) => '·',
            c => c,
        };
        out.push(mapped);
        count = count.saturating_add(1);
        if count >= PREVIEW_CHARS {
            break;
        }
    }
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

/// 셸이 들고 있는 보안 상태(링크 팝오버, 보류 중인 여러 줄 붙여넣기, Secure Keyboard Entry).
#[derive(Debug)]
pub struct State {
    link_menu: Option<LinkMenu>,
    pending_paste: Option<PendingPaste>,
    secure_keyboard: bool,
    window_active: bool,
    apply_secure: fn(bool) -> bool,
}

impl Default for State {
    fn default() -> Self {
        Self::with_applier(platform::set_secure_event_input)
    }
}

impl State {
    pub fn with_applier(apply_secure: fn(bool) -> bool) -> Self {
        Self {
            link_menu: None,
            pending_paste: None,
            secure_keyboard: false,
            window_active: true,
            apply_secure,
        }
    }

    /// 열려 있는 링크 팝오버.
    pub fn link_menu(&self) -> Option<&LinkMenu> {
        self.link_menu.as_ref()
    }

    /// 링크 팝오버를 열거나(`Some`) 닫는다(`None`). 값이 바뀌었으면 `true`.
    pub fn set_link_menu(&mut self, next: Option<LinkMenu>) -> bool {
        if self.link_menu == next {
            return false;
        }
        self.link_menu = next;
        true
    }

    /// 보류 중인 여러 줄 붙여넣기.
    pub fn pending_paste(&self) -> Option<&PendingPaste> {
        self.pending_paste.as_ref()
    }

    pub fn set_pending_paste(&mut self, pending: Option<PendingPaste>) {
        self.pending_paste = pending;
    }

    pub fn take_pending_paste(&mut self) -> Option<PendingPaste> {
        self.pending_paste.take()
    }

    /// 안 보이게 된 칸의 링크 팝오버·보류 붙여넣기를 정리한다.
    pub fn retain_panes(&mut self, visible: &[PaneId]) {
        if self
            .link_menu
            .as_ref()
            .is_some_and(|m| !visible.contains(&m.pane))
        {
            self.link_menu = None;
        }
        if self
            .pending_paste
            .as_ref()
            .is_some_and(|p| !visible.contains(&p.pane))
        {
            self.pending_paste = None;
        }
    }

    /// 사용자가 켠 Secure Keyboard Entry 토글 상태.
    pub fn is_secure_keyboard_enabled(&self) -> bool {
        self.secure_keyboard
    }

    /// Secure Keyboard Entry를 토글하고 변경 후 상태를 돌려준다.
    pub fn toggle_secure_keyboard(&mut self) -> bool {
        self.set_secure_keyboard(!self.secure_keyboard)
    }

    /// Secure Keyboard Entry를 켜거나 끈다. 창이 활성 상태일 때만 OS에 실제로 반영한다.
    pub fn set_secure_keyboard(&mut self, enabled: bool) -> bool {
        self.secure_keyboard = enabled;
        self.sync_os_secure_input();
        self.secure_keyboard
    }

    /// 창이 현재 활성 상태인지 돌려준다.
    #[must_use]
    pub fn is_window_active(&self) -> bool {
        self.window_active
    }

    /// 창 활성 여부가 바뀌었을 때 부른다. 비활성 창이 OS 보안 입력을 잡지 않게 한다.
    pub fn set_window_active(&mut self, active: bool) {
        if self.window_active == active {
            return;
        }
        self.window_active = active;
        self.sync_os_secure_input();
    }

    fn sync_os_secure_input(&self) {
        let engage = self.secure_keyboard && self.window_active;
        (self.apply_secure)(engage);
    }
}

impl Drop for State {
    fn drop(&mut self) {
        if self.secure_keyboard && self.window_active {
            (self.apply_secure)(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::layout::Layout;

    fn pane() -> PaneId {
        Layout::default().active_tab().focused
    }

    #[test]
    fn title_filter_strips_control_and_bidi_chars() {
        let raw = "빌드\x00\x07\x08\t\r\n\x1b[2J\x7f\u{009b}\u{202e} 완료";
        assert_eq!(sanitize_title(raw), "빌드[2J 완료");
    }

    #[test]
    fn link_menu_shows_clean_target_and_closes_with_its_pane() {
        let p = pane();
        let menu = LinkMenu {
            pane: p,
            uri: "https://example.com/\x1b\u{202e}path".into(),
            position: gpui::point(gpui::px(10.), gpui::px(20.)),
        };
        assert_eq!(menu.display_uri(), "https://example.com/path");
        let mut state = State::with_applier(|_| true);
        assert!(state.set_link_menu(Some(menu.clone())));
        assert!(
            !state.set_link_menu(Some(menu.clone())),
            "같은 팝오버는 다시 그리지 않는다"
        );
        state.retain_panes(&[p]);
        assert_eq!(state.link_menu(), Some(&menu));
        state.retain_panes(&[]);
        assert_eq!(state.link_menu(), None, "칸이 사라지면 팝오버도 닫힌다");
    }

    #[test]
    fn multiline_paste_warns_only_when_unbracketed() {
        let p = pane();
        let bracketed = Modes {
            kitty: 0,
            flags: Modes::BRACKETED_PASTE,
        };
        let plain = Modes::default();

        // bracketed paste가 켜져 있으면 여러 줄이어도 즉시 감싸서 보내고 ESC·^C를 제거한다.
        assert_eq!(
            plan_paste(p, b"echo 1\n\x1b[201~rm -rf /\x03\n", bracketed),
            PastePlan::Immediate(b"\x1b[200~echo 1\n[201~rm -rf /\n\x1b[201~".to_vec())
        );

        // bracketed paste가 꺼져 있어도 줄바꿈이 없는 한 줄은 바로 보낸다.
        assert_eq!(
            plan_paste(p, b"echo safe", plain),
            PastePlan::Immediate(b"echo safe".to_vec())
        );

        // bracketed paste가 꺼진 상태에서 줄바꿈(\n·\r\n·끝 \n)이 있으면 보류하고 경고한다.
        let PastePlan::Warn(warn) = plan_paste(p, b"echo 1\r\necho 2\necho 3", plain) else {
            panic!("expected multiline warning");
        };
        assert_eq!(warn.pane, p);
        assert_eq!(warn.lines, 3);
        assert_eq!(warn.payload, b"echo 1\recho 2\recho 3");
        assert!(warn.warning_text().contains("3줄"));
        assert!(warn.warning_text().contains("echo 1⏎echo 2⏎echo 3"));

        // 끝에 줄바꿈 하나만 붙은 명령("sudo rm -rf /\n")도 즉시 실행을 막기 위해 경고한다.
        let PastePlan::Warn(trailing) = plan_paste(p, b"sudo rm -rf /\n", plain) else {
            panic!("expected warning for trailing newline");
        };
        assert_eq!(trailing.lines, 2);
        assert_eq!(trailing.payload, b"sudo rm -rf /\r");
    }

    #[test]
    fn secure_keyboard_toggles_and_releases_on_deactivate_and_drop() {
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        static LAST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        fn mock_apply(on: bool) -> bool {
            CALLS.fetch_add(1, Ordering::SeqCst);
            LAST.store(on, Ordering::SeqCst);
            true
        }

        {
            let mut state = State::with_applier(mock_apply);
            assert!(!state.is_secure_keyboard_enabled());
            assert!(state.toggle_secure_keyboard());
            assert!(LAST.load(Ordering::SeqCst));

            // 창이 비활성화되면 OS 보안 입력을 풀고, 다시 활성화되면 다시 켠다.
            state.set_window_active(false);
            assert!(!LAST.load(Ordering::SeqCst));
            assert!(state.is_secure_keyboard_enabled());

            state.set_window_active(true);
            assert!(LAST.load(Ordering::SeqCst));
        }
        // Drop 시 자동으로 해제한다.
        assert!(!LAST.load(Ordering::SeqCst));
        assert!(CALLS.load(Ordering::SeqCst) >= 4);
    }

    #[test]
    fn carbon_secure_event_input_roundtrip() {
        assert!(!platform::is_secure_event_input_engaged());
        assert!(platform::set_secure_event_input(true));
        assert!(platform::is_secure_event_input_engaged());
        assert!(platform::is_system_secure_event_input_enabled());
        // 중복 호출해도 카운터가 꼬이지 않고 한 번의 false로 해제된다.
        assert!(platform::set_secure_event_input(true));
        assert!(platform::set_secure_event_input(false));
        assert!(!platform::is_secure_event_input_engaged());
    }
}
