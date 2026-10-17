use std::collections::BTreeSet;

use gpui::{AppContext as _, Context, Entity, Render, TestAppContext, Window, div, prelude::*};
use krill_app::i18n::{self, ALL, Language, catalog};
use krill_app::{jump, search, settings};

fn fields(template: &str) -> BTreeSet<&str> {
    template
        .split('{')
        .skip(1)
        .map(|field| {
            assert!(field.contains('}'), "unclosed placeholder: {field}");
            field.split_once('}').map_or(field, |(name, _)| name)
        })
        .collect()
}

#[test]
fn catalog_has_no_missing_translations_or_mismatched_placeholders() {
    let mut ids = BTreeSet::new();
    for key in ALL {
        assert!(ids.insert(key.id), "duplicate key {}", key.id);
        let ko = key.in_language(Language::Ko);
        let en = key.in_language(Language::En);
        assert!(!ko.trim().is_empty() && !en.trim().is_empty(), "{}", key.id);
        assert_eq!(fields(ko), fields(en), "{}", key.id);
        assert!(
            !en.chars().any(|c| ('\u{ac00}'..='\u{d7a3}').contains(&c)),
            "{}: {en}",
            key.id
        );
    }
    assert_eq!(
        settings::Settings::parse("").unwrap().language,
        Language::Ko
    );
    assert!(settings::Settings::parse("language = 'fr'").is_err());
    for language in [Language::Ko, Language::En] {
        let value = settings::Settings {
            language,
            ..settings::Settings::default()
        };
        assert_eq!(settings::Settings::parse(&value.to_toml()).unwrap(), value);
    }
}

struct Views {
    search: Entity<search::Bar>,
    jump: Entity<jump::Palette>,
}

impl Render for Views {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(self.search.clone()).child(self.jump.clone())
    }
}

#[gpui::test]
fn ko_en_ko_switch_refreshes_existing_inputs_results_and_saved_settings(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!("mk-i18n-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        settings::init_at(Some(dir.clone()), cx);
    });
    let (views, cx) = cx.add_window_view(|window, cx| Views {
        search: cx.new(|cx| search::Bar::new(window, cx)),
        jump: cx.new(|cx| jump::Palette::new(vec![], window, cx)),
    });
    cx.update(|window, cx| {
        let bar = views.read(cx).search.clone();
        let input = bar.read(cx).input().clone();
        input.update(cx, |input, cx| {
            input.set_value("한글 draft {id}", window, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let bar = views.read(cx).search.clone();
        bar.update(cx, |bar, cx| {
            bar.set_result(
                Some(&krill_proto::search::SearchResult {
                    req: 1,
                    snapshot: krill_proto::search::Snapshot::default(),
                    total: 0,
                    hit: None,
                    wrapped: false,
                    stale: true,
                    incomplete: false,
                    approximate: false,
                }),
                cx,
            );
        });
    });
    let cached_ko = catalog::shell::LAYOUT_RECOVERED
        .in_language(Language::Ko)
        .to_owned();
    for language in [Language::En, Language::Ko] {
        cx.update(|_, cx| settings::set_language(language, cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert_eq!(settings::current(cx).language, language);
            assert_eq!(i18n::language(), language);
            for key in ALL {
                assert_eq!(key.get(), key.in_language(language), "{}", key.id);
            }
            assert_eq!(
                settings::Settings::parse(
                    &std::fs::read_to_string(dir.join("settings.toml")).unwrap()
                )
                .unwrap()
                .language,
                language
            );
            let view = views.read(cx);
            let bar = view.search.read(cx);
            assert_eq!(
                bar.input().read(cx).presentation().placeholder().as_ref(),
                catalog::search::PLACEHOLDER.get()
            );
            assert_eq!(
                view.jump
                    .read(cx)
                    .input()
                    .read(cx)
                    .presentation()
                    .placeholder()
                    .as_ref(),
                catalog::jump::PLACEHOLDER.get()
            );
            assert_eq!(bar.input().read(cx).value().as_ref(), "한글 draft {id}");
            assert_eq!(
                bar.status(),
                format!(
                    "{} {}",
                    catalog::search::EMPTY.get(),
                    catalog::search::STALE.get()
                )
            );
            let suffix = ": literal {id} 한국어 error";
            assert_eq!(
                i18n::cached_notice(&format!("{cached_ko}{suffix}")),
                format!("{}{suffix}", catalog::shell::LAYOUT_RECOVERED.get())
            );
            assert_eq!(
                i18n::cached_notice("settings.toml: 설정 형식이 올바르지 않습니다\nkeys.toml: raw"),
                format!(
                    "settings.toml: {}\nkeys.toml: raw",
                    catalog::settings::INVALID_SETTINGS.get()
                )
            );
            assert_eq!(
                i18n::cached_notice("VS Code 열기 실패: broken"),
                catalog::open_in::FAILED.format(&[("app", "VS Code"), ("error", "broken")])
            );
            assert_eq!(
                catalog::security::PASTE_WARNING
                    .format(&[("lines", "2"), ("preview", "{lines} 한글")]),
                catalog::security::PASTE_WARNING
                    .get()
                    .replace("{lines}", "2")
                    .replace("{preview}", "{lines} 한글")
            );
        });
    }
    // External edits share the same path. Invalid input retains the previous language.
    std::fs::write(dir.join("settings.toml"), "language = 'en'").unwrap();
    cx.update(|_, cx| settings::reload(cx));
    cx.run_until_parked();
    assert_eq!(i18n::language(), Language::En);
    std::fs::write(dir.join("settings.toml"), "language = 'unsupported'").unwrap();
    cx.update(|_, cx| settings::reload(cx));
    cx.run_until_parked();
    assert_eq!(i18n::language(), Language::En);
    // Reopening configuration picks up persisted language; absent language keeps ko compatibility.
    cx.update(|_, cx| settings::init_at(Some(dir.clone()), cx));
    std::fs::write(dir.join("settings.toml"), "theme = 'dark'").unwrap();
    cx.update(|_, cx| settings::reload(cx));
    cx.run_until_parked();
    assert_eq!(i18n::language(), Language::Ko);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn embedded_input_and_diff_controls_have_korean_translations_without_fallback() {
    let source = include_str!("../../../vendor/gpui-component/locales/ui.yml");
    let backend = gpui_component::_rust_i18n_backend();
    let mut group = "";
    for line in source.lines() {
        if !line.starts_with(' ') && !line.starts_with('#') {
            group = line.trim_end_matches(':');
        } else if line.starts_with("  ") && !line.starts_with("    ") {
            let name = line.trim().trim_end_matches(':');
            if group == "Input" || group == "Diff" || (group == "Dock" && name == "Unnamed") {
                let key = format!("{group}.{name}");
                let ko = backend
                    .translate("ko", &key)
                    .unwrap_or_else(|| panic!("missing ko {key}"));
                let en = backend
                    .translate("en", &key)
                    .unwrap_or_else(|| panic!("missing en {key}"));
                assert!(!ko.is_empty());
                assert_ne!(ko, en, "{key}");
                assert_eq!(fields(&ko), fields(&en), "{key}");
            }
        }
    }
    assert_eq!(
        backend.translate("ko", "Input.Copy").as_deref(),
        Some("복사")
    );
}
