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

fn png_icon(bytes: &'static [u8], image: &'static OnceLock<Arc<Image>>) -> Icon {
    let image = image.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec())));
    Icon::default().image_variants(image.clone(), image.clone())
}

static ANTIGRAVITY: OnceLock<Arc<Image>> = OnceLock::new();
static CURSOR: OnceLock<Arc<Image>> = OnceLock::new();

pub(super) fn agent_icon(agent_id: &str) -> Option<Icon> {
    Some(match krill_proto::agent::canonical_id(agent_id) {
        "claude" => Icon::default()
            .data(CLAUDE_SVG)
            .text_color(rgb(super::CLAUDE_COLOR)),
        // Orca의 currentColor 글리프는 주변 텍스트의 테마·선택·비활성 색을 따른다.
        "codex" => Icon::default().data(OPENAI_SVG),
        "opencode" => Icon::default().data(OPENCODE_SVG),
        "pi" => Icon::default().data(PI_SVG),
        "agy" => png_icon(ANTIGRAVITY_PNG, &ANTIGRAVITY),
        "cursor" | "cursor-agent" => png_icon(CURSOR_PNG, &CURSOR),
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
    use sha2::{Digest as _, Sha256};

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
        ];
        assert_eq!(sources.len(), files.len());
        for (name, bytes) in files {
            let source = sources.iter().find(|s| s["file"] == name).unwrap();
            assert_eq!(
                source["sha256"],
                format!("{:x}", Sha256::digest(bytes)),
                "modified Orca asset: {name}"
            );
        }
    }

    #[gpui::test]
    fn orca_svg_glyphs_rasterize_at_icon_size(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for bytes in [CLAUDE_SVG, OPENAI_SVG, OPENCODE_SVG, PI_SVG] {
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
