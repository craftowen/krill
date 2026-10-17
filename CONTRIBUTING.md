# Contributing to Krill

Krill은 순수 Rust macOS 앱이다. 처음이라면 [README.md](README.md), [아키텍처](docs/ARCHITECTURE.md), [로드맵](docs/ROADMAP.md), [코드 규칙](docs/CONVENTIONS.md), [디자인 시스템](docs/STYLEGUIDE.md) 순서로 읽는다. `docs/planning/`·`docs/research/`는 조사 기록이라 규범 문서와 다르면 규범 문서를 따른다.

## 작업 흐름

1. GitHub 이슈를 먼저 확인하고, 없으면 만든다. 한 이슈 = 하나의 완료 조건, 범위를 넓히지 않는다.
2. 브랜치는 `main`에서 따고, 커밋 첫 줄은 `<이슈번호>: <무엇을>` (예: `123: 세션 목록 페이징`). 한 커밋 = 한 논리 변경.
3. Push 전 필수 (`scripts/check.sh`가 0으로 끝나야 병합된다):
   ```sh
   cargo fmt --all
   perl -e 'alarm shift; exec @ARGV' 300 scripts/check.sh
   ```
   검사는 fmt · clippy(`-D warnings`) · nextest · doctest · `cargo deny --locked`다. 무거운 cargo는 `perl -e 'alarm shift; exec @ARGV' 300`으로 감싼다.
4. PR은 템플릿을 채운다. 작은 PR을 선호한다.

## 코드 규칙 요약 (상세는 CONVENTIONS.md)

- 의존성 버전은 루트 `[workspace.dependencies]`에만. `Cargo.lock` 커밋, `--locked` 빌드.
- 비테스트 코드 `unwrap`·`expect`·`panic!`·`todo!`·`dbg!` 금지. 불변식은 `#[expect(..., reason = "...")]`.
- `unsafe`는 `krill-holder`·`krilld`의 FFI 모듈과 `krill-app`의 `platform` 모듈에만, 모든 블록에 `// SAFETY:` 주석.
- Zed `terminal`·`terminal_view`(GPL-3.0) 코드는 열지 않는다. Orca 소스 차용 시 파일 상단에 출처·저작권 표기.
- 허용 라이선스: Apache-2.0, MIT, BSD-2/3, ISC, Unicode-3.0, Zlib. GPL 계열 금지.
- 측정하지 않은 수치는 문서·커밋·이슈에 쓰지 않는다. 추정은 "추정"이라고 쓴다.

## 보고 금지·주의

- 보안 문제는 이슈·PR에 올리지 말고 [SECURITY.md](SECURITY.md) 절차를 따른다.
- 토큰·키·개인정보를 커밋하지 않는다. 실수로 올렸으면 즉시 maintainers에게 알린다 (이력 정리는 maintainers가 판단).
