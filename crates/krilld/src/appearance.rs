//! 질의 응답에 쓰는 UI 값(MK-020): 색 테마와 셀 픽셀 크기. UI가 푸시하고 모든 세션이 읽는다.
//!
//! - 테마는 받을 때마다 디스크에 저장하고, krilld가 다시 뜨면 그 값으로 시작한다. 파일이 없거나 읽히지 않으면
//!   [`krill_proto::theme::DEFAULT`]로 답한다. 앱이 한 번도 붙지 않아도 색 질의에 즉시 답한다(MK-008 §3: 침묵하면 에이전트
//!   첫 화면이 0.25~0.7초 늦었다).
//! - 셀 픽셀 크기는 저장하지 않는다. 받기 전에는 `CSI 14t`에 답하지 않는다(MK-008 §2 8번: 침묵해도 지연 없음).

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;
use std::sync::Mutex;

use krill_proto::theme::{DEFAULT, Theme};

use crate::session::lock;

pub use krill_proto::theme::CellSize;

#[derive(Debug)]
struct State {
    theme: Theme,
    cell: Option<CellSize>,
}

/// krilld 전체에 하나. 세션은 `Arc`로 나눠 갖는다.
#[derive(Debug)]
pub struct Appearance {
    path: PathBuf,
    state: Mutex<State>,
}

impl Appearance {
    /// `path`에 저장된 마지막 테마로 시작한다. 없거나 깨졌으면 내장 기본 테마.
    pub fn load(path: PathBuf) -> Self {
        let theme = fs::read(&path)
            .ok()
            .and_then(|b| Theme::decode(&b).ok())
            .unwrap_or(DEFAULT);
        Self {
            path,
            state: Mutex::new(State { theme, cell: None }),
        }
    }

    /// UI가 보낸 테마를 적용하고 디스크에 저장한다. 바뀌었으면 `true` — 호출자는 세션마다
    /// [`Session::theme_changed`](crate::Session::theme_changed)를 부른다. 저장이 실패해도 메모리 값은 바뀐다.
    pub fn set_theme(&self, theme: Theme) -> io::Result<bool> {
        let mut s = lock(&self.state);
        let changed = s.theme != theme;
        s.theme = theme;
        self.save(&theme)?;
        Ok(changed)
    }

    pub fn set_cell_size(&self, cell: CellSize) {
        lock(&self.state).cell = Some(cell);
    }

    pub fn theme(&self) -> Theme {
        lock(&self.state).theme
    }

    pub fn cell_size(&self) -> Option<CellSize> {
        lock(&self.state).cell
    }

    /// 임시 파일(0600)에 쓰고 rename한다. 중간에 죽어도 이전 파일이나 새 파일 중 하나만 남는다.
    fn save(&self, theme: &Theme) -> io::Result<()> {
        if let Some(dir) = self.path.parent().filter(|d| !d.as_os_str().is_empty()) {
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        let bytes = theme.encode().map_err(io::Error::other)?;
        let tmp = self.path.with_extension("tmp");
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?
            .write_all(&bytes)?;
        fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use krill_proto::theme::Rgb;

    use super::*;

    #[test]
    fn last_theme_survives_restart() {
        let dir = std::env::temp_dir().join(format!("mk020-look-{}", std::process::id()));
        let path = dir.join("state/theme");
        fs::remove_dir_all(&dir).ok();

        let a = Appearance::load(path.clone());
        assert_eq!(a.theme(), DEFAULT, "파일이 없으면 내장 기본 테마");
        assert_eq!(a.cell_size(), None);

        let mut light = DEFAULT;
        light.background = Rgb {
            r: 0xfa,
            g: 0xfa,
            b: 0xfa,
        };
        assert!(a.set_theme(light).unwrap());
        assert!(!a.set_theme(light).unwrap(), "같은 테마는 변경 아님");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        // krilld 재기동
        assert_eq!(Appearance::load(path.clone()).theme(), light);

        // 깨진 파일은 기본 테마로
        fs::write(&path, b"x").unwrap();
        assert_eq!(Appearance::load(path).theme(), DEFAULT);
        fs::remove_dir_all(&dir).unwrap();
    }
}
