# boost 2026-10-18 — Linux 원격 호스트 연결 완벽 테스트 (최종 보고서 B)

요청: "니가 알아서 완벽 테스트 해줘" (Linux 클라우드 VM을 Krill 원격 호스트로 연결하는 기능, 브랜치 `linux-host`)

**결론(조사 시점): "문제없이 연결된다"고 보고할 수 없다. 효과 큰 수정 3가지**
1. 원격 데몬 재탐색·깨우기가 ssh 터널을 새로 만들 때만 일어난다. 터널이 살아 있는 동안 원격 holder·krilld가 죽거나 호스트 추가 뒤 krill을 설치하면 앱 재시작 전까지 계속 실패하고 "다시 연결"도 무력하다(B2·B3·B4). `manager.rs` `record_ipc_outcome`·`reconnect_now`, `ssh.rs` `remote_query`로 함께 해결.
2. 앱이 죽거나 종료될 때마다 ssh 터널이 고아로 남는다(B1). Mac에 ppid 1 ssh 10개, 원격에도 sshd 세션 누적. `Tunnel` 생성 한 곳에서 해결.
3. 새 VM은 연결 테스트를 먼저 누르지 않으면 호스트키 오류로 실패(B11), 미설치 VM은 "unexpected end of file"만 보인다(B5). 둘 다 `krill-hosts/src/ssh.rs`.

## 1. 1차 조사 정정 (주장 / 실제 / 근거)
| 항목 | 주장 | 실제 | 근거 |
|---|---|---|---|
| B1 수치 | 고아 ssh 8개, 소켓 19개 | 고아 ssh 10개(인스턴스 5개). 소켓 19개 중 2개는 현재 앱(57077) 사용분, 찌꺼기 17개 | `ps -axo pid,ppid,etime,command`, `ls run/` |
| B1 범위 | kill -9일 때만 | 정상 종료도 새는 것으로 보임(추정). `on_app_quit`은 iroh link만 정리. oci-arm `loginctl` active 9, kakaouto-brain 7 | `crates/krill-app/src/main.rs:186-193` |
| B5 줄 | daemon.rs:764 | 765 | `crates/krill-proto/src/daemon.rs:765` |
| B7 | folder.rs:27-31 | 원격 분기 `launch_for` 26-27행. 깨우기가 ssh 명령 cwd(=$HOME)를 물려받아 실사용 영향 작음 → P3 | `/proc/468532/cwd` |
| B12 | holder 누수 제품 버그 | 테스트 경합(`kill_process_group(pgid).unwrap()` ESRCH) | `crates/krill-holder/tests/detach.rs:30-31`, `src/main.rs:28-30` |
| B13 맥락 | 같은 semver면 upgrade 안 함 | 같은 버전이면 `+dev.<소스 해시>` 비교. 해시는 proto·holder·krilld·term·log·vendor만 포함(`crates/krill` 제외) | `control.rs:276-337`, `build.rs:43-70` |
| B15 | oci-arm 비스트립 빌드 | 디스크는 릴리스 tar.gz로 교체됐으나 돌던 holder·krilld는 옛 바이너리(deleted) | `readlink /proc/<pid>/exe` |
| B16 | close_range 미달이면 detach FAILURE | 산출물 glibc 요구 최대 GLIBC_2.34(실측). 미만이면 로더가 실행 거부, `remote_query`의 `>/dev/null 2>&1`에 묻힘 | `objdump -T` |
| B17 | 키 디렉터리 fsync 없음 | 사실이나 암호화 기본 꺼짐 | `store.rs:167-169` |
| B2 현장 | 설치본 0.1.5가 macOS 경로로 터널 유지 | 맞음. 0.1.5에는 Linux 코드가 아예 없음 | `git merge-base`, `strings` |
| 누락 | — | linux-host는 v0.1.4(e52811a9)에서 갈라짐. main은 v0.1.5+10커밋, 병합 충돌 없음 | `git log HEAD..main` |

맞음으로 확인: B2(manager.rs 381-389·488-497·307-321), B3, B4(§4.1 "holder만 크래시" 약속, `krill ls`의 Orphan 재결합), B6, B8(terminal.rs:184), B9(ssh.rs:346), B10(ssh_hosts.rs:531), B11(ssh.rs:154, accept-new는 cfg(test)·앱 연결 테스트에만), B14(install-linux.sh 404), B18.

## 2. 이미 적용·검증된 것
- GCP 22.04 x86_64·aarch64 빌드·smoke 통과(glibc 2.35). 빌드 VM·임시 방화벽 잔존 없음.
- oci-arm: holder ppid 1·자기 sid, Linger=no, KillUserProcesses=no, 상태 디렉터리·run 0700, krilld.sock 0600.
- 코드서명 정책 기본 꺼짐(`scope.rs:224`). Linux nextest 995.

## 3. 결함과 수정안
| # | 항목 | 근거 | 영향 | 최소 수정 위치 | 난이도 | 위험 |
|---|---|---|---|---|---|---|
| 1 | B2·B3·B4 재탐색·깨우기·재결합 부재 | `manager.rs:307-321, 381-389, 488-497`, `ssh.rs:232-245` | 원격 데몬 소실 후 영구 실패 | `record_ipc_outcome`에서 EOF·Reset·Refused면 터널 내림, `reconnect_now`는 무조건 내림, `remote_query`에서 항상 깨우기 | 하 | 핸드셰이크 증가(백오프 ≤5s) |
| 2 | B1 고아 ssh·소켓 | `ssh.rs:495-501, 550`, `krill-app/src/main.rs:186-193` | 누수 | stdin piped + `-N` 대신 `-T` + 원격 `cat >/dev/null` | 중 | ForceCommand 계정 |
| 3 | B11 호스트키 | `ssh.rs:110-129` | 첫 연결 실패 | `common_args`에 `StrictHostKeyChecking=accept-new` | 하 | TOFU |
| 4 | B5 원인 불명 오류 | `ssh.rs:232-245, 501`, `daemon.rs:765` | 첫 경험 | `remote_query` 표식 + `remote_socket_for_home` 오류 변환 | 하 | 낮음 |
| 5 | 버전·릴리스 | `git log HEAD..main`, `install-linux.sh:6`, `lib.rs:290-300` | 설치 한 줄 404 | main 병합 후 publish(사람) | 하 | — |
| 6 | N3 holder가 ssh 세션 scope 안 | `/proc/<holder>/cgroup` = session-N.scope | KillUserProcesses=yes면 로그아웃 때 세션 손실(추정) | 문서·경고, 근본은 `systemd-run --user --scope`+linger | 중 | — |
| 7 | B6 macOS 원격 깨우기 | `ssh.rs:240-243` | 키체인 잠김 시 실패 가능 | 깨우기를 Linux로 한정 | 하 | — |
| 8 | B12 테스트 경합 | `tests/detach.rs:30-31` | 테스트 머신 누수 | pgid 확인 후 killpg | 하 | 없음 |
| 9 | 기타 P3 | B8·B9·B10·B16·B17·B18, `krill-log/src/lib.rs:54`(Linux에 `~/Library/Logs/Krill`) | 국소 | — | 하 | 낮음 |

## 4. 테스트 시나리오(요약)
테스트 사용자 `krilltest`(oci-arm)와 `KRILL_STATE_DIR=/tmp/krill-e2e`로 격리. P0: 기준선(고아 수), 정적 검사, 설치·콜드 깨우기, CLI 입출력·attach, 업그레이드, dev 앱 E2E(GUI, 사용자 동의), B3 재현, B1 재현. P1: krilld만 kill, holder만 kill, 터널 kill, 호스트키(별도 known_hosts), 미설치(kakaouto-brain), 네트워크 단절(사용자 동의). P2: KillUserProcesses=yes(임시 VM), glibc 하한(Debian 11), localhost 실 ssh, 인젝션, 버전 차이. P3: VmRSS, 대량 붙여넣기.

## 5. 미확인(조사 시점)
Cmd-Q 정상 종료 시 터널 누수 여부, 고아 인스턴스의 종료 방식, (deleted) 데몬 BUILD 해시, B6(mbp 오프라인), KillUserProcesses=yes 동작.

---

## 적용·검증 결과 (2026-10-18, 메인)

### 고친 것 (커밋)
| 결함 | 커밋 | 내용 |
|---|---|---|
| B13 업그레이드 경로 없음 | a8b005a1 | Linux `krill update` = 실행 중 데몬 exec 핸드오프, install-linux.sh가 설치 직후 호출 |
| B1·B2·B3·B4·B5·B6·B11 | 0282c391 | 터널 재구성(EOF·reset·다시 연결), 터널 ssh `-T`+원격 `cat`·stdin 파이프, Linux만 매 터널 `krill ls`, 미설치·실행 실패 표식, `StrictHostKeyChecking=accept-new` |
| Linux 로그 경로 | 2ce74eb5 | `~/.local/state/krill/logs` |
| B12 테스트 경합 | 740fc499 | 그룹 리더 확인 후 killpg |
| build-gcp VM 미삭제 | f7a66280 | 트랩 대신 명시 삭제(비동기 함수 서브셸 EXIT 트랩 미실행 실측) |
| 앱 테스트 기대값 | 942222f0 | 다중 호스트 테스트의 `-N`→`-T` |
| N3·glibc 하한 | 881000db, 8c998f59 | 설치 전 실행 확인(glibc 낮으면 멈춤), KillUserProcesses 경고(KillExcludeUsers면 생략), README 요구사항 |

### 정적 검사
- Mac `scripts/check.sh`(942222f0, 다른 에이전트 WIP 제외 worktree): 1,819개 통과, 종료 0.
- oci-arm(Ubuntu 24.04 aarch64, 0282c391~881000db): fmt·clippy `-D warnings` 0, nextest 995개 통과(재시도 통과 2개: `lazy_history_hot_and_cold_pages`, `creator_session_can_wait…` — 이번 변경과 무관한 기존 flaky).
- 새 회귀 테스트: EOF·다시 연결 시 터널 재구성(수정을 되돌리면 실패 확인), 소유 프로세스 SIGKILL 뒤 터널 종료, 원격 질의 표식·깨우기.

### 실환경 시나리오 (Mac → oci-arm 테스트 사용자 krilltest, 실제 ssh)
| 시나리오 | 결과 |
|---|---|
| B5 미설치 | `krill is not installed on the remote host (expected ~/.local/bin/krill; see install-linux.sh)` |
| S1·S2 릴리스 tar.gz 설치·콜드 깨우기 | 첫 명령으로 데몬 기동, holder ppid 1·자기 sid, 두 번째 호출 재기동 없음, 디렉터리 700·소켓 600, 로그 XDG 경로, `~/Library` 미생성 |
| S3 세션 입출력·attach | new/send/wait/read 통과. attach 리사이즈(40×100 반영)·입력·분리·재부착 화면 복원 통과(expect) |
| 터널 kill (HostManager) | 1.6초 뒤 새 터널로 재연결 |
| B3 원격 holder+krilld 동시 kill | 1.4초 뒤 데몬 재기동·재연결(터널 재구성) |
| B4 holder만 kill + 다시 연결 | 1.2초 뒤 새 holder 재결합, 최종 holder·krilld 1쌍 |
| B1 소유 프로세스 kill -9 | 실제 ssh 터널 0.5초 안에 종료, 원격 cat 0 |
| S6 krilld만 kill -9 | 같은 세션 PID 유지, 스크롤백 유지, 입력 응답 |
| B11 새 호스트 키 / 변조 키 | 빈 known_hosts에서 기록 후 연결 / "Host key verification failed"로 거부 |
| 인젝션 | `a;touch${IFS}/tmp/…`, `-oProxyCommand=…` 모두 거부, 파일 생성 없음 |
| 네트워크 단절(VM iptables로 22번 70초 차단, 자동 해제 예약) | Disconnected 전환, 해제 후 약 10초 안에 새 터널로 Connected(t+92s), 규칙 해제 확인 |
| 업그레이드(ubuntu 사용자) | 0.1.4 릴리스 데몬+세션 → 0.1.5-linuxtest 설치 → `krill update`: holder 같은 PID 새 이미지, krilld 새 PID, 세션 PID·입출력 유지 |

### GCP 임시 VM(us-central1-a, 끝나고 삭제·방화벽 삭제·known_hosts 정리)
- Ubuntu 22.04 x86_64·arm64 빌드+smoke 통과(glibc 2.35).
- KillUserProcesses=yes(Ubuntu 22.04): 깨운 ssh가 끝나는 즉시 데몬이 죽어 연결 불가(`unexpected end of file`) → `KillExcludeUsers=macmini`로 연결·세션·로그아웃 뒤 생존 확인.
- Rocky 8(glibc 2.28): Mac에 `failed to start krill on the remote host: … GLIBC_2.29 not found` 표시. install-linux.sh는 이제 설치 전에 멈춤.

### 남은 한계·미확인
- **Mac 앱 GUI에서 Linux 호스트에 붙는 화면 확인은 미확인**(개발 빌드 앱 실행·합성 입력은 사용자 확인 필요). 앱과 같은 HostManager 코드로 실제 ssh 시나리오는 모두 통과.
- 설치본 앱(0.1.5)은 Linux 코드가 없어 Linux 호스트를 쓸 수 없다. linux-host는 v0.1.4에서 갈라져 main(v0.1.5+) 병합 후 릴리스가 필요(사람).
- KillUserProcesses=yes 시스템에서 Mac 쪽 오류는 여전히 EOF 계열(설치 스크립트 경고로 안내).
- Linux 세션 생성 실패 시 앱 오류 문구의 errno 해석 차이, B8(대량 붙여넣기), B9(CLI valid_host 메타문자), B10(Tailscale 발견에 Linux 미표시), B17·B18은 미수정(P3).
- macOS 원격(B6) 회귀는 mbp 오프라인으로 실측 못 함(코드상 macOS 원격은 깨우지 않도록 되돌림).
