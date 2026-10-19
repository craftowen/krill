# OWE-915 — Windows SSH 재활성화와 모바일 게이트웨이

측정일: 2026-10-19(KST). 공유 GCP `krill-win`, Windows Server 2022, Rust 1.98.1 MSVC, `OpenSSH_for_Windows_9.5p2, LibreSSL 3.8.2`. POSIX 대상은 Mac의 작업 전용 비특권 `/usr/sbin/sshd`다. VM 부하와 Mac 빌드가 겹친 관측이므로 아래 시간은 성능 보장이 아니다.

## SSH 파이프 최소 스파이크

코드는 저장소 밖 `~/.cache/krill-win/spikes/OWE-915/`, VM에서는 `C:\spike\OWE-915\`에 두었다. OWE-840의 `-L` 미지원 결론은 유지하고, 연결마다 `ssh -T -W <원격.sock>`을 실행하는 A안을 검증했다.

| 실험 | 실측 | 결론 |
|---|---|---|
| std `Stdio::piped()` stdout | 8초 제한 초과 | 재사용하지 않음 |
| std `io::pipe()` stdout | 8초 제한 초과 | 동기 파이프도 해결하지 못함 |
| tokio named pipe 서버 + 자식 **동기** File stdout | `named-ready` 수신, EOF는 8초 제한 초과 | 데이터 수신만으로 성공 판정하면 안 됨 |
| tokio named pipe 서버 + 자식 **overlapped** File stdout | 첫 정상 왕복 336.906ms, 이후 5회 모두 응답·EOF 수신(1.313–2.839초) | 자식 끝에도 `FILE_FLAG_OVERLAPPED` 필요 |
| stdin·stdout·stderr 3개 모두 overlapped, POSIX `cat` | 2.161초, NUL 포함 17바이트 일치, exit 0 | 바이너리와 stdin EOF 정상 |
| 같은 stdio로 `ssh -W <Mac echo.sock>` | 365.979ms, `hello-world` 11바이트 일치, exit 0 | 실제 streamlocal 채널 성공 |
| 초기 1MiB `-W` 왕복 | 15초 제한 초과 | 공유 부하 관측, 처리량 보장 아님 |
| 최종 3개 overlapped stdio + CREATE_NO_WINDOW, 1MiB `-W` 왕복 | **6.122초**, 1,048,576바이트 일치, stderr 0, exit 0 | 대용량 바이너리·EOF 수신 성공(단일 관측) |

초기 verbose 로그를 일반 File에 보낸 실험도 중간에 멈췄다. 이는 stdout 하나만의 문제라고 단정하지 않는 근거다. 최종 경로는 세 stdio를 모두 overlapped로 만들며, `-E` 디버그 로그를 제외하면 동기 stdio File을 ssh에 넘기지 않는다. 내부 원인의 단정은 하지 않는다.

참고: Win32-OpenSSH의 [redirected stdio hang #1427](https://github.com/PowerShell/Win32-OpenSSH/issues/1427), [stdio mode 변경 #759](https://github.com/PowerShell/openssh-portable/pull/759). `OPENSSH_STDIO_MODE=nonsock`만 지정한 std 파이프도 이 VM에서는 8초 제한을 넘겼다. 이 환경변수에 의존하지 않는다.

## 구현

- `krill-hosts/src/ssh/windows.rs`: 로컬 AF_UNIX 리스너, 인증·생존용 keepalive SSH 1개, 로컬 연결당 `ssh -T -W <원격.sock>` 1개. 로컬 TCP 포트를 열지 않는다. 동시 포워딩은 128개로 제한한다.
- `ssh/windows/stdio.rs`: tokio 안전 API `ServerOptions`로 named pipe 서버를 만들고 `std::fs::OpenOptionsExt::custom_flags(FILE_FLAG_OVERLAPPED)`로 클라이언트 File을 연 뒤 `Stdio`로 넘긴다. unsafe를 추가하지 않았다. tokio는 기존 workspace 의존성을 Windows 멤버 의존성으로만 추가했다.
- 서버는 `first_pipe_instance(true)`, `max_instances(1)`, `reject_remote_clients(true)`다. 자식 끝은 읽기·쓰기 양쪽 권한을 요구한다. 다른 프로세스가 먼저 연결하면 클라이언트 open이 실패하며 전송 전에 닫는다. Windows 관리자·SYSTEM을 적대 경계로 막는 보장은 없다.
- 하나의 tokio 런타임을 공유한다. 실제 블로킹 브리지는 OS 스레드에서 실행해 호출자의 tokio 런타임 안에서 `block_on`하지 않는다. 프로세스 종료·터널 드롭 시 자식을 kill/wait하고 소켓을 지운다. AF_UNIX 읽기는 250ms 타임아웃으로 종료 플래그를 확인한다(OWE-840의 Windows shutdown 제약).
- HOME 조회와 관리형 probe/install도 같은 stdio를 사용한다. HOME은 30초, 관리형 명령은 300초 상한이다. 성공한 자식의 stdin 쓰기 실패도 전파한다. SSH 자식은 `CREATE_NO_WINDOW`다.
- 원격 경로는 Windows `PathBuf::join`으로 조립하지 않는다. POSIX `/`를 보존해 원격 HOME·소켓·설치 경로에 역슬래시가 끼지 않게 했다.

## 모바일 범위

차단점은 `Gateway::start → mobile/net.rs::detect → sys/windows.rs::ipv4_interfaces = Unsupported`였다. krill-link의 AF_UNIX 브리지는 OWE-840에서 이미 구현돼 있어 수정하지 않았다.

- Windows는 허용된 FFI 모듈 `sys/windows.rs`에서 `GetAdaptersAddresses(AF_INET)`를 호출한다. Up 어댑터의 FriendlyName(UTF-16), unicast IPv4, 기본 게이트웨이 유무를 기존 LAN/Tailscale 선택 규칙에 넘긴다. [Microsoft API 계약](https://learn.microsoft.com/en-us/windows/win32/api/iphlpapi/nf-iphlpapi-getadaptersaddresses)에 따라 u64 정렬 15KB 버퍼로 시작하고 크기 변경 시 최대 3회 재시도한다. 모든 unsafe는 이 FFI 모듈에만 있고 SAFETY 근거를 붙였다. PowerShell 구현은 최종 코드에서 제거했다. Mac의 기존 조회 경로는 유지한다.
- cloudflared 준비 확인용 curl의 출력 파일은 Windows `NUL`, Unix `/dev/null`이다. cloudflared·curl의 Windows 자식 창은 숨긴다. 모바일 호스트 메타데이터는 Windows에서 `GetComputerNameExW(ComputerNameDnsHostname)`와 platform `windows`를 사용해 snapshot마다 hostname 자식을 띄우지 않는다. VM의 이전 hostname 자식 경로는 1초 제한에 걸려 display_name이 null이 되는 실패가 관측돼 네이티브 조회로 교체했다.
- 기존 게이트웨이 인증·E2EE·재연결·회수 테스트를 Windows에서도 컴파일/실행하도록 열고 POSIX PTY 전용 테스트만 Unix에 남겼다.

## 모바일 인터페이스 조회 전후 실측

기존 main의 Windows 조회는 `Unsupported`였으므로 시간 비교 기준은 이번 스파이크의 초기 PowerShell 대체 구현이다. 두 방식 모두 같은 VM에서 Ethernet `10.178.0.2`, 기본 게이트웨이 `true`, loopback `127.0.0.1`을 반환했다. `mobile/net.rs`는 이 결과에서 loopback을 제외한다.

| 조회 방식 | 횟수 | 최소 | 중앙값 | 최대 |
|---|---:|---:|---:|---:|
| 교체 전 PowerShell/.NET 자식 기동·JSON 조회 | 10 | 18,697.092ms | 20,452.184ms | 25,182.898ms |
| 교체 후 프로세스 내 `GetAdaptersAddresses` | 10 | 1.299ms | 2.056ms | 5.695ms |

PowerShell 기록은 VM `C:\spike\OWE-915\detect-powershell.json`에 남겼다. 네이티브 관측은 `cargo nextest run -p krilld --lib --locked --run-ignored all -E 'test(measure_ipv4_interfaces)' --no-capture`로 재현한다. 테이블의 네이티브 10개 원본(µs)은 `5695,2429,1889,2612,1498,1423,1299,1994,2554,2117`이다. 빌드가 겹친 공유 VM에서 순차 실행한 한 배치이며, 동일 시점·동일 부하의 엄밀한 벤치마크나 개선 배수 보장은 아니다. 제거한 자식 기동·JSON 직렬화 비용도 이전 조회 시간에 포함된다.

## 재현 환경과 정리

Mac 시스템 설정·launchd·`~/.ssh`는 변경하지 않았다. 작업 디렉터리 안의 sshd config·host key·AuthorizedKeysFile(전용 테스트 키 1개)을 사용했다. `PasswordAuthentication no`, `KbdInteractiveAuthentication no`, 루프백 ListenAddress를 지정했다. 캐시가 외장 SSD로 연결돼 부모 디렉터리 모드 검사가 실패하므로 작업 전용 config에서만 `StrictModes no`를 사용했다.

Mac sshd 포트는 작업 디렉터리의 `port` 파일에 기록했다. Mac→VM SSH 연결의 `-R 127.0.0.1:59150:127.0.0.1:<sshd-port>`로 VM 루프백에서 접근했다. 원격 krilld는 전용 `KRILL_STATE_DIR=~/.cache/krill-win/spikes/OWE-915/state`를 사용했다. 운영 상태와 섞지 않았다.

Windows 실제 세션 검증은 `ssh::windows::tests::real_ssh_home_query_and_remote_session_attach`(명시 실행 ignored 테스트)로 재현한다. `KRILL_SSH_E2E_DEST`, `KRILL_SSH_E2E_PORT`, `KRILL_SSH_E2E_KEY`, `KRILL_SSH_E2E_SOCKET`, `KRILL_SSH_E2E_SESSION`을 전용 대상에 맞춰 지정하고 실행한다. 일반 테스트는 외부 네트워크에 접속하지 않는다. `KRILL_SSH_E2E_PROGRAM`으로 `ssh.exe -o UserKnownHostsFile=C:/spike/OWE-915/known_hosts %*`를 실행하는 전용 `.cmd`를 지정해 사용자 기본 known_hosts를 건드리지 않고 재현할 수 있다.

## 최종 검증

- Windows 네이티브 `cargo nextest run -p krilld --lib --locked -E 'test(mobile::) | test(adapter_name_preserves_korean)' --no-fail-fast`: **29/29 통과**, 최종 정렬 수정 뒤 12.185초(1 flaky). `idle_authenticated_socket_is_pinged_then_closed_without_pong`는 첫 시도 실패 뒤 재시도 통과했다; 직전 실행은 8.829초에 전부 첫 시도 통과했다. 게이트웨이 HTTP/WS·E2EE·인증·기기 회수, 호스트 이름, UTF-16 fixture 포함. POSIX PTY 테스트는 제외한다.
- Windows 네이티브 `cargo nextest run -p krill-hosts --locked`: **11/11 통과**, 외부 SSH e2e 1개는 기본 ignored.
- 전용 환경변수와 `--run-ignored all -E 'test(real_ssh_home_query_and_remote_session_attach)' --no-capture`: **1/1 통과**, 초기 nextest 22.206초·최종 전용 known_hosts wrapper 재실행 24.50초. 실제 `ssh.exe`의 HOME 조회, keepalive 준비, 로컬 AF_UNIX→별도 `ssh -W`→Mac krilld Hello·Attach·`OWE915_READY` 원본 바이트 수신, 터널 드롭 뒤 로컬 소켓 제거를 검증했다.
- e2e 뒤 Mac의 격리 `krill ls`: 세션 **1**, PID **67750**, 여전히 running. SSH 연결을 닫아도 원격 세션을 닫지 않았다.
- macOS `cargo clippy -p krill-hosts -p krilld --all-targets --locked -- -D warnings`: **오류 0, 종료 0**.
- 인계 후 최종 재검사(2026-10-19, `CARGO_BUILD_JOBS=3`, 로그 `~/.cache/krill-win/spikes/OWE-915/{mobile-mac-final,cross-final}.log`):
  - macOS `cargo nextest run -p krilld`: **518/518 통과**, 10 skipped, 146.3초. flaky 2개(`holder::failed_spawn_from_session_leaves_no_creator_residue`, `search::hot_fallback_and_single_match_wrap_without_recording`)는 이번 변경 밖 모듈이며 재시도에서 통과했다(빌드가 겹친 Mac 부하 관측).
  - macOS `cargo clippy -p krill-hosts -p krilld --all-targets -- -D warnings`: **종료 0**.
  - `scripts/check-windows.sh -p krill-hosts -p krilld --clippy --target-dir /Volumes/krill-build/targets/win/OWE-915-windows`: **종료 0**. 첫 실행은 `sys/windows.rs` 측정 테스트의 `#[expect(clippy::print_stderr)]`가 `allow-print-in-tests = true` 때문에 unfulfilled로 실패해, 그 attribute만 제거했다.
  - Windows 대상 clippy는 `clippy.toml`의 `disallowed-types` `std::os::fd::RawFd`가 "does not refer to a reachable type" 경고를 낸다(Windows에는 해당 타입이 없음). 오류가 아니라 종료 0이며, 하네스 파일이라 이번 작업에서 수정하지 않았다(`allow-invalid = true` 추가 여부는 메인테이너 결정).
- 교체 전 .NET/PowerShell 인터페이스 조회 실측: Ethernet `10.178.0.2`, default route `true`; loopback `127.0.0.1`은 기존 필터로 제외한다. 한글 FriendlyName은 UTF-16 fixture로 검증하며 실제 한글 NIC alias는 미확인(공유 VM 이름 변경은 코디네이터가 불허, OWE-916 실기 QA).

**미확인(OWE-916 실기 QA로 넘김)**: 실제 한글 NIC 이름, Windows 10/11 데스크톱, 실제 cloudflared 공개 URL, 폰 브라우저 접속. 앱 kill 뒤 재연결, 원격 krilld kill 생존도 이번 실측 범위에서 미확인이다. 전체 `scripts/check.sh`는 병렬 워커 지시에 따라 코디네이터가 통합 시 실행한다.

정리 완료: 전용 Mac sshd·역방향 터널·echo 서버·격리 krilld/holder와 테스트 셸만 종료했으며 다른 작업 프로세스는 유지했다. VM `C:\spike\OWE-915\client`와 `id_ed25519`는 삭제 뒤 부재를 확인했다. 사용자 known_hosts는 승인된 `ssh-keygen -R '[127.0.0.1]:59150'`만 실행해 해당 항목(4행)을 제거했다. `ssh-keygen`이 만든 known_hosts.old는 기존 파일 백업이다. 최종 재실행은 별도 `C:/spike/OWE-915/known_hosts`를 사용했다. **VM 사용 종료**; 공유 VM 자체는 정지하지 않았다.
