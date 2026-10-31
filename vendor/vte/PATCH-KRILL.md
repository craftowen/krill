# vendor/vte 패치 기록 (Krill 좁은 포크)

원본: crates.io `vte 0.15.0`(Apache-2.0 또는 MIT, https://github.com/alacritty/vte).
`LICENSE-APACHE`·`LICENSE-MIT`·`Cargo.toml.orig`는 원본 그대로 둔다(Apache-2.0 §4(b)).
루트 `[patch.crates-io]`로 연결한다(`alacritty_terminal`이 `^0.15`로 요구).

바뀐 줄마다 `mk-006 patch` 표시가 있다.
원본 대비 전체 차이는 `diff -ru -x .cargo-ok -x .cargo_vcs_info.json -x Cargo.lock`
`~/.cargo/registry/src/*/vte-0.15.0 vendor/vte`로 재생성한다.

## 접근자(동작 변경 없음, MK-006, 오웬 승인 2026-10-15)

| 위치 | 변경 | 이유 |
|---|---|---|
| `src/lib.rs` `Parser::mk_is_ground` | 추가 | 파서 정지점 판정(ground + 미완성 UTF-8 없음) |
| `src/ansi.rs` `Processor::mk_is_quiescent`·`mk_preceding_char` | 추가 | 정지점(ground + sync 아님), REP 직전 문자 |

## 메모리 회수 (2026-10-30)

| 위치 | 변경 | 이유 |
|---|---|---|
| `src/ansi.rs` `Processor::mk_trim_sync_buffer` | 정지점의 빈 sync 버퍼 용량만 반환 | 숨긴 세션 강등 시 파서를 초기화하면 REP 직전 문자를 잃어 이후 출력과 커서 위치가 어긋난다. 파서 상태를 보존하면서 버퍼 메모리를 회수한다. |

## 버그 수정(동작 변경, 오웬 승인 2026-10-15)

| 위치 | 버그 | 재현 |
|---|---|---|
| `src/lib.rs` `advance_partial_utf8` (`mk-006 patch (bug fix)`) | 읽기 경계가 UTF-8 문자를 가르고 다음 바이트가 "ASCII + 미완성 다바이트"면 ASCII 한 글자를 버린다(`valid_bytes - old_bytes` → `c.len_utf8() - old_bytes`) | `spikes/mk-006/tests/findings.rs::vte_split_utf8_drops_next_char` |
