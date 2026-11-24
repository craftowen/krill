//! 설정 모달의 탭 식별자 및 탭 메타데이터(아이콘·제목·부제).

use crate::i18n::catalog::settings as text;

/// 설정 모달의 탭 및 하위 페이지.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdminTab {
    #[default]
    Appearance,
    Agents,
    Accounts,
    Machines,
    Mobile,
    Voice,
    Shortcuts,
    Updates,
    System,
    /// 내비에 없는 하위 페이지. 프로젝트 메뉴·에이전트 탭에서 연다.
    Project,
    /// 내비에 없는 하위 페이지. 에이전트 탭·명령 팔레트·빠른 명령 메뉴에서 연다.
    QuickCommands,
    /// 내비에 없는 하위 페이지. 시스템 탭에서 연다.
    Install,
    /// 내비에 없는 하위 페이지. 시스템 탭·명령 팔레트에서 연다.
    Disk,
    /// 내비 아래 '피드백 보내기' 행과 Help 메뉴에서 여는 단독 페이지.
    Feedback,
}

// 기본 번들 밖 탭 아이콘. 지정한 SVG만 바이너리에 싣는다.
gpui_kit_assets::icon_assets!(
    pub TabIcons,
    [
        Keyboard, Server, Smartphone, Monitor, Package, Palette, RefreshCw
    ]
);

impl AdminTab {
    pub const ALL: [Self; 9] = [
        Self::Appearance,
        Self::Agents,
        Self::Accounts,
        Self::Machines,
        Self::Mobile,
        Self::Voice,
        Self::Shortcuts,
        Self::Updates,
        Self::System,
    ];

    /// 내비에 없고 부모 탭 안에서 여는 페이지.
    pub const SUBPAGES: [Self; 4] = [
        Self::Project,
        Self::QuickCommands,
        Self::Install,
        Self::Disk,
    ];

    /// 내비에서 강조할 탭. 하위 페이지는 부모 탭에 속한다.
    #[must_use]
    pub const fn page(self) -> Self {
        match self {
            Self::Project | Self::QuickCommands => Self::Agents,
            Self::Install | Self::Disk => Self::System,
            tab => tab,
        }
    }

    pub fn selector(self) -> &'static str {
        match self {
            Self::Appearance => "admin-tab-appearance",
            Self::Accounts => "admin-tab-accounts",
            Self::Agents => "admin-tab-agents",
            Self::Install => "admin-tab-install",
            Self::Machines => "admin-tab-machines",
            Self::Project => "admin-tab-project",
            Self::QuickCommands => "admin-tab-quick-commands",
            Self::Shortcuts => "admin-tab-shortcuts",
            Self::System => "admin-tab-system",
            Self::Updates => "admin-tab-updates",
            Self::Disk => "admin-tab-disk",
            Self::Feedback => "admin-tab-feedback",
            Self::Mobile => "admin-tab-mobile",
            Self::Voice => "admin-tab-voice",
        }
    }

    pub fn label(self) -> &'static str {
        self.title_message().get()
    }

    pub fn title_message(self) -> crate::i18n::Message {
        match self {
            Self::Appearance => text::TAB_APPEARANCE,
            Self::Accounts => text::TAB_ACCOUNTS,
            Self::Agents => text::TAB_AGENTS,
            Self::Install => text::TAB_INSTALL,
            Self::Machines => text::TAB_REMOTE,
            Self::Project => text::TAB_PROJECT,
            Self::QuickCommands => text::TAB_QUICK_COMMANDS,
            Self::Shortcuts => text::TAB_SHORTCUTS,
            Self::System => text::TAB_SYSTEM,
            Self::Updates => text::TAB_UPDATES,
            Self::Disk => text::TAB_DISK,
            Self::Feedback => text::TAB_FEEDBACK,
            Self::Mobile => crate::i18n::catalog::mobile::TAB,
            Self::Voice => text::TAB_VOICE,
        }
    }

    pub fn subtitle_message(self) -> crate::i18n::Message {
        match self {
            Self::Appearance => text::TAB_APPEARANCE_DESC,
            Self::Accounts => text::TAB_ACCOUNTS_DESC,
            Self::Agents => text::AGENTS_DESC,
            Self::Install => text::TAB_INSTALL_DESC,
            Self::Machines => text::TAB_REMOTE_DESC,
            Self::Project => text::TAB_PROJECT_DESC,
            Self::QuickCommands => text::TAB_QUICK_COMMANDS_DESC,
            Self::Shortcuts => text::SHORTCUTS_DESC,
            Self::System => text::TAB_SYSTEM_DESC,
            Self::Updates => text::TAB_UPDATES_DESC,
            Self::Disk => text::TAB_DISK_DESC,
            Self::Feedback => text::TAB_FEEDBACK_DESC,
            Self::Mobile => text::TAB_MOBILE_DESC,
            Self::Voice => text::TAB_VOICE_DESC,
        }
    }

    pub fn subtitle(self) -> &'static str {
        self.subtitle_message().get()
    }

    /// 탭 아이콘. 앱 자산(`gpui_kit_assets`)의 전체 Lucide 집합에서 고른다.
    pub fn icon(self) -> gpui_kit_assets::IconName {
        use gpui_kit_assets::IconName;
        match self {
            Self::Appearance => IconName::Palette,
            Self::Accounts => IconName::User,
            Self::Agents => IconName::Bot,
            Self::Install => IconName::Package,
            Self::Machines => IconName::Monitor,
            Self::Project | Self::QuickCommands => IconName::Cpu,
            Self::Shortcuts => IconName::Keyboard,
            Self::System | Self::Voice => IconName::Settings,
            Self::Updates => IconName::RefreshCw,
            Self::Disk => IconName::CircleCheck,
            Self::Feedback => IconName::MessageSquare,
            Self::Mobile => IconName::Smartphone,
        }
    }

    /// 그릴 탭 아이콘. `Keyboard`·`Server`·`Smartphone`·`Monitor`는 기본 번들(`gpui_kit_assets::Assets`)에 없어 경로로 그리면
    /// 빈 칸이 된다(OWE-704 실화면 QA) → 그것만 [`TabIcons`]에서 SVG를 직접 싣는다.
    pub fn icon_element(self) -> gpui_component::Icon {
        use gpui::AssetSource as _;
        if self == Self::Voice {
            return gpui_component::Icon::default().data(crate::ui::icons::MIC_SVG);
        }
        let name = self.icon();
        match TabIcons.load(&name.path()) {
            Ok(Some(svg)) => gpui_component::Icon::default().data(&svg),
            _ => gpui_component::Icon::new(name),
        }
    }
}
