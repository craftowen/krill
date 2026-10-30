# AGENTS.md — Krill

AI 에이전트(와 사람)가 이 저장소에서 일할 때 가장 먼저 읽는 **유일한** 지시 파일이다. `CLAUDE.md`는 이 파일을 import하는 한 줄짜리 포인터다 — 수정은 여기서만.

Krill = 어떤 AI 코딩 에이전트든 끊기지 않게 병렬로 돌리는 순수 Rust macOS 앱. 앱(gpui)과 세션 데몬(krill-holder + krilld)이 분리돼, 앱이 죽거나 업데이트돼도 세션이 산다. 제품·설계: [docs/PRD.md](docs/PRD.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/ROADMAP.md](docs/ROADMAP.md). 코드 규칙: [docs/CONVENTIONS.md](docs/CONVENTIONS.md). UI 디자인 시스템: [docs/STYLEGUIDE.md](docs/STYLEGUIDE.md) — 아래 요약을 건너뛰지 말 것.

**문서 우선순위**: ARCHITECTURE·ROADMAP·CONVENTIONS·STYLEGUIDE가 규범이다. 내부 기획·조사·벤치마크·QA 기록은 로컬 전용이며 공개 저장소에 추가하지 않는다. 라이선스 고지와 제품·기여 문서만 공개한다.

## 디자인 시스템 (`docs/STYLEGUIDE.md`)

모든 UI/UX 작업(레이아웃, 색, 타이포그래피, 간격·반경, 컴포넌트 선택, 화면 전환, UX 동작)은 [docs/STYLEGUIDE.md](docs/STYLEGUIDE.md)와 [`crates/krill-app/src/ui/`](crates/krill-app/src/ui/mod.rs)(`theme`·`primitives`·`icons`·`status`)를 따른다. `ui/`와 `terminal.rs` 밖에서 생 색상(`gpui::rgb`/`hsla`/`black`/`white`, `ui::hex`/`hexa`)이나 임의 픽셀 글자 크기(`.text_size(px(..))`)를 하드코딩하지 않는다(`cargo nextest`가 차단). Orca UI를 참고하더라도 **Krill에 실제 구현된 기능만 표기**하며, 관리자/설정 페이지는 새 창이 아니라 메인 창 안 페이지 전환으로 연다.

## 명령

```bash
scripts/check.sh            # 릴리스 후보의 전체 통합 검사: 비Cargo 회귀 · fmt · clippy · nextest · doctest · cargo deny · NOTICES --check · 공개 표면
python3 scripts/check-regressions.py --scope harness   # Python·Git 훅·빌드 경로·검사 필터 회귀(빌드 없음)
python3 scripts/check-regressions.py --scope mobile    # 독립 Node 회귀(데몬·브라우저 없음)
python3 scripts/check-regressions.py --scope packaging # 패키징 계약 회귀(네이티브 서명·DMG 검사는 별도)
python3 scripts/check-regressions.py --scope telemetry # 수집기·다운로드 통계 회귀(임시 DB·로컬 HTTP)
cargo fmt --all             # 포맷 적용
cargo nextest run -p <크레이트>   # 한 크레이트 테스트
cargo build --workspace --bins && cargo run -p krill-app   # 앱 실행(보조 바이너리 krill·krill-holder·krilld를 먼저 빌드해야 한다)
scripts/check-windows.sh [-p <크레이트>] [--clippy]   # Mac에서 Windows 타깃 cargo check(저장소 밖 격리 툴체인, OWE-811). Windows 코드를 건드리면 0이어야 한다
scripts/install-hooks.sh    # 클론마다 1회: git pre-push 훅(보호 ref 승인 사유 확인 + 공개 표면 점검) 켜기
```

- 반복 수정 중에는 변경한 크레이트의 `cargo check -p <크레이트> --locked`와 변경 동작에 관련된 테스트만 실행한다. 작은 수정마다 워크스페이스 전체를 빌드·검사하지 않는다.
- 시작할 때 `git status`로 기존 작업과 소유 범위를 확인한다. 다른 작업의 미커밋 변경을 덮어쓰거나 자신의 변경으로 커밋하지 않는다. 과거 이력의 수정은 현재 소스·테스트와 대조한 뒤 반영한다.
- 로컬 macOS의 Cargo 산출물(`CARGO_TARGET_DIR`·`--target-dir`·Cargo 설정)은 **마운트된 `/Volumes/krill-build/targets/<이름>` 아래에만** 둔다. 내장 SSD로 폴백하거나 활성 빌드의 캐시를 지우지 않는다. 빌드 전 `python3 scripts/check-build-target.py`로 실제 Cargo 설정을 확인한다(`check.sh`·Windows 교차 검사도 실행). GitHub Actions 러너·네이티브 Windows 경로에는 이 로컬 규칙을 적용하지 않는다.
- 변경 경계별 재발 방지와 검증 선택은 [CONVENTIONS §11](docs/CONVENTIONS.md#11-변경-경계별-재발-방지)를 따른다. 비Cargo 검사에는 Python 3.9 이상·Node 22.13 이상이 필요하다. `check-regressions.py --list`로 범위를 확인하고 `--scope`는 여러 번 지정할 수 있다. 브라우저·네이티브 실행·서명·메모리 측정은 해당 변경이 필요로 할 때 별도로 검증한다.
- 논리 변경을 마무리하거나 커밋할 때는 포맷 검사, 영향받은 크레이트의 clippy, 관련 테스트를 확인한다. 이 범위의 검증으로 개발 작업 완료·커밋이 가능하며, 전체 `scripts/check.sh`를 요구하지 않는다.
- IPC·공유 프로토콜·PTY/FFI·세션 생존 등 여러 크레이트의 계약을 바꾸면 해당 경로의 통합·생존 테스트도 개발 중에 실행한다. 전체 통합 검사와 구분한다.
- 전체 `scripts/check.sh`는 변경을 모은 최종 릴리스 후보에서 실행하고, 0으로 끝난 HEAD만 릴리스한다. Windows 변경이 포함된 후보는 `scripts/check-windows.sh --clippy`도 통과해야 한다. 개발 중 Windows 검증은 영향받은 크레이트로 한정한다.
- 검사 결과에는 대상 HEAD·관련 미커밋 변경(필요시 파일 해시)과 명령·종료 코드·실행 범위를 기록한다. 이미 통과한 동일 코드·입력·도구 설정의 검사는 반복하지 않으며, 릴리스 후보 코드가 바뀌면 최종 HEAD에서 다시 확인한다. 부분 검사·재시도로 통과한 FLAKY·교차 컴파일·헤드리스 fixture를 각각 전체 검사·첫 실행 통과·실기 실행·실제 화면 검증으로 보고하지 않는다.
- 바뀐 파일이 전부 Markdown 문서(`*.md`)이거나 `Cargo.toml`의 `[profile.*]` 디버그 정보 설정(`debug`·`split-debuginfo`)뿐이면 Cargo 검사를 생략하고 `git diff --check`만 확인한다. 다른 코드·스크립트·lint·deny 설정이 섞이면 위 영향 범위별 검증을 따른다. 생략했으면 커밋 메시지에 `check.sh 생략(<사유>)`를 적는다.

툴체인: Homebrew `rustc 1.98.1`(rustup 없음 → `rust-toolchain.toml` 대신 `Cargo.toml`의 `rust-version = "1.98"`로 MSRV 고정). 최신 stable 1.99 전용 기능(패키지 레벨 default-features 덮어쓰기 등)에 기대지 않는다. Xcode 없음(CommandLineTools만) — gpui는 `font-kit` + `runtime_shaders` 기능으로 빌드. Apple Developer 인증서 없음(MK-104).

## 구조

```
Cargo.toml            # 워크스페이스, [workspace.lints], [workspace.dependencies] — 의존성 버전은 여기서만
crates/krill-proto/   # 와이어 포맷(프레임·메시지·키프레임). 엔진·UI 무관. 모든 프로세스가 공유
crates/krill-holder/  # PTY 부모·fd 보관·exec 핸드오프. macOS 빌드 1,300줄 이하 + Linux 전용 sys/linux.rs 300줄 이하 + Windows 빌드 2,000줄 이하(결정 12-2, 2026-10-19 개정, 플랫폼별로 셈 — CONVENTIONS §5), 거의 안 바뀜. unsafe FFI 집중 구역
crates/krilld/        # 세션 데몬: alacritty_terminal·IPC·zstd 세그먼트·hook·상태·git 큐·모바일 게이트웨이
crates/krill-term/    # 프레임워크 중립 그리드 렌더 모델 + IME 상태기계(gpui 실패 시 교체 대비)
crates/krill-hosts/   # 원격 호스트 연결 계층(SSH 소켓 포워딩·피어 관리)
crates/krill-link/    # iroh P2P 원격 터널·기기 페어링
crates/krill-log/     # 상한 회전 운영 로거
crates/krill-app/     # gpui UI (디자인 토큰·프리미티브·아이콘·상태는 src/ui/, 규범은 docs/STYLEGUIDE.md)
crates/krill/         # CLI 단일 바이너리(`krill`, `krill hook` 포함)
docs/STYLEGUIDE.md    # UI/UX 디자인 시스템 규범(Orca STYLEGUIDE·main.css 기반, 모든 UI 작업 시 필수 참조)
docs/licensing/       # 공개 라이선스 고지·생성 근거 (내부 조사 기록은 Git 제외)
scripts/              # check.sh, memory-gate.sh, check-windows.sh
packaging/            # macOS 번들·DMG·Ed25519 매니페스트·설치 스크립트
```

## 반드시 지킬 것

1. **세션은 절대 잃지 않는다.** PTY는 krill-holder만 소유한다. krilld·앱·CLI의 어떤 실패도 PTY를 닫으면 안 된다. 의심되면 생존 테스트(앱 kill -9 / krilld kill -9)를 돌린다.
2. **메모리 예산**: 앱 ≤ 120MB, 데몬 ≤ 60MB(터미널 10개, phys_footprint). 셀 스크롤백 1만 줄 금지(핫 1,000행 + zstd 콜드).
3. **질의 응답자는 krilld 하나.** UI는 터미널 질의에 응답하지 않는다.
4. **Zed 터미널 코드는 GPL-3.0 — 읽고 베끼지 않는다**(클린룸). gpui 자체는 Apache-2.0.
5. **unsafe는 krill-holder·krilld의 FFI 모듈(Unix·Windows FFI 포함)과 krill-app의 platform 모듈(objc2·Win32)에만.** 모든 `unsafe` 블록에 `// SAFETY:` 주석.
6. **데몬 panic 격리**: krilld는 세션 작업 단위 `catch_unwind`(panic=unwind, CONVENTIONS P4), krill-holder는 panic 금지. 비테스트 코드 `unwrap`/`expect`/`panic!` 금지(clippy가 막음, 불변식은 `#[expect(..., reason)]`). C·objc2 콜백 밖으로 panic이 나가지 않게.
7. 측정하지 않은 수치는 문서·커밋·이슈에 쓰지 않는다. 추정이면 "추정".
8. 하네스 파일(`AGENTS.md`, `docs/CONVENTIONS.md`, `scripts/`, lint·deny·fmt 설정)은 메인테이너 승인 없이 바꾸지 않는다.
   기계 강제: `.claude` 훅(`.claude/hooks/guard.sh`, Claude Code 전용)·`.github/CODEOWNERS`·git `pre-push`(`scripts/hooks/`).
9. 사용자가 배포·릴리스·push를 명시적으로 지시하면 에이전트는 그 범위에 필요한 `main` 반영·push, `v*` 태그 push, 게시(`publish-github.sh`)까지 수행한다. 같은 작업의 승인을 단계마다 다시 묻지 않는다. 보호 ref push에는 `KRILL_ALLOW_PROTECTED_PUSH='<사용자 지시와 작업 사유>'`를 지정해 사유를 기록한다. Claude Code의 원격 명령은 첫 줄에 `# KRILL_ALLOW_REMOTE: <사용자 지시와 작업 사유>`를 적으면 현재 세션에서 실행할 수 있다. 승인 표시는 실제 사용자 지시가 있을 때만 쓴다. 최종 HEAD 검사·CI·공개 표면 점검과 기존 릴리스 덮어쓰기 금지는 유지한다. 그 외 원격 쓰기·원격 설정 변경·유료 작업은 별도의 명시 지시가 있을 때만 한다.
10. **UI/UX는 [docs/STYLEGUIDE.md](docs/STYLEGUIDE.md)와 `krill_app::ui` 토큰·프리미티브만 쓴다.** 미구현 기능을 UI에 미리 표기하지 않는다.

## 이슈·작업 흐름

- 로드맵 ID(`[MK-xxx]`)는 `docs/ROADMAP.md` 행 ID다.
- 한 커밋 = 한 논리 변경. 개발 작업 완료·커밋은 위 단계별 검증 규칙을 따르고, 전체 `scripts/check.sh` 통과는 릴리스 후보의 필수 조건이다.
- 장애별 보장 범위(무엇이 살아남고 무엇을 잃을 수 있는지)는 ARCHITECTURE §4.1이 규범이다.
