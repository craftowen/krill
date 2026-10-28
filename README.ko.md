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
  <strong>코딩 에이전트 여러 개를 나란히 돌리는 네이티브 앱 — Electron도 웹뷰도 없습니다.</strong><br/>
  Claude Code, Codex, Antigravity, Cursor, OpenCode마다 Git 워크트리와 터미널을 따로 주고, 나를 기다리는 에이전트 목록과 diff 리뷰를 함께 제공합니다. 2026-10-25 터미널 10개 벤치마크(Krill 0.2.2 vs Orca 1.4.237)에서 Krill은 <strong>1,274.9 MiB</strong> 대신 <strong>85.9 MiB</strong>, 약 15분의 1을 썼습니다. 터미널 20개 출력 테스트 하나에서는 Krill이 더 느렸습니다(아래 표 참고).
</p>

<p align="center">
  <a href="https://krill.page"><img src="https://krill.page/app-screenshot.png" alt="Krill에서 실행 중인 실제 Claude Code 세션과 대화 기록 복원 화면" width="960" /></a>
  <br/>
  <sub>실제 Claude Code 세션 · 대화 기록 복원 및 Git 변경 내역 (macOS 개발 빌드, 한국어 UI). <a href="https://krill.page/app-screenshot-en.png">영문 UI 프리뷰 보기</a>.</sub>
</p>

---

## 왜 Orca(Electron)나 `tmux` 대신 Krill인가요?

[Orca](https://github.com/stablyai/orca) 같은 에이전트 워크스페이스도 이미 앱을 껐다 켜도 터미널 세션을 유지하고 Git 워크트리를 관리합니다. **앱을 닫아도 세션이 살아남는 것은 기본 조건이지, Orca 대비 차별점이 아닙니다.**

진짜 차이는 **자원 효율과 프로세스 구조**입니다. Orca는 Electron(Chromium + Node.js) 기반이라 본체·GPU·렌더러·데몬 프로세스가 따로 뜨고 탭마다 렌더러에 터미널을 유지합니다. 반면 Krill은 같은 병렬 에이전트 워크플로우를 **순수 Rust(`gpui`) 단일 UI 프로세스**로 구현해 보이는 칸만 데몬에 화면을 연결하고, 터미널 상태는 가벼운 데몬(`krilld`) 한 곳에 두며, PTY 디스크립터는 panic 없는 부모 프로세스(`krill-holder`)가 따로 쥐어 데몬 자체가 죽거나 업그레이드돼도 세션을 지킵니다.

### 같은 조건 실측: Krill 0.2.2 vs Orca 1.4.237 (2026-10-25)

같은 **Mac mini (Apple M6, 32 GB, macOS 27.0.1)** 에서 `/Applications` 배포본 그대로, 실행마다 새 격리 프로필·같은 `1200×760pt` 창(`1176×746` 실측)으로 터미널 1·10·20개 × 3회(18/18회 유효, 화면 잠금 상태) 측정한 중앙값입니다. 각 탭은 20,000줄(약 2.8 MB, 한글·ANSI 색·120열 초과 줄)을 출력했습니다.

| 지표 (터미널 10개, 3회 중앙값) | Krill 0.2.2 (Rust · GPUI) | Orca 1.4.237 (Electron) | Krill / Orca |
|---|---:|---:|---|
| **메모리 (`phys_footprint`, 앱+데몬 전 프로세스 합산)** | **85.9 MiB** | 1,274.9 MiB | **15분의 1** (`1 / 14.8`) |
| **유휴 CPU (출력 후 30초 평균, 화면 잠금 상태)** | **1.13%** | 31.0% | **27분의 1** (`1 / 27.4`) |
| **출력 처리 CPU 시간 (10탭 × 2.8 MB 버스트)** | **1.22초** | 6.44초 | **5분의 1** (`1 / 5.3`) |
| **설치 용량 (`.app` 번들)** | **92.1 MiB** | 689.1 MiB | **7분의 1** (`1 / 7.5`) |

#### 메모리는 어디에 쓰이나 (터미널 10개, 프로세스별 중앙값 합)

| 역할 구분 | Krill 0.2.2 | Orca 1.4.237 | 차이가 나는 이유 |
|---|---:|---:|---|
| **앱 · 화면 그리기** | **30.2 MiB** (`krill-app`) | **994.3 MiB** (`GPU 522.9` + `본체 259.3` + `렌더러 212.1`) | 순수 Rust GPUI 단일 프로세스 vs Chromium 본체·GPU·렌더러 멀티 프로세스 |
| **터미널 데몬** | **46.5 MiB** (`krilld 44.9` + `holder 1.6`) | **256.3 MiB** (`orca-terminal-host`) | 핫 1,000행 + `zstd` 콜드 스크롤백 vs Node/JS 터미널 호스트 |
| **기타 보조** | **9.2 MiB** (`krill-link serve`) | **24.1 MiB** (`파일 감시 14.0` + `네트워크 7.6` + `크래시 수집 2.5`) | 모바일·원격 게이트웨이 vs Electron 유틸리티 헬퍼 |
| **합계 (셸 제외)** | **85.9 MiB** | **1,274.9 MiB** | **Krill은 Orca 메모리의 15분의 1 (`1 / 14.8`)** |

#### 터미널 수별 전체 결과 (1 · 10 · 20개, 3회 중앙값)

| 지표 | 터미널 | Krill 0.2.2 | Orca 1.4.237 | Krill / Orca |
|---|---:|---:|---:|---|
| **메모리 (`phys_footprint`)** | 1개 | **56.6 MiB** | 844.9 MiB | `1 / 14.9` (15분의 1) |
| | 10개 | **85.9 MiB** | 1,274.9 MiB | `1 / 14.8` (**15분의 1**) |
| | 20개 | **115.0 MiB** | 1,382.7 MiB | `1 / 12.0` (**12분의 1**) |
| **유휴 CPU (30초 평균, 화면 잠금)** | 1개 | **0.97%** | 32.6% | `1 / 33.6` (34분의 1) |
| | 10개 | **1.13%** | 31.0% | `1 / 27.4` (**27분의 1**) |
| | 20개 | **1.17%** | 29.0% | `1 / 24.8` (**25분의 1**) |
| **출력 처리 CPU 시간 (2.8 MB × N)** | 1개 | **0.05초** | 1.07초 | `1 / 21.4` |
| | 10개 | **1.22초** | 6.44초 | `1 / 5.3` (**5분의 1**) |
| | 20개 | **2.91초** | 7.69초 | `1 / 2.6` |
| **출력 처리 소요 시간 (2.8 MB × N)** | 1개 | **0.21초** | 0.22초 | `1 / 1.0` |
| | 10개 | **2.25초** | 2.92초 | `1 / 1.3` |
| | 20개 | **7.54초** | **3.95초** | **Krill이 1.9배 느림** (회차: `1.85 / 7.54 / 7.86초`) |

<details>
<summary><strong>측정 조건 및 해석 시 유의사항 (숫자와 함께 볼 것)</strong></summary>

- **같은 조건 정의**: Mac mini (Apple M6, 32 GB, macOS 27.0.1, 2026-10-25)에서 `/Applications` 배포본(`Krill 0.2.2`, `Orca 1.4.237`)을 재빌드·설정 변경 없이 실행했습니다. 실행마다 새 격리 프로필(가짜 `HOME`), 같은 위치 `1200×760pt` 창(실측 `1176×746`), 같은 Git 저장소에서 로그인 셸(`/bin/zsh -l`) 탭 `N`개를 열고 한 탭만 보이게 맞췄습니다(18회 모두 `orca_orphaned_terminals = 0`).
- **부하 및 합산 범위**: 각 탭이 2만 줄(약 2.8 MB, 한글·ANSI 색·120열 초과 줄)을 출력한 뒤 30초 대기 후 `phys_footprint`를 5초 간격 3회 측정해 프로세스별 중앙값을 합산했습니다. 헬퍼·GPU·렌더러·데몬(`krill-link serve` 포함)을 모두 더했고, 로그인 셸과 `perl` 프로세스는 양쪽 다 제외했습니다.
- **화면 잠금 상태**: 18회 모두 화면이 잠긴 상태(창이 화면에 합성되지 않음)에서 측정했습니다. 화면이 잠긴 상태에서도 Orca GPU 프로세스는 약 21~24%, 렌더러는 약 7.5%의 CPU를 썼습니다. 화면을 켜 둔 상태(창 표시)의 CPU 값은 별도 데이터셋으로 측정하며 이 표의 수치로 주장하지 않습니다.
- **공용 머신 부하 및 20탭 소요 시간**: 다른 작업이 동시에 도는 공용 머신에서 회차마다 실행 순서를 바꿔 측정했습니다(실행 시작 시 1분 부하 평균 `15.0~184.8`). 메모리는 동시 부하 영향이 작지만 CPU·소요 시간은 흔들릴 수 있으며, **터미널 20개의 출력 처리 소요 시간은 Krill 7.54초로 Orca 3.95초보다 느렸습니다**(Krill 회차: `1.85 / 7.54 / 7.86초`, 동시 부하 영향 미분리).
- **설계 구조 차이**: Krill은 보이는 탭만 앱이 화면 상태를 갖고 터미널 상태는 데몬(`krilld`) 한 곳에 둡니다(터미널 10개 자체 예산 앱 ≤ 120 MiB·데몬 ≤ 60 MiB 대비 실측 앱 `30.2 MiB`·데몬 `46.5 MiB`). Orca는 탭마다 렌더러에 터미널을 두고 터미널 호스트 데몬에도 상태를 둡니다. 에이전트(Claude Code·Codex 등) 자체 프로세스는 두 앱에서 같으므로 제외했습니다.

</details>

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
- **45종 터미널 테마 &amp; 퍼스트클래스 한글 IME** — **Krill Deep**을 포함한 Krill 테마 15종과 Orca 호환 기본 테마 30종(Ghostty·Warp 테마 가져오기 지원)와 전용 한글 조합 상태기계(`krill-term`)를 내장합니다.
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

사용 통계는 별도 동의창 없이 기본 활성화됩니다. 무작위 설치 ID·최초 실행·앱 실행·일별 입력 활동·버전·OS·아키텍처를 집계하며, 프롬프트·터미널 출력·명령·프로젝트 경로·계정 정보는 제외합니다. `krill telemetry disable` 또는 `DO_NOT_TRACK=1`로 수집을 끌 수 있습니다. 전송 상태와 운영자 설정은 [사용 통계 안내](services/telemetry/CLIENT.md)를 참고합니다. 오류 보고는 별도 설정이며 기본 꺼짐입니다.

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

앱 재시작 연속성을 모든 장애에서의 데이터 무손실로 해석하면 안 됩니다. 플랫폼별 복구 및 검증 한계는 [장애별 규범](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)을 참고합니다.

---

## 개발·기여·라이선스

소스 빌드는 Apple Silicon Mac의 Rust 1.98 이상·Git·Command Line Tools가 필요하고, 패키징에는 Python 3이 필요합니다. 로컬 실행에 전체 Xcode·Apple Developer 가입은 필요하지 않습니다. [소스 빌드](GETTING_STARTED.ko.md#macos-소스-빌드)를 따릅니다.

검사는 [기여 안내](CONTRIBUTING.ko.md)의 변경 범위별 절차를 따릅니다. Markdown만 바뀌면 `git diff --check`이며 전체 `scripts/check.sh`는 최종 릴리스 후보의 게이트입니다. [보안 신고](SECURITY.ko.md) · [행동 강령](CODE_OF_CONDUCT.ko.md).

[아키텍처](docs/ARCHITECTURE.md) · [로드맵](docs/ROADMAP.md) · [코드 규칙](docs/CONVENTIONS.md) · [디자인 시스템](docs/STYLEGUIDE.md).

Apache-2.0 ([LICENSE](LICENSE), Copyright 2026 Krill contributors). [서드파티 고지](THIRD_PARTY_NOTICES.md)·[라이선스 기록](docs/licensing/README.md)을 참고합니다. 기여 시 Zed `terminal`·`terminal_view`(GPL-3.0) 코드는 읽거나 복사하지 않으며, 허용된 Orca 소스 차용에는 출처·저작권을 표기합니다.

사용 통계는 별도 동의창 없이 기본 활성화됩니다. 무작위 설치 ID·최초 실행·앱 실행·일별 입력 활동·버전·OS·아키텍처를 집계하며, 프롬프트·터미널 출력·명령·프로젝트 경로·계정 정보는 제외합니다. `krill telemetry disable` 또는 `DO_NOT_TRACK=1`로 수집을 끌 수 있습니다. 전송 상태와 운영자 설정은 [사용 통계 안내](services/telemetry/CLIENT.md)를 참고합니다. 오류 보고는 별도 설정이며 기본 꺼짐입니다.
