//! Orca의 `AgentIcon` 자산과 렌더링 방식을 이식한다.
//! Copyright (c) 2026 Lovecast Inc., MIT License.
//! 출처·원본 리비전·변환 내용·SHA-256·라이선스: `orca/`.

use std::sync::{Arc, OnceLock};

use gpui::{Image, ImageFormat, Styled as _, rgb};
use gpui_component::Icon;
use krill_proto::usage::ProviderKind;

pub const CLAUDE_SVG: &[u8] = include_bytes!("orca/claude.svg");
pub const OPENAI_SVG: &[u8] = include_bytes!("orca/openai.svg");
pub const ANTIGRAVITY_PNG: &[u8] = include_bytes!("orca/antigravity.png");
pub const OPENCODE_SVG: &[u8] = include_bytes!("orca/opencode.svg");
pub const CURSOR_PNG: &[u8] = include_bytes!("orca/cursor.png");
pub const PI_SVG: &[u8] = include_bytes!("orca/pi.svg");
pub const COPILOT_SVG: &[u8] = include_bytes!("orca/copilot.svg");
pub const AIDER_SVG: &[u8] = include_bytes!("orca/aider.svg");
pub const OMP_SVG: &[u8] = include_bytes!("orca/omp.svg");

pub const AMP_PNG: &[u8] = include_bytes!("orca/amp.png");
pub const CLINE_PNG: &[u8] = include_bytes!("orca/cline.png");
pub const CONTINUE_PNG: &[u8] = include_bytes!("orca/continue.png");
pub const CRUSH_PNG: &[u8] = include_bytes!("orca/crush.png");
pub const DEVIN_PNG: &[u8] = include_bytes!("orca/devin.png");
pub const DROID_PNG: &[u8] = include_bytes!("orca/droid.png");
pub const DSH_PNG: &[u8] = include_bytes!("orca/dsh.png");
pub const GOOSE_PNG: &[u8] = include_bytes!("orca/goose.png");
pub const GROK_PNG: &[u8] = include_bytes!("orca/grok.png");
pub const HERMES_PNG: &[u8] = include_bytes!("orca/hermes.png");
pub const KILO_PNG: &[u8] = include_bytes!("orca/kilo.png");
pub const KIMI_PNG: &[u8] = include_bytes!("orca/kimi.png");
pub const KIRO_PNG: &[u8] = include_bytes!("orca/kiro.png");
pub const MISTRAL_VIBE_PNG: &[u8] = include_bytes!("orca/mistral-vibe.png");
pub const MUSE_PNG: &[u8] = include_bytes!("orca/muse.png");
pub const OPENCLAUDE_PNG: &[u8] = include_bytes!("orca/openclaude.png");
pub const OPENCLAW_PNG: &[u8] = include_bytes!("orca/openclaw.png");
pub const QWEN_PNG: &[u8] = include_bytes!("orca/qwen-code.png");
pub const TRAE_PNG: &[u8] = include_bytes!("orca/trae.png");
pub const ZCODE_PNG: &[u8] = include_bytes!("orca/zcode.png");

fn png_icon(bytes: &'static [u8], image: &'static OnceLock<Arc<Image>>) -> Icon {
    let image = image.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec())));
    Icon::default().image_variants(image.clone(), image.clone())
}

static ANTIGRAVITY: OnceLock<Arc<Image>> = OnceLock::new();
static CURSOR: OnceLock<Arc<Image>> = OnceLock::new();
static AMP: OnceLock<Arc<Image>> = OnceLock::new();
static CLINE: OnceLock<Arc<Image>> = OnceLock::new();
static CONTINUE: OnceLock<Arc<Image>> = OnceLock::new();
static CRUSH: OnceLock<Arc<Image>> = OnceLock::new();
static DEVIN: OnceLock<Arc<Image>> = OnceLock::new();
static DROID: OnceLock<Arc<Image>> = OnceLock::new();
static DSH: OnceLock<Arc<Image>> = OnceLock::new();
static GOOSE: OnceLock<Arc<Image>> = OnceLock::new();
static GROK: OnceLock<Arc<Image>> = OnceLock::new();
static HERMES: OnceLock<Arc<Image>> = OnceLock::new();
static KILO: OnceLock<Arc<Image>> = OnceLock::new();
static KIMI: OnceLock<Arc<Image>> = OnceLock::new();
static KIRO: OnceLock<Arc<Image>> = OnceLock::new();
static MISTRAL_VIBE: OnceLock<Arc<Image>> = OnceLock::new();
static MUSE: OnceLock<Arc<Image>> = OnceLock::new();
static OPENCLAUDE: OnceLock<Arc<Image>> = OnceLock::new();
static OPENCLAW: OnceLock<Arc<Image>> = OnceLock::new();
static QWEN: OnceLock<Arc<Image>> = OnceLock::new();
static TRAE: OnceLock<Arc<Image>> = OnceLock::new();
static ZCODE: OnceLock<Arc<Image>> = OnceLock::new();

pub(super) fn agent_icon(agent_id: &str) -> Option<Icon> {
    Some(match krill_proto::agent::canonical_id(agent_id) {
        "claude" => Icon::default()
            .data(CLAUDE_SVG)
            .text_color(rgb(super::CLAUDE_COLOR)),
        // Orca의 currentColor 글리프는 주변 텍스트의 테마·선택·비활성 색을 따른다.
        "codex" => Icon::default().data(OPENAI_SVG),
        "opencode" | "opencode2" => Icon::default().data(OPENCODE_SVG),
        "pi" => Icon::default().data(PI_SVG),
        "copilot" => Icon::default().data(COPILOT_SVG),
        "aider" => Icon::default().data(AIDER_SVG),
        "omp" => Icon::default().data(OMP_SVG),
        "agy" => png_icon(ANTIGRAVITY_PNG, &ANTIGRAVITY),
        "cursor" | "cursor-agent" => png_icon(CURSOR_PNG, &CURSOR),
        "amp" => png_icon(AMP_PNG, &AMP),
        "cline" => png_icon(CLINE_PNG, &CLINE),
        "continue" => png_icon(CONTINUE_PNG, &CONTINUE),
        "crush" => png_icon(CRUSH_PNG, &CRUSH),
        "devin" => png_icon(DEVIN_PNG, &DEVIN),
        "droid" => png_icon(DROID_PNG, &DROID),
        "dsh" => png_icon(DSH_PNG, &DSH),
        "goose" => png_icon(GOOSE_PNG, &GOOSE),
        "grok" => png_icon(GROK_PNG, &GROK),
        "hermes" => png_icon(HERMES_PNG, &HERMES),
        "kilo" => png_icon(KILO_PNG, &KILO),
        "kimi" => png_icon(KIMI_PNG, &KIMI),
        "kiro" => png_icon(KIRO_PNG, &KIRO),
        "mistral-vibe" => png_icon(MISTRAL_VIBE_PNG, &MISTRAL_VIBE),
        "muse" => png_icon(MUSE_PNG, &MUSE),
        "openclaude" => png_icon(OPENCLAUDE_PNG, &OPENCLAUDE),
        "openclaw" => png_icon(OPENCLAW_PNG, &OPENCLAW),
        "qwen" => png_icon(QWEN_PNG, &QWEN),
        "trae" => png_icon(TRAE_PNG, &TRAE),
        "zcode" => png_icon(ZCODE_PNG, &ZCODE),
        _ => return None,
    })
}

pub(super) fn provider_icon(provider: ProviderKind) -> Option<Icon> {
    agent_icon(match provider {
        ProviderKind::Claude => "claude",
        ProviderKind::Codex => "codex",
        ProviderKind::Antigravity => "agy",
        ProviderKind::OpenCode => "opencode",
        ProviderKind::Gemini => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_brand_files_match_the_orca_asset_hashes() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("orca/sources.json")).unwrap();
        let sources = manifest["files"].as_array().unwrap();
        let files = [
            ("claude.svg", CLAUDE_SVG),
            ("openai.svg", OPENAI_SVG),
            ("antigravity.png", ANTIGRAVITY_PNG),
            ("opencode.svg", OPENCODE_SVG),
            ("cursor.png", CURSOR_PNG),
            ("pi.svg", PI_SVG),
            ("copilot.svg", COPILOT_SVG),
            ("aider.svg", AIDER_SVG),
            ("omp.svg", OMP_SVG),
            ("amp.png", AMP_PNG),
            ("cline.png", CLINE_PNG),
            ("continue.png", CONTINUE_PNG),
            ("crush.png", CRUSH_PNG),
            ("devin.png", DEVIN_PNG),
            ("droid.png", DROID_PNG),
            ("dsh.png", DSH_PNG),
            ("goose.png", GOOSE_PNG),
            ("grok.png", GROK_PNG),
            ("hermes.png", HERMES_PNG),
            ("kilo.png", KILO_PNG),
            ("kimi.png", KIMI_PNG),
            ("kiro.png", KIRO_PNG),
            ("mistral-vibe.png", MISTRAL_VIBE_PNG),
            ("muse.png", MUSE_PNG),
            ("openclaude.png", OPENCLAUDE_PNG),
            ("openclaw.png", OPENCLAW_PNG),
            ("qwen-code.png", QWEN_PNG),
            ("trae.png", TRAE_PNG),
            ("zcode.png", ZCODE_PNG),
        ];
        assert_eq!(sources.len(), files.len());
        for (name, bytes) in files {
            let source = sources.iter().find(|s| s["file"] == name).unwrap();
            assert_eq!(
                source["sha256"],
                crate::updater::ed25519::sha256_hex(bytes),
                "modified Orca asset: {name}"
            );
        }
        for preset in krill_proto::agent::PRESET_ORDER {
            assert!(agent_icon(preset).is_some(), "missing icon for {preset}");
        }
    }

    #[gpui::test]
    fn orca_svg_glyphs_rasterize_at_icon_size(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for bytes in [
                CLAUDE_SVG,
                OPENAI_SVG,
                OPENCODE_SVG,
                PI_SVG,
                COPILOT_SVG,
                AIDER_SVG,
                OMP_SVG,
            ] {
                let parsed = cx
                    .svg_renderer()
                    .parse_svg(bytes)
                    .expect("Orca SVG must parse");
                cx.svg_renderer()
                    .render_parsed(
                        &parsed,
                        gpui::SvgSize::Size(gpui::size(
                            gpui::DevicePixels(32),
                            gpui::DevicePixels(32),
                        )),
                    )
                    .expect("Orca glyph must rasterize");
            }
        });
    }
}
