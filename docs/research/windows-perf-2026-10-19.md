# OWE-925 — Windows 터미널 성능 실측·최적화 (2026-10-19)

상태: Windows 최종 전후 측정 완료. 기준 소스 `2f4405d2becc050ac7aec066a39fa7c08c151f6b` (v0.1.10), release 바이너리. 측정하지 않은 항목은 미확인으로 남긴다. 이 문서는 ARCHITECTURE §4.1의 생존·출력 보장을 바꾸지 않는다.

## 결론

- 기본 경로(번들 `conpty.dll` + `OpenConsole`)에서 100MiB 출력은 before 대비 **wall 약 11%, krilld CPU 약 18%가 줄었다**(조용한 창, 변형별 2회). 에코 지연은 잡음 범위다.
- 축소 모드(inbox `conhost`)는 conhost CPU(약 64초/100MiB)가 상한이라 wall은 비슷하다(+2.7%, 원인 미확인). krilld CPU는 약 14% 줄었다.
- 남긴 변경 두 가지:
  1. **ASCII 빠른 경로**(`cli.rs` `Boundary::advance`, `term.rs` RawScan 건너뛰기). ESC가 없는 ASCII 묶음이 들어왔고 파서가 ground 상태이면 바이트 단위 경계 추적과 두 번째 vte 파스를 생략한다. Mac `sample`에서 엔진 시간의 약 22%가 이 두 단계였다(116샘플 중 경계 17, RawScan 9).
  2. **Windows 리더 큐**(`session/windows.rs`). 64KiB 버퍼를 재사용하고(read마다 하던 0 초기화 할당 제거), 큐를 1칸에서 8칸으로 늘렸다. 엔진은 이미 큐에 쌓인 read만 한 번에 모아 먹는다(타이머·대기 없음). bundle에서 묶음 수가 약 13% 줄었다.
- 메모리: krilld Private bytes가 bundle 출력 직후 약 10.8MB에서 11.5~11.8MB로 늘었다(큐·재사용 풀, 세션당 64KiB 버퍼 최대 약 20개). 사용자 지시(성능·사용성 우선)에 따라 받아들였다.

## 변경 hunk (OWE-913 충돌 확인용)

- `crates/krilld/src/session.rs`: **변경 없음.** 중간 단계에서 `read_loop`의 `needless_pass_by_value` expectation 제거와 `let mut source`를 넣었다. 하지만 `Reader::read`를 `&self` + `Cell`로 바꿔 모두 되돌렸다.
- `crates/krilld/src/session/windows.rs`: 모듈 문서 1줄, `use std::cell::Cell`, `ReadMessage`/`QUEUED_READS` 정의, `Reader`의 `recycle`·`pending` 필드, `start()`의 채널 2개와 버퍼 재사용, `read()` 본문(대기 메시지 + 이미 큐에 쌓인 read 모으기), 테스트 `queued_reads_preserve_bytes_through_eof` 추가. `Drop`·`halt`·`READERS`·`Master`는 손대지 않았다.
- `crates/krilld/src/cli.rs`, `term.rs`, `term/tests.rs`: ASCII 빠른 경로와 동등성 테스트(바이트 단위 vs 청크, 미완성 CSI/OSC/DCS/UTF-8 접두 포함).

## 환경과 측정 경계

- Windows: 공유 GCP `krill-win`, e2-standard-4 (4 vCPU, 메모리 16GiB), Windows Server 2022, Rust 1.98.1 MSVC.
- macOS: Mac mini Apple M6, 12코어, 32GB, macOS 27.0.1, Rust 1.98.1. 서로 다른 CPU·가상화라 OS만의 성능 비율로 해석하지 않는다.
- 기본 history 기록(켜짐, 암호화 꺼짐), 80×25, 단일 세션, 앱 미실행, CLI raw 구독자 1개. 자식은 저장소 밖 Rust `perf-probe emit|echo`. holder는 모든 변형에서 v0.1.10 그대로이고 krilld만 바꾼다.
- 처리량: 자식 준비 표식 뒤 CLI가 `go\r`를 보낸 시각부터 100MiB(80B 행 × 1,310,720) 뒤의 완료 표식이 CLI에 도착할 때까지. 매 실행 `x` 102,236,160개를 모두 받았다(손실 0).
- 에코: 워밍업 10회를 버리고 200회. 입력 프레임 전송 직전부터 자식의 고유 응답이 raw 스트림에 도착할 때까지. 물리 키보드·GUI 첫 프레임은 포함하지 않는다.
- CPU: 출력 직전·직후 `Get-Process.TotalProcessorTime` 차이.

## 최종 측정 (조용한 창)

2026-10-19 03:35 UTC 시작. 코디네이터가 OWE-913·915·917 워커의 VM 사용 종료를 확인했고, 시작 시 VM에 다른 cargo·rustc·krilld 프로세스는 없었다. 변형 3개(before = v0.1.10 krilld, ascii = 변경 1만, after = 변경 1+2)를 회차마다 순서를 돌려 2회씩 실행했다(1회차 before→ascii→after, 2회차 after→before→ascii).

| bundle (기본 경로) | 100MiB wall 2회(s) | 평균 MiB/s | krilld CPU(s) | OpenConsole CPU(s) | read 묶음 수 | 에코 p95(ms) |
|---|---|---:|---|---|---|---|
| before | 5.283 / 4.928 | 19.59 | 5.44 / 5.16 | 2.17 / 1.91 | 24,952 / 25,011 | 0.431 / 0.395 |
| ascii | 4.903 / 4.911 | 20.38 | 4.75 / 4.91 | 2.03 / 1.72 | 25,264 / 25,231 | 0.552 / 0.483 |
| after | 4.566 / 4.553 | 21.93 | 4.23 / 4.42 | 1.62 / 1.88 | 21,758 / 21,730 | 0.492 / 0.374 |

| inbox (축소 모드) | 100MiB wall 2회(s) | 평균 MiB/s | krilld CPU(s) | conhost CPU(s) | read 묶음 수 | 에코 p95(ms) |
|---|---|---:|---|---|---|---|
| before | 68.549 / 67.828 | 1.465 | 57.12 / 58.75 | 63.78 / 62.52 | 1,125,558 / 1,096,561 | 0.855 / 0.728 |
| ascii | 68.770 / 68.461 | 1.457 | 56.33 / 56.64 | 64.08 / 63.98 | 1,099,442 / 1,120,110 | 0.721 / 0.749 |
| after | 69.995 / 70.005 | 1.429 | 49.50 / 50.00 | 65.83 / 65.72 | 1,137,360 / 1,121,223 | 0.763 / 0.717 |

- bundle after/before: wall −10.7%, krilld CPU −18%. 변경 1만으로는 wall −3.9%, CPU −8.9%이고, 변경 2가 ascii 대비 wall −7.1%, CPU −10.5%를 더한다. 묶음 수 감소는 큐 모으기 효과다.
- inbox: conhost는 약 86B마다 한 번씩 써서 read가 약 110만 번 일어난다. 엔진이 conhost보다 빨라 큐가 거의 쌓이지 않으므로 묶음 수는 줄지 않았다. krilld CPU −14.1%(ascii만으로는 −2.5%)는 read마다 하던 64KiB 0 초기화 할당을 없앤 효과로 추정한다. after wall +2.7%(두 번 모두 70.0초)와 conhost CPU +4.2%의 원인은 미확인이다. 2회 표본이라 확정하지 않는다.
- 에코 p95는 세 변형이 0.37~0.86ms 안에서 겹쳐 회귀로 보지 않는다(2회 표본).
- 묶음 수 = 엔진 read 반환 횟수가 아니라 CLI가 받은 raw 프레임 수다.

### 부하 중 측정 (참고, 개선율로 쓰지 않음)

같은 날 앞서 다른 워커가 VM에서 release 빌드·nextest를 돌리는 중에 잰 기준값은 bundle 640.8 / 16.7 / 16.3초, inbox 137.2 / 105.0 / 93.0초였다. 조용한 창의 before(약 5초 / 68초)와 3~100배 차이가 나므로 그 수치는 부하 조건으로만 남긴다. 부하 중 bundle 실행 7회 중 3회에서 311~895초 정지가 났다(krilld는 read 대기, OpenConsole CPU는 정상, holder CPU만 2.1~3.5초로 증가). 조용한 창 bundle 6회에서는 재현되지 않았고 원인은 미확인이다.

부하 중 리더 모드 계측(ex-*, 1회씩, 변경 1 켠 상태): inbox 루프 누적에서 엔진은 약 5.4초인데 기록·발행은 26.6~30.0초였다. read 하나당 기록기 zstd flush + 파일 쓰기 + 발행이 고정 비용으로 든다. 이를 더 줄이려면 기록 묶음 단위를 바꿔야 하는데, 크래시 내구성(§4.1)과 맞물려 이번 범위에서 뺐다.

## macOS

- 리더 변경은 Windows 전용이고 Mac에 닿는 변경은 ASCII 빠른 경로뿐이다. 부하 없는 Mac 전후 처리량은 이번에 다시 재지 못했다. 외장 빌드 SSD 포화로 코디네이터가 Mac 빌드를 중단시켰고, 통합 브랜치 빌드에서 재기로 했다. 부하 중 기준값은 100MiB 1.011 / 1.251 / 1.166초(중앙값 85.78MiB/s)다.
- 재현(통합 브랜치에서):

```sh
export CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/Volumes/krill-build/targets/<이름>
cargo build --locked -p krill-holder -p krill --bins
cargo nextest run -p krilld --locked --no-fail-fast
cargo clippy -p krilld --all-targets --locked -- -D warnings
cargo build --release --locked -p krill-holder -p krilld --bins
cp "$CARGO_TARGET_DIR"/release/{krilld,krill-holder} ~/.cache/krill-win/spikes/OWE-925/mac-after-bin/
python3 ~/.cache/krill-win/spikes/OWE-925/final-mac.py   # before/after 교차 각 3회, mac-before-bin = v0.1.10
```

## 검증

- Windows 네이티브 리더 테스트(VM, release, `cargo test --release --locked -p krilld --lib session::windows`): 3 passed, exit 0. 신규 `queued_reads_preserve_bytes_through_eof` 포함, 최종 `Cell` 버전.
- Windows clippy: VM 네이티브 msvc(`cargo clippy --locked --all-targets -p krilld -- -D warnings`). Mac gnu 크로스는 외장 SSD 정지로 끝나지 않아 이것으로 대체했다. 결과는 아래 "Windows clippy 결과" 줄에 적는다.
- Mac nextest·clippy: **미확인.** 코디네이터가 통합 브랜치 빌드에서 확인한다. 이 워크트리에서 돌린 중간 실행들(외부 SIGTERM, 보조 바이너리 없음, SSD 정지로 중단)은 통과로 세지 않는다.
- GUI 앱 콜드 스타트·첫 프레임: 미확인 → OWE-916. Windows 10/11 실기: 미확인.

Windows clippy 결과: VM 네이티브 msvc exit 0, 소스 경고 0. 반복 경고는 공유 `clippy.toml`의 Windows에 없는 `std::os::fd::RawFd` 설정 하나뿐이다(기존 하네스, 수정하지 않음). 검사한 `session/windows.rs`는 최종본에서 기존 주석 한 줄("Gate startup ...")을 되살리기 전 판이다. 코드 차이는 없다(SHA-256 앞 16자리 cli `7af7e33effec8135`, term `b3d6a2f7a5176065`, term/tests `87c61e66bd8f3749`, session `8eb8c03efcb86354`=HEAD).

## 재현·증거

스파이크 소스·스크립트·원시 로그는 저장소 밖 `~/.cache/krill-win/spikes/OWE-925/`(최종 로그 `vm-q/`, 부하 중 실험 `vm-ex/`)와 VM `C:\spike\OWE-925\`에 둔다. 비밀번호·접속 키는 넣지 않는다.

```powershell
# VM: C:\spike\OWE-925\source = git archive 2f4405d2 + 이 브랜치의 krilld 파일
# before-bin\krilld.exe = v0.1.10 release, ascii-bin\krilld.exe = windows.rs·session.rs를 HEAD로 둔 release
# bin-dll\ = conpty.dll + x64\OpenConsole.exe
cargo build --release --locked -p krill-holder -p krilld --bins   # CARGO_BUILD_JOBS=3
powershell -File C:\spike\OWE-925\final-run3.ps1 -Label q -Reps 2
```
