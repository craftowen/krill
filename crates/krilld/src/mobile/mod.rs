//! 앱 수명과 독립적인 M9 모바일 인증·전송.
pub mod devices;
pub mod e2ee;
pub mod gateway;
pub mod net;
pub mod pwa;
pub(crate) mod pwa_rpc;
pub mod relay;

pub(crate) fn prewarm_agy_launch(home: &std::path::Path, config: Option<std::path::PathBuf>) {
    pwa_rpc::composer::prewarm_agy_launch(home, config);
}

pub(crate) fn evict_agy_models_cache() {
    pwa_rpc::composer::evict_agy_cache();
}

pub(crate) fn fold_agy_model_selection(
    config: Option<&std::path::Path>,
    model: &str,
    effort: Option<&str>,
) -> Option<(String, String)> {
    pwa_rpc::composer::fold_agy_model_selection(config, model, effort)
}

pub(crate) fn agy_version_problem(exe: &std::path::Path, ssh: bool) -> Option<(String, String)> {
    pwa_rpc::composer::agy_version_problem(exe, ssh)
}

pub(crate) fn agy_composer_ready(screen: &krill_proto::screen::Screen) -> bool {
    pwa_rpc::composer::agy_composer_ready(screen)
}

pub(crate) fn bot_launch_models(
    home: &std::path::Path,
    agents: &krill_proto::agent::Agents,
    agent: &str,
) -> krill_proto::bots::BotAgentModels {
    pwa_rpc::composer::launch_models(home, agents, agent)
}
mod project_icons;
mod relay_auth;
pub mod tailscale_serve;
mod workspaces;
mod ws;
