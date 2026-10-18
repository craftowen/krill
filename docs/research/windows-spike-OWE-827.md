# OWE-827 — S-W2: Windows AF_UNIX IPC·피어 인증

측정일: 2026-10-18. 환경: 공유 GCP `krill-win`, Windows Server 2022 Datacenter 10.0.20348, x86_64 MSVC, Rust 1.98.1 (`48a229cea`), `uds_windows = 1.2.1`, `windows-sys = 0.61.2`.

**결론: AF_UNIX를 유지할 수 있다. holder가 listener를 보관하고 새 krilld PID마다 `WSADuplicateSocketW` 정보를 새로 발급하는 방식은 실동작했다. 다만 `pair()`는 긴 TEMP에서 panic하므로 그대로 제품 경로에 넣으면 안 된다.** 이 보고서는 조사 기록이며 ARCHITECTURE §4.1의 Windows 생존 보장을 확정하지 않는다.

## 실측 표

| 항목 | 결과 | 관측·범위 |
|---|---|---|
| bind → connect → accept → read/write | 성공 | `%LOCALAPPDATA%\KrillSpikeOWE827\ipc.sock`, `ping` 4바이트 일치 |
| `UnixStream::pair()` | 기본 TEMP 성공 | 양방향 중 한 방향 `pair` 4바이트 일치; 실제 구현은 임시 디렉터리+listener+스레드이며 커널 socketpair가 아님 |
| listener nonblocking | 성공 | `set_nonblocking(true)` 후 빈 accept → 10035 `WouldBlock` |
| stream nonblocking | 성공 | 데이터 없는 read → 10035 `WouldBlock` |
| read timeout | 성공 | 설정/조회 120ms, read → 10060 `TimedOut`, 단일 관측 128ms |
| write timeout | 성공 | 설정/조회 120ms, 상대가 읽지 않는 pair에 131,072바이트 쓴 뒤 10060 `TimedOut`, 단일 관측 139ms |
| ASCII 경로 | 경계 확인 | 103·104·106·107바이트 bind 성공; 108·109바이트 `InvalidInput: path must be shorter than SUN_LEN` |
| 한글 포함 경로 | UTF-8 바이트 기준 확인 | `한`을 포함한 107바이트(105문자) 성공, 108바이트(106문자) 실패 |
| `SIO_AF_UNIX_GETPEERPID` | 성공 | `WSAIoctl`, 출력 DWORD 4바이트; PID 4928(자기 연결) 및 별도 프로세스 PID 4212 확인 |
| `OpenProcess` → `OpenProcessToken` → `GetTokenInformation(TokenUser)` → `EqualSid` | 성공 | 자기 연결 및 같은 사용자 별도 PID → 1; 별도 표준 사용자 LogonUser 토큰과 현재 process token 비교 → 0 |
| 기본 LocalAppData 상속 ACL | 다른 표준 사용자 토큰 차단 | SYSTEM·Administrators·현재 사용자 FullControl; 다른 사용자 `ImpersonateLoggedOnUser` 상태 connect → 10013 `PermissionDenied` |
| 같은 디렉터리에 시험 사용자 Modify 추가 | 연결 허용 | 토큰 SID는 다르며 connect 성공; ACL이 실제 접근에 영향을 준다는 대조군 |
| 상속 제거 + 현재 사용자/SYSTEM만 FullControl | 연결 차단 | 시험 사용자 ACE 제거 후 connect → 10013; socket 파일에도 제한 ACE 상속 |
| listener `WSADuplicateSocketW` → 다른 PID `WSASocketW` | 성공 | 반환 0, 원본 listener drop 후에도 새 프로세스 accept 및 `dupe` echo 일치 |
| holder 모형 listener 보관·재전달 | 성공 | 세대1 PID 4072에서 echo 후 `Child::kill()`(TerminateProcess), 같은 listener를 세대2 PID 5800으로 다시 전달 후 echo 일치; 두 자식 강제종료 exit code 1 |
| 긴 TEMP에서 `pair()` | **panic 재현** | `C:\spike\OWE-827\` + ASCII `t` 100개를 TMP/TEMP로 지정; `catch_unwind` 결과 Err, crate `src/stdnet/net.rs:187` bind unwrap에서 panic |
| 다른 사용자 primary token을 가진 별도 프로세스의 peer SID 거부 e2e | **미확인** | `Start-Process -Credential` 자식이 실행 본문 전에 0xC0000142로 종료; 예약 작업 시도도 실행 증거를 얻지 못함. 이를 ACL 거부로 세지 않음 |
| Windows 10/11 클라이언트·비관리자 서버·악의적 race·장시간 반복 | 미확인 | 이번 환경은 관리자 계정의 Server 2022 SSH 세션 |

시간은 한 번의 관측값이며 성능 보장·분포가 아니다. 소켓 경로 108은 `sun_path` 버퍼 크기이고, 이 라이브러리에서 사용할 수 있는 pathname은 NUL을 제외한 최대 **107 UTF-8 바이트**다.

## 인증과 ACL 해석

PID는 연결 상대 프로세스의 **primary token**을 찾는 단서다. Modify 대조군에서 다른 사용자로 impersonation한 클라이언트는 연결에 성공했지만, 서버가 조회한 process token SID는 서버와 같았다(`sid_same=1`). 따라서 PID→process SID 검증을 호출 스레드의 impersonation 사용자 검증이라고 설명하면 안 된다. 이번 실험은 별도 사용자 토큰에 대한 파일 ACL 검증과 process PID/SID 조회를 각각 검증했으며, 다른 사용자 primary token 프로세스의 거부 통합 검증은 남아 있다.

Phase 3에서는 accept 직후 PID를 얻고 `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`의 HANDLE을 유지한 채 TOKEN_QUERY·TokenUser를 읽어 현재 사용자 SID와 비교한다. 조회 실패·프로세스 종료·SID 불일치 시 연결을 닫고, 확인 전에는 Hello 이외의 세션 접근을 허용하지 않는다. PID 숫자를 장기간 캐시하지 않는다. SID 일치는 같은 계정임을 뜻하며 바이너리 서명·신뢰성 인증은 별도 문제다.

상태 디렉터리는 생성할 때부터 보호된 DACL(현재 사용자 SID + SYSTEM)을 명시하고 reparse point·소유자 검증을 함께 한다. 이번 기본 ACL은 다른 **표준 사용자**를 막았지만 Administrators ACE가 있었으며, 관리자/SYSTEM을 적대 경계로 검증한 것이 아니다. 상속 기본값을 모든 설치 환경의 보장으로 승격하지 않는다. 소켓 파일은 NTFS reparse point이므로 Unix mode no-op만으로는 사적 권한을 보장할 수 없다.

## OWE-833 / Phase 3 설계 권고

1. IPC wire framing은 유지하고 `uds_windows`를 Windows 전송층으로 사용한다. 107바이트 사전 검사, 한글 포함 UTF-8 길이 검사, 긴 사용자 프로필/상태 디렉터리의 오류 처리를 넣는다. macOS 104바이트 규범은 별도로 유지한다. 짧은 경로 대체 위치의 보안은 별도 설계·검증이 필요하다.
2. holder가 listener를 계속 소유하고, krilld 세대마다 대상 PID 전용 `WSAPROTOCOL_INFOW`를 발급한다. 제어 채널에서 이를 전달하고 수신 성공 ACK 후에만 소유권 전환을 확정한다. 이 구조체는 대상 PID 전용·일회용이며 디스크 캐시나 다음 세대 재사용 대상이 아니다. 이번 spike의 파일 전달은 재현용이며 제품에서 사용하지 않는다.
3. holder는 accept·read를 하지 않고 krilld만 listener를 사용한다. 공유 socket의 nonblocking·socket options·이벤트 등록은 공유되므로 양쪽이 독립 소켓이라고 가정하면 안 된다. 공식 문서는 원본 `socket()`이면 수신 WSASocket에 `WSA_FLAG_OVERLAPPED`를 요구하므로 제품은 생성 플래그를 일치시킨다(스파이크의 flags=0은 동기 I/O 성공 범위만 증명).
4. `pair()`를 직접 신뢰하지 않는다. 짧은 보호 디렉터리에서 fallible bind/connect/accept를 수행하는 어댑터를 두거나, 최소한 세션 작업 단위 panic 격리를 유지한다. 전역 TEMP 변경은 다른 스레드에 영향을 주므로 제품 해결책으로 쓰지 않는다. pair 임시 경로 오류와 상대측 실패를 회귀 테스트에 넣는다.
5. Windows에는 SCM_RIGHTS가 없으므로 socket 전달은 Winsock 중복화, ConPTY pipe 전달은 별도 DuplicateHandle 계약으로 분리한다. 이 결과는 ConPTY/HPCON 생존을 증명하지 않는다(OWE-826 범위).
6. 출하 전 다른 표준 사용자 **프로세스**·현재 사용자 일반 권한 서버·PID 종료 race의 fail-closed 통합 테스트를 추가한다. 가능하면 holder 부트스트랩 채널도 사용자 DACL로 제한한다.

## named pipe 비교 (문서 조사, 실기 성능 미측정)

| 선택 | 이점 | 추가 비용·주의 |
|---|---|---|
| AF_UNIX + uds_windows | 기존 Read/Write·listener·pair API와 wire 유지; 이번 listener 재전달 성공 | pathname 107바이트, PID ioctl/토큰 FFI, 안전한 pair 어댑터 필요 |
| named pipe | CreateNamedPipe에 명시 보안 descriptor, GetNamedPipeClientProcessId·impersonation API | listener가 여러 pipe instance를 관리하는 별도 수명 모델; Tokio/IOCP·재기동 복구 어댑터 필요(설계 추정). 기본 DACL은 Everyone/anonymous read가 있어 명시 제한 필요 |

AF_UNIX 필수 항목이 동작하므로 이번 결과만으로 named pipe로 갈아탈 이유는 없다. 긴 경로 대안이나 impersonation identity 요구가 생기면 별도 spike로 검증한다.

## 재현·검증·잔여

원본 코드·Cargo.lock·원시 로그는 저장소 밖 `~/.cache/krill-win/spikes/OWE-827/`, VM 복사본은 `C:\spike\OWE-827\`에 있다. 저장소에는 이 보고서만 추가한다. 기본 명령은 Mac의 `~/.cache/krill-win/gcp/ssh.sh '<PowerShell>'`로 실행한다.

```powershell
Set-Location C:\spike\OWE-827
cargo build --locked
.\target\debug\ipc-spike.exe
.\target\debug\ipc-spike.exe unicode-path
.\target\debug\ipc-spike.exe retransfer
.\acl.ps1
# 별도 SSH 셸에서만 실행(다른 워커의 환경을 바꾸지 않음)
$tmp='C:\spike\OWE-827\'+('t'*100)
New-Item -ItemType Directory -Force $tmp
$env:TMP=$tmp; $env:TEMP=$tmp
.\target\debug\ipc-spike.exe pair-only
```

`cargo build --locked` 오류·경고 0. 기본 실행·재전달·ACL 스크립트 exit 0; 긴 TEMP는 의도적인 panic 음성 검증이며 catch_unwind가 이를 포착했다(PowerShell은 stderr 때문에 exit 1). `run.log`, `unicode.log`, `retransfer.log`, `acl-impersonate.log`, `long-temp.log`에 원시 관측이 있다. 임시 사용자 `krill_spike_827`·시험 예약 작업·임시 자격증명 파일은 제거했다. 공유 VM은 중지하지 않는다. 문서만 변경하므로 크레이트 검사는 추가하지 않았으며 전체 `scripts/check.sh`는 지시대로 코디네이터 통합 검증에 맡긴다.

참조: [Microsoft AF_UNIX 보안·제약](https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/), [WSADuplicateSocketW의 대상 PID·일회 사용·공유 상태·플래그](https://learn.microsoft.com/en-us/windows/win32/api/winsock2/nf-winsock2-wsaduplicatesocketw), [named pipe DACL](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights), 로컬 `uds_windows-1.2.1/src/stdnet/net.rs:180–201`. 근거 boost 보고서의 현재 버전에는 요청에 적힌 §4.4·§6이 없으므로 §4의 Phase 2/3와 §5를 대조했다.
