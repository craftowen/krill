# vendor/alacritty_terminal 패치 기록 (Krill 좁은 포크)

원본: crates.io `alacritty_terminal 0.26.0`(Apache-2.0, https://github.com/alacritty/alacritty).
`LICENSE-APACHE`·`README.md`·`CHANGELOG.md`·`Cargo.toml.orig`는 원본 그대로 둔다(Apache-2.0 §4(b)).
루트 `[patch.crates-io]`로 연결한다. `Cargo.toml`에서 뺀 것: 비루트 `resolver` 줄(경고만 냄),
`tests/` ref 스위트와 그에 딸린 `[[test]]`(46MB 녹화본, 빼고 vendor에 둠).

바뀐 줄마다 `mk-006 patch` 또는 `OWE-474 patch` 표시가 있다.
원본 대비 전체 차이는 `diff -ru -x .cargo-ok -x .cargo_vcs_info.json -x Cargo.lock`
`~/.cargo/registry/src/*/alacritty_terminal-0.26.0 vendor/alacritty_terminal`로 재생성한다.

## 접근자(동작 변경 없음, MK-006, 오웬 승인 2026-10-15)

| 위치 | 변경 | 이유 |
|---|---|---|
| `src/term/mod.rs` `Term` | `inactive_grid`·`active_charset`·`tabs`·`mode`·`scroll_region`·`colors`·`cursor_style`·`title`·`title_stack`·`keyboard_mode_stack`·`inactive_keyboard_mode_stack` → `pub` | 키프레임(MK-023)이 공개 API로 읽거나 쓸 수 없는 상태 |
| `src/term/mod.rs` `TabStops` | 타입과 `tabs` 필드 `pub` | 탭 정지 읽기·쓰기 |
| `src/grid/row.rs` `Row::occ` | `pub(crate)` → `pub` | 지우기 최적화 경계(키프레임 엔진 힌트) |

## 버그 수정(동작 변경, 오웬 승인 2026-10-15)

| 위치 | 버그 | 재현 |
|---|---|---|
| `src/grid/row.rs` `Row::grow` (`mk-006 patch (bug fix)`) | 열을 늘릴 때 `occ`를 안 올려 다음 `reset`이 옛 배경색 셀을 남긴다(`self.occ = self.inner.len()` 추가) | `spikes/mk-006/tests/findings.rs::alacritty_row_grow_leaves_stale_background` |
| `src/term/mod.rs` `push_keyboard_mode` (`OWE-474 patch (bug fix)`) | 키보드 스택이 가득 차면 키보드 스택 대신 **제목 스택**에서 `remove(0)` → 제목 스택이 비어 있으면 panic, 아니면 저장된 제목이 하나 사라진다(`push_title`에서 복사한 줄, `keyboard_mode_stack.remove(0)`으로 수정). 출력 바이트(`CSI > 1 u` × 4097)만으로 Term이 죽는다 | `crates/krilld/src/term/tests.rs::kitty_keyboard_stack_overflow_evicts_oldest` (상류 PR 본문 초안: `docs/spikes/OWE-474.md`) |
