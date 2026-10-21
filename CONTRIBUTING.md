# Contributing to Krill

Krill은 순수 Rust macOS 앱이다. 처음이라면 [README.md](README.md), [아키텍처](docs/ARCHITECTURE.md), [로드맵](docs/ROADMAP.md), [코드 규칙](docs/CONVENTIONS.md), [디자인 시스템](docs/STYLEGUIDE.md) 순서로 읽는다. `docs/planning/`·`docs/research/`는 조사 기록이라 규범 문서와 다르면 규범 문서를 따른다.

## 작업 흐름

1. GitHub 이슈를 먼저 확인하고, 없으면 만든다. 한 이슈 = 하나의 완료 조건, 범위를 넓히지 않는다.
2. 개발 브랜치는 `dev`에서 따고 완료한 변경을 `dev`에 통합한다. `main`은 검증한 릴리스 이력이다. 커밋 첫 줄은 이슈가 있으면 `<이슈 ID>: <무엇을>` (예: `OWE-873: ⌘K 스크롤백 지우기 시 기준선 갱신`), 없으면 `<영역>: <무엇을>` (예: `krilld: …`, `scripts: …`). 한 커밋 = 한 논리 변경.
3. 수정 중에는 변경 크레이트의 `cargo check -p <크레이트> --locked`와 관련 테스트만 실행한다. 작업 완료·커밋 전에는 다음 범위를 확인한다:
   ```sh
   cargo fmt --all
   cargo clippy -p <크레이트> --all-targets --locked -- -D warnings
   cargo nextest run -p <크레이트> --locked
   ```
   작은 수정마다 전체 빌드·`scripts/check.sh`를 돌리지 않는다. IPC·PTY·세션 생존 계약을 바꾸면 해당 경로의 통합·생존 테스트도 확인한다. Markdown 문서 또는 `[profile.*]`의 `debug`·`split-debuginfo`만 바뀌면 AGENTS.md의 예외대로 `git diff --check`만 확인한다.
4. Push 정책: 메인테이너 또는 사용자의 명시적인 배포·릴리스·push 지시를 받은 에이전트는 필요한 `main`·`v*` 태그 push를 수행할 수 있다. `KRILL_ALLOW_PROTECTED_PUSH='<사용자 지시와 작업 사유>'`로 사유를 기록한다. 클론마다 `scripts/install-hooks.sh`로 pre-push 훅을 켜면 승인 사유와 공개 표면을 점검한다. 최종 HEAD 검사와 CI 통과는 필수다. 외부 기여는 fork 브랜치에서 PR로 보낸다.
5. PR은 템플릿을 채운다. 작은 PR을 선호한다.

매일 한국시간 자정에 로컬 맥의 [야간 배포](packaging/README.md#야간-자동-배포)가 커밋된 `dev`와 원격 `main`·`dev`를 별도 clone에 고정한다. 최종 버전 커밋에서 전체 검사·메모리·release 생존 E2E·Windows 검사·Windows/Linux 네이티브 빌드와 스모크·CI를 통과한 뒤에만 `main` 반영과 릴리스 게시를 한다. 변경이 없으면 생략하고, 실패하면 기존 릴리스를 유지한다. 미커밋 파일과 통합하지 않은 기능 브랜치는 배포 대상이 아니다.

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
