# OWE-837 — Windows 배포: 설치 형식·rename 방식 업데이터·롤백

측정일: 2026-10-18. 공유 GCP `krill-win`(Windows Server 2022 10.0.20348, Rust 1.98.1 MSVC, `curl.exe` 8.21.0, System32 `tar.exe`). 작업 디렉터리 `C:\spike\OWE-837\`. 브랜치 `craftowen/win-p4-deploy`.

**결론: 관리자 권한이 없는 사용자 단위 zip 설치(`%LOCALAPPDATA%\Programs\Krill`)와 자체 업데이터를 권장한다.** 업데이터는 macOS 업데이터의 매니페스트 서명·SHA-256·저널·relauncher를 그대로 쓰고, 교체 단계만 Windows용으로 바꾼다. 실행 중인 exe와 로드된 DLL은 지울 수 없지만 같은 볼륨 안에서 이름을 바꾸거나 옮길 수는 있다. 그래서 파일을 하나씩 `Krill.prev`로 옮긴 뒤 새 파일을 놓는다. VM e2e 8개 시나리오가 모두 기대대로 동작했다(§6). exe는 `+crt-static`으로 빌드해 `VCRUNTIME140.dll` 의존을 없앤다(§4).

## 1. 설치 형식 비교와 결정

| 형식 | 관리자 권한 | 사용자 단위 설치 | 서명 없을 때 경고 | 자동 업데이트 적합성 | 실행 중 exe 교체 | 데몬(holder·krilld) 생존 |
|---|---|---|---|---|---|---|
| **zip + 자체 업데이터** (`%LOCALAPPDATA%\Programs\Krill`) | 불필요 | 예 | 브라우저로 받은 zip을 탐색기로 풀면 exe에 MOTW가 붙어 첫 실행 때 SmartScreen 경고가 뜰 수 있다(문서 기준, 실측 안 함). 업데이터가 `curl.exe`로 받은 zip에는 MOTW가 없었다(§6 실측) | 이미 있는 Ed25519 매니페스트·SHA-256·저널·롤백을 그대로 쓴다 | 파일 단위 rename으로 된다(§2 실측) | 업데이터가 데몬을 죽이지 않는다. 실행 중 이미지는 옮겨진 채로 계속 돈다(§6에서 OpenConsole·앱 대역으로 실측, holder 자체는 미확인) |
| Inno Setup (`PrivilegesRequired=lowest`) | 사용자 모드 가능 | 예 | 설치 프로그램 exe 자체가 SmartScreen 대상 | 자동 업데이트 기능이 없어 자체 업데이터가 결국 필요 | Restart Manager로 프로세스를 닫거나 재부팅 뒤 교체(`MoveFileEx` 지연) — 세션 데몬을 닫으면 불변식 위반 | 설치기가 holder를 종료하면 세션을 잃는다. 막으려면 예외 처리가 필요 |
| MSIX | 불필요(사이드로드는 정책 필요) | 예 | **서명 필수**(신뢰된 인증서 없으면 설치 불가) | App Installer 자동 업데이트 | 패키지 버전 폴더가 바뀜, 실행 중이면 종료 후 적용 | 패키지 컨테이너·백그라운드 프로세스 수명 정책이 detached holder와 맞는지 미확인 |
| winget | 매니페스트가 가리키는 설치기에 따름 | 설치기에 따름 | 설치기와 같음 | winget upgrade(사용자 실행) — 앱 내 자동 업데이트가 아님 | 설치기에 따름 | 설치기에 따름 |
| Velopack | 불필요 | 예(`%LOCALAPPDATA%\<app>`) | Setup.exe가 SmartScreen 대상 | 델타·롤백·자동 업데이트 내장 | `current\` 폴더 교체 + Update.exe 재실행. 앱이 종료돼야 적용 | 앱과 같은 폴더의 holder를 교체할 때 앱 종료 훅에 의존. 세션 보유 holder 처리 규칙을 따로 짜야 함. 도입하면 새 의존성이 생김 |

**권장: zip + 자체 업데이터**, 관리자 권한 없는 사용자 단위 `%LOCALAPPDATA%\Programs\Krill`.
- macOS 업데이터(MK-063)의 매니페스트 서명·SHA-256·저널·롤백·relauncher를 그대로 재사용하고 교체 단계만 Windows용(rename)으로 바꾼다. 새 의존성 0.
- 설치기가 데몬을 종료하는 경로가 아예 없다(불변식 1).
- Inno·MSIX·Velopack은 "앱 종료 후 교체"가 기본이라 세션 보유 holder를 다루는 규칙을 다시 짜야 한다. MSIX는 서명이 필수라 OWE-838 전에는 배포할 수 없다.
- winget은 zip(`InstallerType: zip` + `NestedInstallerType: portable`) 매니페스트로 후속 등록할 수 있다. 서명·게시는 사람이 결정한다(외부 게시라 이번 범위 밖).
- 위 표에서 Inno·MSIX·winget·Velopack 항목은 각 공식 문서 기준 비교이고 이 스파이크에서 실측하지 않았다.

## 2. 파일 연산 실측 (실행 중 이미지)

`scripts/prim.ps1`: `PING.EXE` 사본을 실행해 둔 상태와 `version.dll` 사본을 `LoadLibraryW`로 로드한 상태에서 측정했다.

| 연산 | 결과 |
|---|---|
| 실행 중 exe 삭제 / 쓰기로 열기 | **실패**(Access denied / sharing violation) |
| 실행 중 exe 같은 폴더 안 이름 바꾸기, 다른 폴더로 이동 | 성공 |
| 원래 이름 자리에 새 파일 놓기 | 성공 |
| 로드된 DLL을 다른 폴더로 이동 | 성공 |
| 이동 뒤 `GetModuleFileNameW`(DLL), `Process.Path`(exe) | **원래 경로**를 돌려줌(로더가 기록한 이름) |
| 실행 중 exe가 들어 있는 폴더 이름 바꾸기 | 성공 |
| 실행 중 프로세스의 **작업 디렉터리**인 폴더 이름 바꾸기 | **실패**(사용 중) |
| 실행 중 exe가 있는 폴더 재귀 삭제 | 실패. 파일은 남고 프로세스는 계속 돈다 |
| 프로세스가 끝난 뒤 삭제 | 성공 |

함의: 폴더를 통째로 바꾸면 cwd 때문에 실패할 수 있다. 그래서 파일 단위로 옮긴다(`rename.rs`). 지울 수 없는 구 이미지는 `Krill.trash`로 옮겨 두고 다음 업데이트 때 지운다. 이동한 DLL이 자기 경로를 원래 경로로 안다는 점은 §5의 ConPTY 불일치로 이어진다.

## 3. 업데이터 Windows 분기 설계 (구현: `crates/krill-app/src/updater*`)

| 단계 | macOS | Windows (OWE-837) |
|---|---|---|
| 번들 표지·버전·공개키 | `Contents/Info.plist` | 설치 폴더 `Krill.json`(같은 키: `CFBundleShortVersionString`·`KrillUpdatePublicKey`·`KrillUpdateURL`) |
| 다운로드 | `/usr/bin/curl` | `%SystemRoot%\System32\curl.exe`(PATH 가로채기 방지) |
| 매니페스트·아카이브 검증 | Ed25519 + SHA-256 | **같은 코드**. Authenticode는 OWE-838 전까지 없음 → 구조 검사만(`validate_install_layout`: `Krill.json`와 exe 4개) |
| 압축 해제 | `ditto -x -k` | `System32\tar.exe -xf`(zip) |
| 교체 | 제자리 동기화(inode 보존) | `Mode::Rename`: `.trash` 정리 → `.prev` 비우기 → 설치 파일을 `.prev`로 이동 → 스테이징 파일을 설치 폴더로 이동 |
| 실패 롤백 | `.prev` 동기화 | 저널 단계로 판정: 옮기는 중(`MovingToPrev`)에 실패하면 `.prev`만 되돌린다(설치 폴더에 남은 구 파일은 건드리지 않음). 그 뒤 단계면 설치 폴더를 치우고 `.prev`를 되돌린다 |
| 중단 복구(`recover_journal`) | 번들·스테이징 존재로 판정 | `MovingToPrev`: 되돌리고 저널 삭제. `MovingStagingToTarget`: 남은 스테이징을 마저 옮겨 `VerifyingLaunch`로 이어감 |
| 안전장치 | — | 설치 폴더 최상위에 알려지지 않은 항목(사용자 파일)이 있으면 교체 거부 |
| relauncher | `update/relauncher` | `update\relauncher.exe`(`.exe`가 없으면 CreateProcess가 못 찾음), `DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP` |
| 구 앱 종료 대기 | `kill(pid, 0)` | `tasklist /FI "PID eq n"` 폴링(`CREATE_NO_WINDOW`). platform 모듈 밖에 unsafe를 두지 않기 위함(AGENTS 5) |
| `--update-cli`가 GUI 실행 중일 때 | GUI에 TERM → relauncher | GUI를 끄지 않고 바로 rename 교체. GUI는 구 이미지로 계속 돌고 다음 실행부터 새 버전 |

**holder·krilld 교체 순서** (코디네이터 결정: krill-proto·holder 무수정, 1안):
1. 파일 교체는 위 rename으로 한다. 실행 중 holder·krilld·OpenConsole은 옮겨진 구 이미지로 계속 돌기 때문에 세션은 유지된다.
2. 교체 직후 `--update-cli`는 macOS와 같은 기존 IPC `daemon::upgrade_as(Paths{socket, holder: <설치>\krill-holder.exe, krilld: <설치>\krilld.exe})`로 UpgradeHolder·UpgradeKrilld를 요청한다. 번들(B안) 모드면 세션이 있어도 holder 핸드오프가 된다(OWE-832 실측). inbox 축소 모드는 세션이 있으면 EBUSY라 구 holder가 다음 기동까지 남는다.
3. 이번 VM e2e에서는 데몬이 떠 있지 않아(헤드리스 SSH 세션에서 GUI가 바로 종료, §6 T5) **업그레이드 요청 경로는 실측하지 못했다(미확인)**. Windows krilld의 Upgrade 지원은 OWE-833 계열 작업에 달려 있다.
4. 후속(문서화만): 버전별 holder 경로(`<설치>\holder\<ver>\`)를 쓰려면 `krill_proto::daemon::find_exe`에 Windows 탐색을 추가해야 한다. 그러면 구 holder의 conpty.dll과 OpenConsole이 항상 짝으로 남는다(§5).

## 4. VCRUNTIME과 `+crt-static`

`dumpbin /dependents`(VS 2022 Build Tools), release, `C:\spike\OWE-837\target{,-static}`:

| 파일 | 크기(동적) | 크기(`+crt-static`) | CRT 의존(동적) | CRT 의존(static) |
|---|---|---|---|---|
| krilld.exe | 210,432 B | 304,640 B | `VCRUNTIME140.dll` + UCRT(`api-ms-win-crt-*`) | 없음 |
| krill-holder.exe | 356,352 B | 450,048 B | 같음 | 없음 |
| krill.exe | 7,548,416 B | 7,581,184 B | 같음 | 없음 |
| krill-app.exe | 51,912,704 B | 미측정(빌드 생략) | 같음 | 미측정 |
| conpty.dll 1.25 (번들) | 119,608 B | — | UCRT만(`VCRUNTIME` 없음) | — |
| OpenConsole.exe 1.25 (번들) | 1,089,376 B | — | UCRT만 | — |

- `+crt-static` 빌드에는 CRT DLL 의존이 하나도 없었고 `krill.exe` 실행(usage 출력, exit 2)이 동적 빌드와 같았다. 늘어난 크기는 exe당 약 33~94KB였다.
- 번들 ConPTY 파일은 UCRT만 쓴다. UCRT는 Windows 10부터 OS에 들어 있으므로 VC++ 재배포 패키지가 필요 없다(Microsoft 문서 기준, VC++ 런타임이 없는 PC에서 실행하는 것은 실측하지 않음 → 미확인).
- **권장**: 릴리스 빌드에 `RUSTFLAGS=-C target-feature=+crt-static`을 쓴다(`packaging/windows/README.md`). 재배포 패키지를 설치기에 넣을 필요가 없다. 저장소 `.cargo/config.toml`에 넣을지는 하네스 파일이라 메인테이너가 결정한다.

## 5. ConPTY 버전 불일치 실측 (구 holder가 교체 뒤 새 세션을 만들 때)

`scripts/skew.ps1`(PowerShell + C# P/Invoke): conpty.dll을 로드한 뒤 dll과 `x64\OpenConsole.exe`를 `.prev`로 옮기고 다른 버전의 OpenConsole을 원래 자리에 놓는다. 그다음 `ConptyCreatePseudoConsole` → 자식 `cmd /c echo SKEW_CHILD_OK` → `ConptyResizePseudoConsole` 순서로 호출했다. NuGet 1.24.261001001(nupkg SHA-256 `4d6aaddc…ac5f`)과 1.25.260930003을 썼다.

| 로드된 dll | 새로 뜬 OpenConsole | 뜬 경로 | create / resize | 자식 출력 수신 | 자식 5초 내 종료 |
|---|---|---|---|---|---|
| 1.25 (기준) | 1.25 | 원래 경로 | 0 / 0 | 예 | 예 |
| 1.24 (기준) | 1.24 | 원래 경로 | 0 / 0 | 예 | 예 |
| 1.24 → 파일 교체 | **1.25** | `<설치>\x64\OpenConsole.exe`(새 파일) | 0 / 0 | 4/4 예 | 4/4 예 |
| 1.25 → 파일 교체(롤백 방향) | **1.24** | 새 파일 | 0 / 0 | 4/4 예 | 3/4(1회 대기 초과, 출력은 수신) |

- 옮겨진 구 conpty.dll은 원래 폴더의 **새** OpenConsole을 띄운다(§2의 `GetModuleFileNameW` 결과와 일치). 즉 inbox가 아닌 번들 모드 구 holder에서 업그레이드가 실패하거나 지연되면 dll과 OpenConsole이 다른 버전으로 짝지어진다. inbox 축소 모드는 OpenConsole을 쓰지 않으므로 이 문제가 없다.
- 1.24와 1.25 사이에서는 기본 동작(생성·출력·resize)이 됐다. 다른 버전 조합이나 세부 기능(입력 모드, 긴 세션)은 미확인이다. 위험을 없애려면 §3-4의 버전별 holder 경로가 필요하다.

## 6. VM e2e 실측 (`scripts/test.ps1`, 로컬 HTTP `127.0.0.1:8737`, 테스트 Ed25519 키)

패키지는 `packaging/windows/bundle.ps1`로 만들었다(B안: conpty.dll + `x64\OpenConsole.exe` 포함). 0.1.5·0.9.0·0.9.3은 실제 exe다. 0.9.1·0.9.2는 krill-app.exe 자리에 대역(`fakeapp`)을 넣었다. 대역은 준비 신호 파일을 쓰고 20초 동안 살며, 0.9.2는 `BROKEN` 표지가 있어 exit 3으로 끝난다. 매니페스트는 `packaging/macos/ed25519_manifest.py sign`으로 만들었다.

| # | 시나리오 | 결과 |
|---|---|---|
| T1 | `install.ps1`로 zip 설치 | 성공. 8개 파일(`conpty.dll`·`x64\OpenConsole.exe` 포함). 같은 위치에 다시 설치하면 거부 |
| T2 | `--update-cli --check --json` | `available` 0.1.5→0.9.0, 932ms |
| T3 | 다른 키로 서명한 매니페스트 | 거부 `ed25519 signature verification failed`. 설치 불변(exe 해시 동일), staging·prev·저널 없음 |
| T4 | zip 끝에 1바이트 덧붙임 | 거부 `archive sha256 mismatch`. 설치 불변, 잔여물 없음 |
| T5 | 실행 중 교체(`--update-cli` 0.1.5→0.9.0), `x64\OpenConsole.exe` 실행 중 | 성공 686ms. 실행 중 프로세스는 같은 PID로 생존. `.prev`에 구 파일 8개. 새 exe에 `Zone.Identifier` 없음. GUI(krill-app.exe)는 헤드리스 SSH 세션에서 곧바로 exit 0 → GUI 실행 중 교체와 데몬 업그레이드 요청은 미확인 |
| T6 | relauncher 성공(0.9.0→0.9.1), 대기 PID(`ping`) 종료 확인 | exit 0, 3,273ms(대기 약 3초 포함). 저널 `healthy`, 새 앱 실행 중. T5에서 아직 도는 구 OpenConsole은 `.prev`에서 못 지워 `Krill.trash\0-OpenConsole.exe`로 이동 |
| T7 | relauncher 롤백(0.9.1→0.9.2, 새 앱 exit 3) | exit 0, 3,350ms. 저널 `rolled_back`("new app exited before ready (exit code: 3)"), 버전 0.9.1 복원, `BROKEN` 없음, 복원된 앱 재실행 |
| T8 | 교체 실패 롤백: `krilld.exe`를 FILE_SHARE_DELETE 없이 열어 둔 채 0.9.1→0.9.3 | exit 1 `os error 32`. 저널 `rolled_back`, 버전 0.9.1·대역 exe 해시 그대로, 파일 8개 그대로 |
| T8b | 잠금 해제 뒤 재시도, 설치 루트의 `krill-app.exe`(대역) 실행 중 | 성공 602ms → 0.9.3. 실행 중 앱은 생존(이미지 경로는 원래 경로로 보고) |
| 정리 | 프로세스 종료 뒤 `Krill.trash` | 남은 파일 0 |

관찰:
- `krill-app.exe`는 콘솔 서브시스템이다(`windows_subsystem` 없음). 탐색기나 분리된 relauncher가 띄우면 콘솔 창이 생길 것으로 추정한다. GUI 진입점 작업(W3-3)에서 정할 일이다.
- Rust `Command`는 상속 가능한 핸들을 자식에게 넘긴다. 그래서 relauncher의 stdout을 파이프로 받으면 그 파이프를 쥔 새 앱이 끝날 때까지 기다리게 된다(첫 측정 23.7초는 이 때문이었다). 운영 relauncher는 stdio를 NUL로 두고 띄우므로 영향이 없다(OWE-829와 같은 현상).
- curl로 받아 tar로 푼 파일에는 MOTW가 없었다. 브라우저로 받은 zip을 탐색기로 풀었을 때의 SmartScreen 동작은 미확인이다(서버 VM, 서명 없음).

## 7. 재현

스파이크 코드는 저장소 밖 `~/.cache/krill-win/spikes/OWE-837/`(VM `C:\spike\OWE-837\`)에 있다. 파일: `scripts/{prim,skew,test,packages,server}.ps1`, `fakeapp/`, `sign.sh`, 결과 `prim-result.txt`·`skew-result.txt`·`e2e-result.txt`.

```powershell
# VM, C:\spike\OWE-837 (src = 이 브랜치 소스)
$env:CARGO_TARGET_DIR='C:\spike\OWE-837\target'; cargo build --release --locked -p krill-app -p krill -p krilld -p krill-holder
powershell -File scripts\packages.ps1 -PublicKey <테스트 공개키>   # dist\, www\ 에 zip 5개 + 위조 zip
# Mac: sign.sh <zip 해시 목록>  → www\m0xx.json, forged-sig.json, tampered.json 을 VM에 넣는다
powershell -File scripts\test.ps1
powershell -File scripts\skew.ps1 -DllDir <conpty.dll 폴더> -NewExe <다른 버전 OpenConsole.exe> -Move
$env:RUSTFLAGS='-C target-feature=+crt-static'; $env:CARGO_TARGET_DIR='C:\spike\OWE-837\target-static'; cargo build --release --locked -p krill-holder -p krilld -p krill
```

Mac 검증:

```bash
cargo clippy -p krill-app --all-targets --locked -- -D warnings                    # 0
cargo nextest run -p krill-app --locked -E 'test(updater)'                          # 32 passed
scripts/check-windows.sh -p krill-app --clippy --target-dir /Volumes/krill-build/targets/win/OWE-837-windows  # 0
```

## 8. 미확인·후속

| 항목 | 상태 |
|---|---|
| GUI 실행 중 교체·relauncher가 실제 krill-app GUI를 띄워 ready 받기 | 미확인(헤드리스 VM에서 GUI가 바로 종료). 대역 앱으로 relauncher 흐름만 실측 |
| 교체 뒤 UpgradeHolder·UpgradeKrilld 요청(Windows krilld) | 미확인(데몬 미기동). 코드는 macOS와 같은 `upgrade_as` 경로 |
| 버전별 holder 경로(`find_exe` Windows 탐색) | 후속 이슈 필요(krill-proto 변경) |
| VC++ 런타임이 없는 PC에서 실행 | 미확인(`+crt-static` 빌드는 의존 0을 확인) |
| SmartScreen·MOTW(브라우저 다운로드 + 탐색기 압축 해제), 시작 메뉴 바로 가기 | 미확인(`install.ps1 -NoShortcut`로 시험) |
| Authenticode 검증 | OWE-838(유료, 사람 결정) |
| winget 매니페스트 등록 | 후속(외부 게시) |
| 재부팅·로그오프 중 교체 | 미시험 |
