//! Krill 봇 UI(홈·커넥터·설정·그룹·아바타·템플릿) 및 내장 에이전트 모델 목록 크레이트.

// 기존 `bots/*.rs`의 `crate::ui`, `crate::i18n`, `crate::settings` 등 참조를
// 파일 무수정으로 유지하면서 공개 API는 `agent_models`·`bots`만 노출한다.
#[cfg(test)]
pub(crate) use krill_app_core::updater;
pub(crate) use krill_app_core::{
    accounts, hosts, i18n, jump, local_time, onboarding, platform, pull_request, settings,
    terminal, ui,
};

pub mod agent_models;
pub mod bots;
