# OWE-839 — release 워크플로 Windows 잡 + Windows 메모리 게이트 (VM 실측)

측정일: 2026-10-18. 공유 GCP `krill-win`(Windows Server 2022, e2-standard-4, Rust 1.98.1 MSVC). 작업 디렉터리 `C:\spike\OWE-839\`. 소스 `7ae09fe3`(OWE-837 병합) + 이 브랜치 스크립트.

**결론: release.yml에 macOS 잡과 독립인 `build-windows`·`publish-windows` 잡을 넣었고, 잡의 각 단계(버전 해석·ConPTY nupkg 확인·`+crt-static` 빌드·`bundle.ps1`·매니페스트 서명)를 VM과 Mac에서 따로 돌려 확인했다. 워크플로 전체를 GitHub에서 돌리지는 않았다(사람이 실행). Windows 메모리 게이트 `scripts/memory-gate-windows.ps1`은 대역 바이너리로 흐름·측정·`-Inject` 실패까지 확인했지만, 실제 krilld는 Windows `main`이 아직 스텁이라 소켓이 생기지 않아 2(준비 실패)로 끝난다. 실제 데몬 수치는 미확인이다.**

## 1. release.yml Windows 잡

| 잡 | 내용 | macOS 잡과의 관계 |
|---|---|---|
| `build-windows` (`windows-latest`) | 버전 해석(macOS와 같은 규칙) → `RUSTFLAGS=+crt-static` release 빌드(app·krill·krilld·holder·link) → ConPTY nupkg 받기·SHA-256 확인·풀기 → 공개키 유도 → `bundle.ps1` → `.zip.sha256` → `latest-windows.json` 서명 → artifact 업로드 | `needs` 없음. 서로의 빌드 실패에 영향 없음 |
| `publish-windows` (`ubuntu-latest`) | artifact를 받아 릴리스에 올린다. 릴리스가 있으면 `clobber`일 때 덮어쓰고, 아니면 zip이 이미 있을 때만 건너뛴다. 없으면 만든다 | `needs: [release, build-windows]`, `if: !cancelled() && build-windows 성공`. macOS 결과와 무관하게 돈다 |

- `publish-windows`가 macOS 잡 뒤에 도는 이유: macOS 잡은 릴리스가 이미 있고 `clobber=false`면 업로드를 통째로 건너뛴다. Windows가 먼저 릴리스를 만들면 macOS 자산이 빠진다. macOS가 실패해 Windows가 릴리스를 만든 경우 macOS를 다시 돌릴 때 `clobber=true`가 필요하다(잡 주석에 적음).
- 서명 키: 저장소 워크플로에는 Ed25519 시크릿이 없었다. macOS 매니페스트는 로컬 `packaging/publish-github.sh`가 키 파일로 서명한다. 그래서 `ed25519_manifest.py`가 이미 읽는 환경 변수 이름 `KRILL_RELEASE_SIGNING_KEY`를 시크릿 이름으로 썼다. 값은 macOS 업데이트 키 파일과 같은 64hex 시드여야 같은 공개키가 된다. **시크릿 등록은 사람이 한다.** 없으면 경고를 내고 공개키 없는 zip(업데이터 꺼짐)만 올린다.
- 매니페스트 이름: `latest-windows.json`. `bundle.ps1 -UpdateUrl`로 `Krill.json`의 `KrillUpdateURL`에 넣는다. 비우면 업데이터 기본값인 macOS `latest.json`을 보게 된다.
- Authenticode는 넣지 않았다. 자리만 `TODO(OWE-838)` 주석으로 남겼다.

### 검증

| 항목 | 결과 |
|---|---|
| `actionlint .github/workflows/release.yml` (1.7.12, Homebrew) | 경고 1건 SC2129(style) — **기존 macOS 잡 38행**. HEAD 원본 파일에서도 같은 1건. `-ignore SC2129`로 0 |
| 버전 해석·`PROTO_MIN` 추출(bash 단계, VM Git Bash) | `tag=v0.1.5`, `version=0.1.5`, `min_daemon_proto=1` |
| ConPTY 단계(VM, Windows PowerShell 5.1) | nupkg SHA-256 `02b07b34…dae7ee` 일치, `conpty.dll`·`x64\OpenConsole.exe` 추출 |
| release 빌드 `+crt-static`(VM, 콜드) | 성공, 21분 48초. exe 5개 모두 CRT DLL 의존 0(`dumpbin /dependents`: `VCRUNTIME*`·`api-ms-win-crt-*`·`ucrtbase` 없음). krill-app 52,325,376B(OWE-837에서 미측정이던 항목) |
| `bundle.ps1 -ConptyDir conpty -PublicKey <64hex> -UpdateUrl …/latest-windows.json` | 4초. zip 27,209,344B, 파일 9개 + `x64\`(app·krill·krilld·holder·link·conpty.dll·OpenConsole·Krill.json·고지). `Krill.json` BOM 없음(첫 바이트 `{`) |
| `.zip.sha256` | `c7b992bf…22c7ad  Krill-0.1.5-windows-x64.zip`(CRLF). 서명 단계의 `awk '{print $1}'`로 해시만 읽힘 |
| 매니페스트 서명(Mac, 일회용 키를 `KRILL_RELEASE_SIGNING_KEY`로) | `keygen`이 키 파일을 만들지 않고 공개키만 출력. `sign` 결과 `version`·`url`·`sha256`·`ed25519_sig`·`min_daemon_protocol`·`channel`·`rollout_pct` |
| GitHub Actions 실제 실행·업로드 | **미확인**(사람이 실행) |

## 2. Windows 메모리 게이트

지표(OWE-828 §2): PrivateUsage(`Get-Process` `PrivateMemorySize64`) 게이트, Private WS(CIM `Win32_PerfFormattedData_PerfProc_Process`) 보조. 예산은 macOS와 같다(데몬 60, 앱 120, 합계 200MiB). 사용법은 [docs/testing/memory-gate-windows.md](../testing/memory-gate-windows.md).

| 실행 | 결과 |
|---|---|
| 대역 바이너리(holder·krilld·krill 흉내, krilld는 8MiB + 주입량을 채워 쥠), 세션 10개 | 0(통과), 21초. `holder 0.6 krilld 8.5 daemon 9.1 MiB PrivateUsage`, Private WS `daemon 8.7` |
| 같은 대역, `-Inject 80` | **1(실패, 기대대로)**, 13초. `krilld 88.7 daemon 89.4 MiB` → `daemon > 60 MiB` |
| 정리 확인(두 실행 뒤) | 남은 holder·krilld·생성기 프로세스 0, `%TEMP%\mkw-*` 0 |
| 실제 release 바이너리, 기본·`-Inject 80` | 둘 다 **2(준비 실패)**, 33~37초. `daemon socket did not appear in 30s`. `krilld.exe` 단독 실행도 exit 1 — Windows `main`이 `daemon unavailable on Windows`로 끝나는 스텁이다(OWE-833 계열 진행 중) |
| `-App`(사람 GUI 세션) | 미실행(미확인). 헤드리스 VM에 대화형 세션이 없다 |

첫 대역 실행에서 고친 것 두 가지: (1) `finally`의 `taskkill`이 이미 끝난 자식 때문에 stderr를 쓰면 `ErrorActionPreference=Stop`이 통과 결과를 1로 바꿨다 → `cmd /c ... >nul 2>&1`로 삼킨다. (2) holder를 `-WindowStyle Hidden`으로 띄우면 holder 자신의 conhost가 'ConPTY 호스트'로 잡혔다 → `-NoNewWindow`.

## 3. 후속에 주는 함의

1. **krilld Windows `main`이 `ballast()`를 불러야 `-Inject`가 실제 krilld에서 동작한다.** 지금 `crates/krilld/src/main.rs`의 `ballast()`는 `#[cfg(unix)]`다. OWE-833 계열에서 Windows `main`을 구현할 때 함께 넣는다.
2. 실제 데몬 게이트 수치(holder + krilld ≤ 60MiB, ConPTY 호스트 합)는 Windows krilld가 세션을 만들 수 있게 된 뒤 이 스크립트를 그대로 돌려 채운다. ConPTY 호스트(conhost·OpenConsole)는 macOS 커널 PTY에 해당해 예산 밖으로 두고 기록만 한다. 이 분류는 제안이며 예산 문서는 바꾸지 않았다.
3. 앱 게이트(`-App`)는 세션을 탭으로 여는 것을 사람이 한다. macOS처럼 레이아웃 파일로 자동화하려면 `Layout` 저장 형식을 스크립트에서 쓸 방법(예: `krill` 하위 명령)이 필요하다.
4. 시크릿 `KRILL_RELEASE_SIGNING_KEY` 등록과 워크플로 첫 실행은 사람이 한다. 첫 실행 뒤 `latest-windows.json`을 실제 설치본(`--update-cli --check --json`)으로 확인한다.

## 4. 재현

스파이크 파일은 저장소 밖 `~/.cache/krill-win/spikes/OWE-839/`(`fake/` 대역 크레이트, `meta-test.sh`, `fake-run.txt`, `real-run.txt`, `build.log`)에 있다.

```powershell
# VM, C:\spike\OWE-839\src (이 브랜치 소스)
$env:CARGO_TARGET_DIR='C:\spike\OWE-839\target'; $env:RUSTFLAGS='-C target-feature=+crt-static'
cargo build --release --locked -p krill-app -p krill -p krilld -p krill-holder -p krill-link
powershell -File scripts\memory-gate-windows.ps1 -BinDir C:\spike\OWE-839\target\release [-Inject 80]   # 지금은 2
powershell -File scripts\memory-gate-windows.ps1 -BinDir C:\spike\OWE-839\fakebin [-Inject 80]         # 0 / 1
.\packaging\windows\bundle.ps1 -Version 0.1.5 -TargetDir C:\spike\OWE-839\target\release -ConptyDir conpty -PublicKey <64hex> -UpdateUrl <…/latest-windows.json>
```

```bash
actionlint .github/workflows/release.yml   # SC2129 1건(기존 macOS 잡)
```
