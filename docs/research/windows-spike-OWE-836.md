# OWE-836 — Windows 세션 생존 e2e (앱 강제종료 1,000회·krilld/holder kill·출력 해시)

측정일 2026-10-18(VM 시각). 공유 GCP `krill-win`(Windows Server 2022, e2-standard-4), Rust 1.98.1 MSVC **release** 빌드, 브랜치 `craftowen/win-p3-e2e`(기반 `8f3cdf6b` + 수정 `b8811cdb`). 비밀번호 로그온 예약 작업(`Register-ScheduledTask -User krill -Password`, LogonType Password)에서 실행했다. 이 문서는 조사 기록이며 ARCHITECTURE §4.1을 바꾸지 않는다.

**결론: 앱(헤드리스 앱 프로세스) `taskkill /F` 1,000회를 inbox ConPTY와 번들 OpenConsole에서 각각 돌렸고, 세션·krilld 교체 없이 원본 바이트 SHA-256 일치, Gap 0, 누락 줄 0이었다. krilld `taskkill /F` 30회도 세션은 매번 살았고, 손실은 §4.1대로 Gap(len 0) 표시와 함께 0~50줄(18만~22만 줄 중)이었다. holder kill 뒤 krilld는 고아 세션을 무손실로 계속 서빙했다. 측정 중 Windows krilld 기록 저장소가 늘 꺼져 있던 제품 버그(`Store::open` os error 5)를 찾아 최소 수정했다(`b8811cdb`). 수정 전에는 링(256KiB) 밖 재생이 전부 Gap이었다. 또 holder가 죽은 뒤 앱식 재접속은 5초 Timeout으로 끝나고, 시도할 때마다 holder+krilld 2개가 남는다(Adopt 미구현과 연결된 후속 과제).**

## 방법

- 테스트: `crates/krill-app/tests/win_survival_e2e.rs`(`#![cfg(windows)]`, 4개 모두 `#[ignore]`). DPAPI가 되는 로그온과 release holder·krilld가 있어야 해서 기본 실행에서 뺐다. 횟수는 환경변수로 정한다.
- 세션 셸은 `cmd.exe /D`다. holder·krilld는 매번 새 상태 디렉터리(`KRILL_STATE_DIR`)에서 새로 띄운다. 이 환경변수는 자식 프로세스에만 주고(`set_var` 금지), holder는 그 자식이 bootstrap으로 띄운다.
- **해시 기준**: ConPTY는 자식이 쓴 바이트를 다시 그린 VT로 바꿔 내보낸다. 그래서 생성기 바이트와 PTY 바이트는 원래 다르다(아래 시나리오 4의 바이트 수 참고). 바이트 해시는 "krilld가 PTY에서 읽은 원본"끼리 비교한다. 한쪽은 클라이언트가 실시간으로 받은 `Stream::Raw`, 다른 쪽은 끝에서 `Raw { from: 0 }`으로 받은 링+디스크 세그먼트 재생이다. 생성기와는 줄 단위로 비교한다. 각 줄은 번호와 검사 꼬리표(`{p}{n:07}-{tag:08x}`)를 갖고, VT 제어열을 뺀 평문에서 모든 번호가 맞는 꼬리표로 있는지 본다.
- **시나리오 1(앱)**: 자식 프로세스가 앱과 같은 경로로 붙는다. 순서는 감시 연결 → `krill_app::layout` → `pane::ensure_session` → `pane::attach` + `Pane`이다. 그다음 `(for /L … do @echo Iiiii-Ln)&echo Diiii`(200줄)를 입력하고, 입력 에코가 화면에 보이면(출력이 흐르는 중) 드라이버가 `taskkill /F`로 죽인다. 다음 실행은 레이아웃의 같은 세션에 붙어, 화면에 앞 라운드의 `D` 줄이 보이는지 확인한다. 각 실행은 해시 검사용 원본 스트림을 앞 실행들이 받은 길이부터 이어 받아 파일에 붙인다. 이음이 어긋나거나 Gap이 오면 기록한다. 끝에 이 파일과 `from: 0` 재생을 비교한다.
- **시나리오 2(krilld)**: 생성기(이 테스트 바이너리, 50줄마다 10ms 쉼)가 출력하는 동안 1.5초 간격으로 krilld를 `taskkill /F`한다. holder의 1초 안정 기준을 넘겨 즉시 재기동 경로를 탄다. 실시간 클라이언트는 앱처럼 다시 붙어 받은 위치부터 이어 받는다.
- **시나리오 3(holder)**: 출력 중 holder를 `taskkill /F`한다. 이어서 고아 서빙, 새 세션 거절, 앱식 재접속(`daemon::connect`), 입력을 확인한다. 끝으로 krilld까지 죽여 holder·krilld 동시 소실 뒤 무엇이 남는지 본다.
- **시나리오 4(충실도)**: cmd `type`으로 SGR 색이 들어간 10만 줄(3,900,013B)을 출력한다. 이어서 대체 화면 TUI 파일 두 개를 출력한다. 첫째는 `?1049h` + 50프레임 × 20행 커서 이동 그리기, 둘째는 `?1049l` 복귀다.
- ConPTY 두 모드: inbox(kernel32)와 번들(OWE-832의 `conpty.dll` + `x64\OpenConsole.exe`를 holder 옆에 둠).

## 결과표

마지막 실행(최종 바이너리)의 값이다. 괄호 안은 같은 코드의 앞선 실행값이다.

| 시나리오 | ConPTY | 횟수 | 손실 | 해시·Gap | 소요 |
|---|---|---:|---|---|---|
| 1 앱 `taskkill /F` | inbox | 1,000 | 누락 줄 0 / 200,000줄, 라운드 끝 표시 1,000/1,000 | 원본 15,706,912B, 앱 수신분+이어받기 = 재생 SHA-256 일치(`b8ed9f94…`), Gap 0 | 117.3초(112.1). 라운드 p50 115.0ms·최대 699.7ms, 재접속 p50 35.3ms·최대 79.9ms |
| 1 앱 `taskkill /F` | 번들 | 1,000 | 누락 줄 0, 끝 표시 1,000/1,000 | 원본 25,400,191B, SHA-256 일치(`278ba120…`, 두 실행 바이트까지 동일), Gap 0 | 114.5초(116.1). 라운드 p50 113.0ms·최대 731.2ms, 재접속 p50 34.0ms·최대 72.0ms |
| 2 krilld `taskkill /F` | inbox | 30 | 1줄 / 184,000줄(앞선 실행 7 / 185,050) | 실시간 = 재생 3,496,993B 일치, 재생 Gap 29개 모두 len 0 | 복구 p50 140.1ms·최대 200.1ms, kill 구간 52.1초 |
| 2 krilld `taskkill /F` | 번들 | 30 | 0 / 223,950줄(앞선 실행 50 / 224,250) | 실시간 = 재생 4,256,111B 일치, Gap 29개 len 0 | 복구 p50 136.9ms·최대 148.6ms |
| 3 holder `taskkill /F` | inbox 5회·번들 4회 | 9 | 고아 서빙 중 누락 0(22,500~28,800줄) | 실시간 = 재생 일치, Gap 0 | 앱식 재접속 Timeout 5,016~5,040ms |
| 4 큰 출력 + TUI | inbox | 3 | 누락 줄 0 / 100,000, 꼬리표 오류 0 | PTY 원본 4,104,465~4,105,064B 실시간 = 재생, Gap 0. 대체 화면 그림·복귀 확인 | `type` 11.9~12.2초 |
| 4 큰 출력 + TUI | 번들 | 2 | 누락 줄 0 | PTY 원본 3,918,770B(두 실행 동일) 실시간 = 재생, Gap 0. 대체 화면 확인 | `type` 5.3~5.4초 |

- 1,000회는 처리율이 높아(약 9라운드/초) 줄이지 않았다.
- 시나리오 1은 매 라운드 krilld pid가 같았고, 첫 라운드만 세션을 만들었다(`created=true`). 세션 프로세스는 끝까지 살아 있었다.
- 시나리오 2는 매번 새 krilld가 뜨고, 세션 pid가 살아 있었다. 끝에서 새 krilld의 부모가 원래 holder임을 확인했다. 30회에 Gap이 29개인 이유(한 번은 미기록 꼬리가 없었을 것으로 **추정**)는 확인하지 않았다.
- 3회 kill 스모크도 별도로 통과했다(누락 0~4줄).

### 시나리오 3 세부(9회 모두 같은 결과)

| 항목 | 결과 |
|---|---|
| holder 사망 뒤 krilld·세션 | 둘 다 생존, 출력 계속, 입력(`echo ORPHAN_…`)도 화면에 반영 |
| 고아 상태 새 세션 | `Failed(NoHolder)` |
| 앱식 재접속 `daemon::connect` | `Timeout`(약 5.03초). 고아 krilld를 본 클라이언트가 새 holder를 띄우지만, 새 holder의 krilld가 고아에게서 세션을 넘겨받지 못한다(Adopt 미구현) |
| 재접속 시도의 잔여물 | **시도마다 holder+krilld 2개가 남는다**. connect의 타임아웃 정리(`holder.kill()`)는 이미 끝난 bootstrap만 겨냥하고, 실제 holder는 그대로 남는다. 고아 세션에는 영향이 없었다(이후 출력 무손실·입력 정상) |
| 이어서 krilld kill(holder·krilld 동시 소실) | 세션 프로세스가 10초 안에 사라짐. 다음 기동에서 새 holder·krilld, 목록 비어 있음. 디스크 기록의 콜드 화면에 마지막 출력(`ORPHAN_INPUT_OK`)이 있음 |

## 발견한 제품 버그와 수정

**Windows krilld 기록 저장소가 항상 꺼져 있었다** (수정 `b8811cdb`, `crates/krilld/src/store/privacy.rs`).

- 증상: 상태 디렉터리 `record-warnings.log`에 `store open failed: Access is denied. (os error 5)`가 남았다. 첫 스모크에서 다음이 관측됐다.
  - 시나리오 4 재생이 정확히 262,144B(= `RING`)에서 멈추고 Gap 1개, 재생 기준 누락 13,690/20,000줄.
  - 시나리오 2 재생이 32,506B뿐(실시간 354,258B), 누락 17,850줄.
- 오진 배제:
  - 같은 예약 작업에서 PowerShell DPAPI는 성공했다.
  - krilld 단위 테스트 `dpapi_round_trip_and_tamper`·`key_file_is_dpapi_…` 2개도 통과했다.
  - holder가 띄운 세션 프로그램 안에서도 두 테스트가 통과했다.
  - 따라서 DPAPI가 아니다.
- 원인: `privacy::prepare`가 `sessions` 디렉터리를 `File::open`으로 연다. Windows는 `FILE_FLAG_BACKUP_SEMANTICS` 없이 디렉터리 핸들을 주지 않는다(OWE-835의 디렉터리 fsync 실측과 같은 현상). Windows `exclude_from_backup`은 원래 no-op이다. 기존 store 테스트가 `cfg(unix)`라 Windows에서 `Store::open` 전체를 돈 적이 없었다.
- 수정: Windows에서만 `OpenOptionsExt::custom_flags(FILE_FLAG_BACKUP_SEMANTICS)`로 연다(unsafe 없음).
- 수정 뒤:
  - 시나리오 4 재생 = 실시간(3.9~4.1MB), Gap 0.
  - 시나리오 2 재생에 kill 지점마다 디스크의 Gap(len 0) 표시가 남는다.
  - holder·krilld 동시 소실 뒤 콜드 화면이 복원된다.
- OWE-833-8의 e2e는 출력이 링 안이고 재생을 보지 않아 이 문제가 드러나지 않았다.

## 테스트 쪽에서 고친 함정(제품 문제 아님)

- 화면 스트림에 붙은 입력 연결이 프레임을 읽지 않은 채 대량 출력이 10초 넘게 흐르면, krilld가 `attach.rs`의 `WRITE_TIMEOUT`(10초)으로 연결을 끊는다(10054). 이것은 설계된 느린 클라이언트 처리다. 테스트는 입력마다 새 연결을 연다.
- 드라이버가 `daemon::connect`를 직접 부르면, 드라이버에 `KRILL_STATE_DIR`이 없어 holder가 기본 `%LOCALAPPDATA%\Krill`로 뜬다. 데몬 기동·재접속은 모두 자식 프로세스로 옮겼다. 초기 시험 중 기본 디렉터리에 생긴 `sessions`·`monitor.log`·`scheduler.lock`은 생성 시각(15:42 이후)으로 골라 지웠다. 기존 `Logs`는 남겼다.
- krilld 운영 로그는 `KRILL_STATE_DIR`과 무관하게 `%LOCALAPPDATA%\Krill\Logs\krilld.log`에 덧붙는다. 테스트 격리상 참고 사항이다.

## 재현

```sh
# Mac
~/.cache/krill-win/gcp/up.sh
git bundle create ~/.cache/krill-win/spikes/OWE-836/src.bundle HEAD
~/.cache/krill-win/spikes/OWE-836/scp.sh ~/.cache/krill-win/spikes/OWE-836/src.bundle C:/spike/OWE-836/src.bundle
# VM: clone 뒤 빌드(첫 빌드 21분 32초, 테스트만 바꾸면 약 1분)
#   $env:CARGO_TARGET_DIR="C:\spike\OWE-836\target"
#   cargo build --release --locked --workspace --bins
#   cargo test --release --locked -p krill-app --test win_survival_e2e --no-run
#   bin-inbox\ = krill-holder.exe·krilld.exe·krill.exe, bin-dll\ = 같은 것 + conpty.dll + x64\OpenConsole.exe
# 비밀번호 로그온 예약 작업(비밀번호는 출력하지 않음)으로 실행
~/.cache/krill-win/spikes/OWE-836/register.sh
~/.cache/krill-win/spikes/OWE-836/mkjob.sh s1-inbox app_taskkill_loop_keeps_session_and_bytes inbox KRILL_E2E_APP_KILLS=1000
~/.cache/krill-win/spikes/OWE-836/job.sh s1-inbox ~/.cache/krill-win/spikes/OWE-836/jobs/s1-inbox.ps1
# 나머지: krilld_taskkill_during_output(KRILL_E2E_KRILLD_KILLS=30), holder_taskkill_keeps_orphan_serving,
#         big_output_and_alt_screen_hash(KRILL_E2E_BIG_LINES=100000), 각각 inbox|dll
```

VM에서 직접 실행할 때는 `win_survival_e2e-*.exe --ignored --exact <테스트> --nocapture --test-threads=1`을 쓴다. 환경변수는 `KRILL_E2E_BIN`(holder·krilld 디렉터리)과 `KRILL_E2E_DIR`(작업 루트, 지정하면 남김)이다. 증거 로그는 저장소 밖 `~/.cache/krill-win/spikes/OWE-836/vm-logs/`와 VM `C:\spike\OWE-836\logs`·`runs`에 있다. 예약 작업 `OWE836`은 끝나고 등록 해제했다.

## 검증

| 명령 | 결과 |
|---|---|
| Mac `cargo clippy -p krilld -p krill-app --all-targets --locked -- -D warnings` | 0 |
| Mac `cargo nextest run -p krilld -p krill-app --locked --no-fail-fast` | 1,205 통과·12 skipped·실패 0 |
| Mac `cargo fmt --all --check` | 0 |
| `scripts/check-windows.sh --clippy`(워크스페이스) | 0(공유 `clippy.toml` RawFd 설정 경고 1건만, 코드 무관) |
| VM MSVC release `cargo test --no-run` | 경고 0 |
| VM 시나리오 1~4 | 위 표, 마지막 실행 모두 통과 |

## Phase 3 설계에 주는 함의

1. **Windows에서 "앱 크래시 → 무손실"은 링·세그먼트 범위에서 실측으로 성립한다.** 1,000회 × 2모드(원본 15.7MB·25.4MB), Gap 0. 단 `b8811cdb` 수정이 있어야 링 밖이 성립한다. Windows에서 `Store::open`을 끝까지 도는 단위 테스트가 없으니 하나 두는 것이 좋다(store 테스트는 `cfg(unix)`, 후속).
2. **krilld 크래시 손실은 macOS와 같은 모양이다.** 세그먼트에 쓰기 전 바이트만 잃고 Gap(len 0)으로 표시되며 세션은 산다. 30회에 0~50줄. 복구는 p50 약 140ms, 최대 200ms다.
3. **holder 크래시 복구가 Windows 최대 공백이다.**
   - 고아 서빙은 무손실이다.
   - 하지만 앱의 표준 재접속(`daemon::connect`)은 고아를 보면 새 holder를 띄우고, 5초 Timeout 뒤 holder+krilld를 남긴다.
   - Adopt 구현 전까지 Windows connect는 고아 krilld에 그대로 붙거나(재결합 없이), 실패 시 실제 holder까지 정리해야 한다(proto, 후속).
   - holder 코드 상한(약 1,680/1,700줄) 때문에 이번에는 고치지 않았다.
4. **holder·krilld 동시 소실이면 Windows 세션은 죽는다**(10초 안). 남는 것은 디스크 기록의 콜드 화면이다. §4.1 macOS 행과 같다.
5. **ConPTY 충실도**:
   - 줄 단위로는 두 모드 모두 완전했다.
   - 바이트는 다시 그려진다. 3.90MB 원본이 inbox에서는 4.10MB가 되고 변동이 있다. 번들에서는 3.92MB이고 반복 실행 바이트가 같았다.
   - 4MB `type`은 inbox가 번들보다 약 2.2배 느렸다.
   - 번들 OpenConsole 쪽이 결정적이고 빠르다.
6. **DPAPI 로그온 의존**: SSH 키 로그온에서는 krilld 기록이 꺼진다(OWE-833). 생존 e2e는 비밀번호 로그온(또는 대화형)에서 돌려야 링 밖 무손실을 볼 수 있다.

## 미확인

- 실제 `krill-app` GUI 프로세스 kill. VM에 대화형 데스크톱이 없어 앱과 같은 라이브러리 경로를 쓰는 헤드리스 프로세스로 대신했다.
- Windows 10/11, 대화형 로그온에서의 DPAPI.
- 새 holder Adopt(미구현).
- krilld 업그레이드 인계.
- 잠자기·재부팅.
- 메모리.
- nextest 러너의 Job 안에서 holder bootstrap breakaway. 테스트 바이너리를 직접 실행했다.
- 시나리오 2의 Gap이 29개(30이 아님)인 정확한 이유.

## 후속 수정 (코디네이터, 2026-10-19)

`daemon::connect`가 Windows에서 고아 krilld를 보면 새 holder를 띄우던 동작을 바꿨다: Adopt가 구현될 때까지 Windows는 고아 krilld에 그대로 붙고(`ready_handshake`), 감시 연결(`wait_closed`·`watch_for`)도 `Orphan` 통보로 깨지 않는다(깨면 같은 고아에 다시 붙는 바쁜 루프가 된다). Unix 동작은 그대로다. 재실측(같은 시나리오 3, release·비밀번호 로그온):

| 모드 | 앱식 재접속 | 잔여 프로세스 | 고아 서빙 |
|---|---|---|---|
| inbox | 고아 krilld에 연결, 19ms | 0 | 누락 0, SHA 일치, 새 세션 `NoHolder` |
| 번들 | 고아 krilld에 연결, 21ms | 0 | 누락 0, SHA 일치, 새 세션 `NoHolder` |

`strays` 조회가 조회용 powershell 자신을 잡던 것도 제외했다(수정 전 잔여 2개 중 holder·krilld만 실제 잔여였다).
