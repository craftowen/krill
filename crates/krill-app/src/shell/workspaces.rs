//! 탭 표시는 활성 워크스페이스(호스트 + 워크트리/폴더)에 한정한다.
//! 전체 레이아웃은 그대로 보관하므로 다른 워크스페이스의 세션·분할·탭 속성도 유지된다.

use super::*;
use krill_app::hosts::HostId;

#[cfg(all(test, unix))]
mod tests;

#[derive(Debug, PartialEq, Eq)]
struct Workspace {
    host: HostId,
    folder: PathBuf,
}

impl Shell {
    fn tab_workspace(&self, index: usize, cx: &App) -> Option<Workspace> {
        // 분할 포커스 이동만으로 탭의 소속이 바뀌지 않게 첫 칸을 기준으로 삼는다.
        let pane = *self.layout.tab_panes(index).first()?;
        let host = self.layout.host(pane).clone();
        // 실행 폴더는 영속 상태다. 라이브 cwd(cd)는 워크스페이스 소속을 바꾸지 않는다.
        let folder = self
            .layout
            .launch(pane)
            .map(|launch| PathBuf::from(std::ffi::OsString::from_vec(launch.cwd.clone())))
            .or_else(|| {
                self.layout
                    .session(pane)
                    .and_then(|sid| self.sidebar.read(cx).folder_on(&host, sid))
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_else(|| {
                PathBuf::from(std::ffi::OsString::from_vec(self.launch_for(pane).cwd))
            });
        let folder = self
            .sidebar
            .read(cx)
            .workspace_for_path_on(&host, &folder)
            .unwrap_or(folder);
        Some(Workspace { host, folder })
    }

    pub(super) fn workspace_tabs(&self, cx: &App) -> Vec<usize> {
        self.tabs_in_workspace(self.layout.active(), cx)
    }

    pub(super) fn tabs_in_workspace(&self, index: usize, cx: &App) -> Vec<usize> {
        let Some(workspace) = self.tab_workspace(index, cx) else {
            return Vec::new();
        };
        (0..self.layout.tabs().len())
            .filter(|&i| self.tab_workspace(i, cx).as_ref() == Some(&workspace))
            .collect()
    }

    pub(super) fn cycle_workspace_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let tabs = self.workspace_tabs(cx);
        if let Some(pos) = tabs.iter().position(|&i| i == self.layout.active()) {
            let next = (pos.cast_signed() + delta)
                .rem_euclid(tabs.len().cast_signed())
                .cast_unsigned();
            self.change(true, cx, |l| l.activate(tabs[next]));
        }
    }

    /// 전체 인덱스를 유지해 닫기·드래그 등 기존 액션과 같은 탭을 가리킨다.
    pub(super) fn workspace_tab_labels(&self, cx: &App) -> Vec<(usize, String)> {
        let indices = self.workspace_tabs(cx);
        let mut labels: Vec<_> = indices
            .iter()
            .map(|&i| self.tab_display_label(i, cx))
            .collect();
        tabs::number_duplicates(&mut labels);
        indices.into_iter().zip(labels).collect()
    }

    pub(super) fn close_workspace_tabs(
        &mut self,
        closing: Vec<usize>,
        prefer: usize,
        cx: &mut Context<Self>,
    ) {
        let remaining = std::iter::once(prefer)
            .chain(self.workspace_tabs(cx))
            .find(|i| !closing.contains(i))
            .and_then(|i| self.layout.tabs().get(i))
            .map(|t| t.focused);
        for &i in &closing {
            self.record_closed_tab(i, cx);
        }
        self.close_layout(cx, |l| {
            for i in closing.into_iter().rev() {
                l.close_tab(i);
            }
            if let Some(pane) = remaining
                && let Some(index) = l.tabs().iter().position(|tab| tab.root.contains(pane))
            {
                l.activate(index);
                l.focus(pane);
            }
        });
    }
}
