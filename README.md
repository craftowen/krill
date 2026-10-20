# Krill

**AI 코딩 에이전트를 병렬로 돌리는 순수 Rust macOS 앱 — 세션은 절대 잃지 않습니다.**

앱(`krill-app`)과 터미널 데몬(`krill-holder` + `krilld`)을 분리해 앱을 재시작해도 AI 세션이 유지된다. `krill` CLI로 앱 없이 세션을 조작할 수 있다.

Orca 기능을 참고해 순수 Rust로 만드는 초경량 앱이다.

실측 기록(기존 테스트 기록 인용, 추정 없음): 데몬 합계 **47.39MiB**(10세션 120×40 × 24에포크 가속 소크, 앱 제외), 앱 SIGKILL 생존 **3/3회·출력 408,058바이트 SHA-256 일치**. 24에포크는 실제 24시간 실행이 아니다. 메모리 측정은 macOS `phys_footprint`, 생존은 실제 release 바이너리 `kill -9` 후 세션 PID·출력 바이트·SHA-256 검증이다. 목표는 터미널 10개에서 앱+데몬 180MB 이하, 출시 차단선은 200MB이며 위 수치만으로 전체 예산 통과를 주장하지 않는다. krilld 크래시에서는 세션은 살지만 아직 기록되지 않은 출력은 잃을 수 있어 Gap을 표시한다. 장애별 보장 범위는 [ARCHITECTURE §4.1](docs/ARCHITECTURE.md)을 따른다.

## 요구 사항

- macOS (Apple Silicon. 릴리스 번들은 arm64만 배포하며, Intel Mac은 소스 빌드), Command Line Tools
- Rust 1.98 이상 (MSRV는 루트 `Cargo.toml`, Homebrew rustc 권장, rustup 불필요)
- Python 3.9 이상 (릴리스 조립 스크립트용)

Xcode·Apple Developer 인증서는 없어도 소스 빌드·실행이 된다. 서명된 배포·공증은 별도 게이트(MK-104)다.

## 설치

### 방법 1: 원클릭 릴리스 설치 (권장)

```sh
curl -fsSL https://krill.page/install.sh | bash
```

GitHub Releases의 최신 `Krill.app` 번들과 `krill` CLI를 설치한다. 소스에서 직접 빌드해 설치하려면 `--from-source --tag main`을 넘긴다.

### 방법 2: 소스 빌드

```sh
git clone https://github.com/craftowen/krill.git
cd krill
cargo build --release --locked --bins
```

`krill` CLI와 헬퍼(`krill-holder`, `krilld`)를 `~/.local/bin`에 설치한다:

```sh
bash packaging/install.sh --source-dir "$PWD"
export PATH="$HOME/.local/bin:$PATH"
command -v krill
```

앱 GUI(`Krill.app` 번들 조립)는 [packaging/README.md](packaging/README.md) 절차를 따른다.

### 방법 3: 개인 Homebrew tap (로컬 조립)

개인 tap 조립 절차는 [packaging/README.md](packaging/README.md) (MK-072)를 따른다. 공식 homebrew-core·homebrew-cask 제출은 하지 않는다.

### Gatekeeper 안내

- 소스 빌드 산출물은 격리 속성(quarantine)이 없어 그대로 실행된다.
- 내려받은 ad-hoc 서명 앱·DMG는 Gatekeeper에 막힐 수 있다 (공증 없음, MK-104 대기). `install.sh`는 다운로드한 번들의 격리 속성을 정리하고 설치한다.
- cask·formula는 앱 제거 시 세션 프로세스를 죽이지 않는다. 완전 삭제 전에는 세션을 먼저 종료한다 ([packaging/README.md](packaging/README.md) 절차).

## 사용

```sh
cargo build --workspace --bins && cargo run -p krill-app   # 앱 실행 (보조 바이너리 선행 빌드 필수)
krill ls/new/fork/send/read/wait/attach                    # 세션 조작
krill set --comment <text> | krill history clear | krill doctor [--bundle]
krill changes [--repo <path>] | krill agent list | krill hook <agent>
krill worktree new <name> | krill worktree rm <path>
```

상세: [아키텍처](docs/ARCHITECTURE.md) · [로드맵](docs/ROADMAP.md) · [코드 규칙](docs/CONVENTIONS.md) · [디자인 시스템](docs/STYLEGUIDE.md) · [릴리스 조립](packaging/README.md).

## 개발·기여

전체 검사는 `scripts/check.sh` (fmt·clippy `-D warnings`·nextest·doctest·cargo deny, 0 종료 필수). 기여 절차는 [CONTRIBUTING.md](CONTRIBUTING.md), 보안 신고는 [SECURITY.md](SECURITY.md), 행동 강령은 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).

## 라이선스

Apache-2.0 ([LICENSE](LICENSE), `Copyright 2026 Krill contributors`). 각 크레이트의 `license` 필드는 워크스페이스에서 상속한다. 서드파티 고지는 [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) 및 [docs/licensing/README.md](docs/licensing/README.md)를 참고한다. 재현 벤치는 [docs/benchmarks/MK-074.md](docs/benchmarks/MK-074.md)(`python3 docs/benchmarks/mk074.py`)에 있다. Zed `terminal`·`terminal_view`(GPL-3.0) 코드는 읽지도 베끼지도 않는다. Orca(MIT) 설계·코드 차용 시 파일 상단에 출처·저작권을 표기한다.
