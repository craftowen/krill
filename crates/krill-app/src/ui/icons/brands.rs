//! 공식 브랜드 배포 파일을 수정 없이 임베드한다. 출처와 SHA-256: `official/sources.json`.

use std::sync::{Arc, OnceLock};

use gpui::{Image, ImageFormat};
use gpui_component::Icon;
use krill_proto::usage::ProviderKind;

pub const CLAUDE_SVG: &[u8] = include_bytes!("official/claude.svg");
pub const OPENAI_SVG: &[u8] = include_bytes!("official/openai-light.svg");
pub const ANTIGRAVITY_SVG: &[u8] = include_bytes!("official/antigravity.svg");
pub const OPENCODE_SVG: &[u8] = include_bytes!("official/opencode-light.svg");
pub const CURSOR_SVG: &[u8] = include_bytes!("official/cursor-light.svg");
pub const PI_SVG: &[u8] = include_bytes!("official/pi.svg");

struct Mark {
    light: &'static [u8],
    dark: &'static [u8],
    images: OnceLock<(Arc<Image>, Arc<Image>)>,
}

impl Mark {
    const fn new(light: &'static [u8], dark: &'static [u8]) -> Self {
        Self {
            light,
            dark,
            images: OnceLock::new(),
        }
    }

    fn icon(&self) -> Icon {
        let (light, dark) = self.images.get_or_init(|| {
            let light = Arc::new(Image::from_bytes(ImageFormat::Svg, self.light.to_vec()));
            let dark = if self.light == self.dark {
                light.clone()
            } else {
                Arc::new(Image::from_bytes(ImageFormat::Svg, self.dark.to_vec()))
            };
            (light, dark)
        });
        Icon::default().image_variants(light.clone(), dark.clone())
    }
}

static CLAUDE: Mark = Mark::new(CLAUDE_SVG, CLAUDE_SVG);
static OPENAI: Mark = Mark::new(OPENAI_SVG, include_bytes!("official/openai-dark.svg"));
static ANTIGRAVITY: Mark = Mark::new(ANTIGRAVITY_SVG, ANTIGRAVITY_SVG);
static OPENCODE: Mark = Mark::new(OPENCODE_SVG, include_bytes!("official/opencode-dark.svg"));
static CURSOR: Mark = Mark::new(CURSOR_SVG, include_bytes!("official/cursor-dark.svg"));
static PI: Mark = Mark::new(PI_SVG, PI_SVG);

pub(super) fn agent_icon(agent_id: &str) -> Option<Icon> {
    Some(match krill_proto::agent::canonical_id(agent_id) {
        "claude" => CLAUDE.icon(),
        "codex" => OPENAI.icon(),
        "agy" => ANTIGRAVITY.icon(),
        "opencode" => OPENCODE.icon(),
        "cursor" | "cursor-agent" => CURSOR.icon(),
        "pi" => PI.icon(),
        _ => return None,
    })
}

pub(super) fn provider_icon(provider: ProviderKind) -> Option<Icon> {
    Some(match provider {
        ProviderKind::Claude => CLAUDE.icon(),
        ProviderKind::Codex => OPENAI.icon(),
        ProviderKind::Antigravity => ANTIGRAVITY.icon(),
        ProviderKind::OpenCode => OPENCODE.icon(),
        ProviderKind::Gemini => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

    #[test]
    fn bundled_brand_files_match_the_official_download_hashes() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("official/sources.json")).unwrap();
        let sources = manifest.as_array().unwrap();
        let files = [
            ("claude.svg", CLAUDE_SVG),
            ("openai-light.svg", OPENAI.light),
            ("openai-dark.svg", OPENAI.dark),
            ("antigravity.svg", ANTIGRAVITY_SVG),
            ("opencode-light.svg", OPENCODE.light),
            ("opencode-dark.svg", OPENCODE.dark),
            ("cursor-light.svg", CURSOR.light),
            ("cursor-dark.svg", CURSOR.dark),
            ("pi.svg", PI_SVG),
        ];
        assert_eq!(sources.len(), files.len());
        for (name, bytes) in files {
            let source = sources.iter().find(|s| s["file"] == name).unwrap();
            assert_eq!(
                source["sha256"],
                format!("{:x}", Sha256::digest(bytes)),
                "modified official asset: {name}"
            );
            assert_eq!(source["modifications"], "none");
        }
    }

    #[gpui::test]
    fn original_theme_variants_rasterize_at_icon_size(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            for mark in [&CLAUDE, &OPENAI, &ANTIGRAVITY, &OPENCODE, &CURSOR, &PI] {
                for bytes in [mark.light, mark.dark] {
                    let parsed = cx
                        .svg_renderer()
                        .parse_svg(bytes)
                        .expect("official SVG must parse");
                    cx.svg_renderer()
                        .render_parsed(
                            &parsed,
                            gpui::SvgSize::Size(gpui::size(
                                gpui::DevicePixels(32),
                                gpui::DevicePixels(32),
                            )),
                        )
                        .expect("official colors, filters and masks must rasterize");
                }
            }
        });
    }
}
