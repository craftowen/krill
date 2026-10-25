<h1 align="center">
  <a href="https://krill.page"><img src="https://krill.page/icon-192.png" alt="Krill" width="56" valign="middle" /></a> Krill
</h1>

<p align="center">
  <a href="https://github.com/craftowen/krill/stargazers"><img src="https://img.shields.io/github/stars/craftowen/krill?style=flat&amp;label=%E2%98%85&amp;color=ff5238" alt="GitHub 스타" /></a>
  <a href="https://github.com/craftowen/krill/releases/latest"><img src="https://img.shields.io/github/v/release/craftowen/krill?style=flat&amp;color=2a3040" alt="최신 릴리스" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-3b82f6?style=flat" alt="라이선스: Apache-2.0" /></a>
  <img src="https://img.shields.io/badge/macOS%20Apple%20Silicon%20%7C%20Windows%20x64%20%7C%20Mobile%20PWA-10b981?style=flat" alt="지원 플랫폼: macOS Apple Silicon, Windows x64 프리뷰, 모바일 PWA" />
</p>

<p align="center">
  <sub><a href="README.md">English</a> · <a href="https://krill.page">웹사이트</a> · <a href="https://krill.page/docs">CLI 레퍼런스</a> · <a href="GETTING_STARTED.ko.md">첫 실행 가이드</a> · <a href="https://github.com/craftowen/krill/releases/latest">릴리스 다운로드</a></sub>
</p>

<p align="center">
  <strong>여러 AI 코딩 에이전트를 병렬로. 앱을 닫아도 살아 있는 터미널 세션.</strong><br/>
  Claude Code, Codex, Antigravity, Cursor, OpenCode를 격리된 Git 워크트리에서 동시에 실행하세요 — 순수 Rust 3단 프로세스 격리 아키텍처(<code>krill-app</code> + <code>krilld</code> + <code>krill-holder</code>)로 동작합니다.
</p>

<p align="center">
  <a href="https://krill.page"><img src="https://krill.page/app-screenshot.png" alt="Krill에서 실행 중인 실제 Claude Code 세션과 대화 기록 복원 화면" width="960" /></a>
  <br/>
  <sub>실제 Claude Code 세션 · 대화 기록 복원 및 Git 변경 내역 (macOS 개발 빌드, 한국어 UI). <a href="https://krill.page/app-screenshot-en.png">영문 UI 프리뷰 보기</a>.</sub>
</p>

---

## 왜 Krill인가요?

일반 터미널로 여러 에이전트를 돌리면 브랜치와 출력이 뒤섞이고, `tmux`는 Git 워크트리 관리·diff 리뷰·네이티브 알림이 없으며, 무거운 Electron 래퍼 앱은 UI가 멈추거나 업데이트될 때 실행 중이던 에이전트 세션까지 함께 끊어집니다.

Krill은 **순수 Rust(GPUI)** 와 **3단 프로세스 격리**로 이 문제를 풉니다:
- **`krill-app` (네이티브 UI)** — 스냅샷과 행 델타만 받아 보이는 화면만 그립니다(메모리 설계 목표 ≤ 120 MB). 앱을 닫거나 강제 종료·업데이트해도 에이전트는 죽지 않습니다.
- **`krilld` (세션 데몬)** — 터미널 질의의 유일한 응답자입니다. 핫 1,000행 그리드와 `zstd` 콜드 스크롤백 세그먼트를 관리합니다(10세션 기준 설계 목표 ≤ 60 MB, 현재 릴리스의 실측 보증이 아닌 설계 목표).
- **`krill-holder` (PTY 부모)** — panic 없는 작은 부모 프로세스(macOS 1,300줄 이하)가 PTY 디스크립터를 쥐고 `SCM_RIGHTS`로 `krilld`에 넘깁니다.

---

## 핵심 기능

<table>
<tr>
<td width="50%" valign="top">

### 1. 앱이 꺼져도 살아 있는 세션
앱을 닫거나 재시작·업데이트해도 백그라운드에서 에이전트가 계속 작업합니다. 다시 열거나 `krill attach <id>`로 붙으면 키프레임과 `zstd` 세그먼트 기록을 이어 붙이며, 보존 한도를 넘긴 구간은 명시적으로 `Gap` 표시합니다.

[아키텍처 §4.1 →](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)

</td>
<td width="50%" valign="top">

### 2. 병렬 Git 워크트리 &amp; 내장 Diff 리뷰
사이드바나 `krill worktree new`로 작업마다 격리된 Git 워크트리를 만들고, 저장소 setup 스크립트를 검토 후 실행합니다. 터미널 바로 옆 5개 탭(`Changes`, `Files`, `Search`, `Checks`, `History`)에서 변경 파일과 diff를 확인하고 줄 단위 코멘트를 남길 수 있습니다.

[첫 실행 가이드 →](GETTING_STARTED.ko.md)

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 3. 기다리는 에이전트가 먼저 (`Needs You`)
내장 상태 훅(`krill hook install`)이 `working`, `waiting`, `done`, `blocked`, `idle`, `exited` 6가지 상태를 실시간으로 받아, 사람 판단이나 입력을 기다리는 에이전트를 **Needs You** 맨 위, macOS 알림, Dock 배지로 즉시 알려 줍니다.

[CLI 훅 문서 →](https://krill.page/docs#c-agents)

</td>
<td width="50%" valign="top">

### 4. 멀티 에이전트 작업 DAG &amp; 결정 게이트
코디네이터 에이전트가 의존성 있는 작업을 나누고(`krill orchestration run-create`, `task-create`) 각 워커를 별도 워크트리에서 기동(`worker-start`)합니다. 블로킹 질문·답변(`ask`/`reply`)과 사람 승인 게이트(`gate-create`)로 안전하게 협업합니다.

[오케스트레이션 문서 →](https://krill.page/docs#c-orch)

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 5. 종단간 암호화 모바일 웹 앱 (PWA)
Mac 앱의 **설정 → 모바일 원격 제어**에서 QR 코드를 스캔하면, 폰 브라우저나 홈 화면 앱(PWA)에서 에이전트 상태를 확인하고 터미널에 접속합니다. Relay·Tailscale 경로와 **ChaCha20-Poly1305**(`HKDF-SHA256`) 종단간 암호화로 보호됩니다.

[모바일 연결 안내 →](https://krill.page/docs)

</td>
<td width="50%" valign="top">

### 6. 아티팩트 1클릭 링크 공유 (`share.krill.page`)
에이전트가 작성한 HTML 보고서, 차트, Markdown 문서를 `krill artifacts share` 명령 한 줄로 샌드박스 공개 링크(90일 보관, 2 MB 상한)로 발행하고, 같은 링크를 업데이트하거나 회수합니다.

[아티팩트 가이드 →](docs/ARTIFACTS.md)

</td>
</tr>
</table>

**기본 내장된 부가 기능:**

- **GUI 없이 동작하는 `krill` CLI &amp; SSH 원격 호스트** — 단일 바이너리가 데몬에 직접 붙어 세션 생성·입출력·`attach`·맥락 인계(`krill handoff`)를 수행하며, 지원되는 원격 Mac 호스트(`krill --host user@host`)도 제어합니다.
- **계정 전환 및 자동 로테이션 (`krill account`)** — 에이전트 계정을 전환하고 사용량 한도 도달 시 자동으로 다음 계정으로 전환(`auto-switch`)합니다.
- **UTC Cron 예약 실행 (`krill schedule`)** — 반복 에이전트 작업을 스케줄에 맞춰 자동 실행합니다([스케줄링 문서](docs/SCHEDULING.md)).
- **31종 터미널 테마 &amp; 퍼스트클래스 한글 IME** — **Krill Deep** 및 30종의 기본 테마(Ghostty·Warp 테마 가져오기 지원)와 전용 한글 조합 상태기계(`krill-term`)를 내장합니다.
- **빠른 열기 &amp; 워크스페이스 컨텍스트** — 워크트리, 세션, 파일, 로컬 서버 포트(`krill ports`), 참고 URL(`krill reference`)을 빠르게 탐색합니다.

---

## 지원 에이전트

**모든 CLI 에이전트 및 터미널 TUI**와 함께 작동합니다 — 터미널에서 도는 프로그램이라면 Krill의 PTY 보호를 받습니다. 아래 에이전트는 전용 프로필과 상태 훅(`krill hook install`)을 기본 내장합니다:

<p>
  <kbd>Claude Code</kbd> &nbsp;
  <kbd>OpenAI Codex</kbd> &nbsp;
  <kbd>Antigravity (agy)</kbd> &nbsp;
  <kbd>Cursor CLI</kbd> &nbsp;
  <kbd>OpenCode</kbd> &nbsp;
  <kbd>+ 모든 셸 · 개발 서버 · TUI</kbd>
</p>

---

## 릴리스 설치

바이너리 설치에는 Rust·소스 빌드가 필요하지 않습니다. 에이전트는 별도로 설치·인증해야 하며 구독·인증 정보는 포함되지 않습니다. Git 워크트리에는 Git이 필요합니다.

### Apple Silicon Mac (`arm64`)

Bash·`curl`·Python 3·macOS 기본 압축 및 서명 도구가 필요합니다. 설치기는 `/Applications/Krill.app`을 설치하고 `~/.local/bin/krill`을 연결한 뒤 앱을 엽니다:

```sh
curl -fsSL https://krill.page/install.sh -o /tmp/krill-install.sh
# 내용을 검토한 뒤 실행합니다.
bash /tmp/krill-install.sh
export PATH="$HOME/.local/bin:$PATH"
krill --help
```

macOS 릴리스는 **공증되지 않았습니다**. 설치기는 체크섬·앱 서명을 검사하고 다운로드 격리 속성을 제거합니다. 로컬·ad-hoc 서명이 유효하다고 Apple Developer ID 승인을 받은 것은 아닙니다.

### Windows (`x64` 프리뷰)

PowerShell에서 설치기를 내려받아 검토하고 실행합니다:

```powershell
Invoke-WebRequest https://krill.page/install.ps1 -OutFile "$env:TEMP\krill-install.ps1"
Get-Content "$env:TEMP\krill-install.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\krill-install.ps1"
```

ZIP SHA-256 검사 뒤 `%LOCALAPPDATA%\Programs\Krill`에 설치하고 시작 메뉴·사용자 `PATH`를 등록하며 앱을 엽니다. 새 터미널에서 `krill --help`를 확인합니다. Windows Authenticode 서명은 아직 준비 중입니다. 체크섬·업데이트 서명 검사가 신뢰된 게시자 서명을 뜻하지는 않습니다.

[최신 릴리스 자산](https://github.com/craftowen/krill/releases/latest)에서 선택한 버전의 파일을 확인합니다. [macOS 패키징](packaging/README.md)·[Windows 패키징](packaging/windows/README.md)에 세부 절차가 있습니다. Homebrew 도구는 개인 tap 조립용이며 공식 Homebrew 설치 명령은 제공하지 않습니다.

### 지원 플랫폼 범위

| 플랫폼 | 현재 범위 | 설치 |
|---|---|---|
| **macOS Apple Silicon (`arm64`)** | 주 데스크톱 플랫폼 | 릴리스 앱·CLI 또는 소스 빌드 |
| **Windows `x64`** | 네이티브 데스크톱 프리뷰. Windows Server VM 기록 외에 데스크톱 GUI·대화형 로그온·절전·재부팅 검증이 더 필요 | 릴리스 ZIP·PowerShell 설치기 |
| **모바일 (iOS / Android)** | QR 코드로 페어링하는 모바일 웹 앱(PWA, Relay·Tailscale + ChaCha20-Poly1305 종단간 암호화) | Mac 앱 **설정 → 모바일 원격 제어**에서 QR 스캔(앱 스토어 앱 아님) |
| **Intel Mac (`x86_64`)** | 이번 공개 프리뷰에서 미지원 | 지원 번들·소스 설치를 약속하지 않음 |
| **Linux** | 이번 공개 프리뷰에서 미지원 | 데스크톱·원격 호스트 다운로드를 약속하지 않음 |
| **Windows ARM64·32비트** | 미지원 | 릴리스 패키지 없음 |

저장소에 Linux 구현·패키징 실험 코드가 있다고 지원 다운로드가 제공되는 것은 아닙니다. 플랫폼별 장애 보장과 검증 범위는 [ARCHITECTURE §4.1](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)을 따릅니다.

---

## 첫 성공

1. 앱의 **폴더 열기…**로 로컬 Git 저장소를 엽니다. 언어는 **설정 → 일반 · 외관 → 언어**에서 바꿉니다.
2. **새 세션**(`+`) 메뉴에서 **터미널** 또는 설치·인증한 Claude Code·Codex 등의 에이전트를 선택합니다.
3. 사이드바의 **새 워크트리**에서 고유한 이름으로 **만들기**를 누릅니다. setup은 내용을 확인해 승인하거나 **setup 건너뛰기**를 선택합니다. 두 번째 에이전트를 실행합니다.
4. `krill ls`로 세션 ID를 기록합니다. 탭을 남겨 둔 채 앱을 종료·재실행하고 같은 세션에 다시 연결되는지 확인합니다. 탭·칸을 명시적으로 닫으면 해당 세션이 종료됩니다.

복사해 실행할 수 있는 CLI 예제와 소스 빌드는 [첫 실행 가이드](GETTING_STARTED.ko.md)를 따릅니다.

---

## 장애별 보장

| 상황 | 실행 중인 에이전트 | 출력·복구 |
|---|---|---|
| **앱 종료·크래시·재시작** | 세션 서비스가 살아 있으면 계속 실행 | 같은 ID에 재연결. 기록 보존량은 유한하며 초과 시 `Gap` 가능 |
| **`krilld` 크래시** | holder가 PTY·핸들을 보유해 생존 가능 | 읽었지만 저장하지 않은 바이트는 손실될 수 있으며 `Gap` 표시 |
| **holder·`krilld` 동시 소실 또는 재부팅** | 기존 프로세스 종료 | 저장된 기록은 실행 중인 프로세스가 아니며 에이전트 대화 재개는 별도 동작 |

앱 재시작 연속성을 모든 장애에서의 데이터 무손실로 해석하면 안 됩니다. [장애별 규범](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)을 참고합니다. 기존 수치로 일반적인 성능 우위나 앱+데몬 전체 메모리 예산 달성을 주장하지 않습니다.

---

## 개발·기여·라이선스

소스 빌드는 Apple Silicon Mac의 Rust 1.98 이상·Git·Command Line Tools가 필요하고, 패키징에는 Python 3이 필요합니다. 로컬 실행에 전체 Xcode·Apple Developer 가입은 필요하지 않습니다. [소스 빌드](GETTING_STARTED.ko.md#macos-소스-빌드)를 따릅니다.

검사는 [기여 안내](CONTRIBUTING.ko.md)의 변경 범위별 절차를 따릅니다. Markdown만 바뀌면 `git diff --check`이며 전체 `scripts/check.sh`는 최종 릴리스 후보의 게이트입니다. [보안 신고](SECURITY.ko.md) · [행동 강령](CODE_OF_CONDUCT.ko.md).

[아키텍처](docs/ARCHITECTURE.md) · [로드맵](docs/ROADMAP.md) · [코드 규칙](docs/CONVENTIONS.md) · [디자인 시스템](docs/STYLEGUIDE.md).

Apache-2.0 ([LICENSE](LICENSE), Copyright 2026 Krill contributors). [서드파티 고지](THIRD_PARTY_NOTICES.md)·[라이선스 기록](docs/licensing/README.md)을 참고합니다. 기여 시 Zed `terminal`·`terminal_view`(GPL-3.0) 코드는 읽거나 복사하지 않으며, 허용된 Orca 소스 차용에는 출처·저작권을 표기합니다.
