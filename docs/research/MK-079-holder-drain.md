> 2026-10-15 스파이크 기록(옛 브랜치 mk-owe-444에서 옮김). 본문의 경로(`crates/mk-holder/...`)와 커밋은 개명 전 기준이라 지금 코드와 다르다. MK-079는 아직 미구현이다.

# MK-079 — 부분: 현행 드레인 부재를 조사했으며, 단독 PTY 리더로의 변경은 사람 결정까지 보류한다

OWE-444의 기능 구현 완료 보고가 아니다. 지휘자는 holder의 단독 PTY 읽기가 생존 계층의 구조 변경이므로 사람 결정으로 보류하고 조사·설계 문서만 남기도록 지시했다(2026-10-15). 제품 코드·프로토콜·하네스는 변경하지 않았다.

## 합격 기준

| 기준 | 측정값 | 판정 | 재현 명령 |
|---|---|---|---|
| krilld 30초 SIGSTOP 중 출력 writer가 막히지 않음 | 30.006초 정지 중 완료된 1,024B write 0회(목표 512회) | 불합격 | §재현 |
| holder 링 범위에서 원본 손실 0 | holder 출력 링이 없으므로 미측정 | 미충족 | 구현 승인 후 원본 해시 비교 필요 |
| SIGCONT 후 세션 출력 진행 재개 | 재개 후 0.128초 안에 512회 완료 | 합격(기존 세션 생존만) | §재현 |

## 환경

- 조사 기준 커밋: `70a5cde7c39968d01f56aaaf8f9fde23c5512408`.
- 기종 식별자 `Mac18,5`, `arm64`; macOS `27.0.1` (`26A434`), `sw_vers`·`uname -m`·`sysctl -n hw.model`로 확인.
- `rustc 1.98.1 (48a229cea 2026-09-01) (Homebrew)`.
- 실제 `mk-holder`·`krilld`·`mk` 개발 빌드(`0.1.0`), `Cargo.lock` 고정 의존성: `alacritty_terminal 0.26.0`, `zstd 0.14.2`.
- 실제 에이전트 CLI를 실행하지 않았다. 결정적 출력 생성기로 PTY 쓰기 진행 여부만 확인한다.

## 현행 경로와 막히는 이유

아래 파일·줄은 조사 기준 커밋의 위치다.

1. holder는 master fd를 보관하지만 PTY 읽기 이벤트는 등록하지 않는다. `crates/mk-holder/src/holder.rs:105`의 이벤트 루프는 krilld 링크·자식 시그널·adopt한 세션 종료만 처리한다. `holder.rs:389`에서는 재기동한 krilld에 master fd를 다시 보낸다.
2. krilld의 `crates/krilld/src/session.rs:691`은 master와 halt 파이프를 poll하고, `session.rs:702`·`session.rs:740`에서 master를 직접 읽는다. 따라서 프로세스 전체를 SIGSTOP하면 모든 세션 리더가 멈춘다.
3. holder의 `holder.rs:301`은 `wait(WaitOptions::NOHANG)`으로 종료를 거두고, `holder.rs:352`에서 종료 후 재기동을 예약한다. 살아 있지만 멈춘 krilld를 대신 읽는 상태 전이는 없다.
4. `docs/planning/T2-final.md:290`의 제안은 “5초 넘게 없으면” 세션당 1MiB 링으로 대신 읽는 방식이다. 이는 조사 기록이다. 이 제안을 그대로 적용해도 시작 전 쓰기 정체를 없앨 수 없으며, SIGSTOP은 프로세스 소실과도 다르다. 완료 조건은 `docs/ROADMAP.md`의 MK-079 행을 따른다.

SIGCONT 시 경합은 설계상 추론이며 이번에 경쟁 리더를 구현해 측정한 현상은 아니다. holder가 대리 읽기를 시작한 뒤 krilld가 직접 읽기를 재개하면 두 프로세스가 같은 master를 읽는다. holder가 보관한 이전 바이트와 krilld가 먼저 읽은 이후 바이트를 처리하는 순서가 역전될 수 있다. 하트비트·프로세스 정지 감지만으로는 모든 read 경계의 배타성이 보장되지 않는다. 반대로 krilld의 정지 확인 응답을 기다리면 SIGSTOP한 krilld는 그 응답을 보낼 수 없다.

## 두 설계안의 범위와 위험

아래 줄 수는 **추정**이며 구현·측정값이 아니다. 테스트를 제외한 제품 코드의 순증 범위다. 현행 `wc -l crates/mk-holder/src/*.rs`는 `holder.rs` 577줄, holder 전체 소스 1,102줄이었다. 규범의 holder 800줄 목표를 어느 집계로 적용할지도 사람이 확인해야 한다. 이번 작업에서 파일을 나누어 목표를 우회하지 않는다.

| 안 | 동작·범위 | 기대하는 보장 | 위험·추정 줄 수 |
|---|---|---|---|
| A. 정지 감지 후 일시 드레인 | 정지·부재 감지, PTY read, 링, 재전달, 읽기 소유권 이전 프로토콜 | 크래시 반복 중 출력 정체를 줄일 수 있음 | 감지 전 정체·SIGCONT 읽기 경합이 남음. 원래 조건을 충족하려면 추가 동기화가 필요하며, 임의 시점 SIGSTOP까지 다루는 검증된 이전 절차는 미정. holder +150~250줄, 연동 +100~200줄 추정 |
| B. 협상된 세션은 holder 단독 read | master fd는 생존·입력·크기 조절용으로 양쪽이 보관, 출력은 별도 비차단 fd로 krilld에 전달. holder가 출력 링과 overflow 구간을 관리 | krilld SIGSTOP 동안에도 PTY를 계속 비울 수 있고 같은 세션에 두 PTY 리더가 생기지 않음 | 생존 계층에 출력 전달 기능이 들어감. holder +250~450줄, krilld·프로토콜·재결합 연동 +200~400줄 추정. 메모리·지연 추가 비용 미측정 |

B는 승인받아 구현한 설계가 아니라 권고 후보다. 구체화할 때 다음 항목을 함께 해결해야 한다.

- holder v4를 명시 협상한 세션만 활성화한다. 기존 kind의 fd 개수를 바꾸면 구 디코더가 거절하므로 새 요청·응답 kind 또는 별도 fd 전달 메시지가 필요하다. 현행 동결 규칙은 `crates/mk-proto/src/holder.rs:17` 참조. 구 holder·구 krilld 조합은 기존 direct read로 유지한다.
- holder의 출력 전송이 막히면 PTY 읽기·제어 링크·다른 세션까지 막으면 안 된다. 읽기·쓰기 처리는 비차단이어야 하며 세션별 처리량 상한과 공정성이 필요하다. 기존 `mk_holder::link::Link::send`는 프레임 전체를 다 쓸 때까지 기다리므로 출력 폭주를 그대로 이 링크에 싣는 것은 적합하지 않다(`crates/mk-holder/src/link.rs:67`).
- 링은 T2 제안의 세션당 원본 1MiB 한도를 후보로 삼는다. ARCHITECTURE §3의 zstd 단일 코덱 결정을 지키면서 압축 후 메모리 상한과 압축 불가 출력도 제한해야 한다. overflow는 명시적 Gap과 원본 순번으로 전달하고 뒤의 바이트가 앞서 처리되지 않게 한다. 이미 소켓에 들어간 바이트·링에 남은 바이트·krilld가 읽은 바이트의 경계를 구별해야 한다.
- krilld 재기동은 relay의 아직 읽지 않은 출력과 링을 이어받아야 한다. 기존 `Kept`와 `Answer`는 master 한 개만 표현한다(`crates/krilld/src/holder.rs:28`). 초기 복원은 holder 응답 리더 스레드 시작 전이므로, 복원 중 동기 요청을 추가하면 자기 응답을 기다리는 교착을 만들 수 있다(`crates/krilld/src/main.rs:77`).
- holder crash 시 krilld는 relay EOF만 보고 세션을 종료하면 안 된다. relay 잔여 출력·부분 프레임을 먼저 정리하고, holder 소실이 확인된 뒤 master direct read로 전환해야 한다. 두 프로세스가 동시에 읽는 전환은 금지다. holder가 죽으면서 잃는 사용자 공간 링이 ARCHITECTURE §4.1의 “holder만 크래시 → 무손실” 보장을 훼손하지 않도록 공유 저장이나 동등한 보존 수단까지 설계해야 한다.
- 새 holder 재결합은 `crates/krilld/src/rejoin.rs:112`의 리더 정지·master 전달만으로 충분하지 않다. 출력 소스·잔여 프레임·링 경계의 인계도 필요하다. adopt 완료 전에 새 holder의 PTY read를 켜면 고아 krilld와 경합한다.
- holder exec는 현재 master·링크·listener만 보존한다(`crates/mk-holder/src/holder.rs:438`, `crates/mk-holder/src/sys.rs:53`). 새 출력 fd·보관 링·순번·부분 송신 프레임도 살아남아야 업그레이드 무손실을 유지한다. fd만 넘기고 사용자 공간 링을 버려서는 안 된다.
- 터미널 질의 응답은 계속 krilld만 해야 한다(ARCHITECTURE §4). SIGSTOP 중 DA/DSR 등의 응답까지 기다리는 실제 에이전트의 진행을 보장하는 것은 PTY write 정체 방지와 다른 조건이다. holder에 질의 응답기를 넣어 우회하지 않는다.
- 앱 프로토콜은 이번 조사에 필요하지 않다. 지휘자가 예약한 기능 비트 `1<<47`과 control 한 쌍은 구현·사용·등록하지 않았다.

## 결정과 근거

기능 구현은 보류한다. A의 단순 대리 읽기로 원래 조건을 충족했다고 주장할 수 없고, B는 holder crash·exec·재결합 보존까지 한꺼번에 바꾸므로 사람이 선택해야 한다. 지휘자도 이 이유로 단독 리더 구조 변경을 승인하지 않았다. 본 보고서는 조사 산출물이며 OWE-444의 “에이전트 블록 없음, 링 범위 손실 0”은 미완료다.

## 재현

저장소 루트에서 실행한다. 자식 환경의 `MK_STATE_DIR`만 임시 디렉터리로 바꾸고 사용자 전역 설정에는 쓰지 않는다. 테스트가 직접 띄운 holder의 유일한 자식 PID를 krilld로 확인한 뒤 그 PID만 SIGSTOP/SIGCONT하며, 마지막에 생성한 PID만 정리한다. 다른 세션·사용자 프로세스에는 시그널을 보내지 않는다.

```sh
perl -e 'alarm shift; exec @ARGV' 300 cargo build --locked -p mk-holder -p krilld -p mk
```

아래 Python을 `/tmp/mk079-repro.py`에 저장하고 `python3 /tmp/mk079-repro.py`로 실행한다. 생성기의 완료된 write 횟수를 별도 파일로 센다. krilld를 멈춘 뒤 출력 시작을 지시하므로 기존 큐에 있던 진행과 섞이지 않는다. 각 write는 1,024바이트이고 총 512회다. 줄바꿈의 PTY 변환 후 원본량은 별도로 측정하지 않았다.

```python
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

bin_dir = Path.cwd() / "target/debug"
state = Path(tempfile.mkdtemp(prefix="mk079-", dir="/tmp"))
env = dict(os.environ, MK_STATE_DIR=str(state))
holder = subprocess.Popen(
    [str(bin_dir / "mk-holder"), str(bin_dir / "krilld"),
     str(state / "run/krilld.sock")],
    env=env, stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
)
daemon = None
writer = None


def wait_for(test, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        if test():
            return
        time.sleep(0.01)
    raise RuntimeError("timed out")


def count():
    path = state / "progress"
    return len(path.read_text().splitlines()) if path.exists() else 0


try:
    wait_for(lambda: (state / "run/krilld.sock").exists())
    children = subprocess.check_output(
        ["/usr/bin/pgrep", "-P", str(holder.pid)], text=True
    ).split()
    assert len(children) == 1, children
    daemon = int(children[0])
    generator = r'''
import os, time
from pathlib import Path
s = Path(os.environ["MK_STATE_DIR"])
(s / "writer-pid").write_text(str(os.getpid()))
while not (s / "go").exists():
    time.sleep(0.001)
with (s / "progress").open("w", buffering=1) as p:
    for i in range(512):
        data = (f"{i:06d}:" + "x" * 1016 + "\n").encode()
        assert len(data) == 1024
        while data:
            data = data[os.write(1, data):]
        p.write(f"{i}\n")
(s / "done").touch()
'''
    session = subprocess.check_output(
        [str(bin_dir / "mk"), "new", "--cwd", str(state), "--",
         "/usr/bin/python3", "-c", generator], env=env, text=True, timeout=10
    ).strip()
    wait_for(lambda: (state / "writer-pid").exists())
    writer = int((state / "writer-pid").read_text())
    os.kill(daemon, signal.SIGSTOP)
    wait_for(lambda: "T" in subprocess.check_output(
        ["/bin/ps", "-o", "stat=", "-p", str(daemon)], text=True
    ))
    start = time.monotonic()
    (state / "go").touch()
    time.sleep(1)
    early = count()
    time.sleep(max(0, 30 - (time.monotonic() - start)))
    late = count()
    done_while_stopped = (state / "done").exists()
    stopped_seconds = time.monotonic() - start
    os.kill(daemon, signal.SIGCONT)
    resumed = time.monotonic()
    wait_for(lambda: (state / "done").exists())
    print(json.dumps(dict(
        session=session, payload_bytes=512 * 1024,
        completed_writes_at_1s=early, completed_writes_at_30s=late,
        done_while_stopped=done_while_stopped, stopped_seconds=stopped_seconds,
        completed_writes_after_continue=count(),
        resume_to_done_seconds=time.monotonic() - resumed,
        writer_block_reproduced=early == late and late < 512,
    ), indent=2))
finally:
    # Only PIDs created by this invocation; stop holder first to prevent respawn.
    holder.kill()
    holder.wait(timeout=5)
    for pid in (daemon, writer):
        if pid is not None:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    import shutil
    shutil.rmtree(state)
```

## 실측 결과

2026-10-15 실측, 1회. SIGSTOP 유지 시간 30.005867초, 시작 1초와 30초에서 완료된 write 모두 0회, 정지 중 생성 완료 없음. SIGCONT 후 0.128471초 안에 512회(생성기 입력 기준 524,288바이트) 쓰기를 모두 완료했다. 첫 write가 완료되지 않아 현행 경로의 출력 정체가 재현되었다. 원본 스트림 해시를 비교하지 않았으므로 손실 0을 입증한 결과가 아니다.

```json
{
  "session": "1",
  "payload_bytes": 524288,
  "completed_writes_at_1s": 0,
  "completed_writes_at_30s": 0,
  "done_while_stopped": false,
  "stopped_seconds": 30.005867374999998,
  "completed_writes_after_continue": 512,
  "resume_to_done_seconds": 0.12847145799999993,
  "writer_block_reproduced": true
}
```

## 검증

`perl -e 'alarm shift; exec @ARGV' 300 cargo build --locked -p mk-holder -p krilld -p mk`와 같은 300초 제한의 `cargo fmt --all`은 exit 0이었다. 리베이스 후 전체 체크 결과는 아래에 추가한다. 전체 체크 통과는 기존 코드의 회귀 게이트이며 드레인 기능 완료의 근거가 아니다.

## 미확인

- 경쟁 리더의 실제 바이트 순서 역전: 설계 추론만 했으며 측정하지 않음.
- holder 링 원본 해시, overflow Gap, 압축 불가 출력의 메모리 상한: 미구현·미측정.
- relay의 footprint·처리량·입력 지연·다중 세션 공정성: 미구현·미측정.
- relay 중 holder kill -9·krilld kill -9·holder exec·재결합의 무손실: 미구현·미측정.
- 실제 claude·codex가 질의 응답을 기다릴 때의 동작: 미실행.

## 오웬 확인 필요

- [ ] 완료 조건의 “블록 없음”이 PTY 출력 write의 backpressure에 한정되는지, 터미널 질의 응답까지 포함하는지 확정한다.
- [ ] 감지 전 일시 정체를 허용하는 A로 조건을 바꿀지, B의 단독 리더 출력 전달을 채택할지 결정한다.
- [ ] B 채택 시 holder crash 시 무손실을 유지할 링 보존 방식과 exec·재결합 전환 절차를 승인한다.
- [ ] holder 800줄 목표의 적용 단위와 크기 증가 허용 범위를 결정한다.
- [ ] 구현 후 30초 SIGSTOP 동안 모든 출력 완료, SIGCONT 후 결정적 원본 해시 일치, overflow Gap, 다중 세션·holder crash·exec·재결합을 필수 회귀 조건으로 검토한다.
