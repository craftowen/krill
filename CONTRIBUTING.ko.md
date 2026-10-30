# Krill 기여 안내

[English](CONTRIBUTING.md)

[README](README.ko.md)·[첫 실행](GETTING_STARTED.ko.md)에 이어 [AGENTS.md](AGENTS.md), [아키텍처](docs/ARCHITECTURE.md), [로드맵](docs/ROADMAP.md), [코드 규칙](docs/CONVENTIONS.md), [디자인 시스템](docs/STYLEGUIDE.md)을 읽습니다. ARCHITECTURE·ROADMAP·CONVENTIONS·STYLEGUIDE가 규범이며 조사 기록과 다르면 규범을 따릅니다.

## 작업 흐름

1. 기존 이슈를 확인하고 없으면 완료 조건이 명확한 이슈를 만듭니다. 이슈 밖 변경은 분리합니다.
2. 개발은 `dev` 기준이며 `main`은 검증한 릴리스 이력입니다. 외부 기여는 fork 브랜치에서 `dev`로 PR을 보냅니다. 한 커밋은 한 논리 변경입니다. 커밋 첫 줄은 이슈가 있으면 `OWE-873: …`, 없으면 `krilld: …`처럼 영역을 씁니다.
3. 아래 범위별 검사를 수행하고 PR 템플릿에 검사한 커밋·명령·결과·한계를 기록합니다.
4. 메인테이너가 완료한 변경을 `dev`에 통합합니다. 야간 배포는 원격 `dev`에 push한 변경만 봅니다. 미커밋 파일·push하지 않은 커밋·미통합 기능 브랜치는 대상이 아닙니다. 에이전트의 배포·보호 ref push는 명시 지시가 있어야 하며 PR 작성만으로 승인되지 않습니다.

클론마다 `scripts/install-hooks.sh`로 pre-push 훅을 켭니다. 명시적으로 승인된 보호 ref push에는 `KRILL_ALLOW_PROTECTED_PUSH='<사용자 지시와 작업 사유>'`를 기록하며 일상적인 우회 수단으로 쓰지 않습니다. [야간 배포](packaging/README.md#야간-자동-배포)는 원격 리비전을 별도 clone에 고정하고 검사 실패 시 기존 릴리스를 유지합니다.

## 변경 범위별 검사

반복 수정 중에는 변경 크레이트의 `cargo check -p <크레이트> --locked`와 관련 테스트만 실행합니다. 논리 변경 완료·커밋 전에는 포맷·영향받은 크레이트 clippy·관련 테스트를 확인합니다. 아래 `<크레이트>`는 실제 패키지명으로 바꿉니다.

```sh
cargo fmt --all -- --check
cargo clippy -p <크레이트> --all-targets --locked -- -D warnings
cargo nextest run -p <크레이트> --locked
git diff --check
```

IPC·공유 프로토콜·PTY/FFI·세션 생존 계약을 바꾸면 해당 경로의 통합·생존 테스트도 실행합니다. Windows 코드를 바꾸면 `scripts/check-windows.sh -p <크레이트>`도 확인하고 필요에 따라 `--clippy`를 추가합니다. 검사 대상 HEAD·실행 범위를 적고 부분 검사를 전체 검사로 보고하지 않습니다.

변경이 **전부 Markdown(`*.md`)**이거나 그 외 변경이 Cargo `[profile.*]`의 `debug`·`split-debuginfo`뿐이면 Cargo 검사 없이 `git diff --check`만 확인합니다. 해당 변경을 커밋할 때 메시지에 `check.sh 생략(<사유>)`를 적습니다. 스크립트·YAML·lint·코드 변경은 이 예외가 아니며 영향받는 동작을 검증합니다.

전체 `scripts/check.sh`는 작은 개발 변경·문서 PR마다 요구하지 않으며 **최종 릴리스 후보**에서 필수입니다. 포맷·clippy·nextest·doctest·의존성 정책·서드파티 고지·공개 표면을 검사하고 통과한 HEAD만 릴리스합니다. Windows 변경이 있는 후보는 `scripts/check-windows.sh --clippy`도 통과해야 하며 CI와 별도 릴리스 게이트도 유지합니다.

## 소스 빌드 환경

[소스 빌드 가이드](GETTING_STARTED.ko.md#macos-소스-빌드)를 따릅니다. MSRV는 Rust 1.98이며 1.99 전용 기능에 기대지 않습니다. 로컬 기준 툴체인은 Homebrew Rust이고 rustup은 필요하지 않습니다. 공용 메인테이너 작업 환경의 Cargo 산출물은 `CARGO_TARGET_DIR`를 덮어쓸 때도 `/Volumes/krill-build/targets/<이름>` 아래에만 둡니다. 내장 SSD에 target 디렉터리를 만들지 않습니다.

## 코드·리뷰 규칙

- 의존성 버전은 루트 `[workspace.dependencies]`에서만 정하고 `Cargo.lock`을 커밋하며 `--locked`를 씁니다.
- PTY는 `krill-holder`가 소유합니다. 앱·데몬 장애가 세션을 닫으면 안 되며 [장애별 보장](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)을 확인합니다.
- 비테스트 코드에서 `unwrap`·`expect`·`panic!`·`todo!`·`dbg!`를 금지하며 불변식은 근거 있는 `#[expect(..., reason = "...")]`로 한정합니다.
- `unsafe`는 holder·데몬 FFI와 앱 platform 모듈에만 쓰고 모든 블록에 `// SAFETY:` 근거를 적습니다.
- UI는 STYLEGUIDE·`krill_app::ui` 토큰과 프리미티브를 따르고 구현된 기능만 표시합니다.
- AGENTS·CONVENTIONS·scripts·lint/deny/fmt 등 하네스는 메인테이너 승인 없이 바꾸지 않습니다.
- Zed `terminal`·`terminal_view`(GPL-3.0) 코드는 열거나 복사하지 않습니다. 허용된 Orca 소스 차용에는 파일 상단에 출처·저작권을 적습니다. 허용 의존성 라이선스는 Apache-2.0·MIT·BSD-2/3·ISC·Unicode-3.0·Zlib 등이며 GPL 계열은 금지합니다.
- 측정값에 재현 근거를 붙이고 추정은 추정으로 표시합니다. 과거 벤치를 새로운 성능 주장으로 바꾸지 않습니다.

토큰·키·개인 경로·개인정보를 코드·로그·스크린샷에 올리지 않습니다. 비밀을 커밋했다면 즉시 메인테이너에게 알리고 이력 정리는 메인테이너가 결정합니다. 취약점은 [보안 정책](SECURITY.ko.md)으로 신고하고 [행동 강령](CODE_OF_CONDUCT.ko.md)을 따릅니다.
