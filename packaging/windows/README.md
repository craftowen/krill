# Windows zip 패키지 (OWE-837)

관리자 권한 없이 사용자 단위로 `%LOCALAPPDATA%\Programs\Krill`에 설치하는 zip과 자체 업데이터용 레이아웃이다.
설치 형식을 이렇게 고른 근거와 실측은 [docs/research/windows-spike-OWE-837.md](../../docs/research/windows-spike-OWE-837.md)에 있다.
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
`conpty.dll`·`OpenConsole.exe`가 없으면 holder는 kernel32 축소 모드로 동작한다(OWE-826 §7).

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

## 릴리스 워크플로 (OWE-839)

`.github/workflows/release.yml`의 `build-windows` 잡이 위 빌드(`+crt-static`)·ConPTY nupkg 해시 확인·`bundle.ps1`을 돌려
`Krill-<ver>-windows-x64.zip`·`.zip.sha256`·`latest-windows.json`을 만들고, `publish-windows` 잡이 macOS 잡이 끝난 뒤 같은 릴리스에 올린다.
서명 비밀키는 저장소 시크릿 `KRILL_RELEASE_SIGNING_KEY`(macOS 업데이트 키와 같은 64hex 시드, `ed25519_manifest.py`가 읽는 환경 변수 이름)다.
시크릿이 없으면 공개키 없는 zip(업데이터 꺼짐)만 올리고 매니페스트는 건너뛴다. Authenticode 서명 자리는 주석으로만 남겼다(OWE-838).

## 설치·업데이트·제거

```powershell
powershell -ExecutionPolicy Bypass -File install.ps1 -Zip .\Krill-0.1.5-windows-x64.zip [-AddToPath]
& "$env:LOCALAPPDATA\Programs\Krill\krill-app.exe" --update-cli [--check] [--json]
```

- 설치: zip을 풀어 `%LOCALAPPDATA%\Programs\Krill`로 옮기고 시작 메뉴 바로 가기를 만든다. 이미 있으면 거부한다.
- 업데이트: 실행 중인 exe는 덮어쓸 수 없어 파일 단위로 `Krill.prev`로 옮기고 새 파일을 놓는다. 실패하면 `Krill.prev`에서 되돌린다.
  지울 수 없는 구 이미지(아직 도는 holder 등)는 `Krill.trash`로 옮겨 두고 다음 업데이트 때 지운다.
- 제거: Krill 프로세스를 끝낸 뒤 `%LOCALAPPDATA%\Programs\Krill`(와 `.prev`·`.trash`), 시작 메뉴 `Krill.lnk`를 지운다.
  세션·설정까지 지우려면 `%LOCALAPPDATA%\Krill`도 지운다.
