# Krill — AI 코딩 워크스페이스

[English](README.md) · [웹사이트](https://krill.page) · [첫 실행 가이드](GETTING_STARTED.ko.md)

**AI 코딩 에이전트를 병렬로 실행하고, 터미널 세션과 Git 워크트리를 관리하는 네이티브 Rust 워크스페이스입니다.**

GUI(`krill-app`)와 세션 서비스(`krill-holder`·`krilld`)를 분리해 앱 재시작 뒤 실행 중인 세션에 다시 연결합니다. `krill` CLI로 GUI 없이 세션을 관리할 수도 있습니다.

## 지원 플랫폼

| 플랫폼 | 현재 범위 | 설치 |
|---|---|---|
| macOS Apple Silicon (arm64) | 주 데스크톱 플랫폼 | 릴리스 앱·CLI 또는 소스 빌드 |
| Windows x64 | 네이티브 데스크톱 프리뷰. Windows Server VM 기록 외에 데스크톱 GUI·대화형 로그온·절전·재부팅 검증이 더 필요 | 릴리스 ZIP·PowerShell 설치기 |
| Intel Mac | 이번 공개 프리뷰에서 미지원 | 지원 번들·소스 설치를 약속하지 않음 |
| Linux | 이번 공개 프리뷰에서 미지원 | 데스크톱·원격 호스트 다운로드를 약속하지 않음 |
| Windows ARM64·32비트 | 미지원 | 릴리스 패키지 없음 |

저장소에 Linux 구현·패키징 실험 코드가 있다고 지원 다운로드가 제공되는 것은 아닙니다. 플랫폼별 장애 보장과 검증 범위는 [ARCHITECTURE §4.1](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)을 따릅니다.

## 릴리스 설치

바이너리 설치에는 Rust·소스 빌드가 필요하지 않습니다. 에이전트는 별도로 설치·인증해야 하며 구독·인증 정보는 포함되지 않습니다. Git 워크트리에는 Git이 필요합니다.

**Apple Silicon Mac:** Bash·curl·Python 3·macOS 기본 압축 및 서명 도구가 필요합니다. 설치기는 `/Applications/Krill.app`을 설치하고 `~/.local/bin/krill`을 연결한 뒤 앱을 엽니다.

```sh
curl -fsSL https://krill.page/install.sh -o /tmp/krill-install.sh
# 내용을 검토한 뒤 실행합니다.
bash /tmp/krill-install.sh
export PATH="$HOME/.local/bin:$PATH"
krill --help
```

macOS 릴리스는 **공증되지 않았습니다**. 설치기는 체크섬·앱 서명을 검사하고 다운로드 격리 속성을 제거합니다. 로컬·ad-hoc 서명이 유효하다고 Apple Developer ID 승인을 받은 것은 아닙니다.

**Windows x64:** PowerShell에서 설치기를 내려받아 검토하고 실행합니다.

```powershell
Invoke-WebRequest https://krill.page/install.ps1 -OutFile "$env:TEMP\krill-install.ps1"
Get-Content "$env:TEMP\krill-install.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\krill-install.ps1"
```

ZIP SHA-256 검사 뒤 `%LOCALAPPDATA%\Programs\Krill`에 설치하고 시작 메뉴·사용자 PATH를 등록하며 앱을 엽니다. 새 터미널에서 `krill --help`를 확인합니다. Windows Authenticode 서명은 아직 준비 중입니다. 체크섬·업데이트 서명 검사가 신뢰된 게시자 서명을 뜻하지는 않습니다.

[최신 릴리스 자산](https://github.com/craftowen/krill/releases/latest)에서 선택한 버전의 파일을 확인합니다. [macOS 패키징](packaging/README.md)·[Windows 패키징](packaging/windows/README.md)에 세부 절차가 있습니다. Homebrew 도구는 개인 tap 조립용이며 공식 Homebrew 설치 명령은 제공하지 않습니다.

## 첫 성공

1. 앱의 **폴더 열기…**로 로컬 Git 저장소를 엽니다. 언어는 **설정 → 일반 · 외관 → 언어**에서 바꿉니다.
2. **새 세션**(`+`) 메뉴에서 **터미널** 또는 설치·인증한 Claude Code·Codex 등의 에이전트를 선택합니다.
3. 사이드바의 **새 워크트리**에서 고유한 이름으로 **만들기**를 누릅니다. setup은 내용을 확인해 승인하거나 **setup 건너뛰기**를 선택합니다. 두 번째 에이전트를 실행합니다.
4. `krill ls`로 세션 ID를 기록합니다. 탭을 남겨 둔 채 앱을 종료·재실행하고 같은 세션에 다시 연결되는지 확인합니다. 탭·칸을 명시적으로 닫으면 해당 세션이 종료됩니다.

복사해 실행할 수 있는 CLI 예제와 소스 빌드는 [첫 실행 가이드](GETTING_STARTED.ko.md)를 따릅니다.

## 장애별 보장

| 상황 | 실행 중인 에이전트 | 출력·복구 |
|---|---|---|
| 앱 종료·크래시·재시작 | 세션 서비스가 살아 있으면 계속 실행 | 같은 ID에 재연결. 기록 보존량은 유한하며 초과 시 Gap 가능 |
| `krilld` 크래시 | holder가 PTY·핸들을 보유해 생존 가능 | 읽었지만 저장하지 않은 바이트는 손실될 수 있으며 Gap 표시 |
| holder·krilld 동시 소실 또는 재부팅 | 기존 프로세스 종료 | 저장된 기록은 실행 중인 프로세스가 아니며 에이전트 대화 재개는 별도 동작 |

앱 재시작 연속성을 모든 장애에서의 데이터 무손실로 해석하면 안 됩니다. [장애별 규범](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)을 참고합니다. 기존 수치로 일반적인 성능 우위나 앱+데몬 전체 메모리 예산 달성을 주장하지 않습니다.

## 개발·기여·라이선스

소스 빌드는 Apple Silicon Mac의 Rust 1.98 이상·Git·Command Line Tools가 필요하고, 패키징에는 Python 3이 필요합니다. 로컬 실행에 전체 Xcode·Apple Developer 가입은 필요하지 않습니다. [소스 빌드](GETTING_STARTED.ko.md#macos-소스-빌드)를 따릅니다.

검사는 [기여 안내](CONTRIBUTING.ko.md)의 변경 범위별 절차를 따릅니다. Markdown만 바뀌면 `git diff --check`이며 전체 `scripts/check.sh`는 최종 릴리스 후보의 게이트입니다. [보안 신고](SECURITY.ko.md) · [행동 강령](CODE_OF_CONDUCT.ko.md).

[아키텍처](docs/ARCHITECTURE.md) · [로드맵](docs/ROADMAP.md) · [코드 규칙](docs/CONVENTIONS.md) · [디자인 시스템](docs/STYLEGUIDE.md).

Apache-2.0 ([LICENSE](LICENSE), Copyright 2026 Krill contributors). [서드파티 고지](THIRD_PARTY_NOTICES.md)·[라이선스 기록](docs/licensing/README.md)을 참고합니다. 기여 시 Zed `terminal`·`terminal_view`(GPL-3.0) 코드는 읽거나 복사하지 않으며, 허용된 Orca 소스 차용에는 출처·저작권을 표기합니다.
