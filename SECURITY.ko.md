# 보안 정책

[English](SECURITY.md)

## 지원 버전

| 버전 | 지원 여부 |
|---|---|
| 최신 `main` | 지원 |
| 릴리스 태그 (첫 릴리스 이후) | 최신 태그만 지원 |

첫 정식 릴리스 전까지는 최신 `main`만 지원합니다.

## 비공개 취약점 신고

[GitHub 비공개 취약점 신고 폼](https://github.com/craftowen/krill/security/advisories/new)을 사용합니다. **2026-10-23** 저장소 설정에서 비공개 취약점 신고 기능을 활성화하고 확인했습니다.

폼이 비활성화됐거나 열리지 않으면 [소유자에게 비공개 연락 방법을 묻는 이슈](https://github.com/craftowen/krill/issues/new?template=feature_request.md&title=Private%20security%20contact%20request)를 만듭니다. 공개 이슈에는 연락 방법 요청만 적고 취약점·재현 절차·입력·로그·악용 상세를 쓰지 않습니다. 비공개 채널을 안내받은 뒤 보고서를 보냅니다. 공개 이슈·PR·토론에 취약점을 공개하지 않습니다.

비공개 보고서에는 다음을 포함합니다.

- 영향받는 바이너리·컴포넌트·입력.
- 재현 명령과 환경: Krill 커밋·태그, OS 버전·아키텍처, 소스 빌드라면 Rust 버전.
- 임의 코드 실행·세션 탈취·민감정보 유출 등 실제 영향.

응답 목표는 **7일 이내**입니다. 수정이 릴리스되기 전까지 기술 상세 공개를 미뤄 주세요.

## 범위·한계

신고 범위는 `crates/` Rust 코드, `packaging/install.sh`·`packaging/release.sh`, GitHub Actions 워크플로입니다.

알려진 한계: 로컬·ad-hoc 서명 및 미공증 macOS 앱을 설치하기 위해 문서에 안내한 Gatekeeper 우회 자체는 권한 상승 취약점으로 보지 않습니다. 직접 빌드를 원하면 [소스 빌드](docs/GETTING_STARTED.ko.md#macos-소스-빌드)를 사용합니다. 이 한계가 다른 설치기·업데이터 취약점을 제외하지는 않습니다.

## 비밀·진단 정보

[사용 통계](services/telemetry/CLIENT.md)는 별도 동의창 없이 기본 활성화됩니다. 무작위 설치 ID·이벤트 ID/종류/시각·앱 버전·OS·아키텍처를 보내며 프롬프트·터미널 출력·명령·저장소 이름·경로·계정 정보·하드웨어 식별자는 제외합니다. 수집기는 통계 데이터베이스에 클라이언트 IP를 저장하지 않습니다. `krill telemetry disable`, `DO_NOT_TRACK=1`, `KRILL_TELEMETRY_DISABLED=1`로 수집을 끌 수 있으며, 로컬 비활성화는 이미 수신한 서버 기록의 삭제를 뜻하지 않습니다. 오류 보고는 별도 opt-in입니다. 수집기 관리에는 서버 비밀키가 필요하며 데스크톱 빌드에 포함하지 않습니다.

Krill은 [진단 구현](crates/krill-proto/src/diag.rs)의 규칙으로 진단 번들을 만들 때 `[REDACTED]` 처리합니다. 원본 로컬 panic 로그에는 가리지 않은 메시지·경로가 있을 수 있습니다. 모든 비밀 제거를 보장하지 않으므로 공유 전에 직접 검토합니다. 별도의 [오류 보고](docs/agents/error-reporting.md)는 기본 꺼짐이며, 정해진 메타데이터만 보내고 원본 파일을 업로드하지 않습니다. 민감 패턴이 새면 보안 신고로 다룹니다. 업데이터 배포 서명 개인키는 커밋하지 않습니다.
