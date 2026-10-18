# AGENTS.md — Krill

AI 에이전트(와 사람)가 이 저장소에서 일할 때 가장 먼저 읽는 **유일한** 지시 파일이다. `CLAUDE.md`는 이 파일을 import하는 한 줄짜리 포인터다 — 수정은 여기서만.

Krill = 어떤 AI 코딩 에이전트든 끊기지 않게 병렬로 돌리는 순수 Rust macOS 앱. 앱(gpui)과 세션 데몬(krill-holder + krilld)이 분리돼, 앱이 죽거나 업데이트돼도 세션이 산다. 제품·설계: [docs/PRD.md](docs/PRD.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md), [docs/ROADMAP.md](docs/ROADMAP.md). 코드 규칙: [docs/CONVENTIONS.md](docs/CONVENTIONS.md). UI 디자인 시스템: [docs/STYLEGUIDE.md](docs/STYLEGUIDE.md) — 아래 요약을 건너뛰지 말 것.

**문서 우선순위**: ARCHITECTURE·ROADMAP·CONVENTIONS·STYLEGUIDE가 규범이다. `docs/planning/T*-final.md`·`docs/research/*`는 조사 기록이라 규범 문서와 다르면 규범 문서를 따른다.

## 디자인 시스템 (`docs/STYLEGUIDE.md`)

모든 UI/UX 작업(레이아웃, 색, 타이포그래피, 간격·반경, 컴포넌트 선택, 화면 전환, UX 동작)은 [docs/STYLEGUIDE.md](docs/STYLEGUIDE.md)와 [`crates/krill-app/src/ui/`](crates/krill-app/src/ui/mod.rs)(`theme`·`primitives`·`icons`·`status`)를 따른다. `ui/`와 `terminal.rs` 밖에서 생 색상(`gpui::rgb`/`hsla`/`black`/`white`, `ui::hex`/`hexa`)이나 임의 픽셀 글자 크기(`.text_size(px(..))`)를 하드코딩하지 않는다(`cargo nextest`가 차단). Orca UI를 참고하더라도 **Krill에 실제 구현된 기능만 표기**하며, 관리자/설정 페이지는 새 창이 아니라 메인 창 안 페이지 전환으로 연다.

## 명령

```bash
scripts/check.sh            # fmt --check · clippy -D warnings · nextest · doctest · cargo deny --locked — 작업 완료 주장 전 반드시 0으로 끝나야 한다
cargo fmt --all             # 포맷 적용
cargo nextest run -p <크레이트>   # 한 크레이트 테스트
cargo build --workspace --bins && cargo run -p krill-app   # 앱 실행(보조 바이너리 krill·krill-holder·krilld를 먼저 빌드해야 한다)
```

툴체인: Homebrew `rustc 1.98.1`(rustup 없음 → `rust-toolchain.toml` 대신 `Cargo.toml`의 `rust-version = "1.98"`로 MSRV 고정). 최신 stable 1.99 전용 기능(패키지 레벨 default-features 덮어쓰기 등)에 기대지 않는다. Xcode 없음(CommandLineTools만) — gpui는 `font-kit` + `runtime_shaders` 기능으로 빌드. Apple Developer 인증서 없음(MK-104).

## 구조

```
Cargo.toml            # 워크스페이스, [workspace.lints], [workspace.dependencies] — 의존성 버전은 여기서만
crates/krill-proto/   # 와이어 포맷(프레임·메시지·키프레임). 엔진·UI 무관. 모든 프로세스가 공유
crates/krill-holder/  # PTY 부모·fd 보관·exec 핸드오프. macOS 빌드 1,250줄 이하 + Linux 전용 sys/linux.rs 300줄 이하(결정 12-2, 2026-10-18 개정), 거의 안 바뀜. unsafe FFI 집중 구역
crates/krilld/        # 세션 데몬: alacritty_terminal·IPC·zstd 세그먼트·hook·상태·git 큐·모바일 게이트웨이
crates/krill-term/    # 프레임워크 중립 그리드 렌더 모델 + IME 상태기계(gpui 실패 시 교체 대비)
crates/krill-hosts/   # 원격 호스트 연결 계층(SSH 소켓 포워딩·피어 관리)
crates/krill-link/    # iroh P2P 원격 터널·기기 짝짓기
crates/krill-log/     # 상한 회전 운영 로거
crates/krill-app/     # gpui UI (디자인 토큰·프리미티브·아이콘·상태는 src/ui/, 규범은 docs/STYLEGUIDE.md)
crates/krill/         # CLI 단일 바이너리(`krill`, `krill hook` 포함)
docs/STYLEGUIDE.md    # UI/UX 디자인 시스템 규범(Orca STYLEGUIDE·main.css 기반, 모든 UI 작업 시 필수 참조)
docs/planning/        # 기획 근거(T1~T4, 2차 검증본) — 읽기 전용
scripts/              # check.sh, memory-gate.sh
packaging/            # macOS 번들·DMG·Ed25519 매니페스트·설치 스크립트
```

## 반드시 지킬 것

1. **세션은 절대 잃지 않는다.** PTY는 krill-holder만 소유한다. krilld·앱·CLI의 어떤 실패도 PTY를 닫으면 안 된다. 의심되면 생존 테스트(앱 kill -9 / krilld kill -9)를 돌린다.
2. **메모리 예산**: 앱 ≤ 120MB, 데몬 ≤ 60MB(터미널 10개, phys_footprint). 셀 스크롤백 1만 줄 금지(핫 1,000행 + zstd 콜드).
3. **질의 응답자는 krilld 하나.** UI는 터미널 질의에 응답하지 않는다.
4. **Zed 터미널 코드는 GPL-3.0 — 읽고 베끼지 않는다**(클린룸). gpui 자체는 Apache-2.0.
5. **unsafe는 krill-holder·krilld의 FFI 모듈과 krill-app의 objc2 모듈에만.** 모든 `unsafe` 블록에 `// SAFETY:` 주석.
6. **데몬 panic 격리**: krilld는 세션 작업 단위 `catch_unwind`(panic=unwind, CONVENTIONS P4), krill-holder는 panic 금지. 비테스트 코드 `unwrap`/`expect`/`panic!` 금지(clippy가 막음, 불변식은 `#[expect(..., reason)]`). C·objc2 콜백 밖으로 panic이 나가지 않게.
7. 측정하지 않은 수치는 문서·커밋·이슈에 쓰지 않는다. 추정이면 "추정".
8. 하네스 파일(`AGENTS.md`, `docs/CONVENTIONS.md`, `scripts/`, lint·deny·fmt 설정)은 메인테이너 승인 없이 바꾸지 않는다.
9. `git push`, 원격 생성, 유료 작업 금지(사람이 한다).
10. **UI/UX는 [docs/STYLEGUIDE.md](docs/STYLEGUIDE.md)와 `krill_app::ui` 토큰·프리미티브만 쓴다.** 미구현 기능을 UI에 미리 표기하지 않는다.

## 이슈·작업 흐름

- 로드맵 ID(`[MK-xxx]`)는 `docs/ROADMAP.md` 행 ID다.
- 한 커밋 = 한 논리 변경. 작업 완료 전 `scripts/check.sh`가 0으로 끝나야 한다.
- 장애별 보장 범위(무엇이 살아남고 무엇을 잃을 수 있는지)는 ARCHITECTURE §4.1이 규범이다.
