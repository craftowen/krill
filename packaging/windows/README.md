# Windows zip 패키지 (OWE-837)

관리자 권한 없이 사용자 단위로 `%LOCALAPPDATA%\Programs\Krill`에 설치하는 zip과 자체 업데이터용 레이아웃이다.
이 문서는 Windows 설치·패키징 절차를 설명한다. 내부 검증 원자료는 공개 저장소에 포함하지 않는다.
스크립트는 서명·업로드·원격 생성을 하지 않는다. Authenticode 서명은 OWE-838(유료 인증서, 사람이 결정)에서 다룬다.

## 레이아웃

```text
Krill-<ver>-windows-x64.zip
  Krill\Krill.json               CFBundleShortVersionString · KrillUpdatePublicKey · KrillUpdateURL
  Krill\krill-app.exe  krill.exe  krilld.exe  krill-holder.exe  [krill-link.exe]
  Krill\conpty.dll               ConPTY B안: holder가 자기 exe 옆에서 로드
  Krill\x64\OpenConsole.exe      conpty.dll이 <dll 폴더>\x64\OpenConsole.exe를 띄운다
  Krill\THIRD_PARTY_NOTICES.txt  ConPTY MIT 고지
```

`Krill.json`은 macOS `Info.plist`와 같은 키를 쓰는 JSON이다. 업데이터는 이 파일이 있는 exe 폴더를 설치 폴더로 본다.
새 터미널 세션에는 고정된 `conpty.dll`·`x64\OpenConsole.exe`가 모두 필요하다. 누락되거나 해시가 다른 패키지는
생성·업데이트를 거부한다. 기존 inbox 세션의 재연결은 보존하지만 새 세션에서 불투명한 OS HPCON 구조를 역참조하지 않는다.

## ConPTY 파일 받기 (저장소에 넣지 않음)

| 항목 | 값 |
|---|---|
| 패키지 | NuGet `Microsoft.Windows.Console.ConPTY` 1.25.260930003, 라이선스 MIT |
| 내려받기 | `https://www.nuget.org/api/v2/package/Microsoft.Windows.Console.ConPTY/1.25.260930003` |
| nupkg SHA-256 | `02b07b349af66d801159bdf9e440d4a1ce78bb951f37fc8609731665afdae7ee` |
| `runtimes\win-x64\native\conpty.dll` SHA-256 | `feeef341d891643c62d30b6b07800bc70f0bc148f44cb8c3bee84aa557ae805a` |
| `build\native\runtimes\x64\OpenConsole.exe` SHA-256 | `3d66b23d0a71bb8eed2b77edc8b9df9bf54ce6c8fb74c863a30e760997f80586` |

nupkg는 zip이다. `tar -xf conpty.nupkg -C <폴더>`로 풀고 그 폴더를 `-ConptyDir`로 준다.
`bundle.ps1`은 두 파일의 해시가 위 값과 다르면 묶지 않는다. 버전을 올릴 때는 이 표와 `bundle.ps1`의 `$ConptyPins`를 함께 바꾼다.

## 묶기 (Windows, PowerShell)

```powershell
$env:RUSTFLAGS = '-C target-feature=+crt-static'   # VC++ 런타임 의존 제거(근거: 연구 문서 §4)
cargo build --release --locked -p krill-app -p krill -p krilld -p krill-holder -p krill-link
powershell -ExecutionPolicy Bypass -File packaging\windows\bundle.ps1 -Version 0.1.5 `
  -ConptyDir C:\deps\conpty-1.25.260930003 -PublicKey <64hex 배포 공개키>
```

`packaging\dist\`(gitignore)에 `Krill\`과 zip을 만들고 zip의 SHA-256을 출력한다. 기존 산출물은 덮어쓰지 않는다.
Windows 매니페스트는 `latest-windows.json`이다. macOS와 같은 키·`packaging/macos/ed25519_manifest.py sign`으로 만든다(서명 형식이 플랫폼과 무관하다).
macOS `latest.json`과 이름이 겹치지 않게 `-UpdateUrl`을 `…/releases/latest/download/latest-windows.json`으로 준다. 비우면 업데이터 기본값(macOS `latest.json`)을 본다.

`build.ps1`은 위 빌드·nupkg 확인(캐시 `%LOCALAPPDATA%\krill-release-cache`)·`bundle.ps1`·`.zip.sha256`를 한 번에 한다.
`smoke.ps1 -Zip <zip>`은 임시 폴더에 `install.ps1`로 설치해 데몬 기동·세션 입출력·krilld 강제 종료 생존을 보고,
`krill-app.exe`를 5초 띄워 결과만 적는다(헤드리스 SSH에선 창이 못 떠 끝날 수 있어 실패로 보지 않는다).

## 맥에서 게시 (`publish-github.sh`)

`KRILL_WIN_VM_DIR=<VM 접속 폴더>`를 주면 `publish-github.sh`가 `build-vm.sh`로 상시 Windows VM을 켜서
`git archive HEAD`를 `build.ps1`·`smoke.ps1`로 빌드·검사하고 zip·`.sha256`을 가져온 뒤 VM을 끈다(삭제 안 함).
공개키(`-PublicKey`)는 맥의 업데이트 키에서 뽑아 넘기고, 비밀키는 VM에 보내지 않는다. `latest-windows.json`은
맥에서 macOS와 같은 키로 서명하고, zip·`.sha256`·`latest-windows.json`·`install.ps1`을 릴리스 자산에 넣는다.
VM 접속 폴더에는 `up.sh`·`ssh.sh`·`down.sh`·`ip`·`id_ed25519`·`known_hosts`가 있어야 한다(저장소 밖).

## 릴리스 워크플로 (OWE-839)

`.github/workflows/release.yml`의 `build-windows` 잡이 위 빌드(`+crt-static`)·ConPTY nupkg 해시 확인·`bundle.ps1`을 돌려
`Krill-<ver>-windows-x64.zip`·`.zip.sha256`·`latest-windows.json`을 만들고, `publish-windows` 잡이 macOS 잡이 끝난 뒤 같은 릴리스에 올린다.
서명 비밀키는 저장소 시크릿 `KRILL_RELEASE_SIGNING_KEY`(macOS 업데이트 키와 같은 64hex 시드, `ed25519_manifest.py`가 읽는 환경 변수 이름)다.
시크릿이 없으면 공개키 없는 zip(업데이터 꺼짐)만 올리고 매니페스트는 건너뛴다. Authenticode 서명 자리는 주석으로만 남겼다(OWE-838).

## 설치·업데이트·제거

Windows 릴리스 ZIP과 `.sha256`이 공개된 뒤에는 PowerShell에서 한 줄로 설치한다.
홈페이지는 배포 전까지 준비 중으로 표시한다.

```powershell
irm https://krill.page/install.ps1 | iex
```

인자 없이 실행하면 최신 릴리스의 x64 ZIP과 SHA-256을 내려받고 검증한 뒤 설치하며,
시작 메뉴·사용자 PATH를 등록하고 앱을 연다. ARM64·32비트 Windows는 거부한다.
`-Release v<버전>`으로 버전을 선택하고 `-NoOpen`으로 자동 실행을 생략할 수 있다.
홈페이지 Worker는 이 디렉터리의 `install.ps1` 원본을 직접 번들한다.

이미 받은 ZIP으로 설치하거나 업데이트하려면:

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1 -Zip .\Krill-0.1.5-windows-x64.zip [-AddToPath]
& "$env:LOCALAPPDATA\Programs\Krill\krill-app.exe" --update-cli [--check] [--json]
```

- 설치: zip을 풀어 `%LOCALAPPDATA%\Programs\Krill`로 옮기고 시작 메뉴 바로 가기를 만든다. 이미 있으면 거부한다.
- 업데이트: 실행 중인 exe는 덮어쓸 수 없어 파일 단위로 `Krill.prev`로 옮기고 새 파일을 놓는다. 실패하면 `Krill.prev`에서 되돌린다.
  지울 수 없는 구 이미지(아직 도는 holder 등)는 `Krill.trash`로 옮겨 두고 다음 업데이트 때 지운다.
- 제거: Krill 프로세스를 끝낸 뒤 `%LOCALAPPDATA%\Programs\Krill`(와 `.prev`·`.trash`), 시작 메뉴 `Krill.lnk`를 지운다.
  세션·설정까지 지우려면 `%LOCALAPPDATA%\Krill`도 지운다.

설치기 회귀 검사: `pwsh -NoProfile -File packaging/windows/test_install.ps1`.
임시 ZIP과 가짜 다운로드 응답으로 해시 검증·미배포·기존 설치 보존·설치를 검사한다.
실제 Windows의 바로 가기·PATH 등록·앱 실행 검증은 별도다.
