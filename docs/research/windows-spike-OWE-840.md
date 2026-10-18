# OWE-840 — krill-link·krill-hosts Windows화 (원격·부가, 모바일 게이트웨이 제외)

측정일: 2026-10-18. 환경: 공유 GCP `krill-win`, Windows Server 2022 Datacenter, Rust 1.98.1 MSVC, `uds_windows = 1.2.1`, ssh 클라이언트 `OpenSSH_for_Windows_9.5p2, LibreSSL 3.8.2`, VM sshd `OpenSSH_for_Windows_8.9`.

**결론.**
- **krill-link**: Windows에서도 `AF_UNIX`로 실구현했다. tokio에 Windows `AF_UNIX`가 없으므로 `uds_windows` 블로킹 소켓을 스레드로 tokio `DuplexStream`에 잇는다. VM 테스트 41/41 통과(터널·서버·resume e2e 포함).
- **krill-hosts SSH 포워딩**: Windows에서는 `io::ErrorKind::Unsupported`로 둔다. 시도한 세 경로가 모두 막혔다.
  - `-L <로컬.sock>`: ssh.exe가 지원하지 않는다.
  - 루프백 TCP `-L`: 같은 PC의 다른 사용자를 막을 수 없다.
  - 연결마다 `ssh -W`: 파이프 stdout을 쓰면 ssh.exe가 핸드셰이크에서 멈춘다.

## 실측 표

| 항목 | 결과 | 관측 |
|---|---|---|
| ssh.exe `-L <로컬.sock>:<원격.sock>` | **불가** | 표기 5종(대괄호·백슬래시·슬래시·상대경로·POSIX 원격) 모두 `Bad local forwarding specification` 또는 `Unix domain server sockets are not supported` / `unix_listener: cannot bind to path ...: Operation not supported` |
| ssh.exe `-L 127.0.0.1:P:/원격.sock` | 클라이언트 수락 | `Local forwarding listening on 127.0.0.1 port P`, 연결 시 `direct-streamlocal@openssh.com` 채널 요청. VM sshd(Windows)는 streamlocal 채널을 `open failed`로 거부 |
| ssh.exe `-W /원격.sock` | 클라이언트 수락 | `channel_connect_stdio_fwd: /tmp/x.sock:-2`, `new stdio-forward`. Windows sshd는 거부(위와 같음) |
| `-T <dest> "cat >/dev/null"` argv 전달 | 한 인자로 전달됨 | 원격 Windows sshd(PowerShell)가 `cat >/dev/null`을 한 명령으로 받아 실행(`C:\dev\null` 경로 오류). POSIX 원격에서 stdin EOF로 끝나는지는 **미확인** |
| `ssh -T <dest> exit` 왕복(PowerShell, VM 자기 sshd) | 중앙값 **393ms** | 10회 정렬 383–421ms. 재측정 중앙값 430ms(406–460) |
| Rust `Command`로 띄운 ssh.exe, stdout 상속 | 정상 | `-T exit` 377–398ms, `-W`(stdin null) 402–430ms |
| Rust `Command`로 띄운 ssh.exe, **stdout = `Stdio::piped()`** | **8초 넘게 멈춤** | `-T host echo …`(stdin null·파이프 둘 다)와 `-W`(stdin·stdout 파이프) 모두 `debug1: SSH2_MSG_KEXINIT sent`/`received` 이후 진행 없음. 원인(동기 익명 파이프 처리)은 추정 |
| `type file \| ssh -W …`(cmd 파이프 stdin) | 15초 넘게 멈춤 | 출력 없음 |
| `ssh -W …` + 파일 stdin·파일 stdout | 정상 종료 | 에코 수신 후 종료 |
| Windows `AF_UNIX` 블로킹 recv + 다른 스레드의 `shutdown(Both)` | **깨어나지 않음** | 3초 뒤에도 대기. 상대가 소켓을 닫으면 15µs 만에 `Ok(0)` |
| `SO_RCVTIMEO` 50ms 10회 만료 뒤 사용 | 정상 | 10회 모두 `TimedOut`. 이후 890바이트 무손실 수신, 반대 방향 쓰기 정상 |
| 없는 소켓 경로 connect(부모 디렉터리 없음) | `NetworkDown`(10050) | Unix의 `NotFound`/`ConnectionRefused` 자리 |
| `cargo nextest run -p krill-link -p krill-hosts --locked` (VM) | 52/52 통과 | 아래 「검증」 |

시간은 VM 루프백 단일 관측이며 WAN 지연·분포가 아니다.

## krill-link 설계

- `src/local.rs`
  - Unix: tokio `UnixListener`·`UnixStream`을 그대로 쓴다.
  - Windows `Stream`: `uds_windows::UnixStream`에 읽기·쓰기 스레드 2개를 붙여 tokio `DuplexStream`(64KiB)에 잇는다.
    - 읽기 스레드는 `SO_RCVTIMEO` 250ms마다 닫힘 플래그를 확인한다(위 실측: `shutdown`으로는 깨울 수 없다).
    - 드롭할 때 쓰기 스레드는 duplex에 남은 바이트를 마저 보낸 뒤 `shutdown(Write)`한다. 보낸 데이터를 버리지 않는다는 점에서 Unix close와 같다.
  - Windows `Listener`: 논블로킹 accept를 50ms마다 폴링하는 스레드 1개.
- 스레드 비용: 연결당 2개(스트림 상한 `MAX_STREAMS` = 32), 리스너당 1개.
- `Logical`의 읽기는 `readable()` + `try_read` 조합에서, 취소 안전한 `AsyncReadExt::read` + `tokio::io::split`으로 바꿨다. Unix도 같은 코드를 쓴다.
- 경로와 상한
  - 상태 디렉터리는 `krill_proto::daemon::state_dir()`이다. Windows에서는 `%LOCALAPPDATA%\Krill`.
  - 소켓 경로는 `SUN_PATH_MAX`(macOS 104, Windows 108)로 검사한다(CONVENTIONS A5·A6).
- 터널 생존 판정(`host_status`)
  - Windows는 PID 조회가 unsafe FFI라 쓰지 않는다.
  - 대신 상태 파일 신선도로 판단한다. 살아 있는 터널은 400ms마다 상태 파일을 다시 쓰고, 2초 넘게 갱신이 없으면 끊긴 것으로 본다.
- 접근 통제
  - Windows `mode`는 no-op이다.
  - 다른 표준 사용자는 `%LOCALAPPDATA%` 상속 ACL이 막는다(OWE-827 실측 10013).
  - 피어 PID·SID 확인은 `forbid(unsafe_code)` 크레이트라 넣지 않았다. 명시 DACL·피어 SID 검사는 krilld FFI와 함께 후속으로 둔다. 관리자·SYSTEM을 적대 경계로 막는 보장은 없다.
- launchd `LaunchAgent`는 macOS 전용으로 유지한다. Windows 자동 시작은 범위 밖이다(`Unsupported`).
- iroh 쪽은 수정하지 않았다.

## krill-hosts: 검토한 Windows 대안

| 안 | 판정 | 이유 |
|---|---|---|
| `-L <로컬.sock>:<원격.sock>` | 불가 | ssh.exe 미지원(실측) |
| 루프백 TCP `-L 127.0.0.1:P:<원격.sock>` + 토큰 | 기각 | ssh.exe가 포트를 직접 리슨하므로 토큰을 검사할 지점이 없다. 터널이 살아 있는 동안 같은 PC의 모든 사용자·프로세스가 원격 krilld(원격 셸 권한)에 붙을 수 있다 |
| (A) 로컬 `AF_UNIX`는 krill이 열고, 연결마다 `ssh -T -W <원격.sock>` 자식의 stdio에 잇기. keepalive ssh 1개가 인증·생존 담당 | 구현 후 철회 | 가짜 ssh(.cmd)로는 단위 테스트를 통과했다. 그러나 실제 ssh.exe는 `Stdio::piped()` stdout이면 핸드셰이크에서 멈춘다(실측). keepalive의 준비 표식 읽기도 같은 이유로 막힌다 |

A안 비용(재활성화 판단용)
- 앱은 원격 호스트당 장기 연결 P+1개를 연다(보이는 원격 pane P개 + 사이드바 폴러 1개).
- 짧은 연결도 생긴다. pane 붙이기마다 2~3개(`ensure/resume_session` + `AgentLaunch`, 콜드면 `load_cold`), 사이드바·handoff 동작마다 1개.
  - 근거: `crates/krill-app/src/pane.rs:147,227,306,395,99`, `sidebar/ipc.rs:379`, `sidebar.rs:~998`(500ms 폴링).
- CLI는 호출당 1~2개다(`crates/krill/src/session.rs:114,156`).
- 연결마다 핸드셰이크가 VM 루프백 기준 약 400ms 붙는다(WAN은 미측정).

A안 코드는 저장소 밖 `~/.cache/krill-win/spikes/OWE-840/design-A-windows.rs`·`design-A.patch`에 보존했다. 들어 있는 것:
- 준비 표식 `echo krill-tunnel-ready; cat >/dev/null`
- 수락마다 `ssh -W` 실행
- Drop의 명시 kill
- 클라이언트 EOF 뒤 2초 유예 후 회수(파이프 stdin EOF가 원격에 전달되지 않는 경우 대비)

**현재 동작.**
- Windows `Tunnel::start_at_with_program`과 원격 `$HOME` 조회(`query_remote_home_with_program`)는 ssh를 띄우기 전에 `Unsupported`("ssh remote hosts are not supported on Windows yet")로 끝난다.
- 원격 `$HOME` 조회는 `.output()`(stdout 파이프)를 쓰므로 같은 멈춤이 의심된다(**미확인**). 실패를 기다리지 않고 바로 끊는다.
- 원격 소켓 부재 `NetworkDown`은 `ConnectionRefused`로 정규화해 재시도 대상으로 둔다.
- Unix 경로(`-L` 유닉스 소켓)는 바꾸지 않았다.

## 후속 제안

1. **overlapped 파이프 stdio 헬퍼**: 허용된 FFI 모듈에서 `CreateNamedPipeW(FILE_FLAG_OVERLAPPED)`로 자식 stdio를 만들어 넘긴다. Win32-OpenSSH가 ProxyCommand 자식에게 주는 형태다. 그 뒤 A안을 재활성화한다.
   - 먼저 그 핸들로 ssh.exe가 멈추지 않는지 실측해야 한다.
   - 핸들 상속은 OWE-829 권고대로 `bInheritHandles=false` + HANDLE_LIST로 최소화한다.
2. POSIX sshd 대상 e2e(원격 `cat` 종료, `-W` streamlocal)는 **미확인**이다. Mac Remote Login·authorized_keys는 건드리지 않았다.
3. 모바일 게이트웨이(krilld) Windows화는 OWE-833·832 작업 이후 별도로 한다.
4. krill-link Windows 로컬 소켓의 명시 DACL·피어 SID 검사(krilld FFI와 공용).

## 재현

스파이크 코드(저장소 밖): `~/.cache/krill-win/spikes/OWE-840/`
- `src/main.rs`(udsprobe: `serve`·`client`·`tcp`·`unblock`·`timeouts`)
- `sshfwd.ps1`(`-L` 표기 매트릭스. VM 전용 키를 `administrators_authorized_keys`에 추가)
- `e2e/`(`spawnexit`: Rust spawn stdio 매트릭스)

VM 사본은 `C:\spike\OWE-840\`에 있다.

```powershell
Set-Location C:\spike\OWE-840\probe; cargo build; .\target\debug\udsprobe.exe unblock C:\spike\OWE-840\u.sock; .\target\debug\udsprobe.exe timeouts C:\spike\OWE-840\t.sock
& C:\spike\OWE-840\sshfwd.ps1        # -L 표기별 결과
& C:\spike\OWE-840\measure.ps1       # ssh -T exit 10회 중앙값
Set-Location C:\spike\OWE-840\e2e; cargo build; Start-Process -NoNewWindow -Wait .\..\target-e2e\debug\e2e.exe spawnexit   # stdout 파이프 멈춤
```

## 검증

- Mac
  - `cargo clippy -p krill-link -p krill-hosts --all-targets --locked -- -D warnings`: 0
  - `cargo nextest run -p krill-link -p krill-hosts --locked`: 63/63(1 skipped)
  - 앞선 krill-link 단독 실행 한 번에서 `status_json_and_lib_include_suspended_stream_metrics`가 flaky였다(재시도 통과)
- `scripts/check-windows.sh -p krill-link -p krill-hosts --clippy`: 0
- VM `cargo nextest run -p krill-link -p krill-hosts --locked`(최종 커밋 `0f04ed8`): **52/52 통과**. krill-link 41개는 `local` 브리지와 resume e2e 포함. A안 철회 전에는 53/53이었다.
- 전체 `scripts/check.sh`는 코디네이터 통합 검증에 맡긴다.
