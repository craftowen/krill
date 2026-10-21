# 원격 PC 프로젝트 목록이 로컬 화면과 다른 문제 (OWE-990)

기준: `a58a6825` 위 작업 트리, 설치 앱 v0.1.25. 2026-10-21 사용자 스크린샷의
원격 `Mac-mini`는 홈 폴더 세션만 보이고, 해당 PC의 로컬 화면은 `gwiinsaju` 프로젝트를 보였다.

## 운영 데이터 대조

읽기 전용 IPC `List`·`Sidebar`로 확인했다. 원격 소켓
`run/hosts/mac-mini.sock`의 ServerHello instance는 `59208`, build는
`0.1.25+dev.9928d32a3e4d9937`이다. 세션 1개(ID 5, cwd `/Users/<remote-user>`)와
등록 프로젝트 `/Users/<remote-user>/Documents/wProject/gwiinsaju`를 모두 정상으로 반환했다.
즉 원격 프로젝트 데이터 자체가 누락된 것은 아니다.

현재 앱 `sidebar.bin`의 프로젝트 선택 필터는 `/Users/<local-user>` 아래 로컬 프로젝트
5개만 포함한다. 다른 호스트의 원격 프로젝트 경로는 이 집합에 포함되지 않는다.
`remote_rows`가 이 로컬 경로 집합을 그대로 적용하여 정상 수신한 원격 프로젝트를 숨겼다.
소속 Git 워크트리가 없는 홈 폴더 세션은 별도 행 구성 경로로 남아서 서로 다른 목록처럼 보였다.

## 수정

프로젝트 선택 패널의 정본은 로컬 `snapshot.trees`이므로 그 경로 선택은 로컬 행에만 적용한다.
원격 행 구성 시 프로젝트 경로 선택 집합만 비운 옵션 사본을 사용한다. 사용자의 저장된 로컬 선택은
그대로 유지한다. 명시적 호스트 가시성 선택과 다른 필터·정렬은 유지한다. IPC 프로토콜,
원격 연결 대상, 기존 세션 ID·프로세스는 바꾸지 않는다.

## 검증 범위

- 새 GPUI 회귀 테스트: 로컬 프로젝트 선택 유지, 다른 사용자 경로의 세션 없는 원격 프로젝트와
  동일 절대경로인 별도 PC 프로젝트 표시, 원격 홈 폴더 세션 보존, 명시적 호스트 숨김 유지.
- 관련 GPUI nextest 3개 PASS: 위 회귀 + 다중 호스트 그룹 + 필터/영속화.
- `cargo clippy -p krill-app --all-targets --locked -- -D warnings` PASS.
- Windows 영향 크레이트 검사 `check-windows.sh -p krill-app --clippy` PASS.
- fmt·diff 공백 검사 PASS.
- 원시 운영 IPC 및 검사 로그: `/Volumes/krill-build/remote-session-consistency-20261021/`.

소스 수정과 실제 배포는 구분한다. 이 기록 시점의 설치 앱·공개 v0.1.25에는 수정이 없다.
전체 릴리스 검사·새 앱 게시를 수행했다고 보고하지 않는다. 다른 세션의 모바일 WIP 14파일은 보존한다.
