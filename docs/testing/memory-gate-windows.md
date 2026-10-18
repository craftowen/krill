# Windows 메모리 회귀 게이트 (OWE-839, MK-066 Windows판)

`scripts/memory-gate-windows.ps1`. macOS 게이트(`scripts/memory-gate.sh`, `phys_footprint`)와 같은 시나리오를 Windows 지표로 잰다.
지표를 고른 근거는 [windows-spike-OWE-828.md §2](../research/windows-spike-OWE-828.md)이고, VM 실측은 [windows-spike-OWE-839.md](../research/windows-spike-OWE-839.md)에 있다.

## 지표와 예산

| 항목 | 값 |
|---|---|
| 게이트 지표 | PrivateUsage(커밋) = `Get-Process`의 `PrivateMemorySize64`. 작업 집합 트리밍·페이지 아웃으로 줄지 않는다 |
| 보조 기록 | Private Working Set(`Win32_PerfFormattedData_PerfProc_Process.WorkingSetPrivate`, CIM). 성능 카운터 경로는 지역화돼 `Get-Counter`를 쓰지 않는다 |
| 데몬 | krill-holder + krilld ≤ 60MiB |
| 앱(`-App`) | krill-app ≤ 120MiB, 앱 + 데몬 ≤ 200MiB |
| 예산 밖 | 세션 프로세스(생성기), ConPTY 호스트(holder 자식 conhost·OpenConsole). 호스트 합은 기록만 한다. Windows에는 macOS 커널 PTY에 해당하는 사용자 모드 호스트가 세션마다 붙기 때문이다 |

## 시나리오

1. 격리한 상태 디렉터리(`%TEMP%\mkw-<pid>`, `KRILL_STATE_DIR`)에 release `krill-holder --detach krilld <소켓>`을 직접 띄운다. 정리는 이 holder 트리만 하므로 사용자가 쓰는 Krill 데몬은 건드리지 않는다.
2. `krill new`로 세션 N개(기본 10, 120×40)를 만든다. 각 세션의 생성기(PowerShell)는 `go` 파일을 기다렸다가 한글·색·120열을 넘는 줄 2만 개를 내고 `done-<i>`를 남긴 채 산다.
3. (`-App`) krill-app을 띄우고, 사람이 세션 N개를 탭으로 연 뒤 Enter를 누른다.
4. `go`를 만들고 모든 `done-<i>`를 기다린 뒤 10초 쉬고 잰다.

빌드는 release.yml과 같은 `+crt-static` release다(`-BinDir`를 주면 빌드를 건너뛴다).

## 실행

```powershell
powershell -ExecutionPolicy Bypass -File scripts\memory-gate-windows.ps1                # 데몬 게이트(SSH 등 비대화형 세션 가능)
powershell -ExecutionPolicy Bypass -File scripts\memory-gate-windows.ps1 -Inject 80     # 회귀 주입: 1이어야 정상
powershell -ExecutionPolicy Bypass -File scripts\memory-gate-windows.ps1 -App           # 사람이 로그온한 GUI 세션에서만
```

종료 코드: 0 통과, 1 예산 초과, 2 준비 실패(빌드·데몬 기동·세션 생성·출력 시간 초과).
`-Inject <MiB>`는 `KRILL_TEST_BALLAST_MB`를 holder→krilld에 넘긴다. krilld가 그만큼 0이 아닌 값으로 채운 버퍼를 쥐어 게이트가 회귀를 잡는지 본다.

## 현재 상태 (2026-10-18)

- **데몬 게이트는 아직 실제로 돌지 않는다.** Windows krilld `main`이 "daemon unavailable on Windows"로 바로 끝나서(OWE-833 계열 진행 중) 소켓이 생기지 않고 게이트는 2로 끝난다.
- `-Inject`가 실제 krilld에서 동작하려면 Windows krilld `main`도 `ballast()`를 불러야 한다. 지금 `crates/krilld/src/main.rs`의 `ballast()`는 `#[cfg(unix)]`이다(OWE-833 후속).
- 스크립트 흐름(기동·세션 생성·대기·측정·판정·정리)과 `-Inject` 실패는 holder·krilld·krill 대역 바이너리로 VM에서 확인했다(연구 문서 §2).
- `-App` 모드는 실행하지 않았다(미확인). 헤드리스 VM에는 대화형 세션이 없다.
