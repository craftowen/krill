# boost 2026-10-19 — Windows 포팅(craftowen/window) → main 병합 검증

/boost 2단계(1차 독립 조사 → 2차 검증·정정) 최종 보고서 B. 조사는 읽기 전용 git과 오프라인 cargo(metadata·fmt·deny)만 썼다.

**결론**:
- 충돌 4파일·7헌크와 리허설(a23ff15a) 해결이 맞다. `a23ff15a + 896589e4` 병합 트리는 충돌 없이 `62663dd9c8bd70be8923fd9eb956773456f37716`이고, 실제 병합 커밋 2d1014d3의 트리와 같다.
- 예상 트리에서 `cargo metadata --locked --offline`, `cargo fmt --all --check`, `cargo deny --offline check bans licenses sources`가 모두 통과했다.
- 로컬 main은 origin/dev를 추적하고 있고, 릴리스 이력에는 dev 리셋(v0.1.6)과 병합(v0.1.7)이 섞여 있다. 그래서 다음 릴리스에서 Windows 포팅이 빠질 위험이 있다(R1).

## 1. 1차 조사 정정

| 주장 | 실제 |
|---|---|
| 단일 루트 c88f0341 | 저장소 루트는 2개다(184f1108은 옛 m10 브랜치에만 있음). window와 origin/main의 루트는 c88f0341 하나라 결론은 같다 |
| 결과 vs origin/main 284파일 +13,812/−3,602 | 해결 트리 기준으로 284파일 +13,695/−3,623 |
| origin/main의 새 unix 전용 API 4건 | 1줄(`crates/krill/tests/cli.rs` PermissionsExt). 이 파일은 `#![cfg(unix)]`다 |
| 로컬 경로가 문서 3개에 8줄 | 8줄 모두 `docs/boost-2026-10-18-windows-port.md`에 있다. VM·TP PC 정보는 research 문서 여러 개에 있다. push할 때만 문제라 P3 |
| `wire_errno` WSA 오변환 (P2) | 경로는 실제로 있다(`from_wire(10035)` → 11). 이 코드가 실제로 나올 가능성은 낮다(추정). P3 |
| 하네스 승인 기록 있음 | 일부만 확인된다. `scripts/check-windows.sh`는 사용자 승인 목록에 있었다(2026-10-18). `scripts/memory-gate-windows.ps1`은 커밋 메시지에 승인 표기가 없다 |
| 새 Mac 문구는 LAN_DESC 하나 | 3건이다. TAB_SSH_DESC와 SSH_DISCOVERED_DESC는 원격 Mac을 가리키므로 그대로 두는 게 맞다 |

## 2. 적용 (2026-10-19)

1. origin/main 977e36db에서 통합 브랜치를 만들고, window SHA 896589e4를 고정해 `merge --no-ff`했다.
   - rerere가 4파일을 해결했고, `git diff a23ff15a -- <4파일>`은 0줄, 충돌 마커는 0이다.
   - 병합 커밋 2d1014d3의 트리는 62663dd9로, 예상값과 같다.
2. 후속 커밋 68d5421e:
   - `mobile::LAN_DESC`를 `mac_or(other_os::LAN_DESC)`로 바꿨다.
   - 쓰이지 않는 `other_os::HOST_LOCAL_MAC`을 삭제했다.
3. 검증: macOS `scripts/check.sh`와 `scripts/check-windows.sh --clippy`. 결과는 커밋 기록과 최종 보고에 남긴다.
4. 로컬 main: `~/.cache/krill-int-check`에서 `merge --ff-only`만 한다. `pull`(upstream이 origin/dev), `branch -f`, `update-ref`는 쓰지 않는다. push하지 않는다.

## 3. 남은 위험·후속

| # | 항목 | 조치 |
|---|---|---|
| R1 | 다음 릴리스를 dev 기준으로 만들면 Windows 포팅이 빠진다. window와 origin/dev는 21파일이 충돌한다 | 릴리스 전에 dev 통합이 필요하다(사람 결정) |
| R2 | 공개 저장소에 push하면 로컬 경로·VM·TP PC 정보가 드러난다 | push 전에 docs/research와 boost 문서에서 공개할 것을 고른다 |
| R3 | Windows에서 `wire_errno`가 10000 이상 코드를 잘못 바꾼다. `os()`의 EIO 기본값도 같이 봐야 한다 | 후속 이슈 |
| R4 | release.yml Windows 잡(workflow_dispatch 전용, macOS 업로드와 독립) | 릴리스 담당자에게 알린다 |
| — | krill-app UI 테스트 모듈이 `cfg(unix)`라 Windows에서는 컴파일 검사만 받는다 | 실기 QA(OWE-916) |

## 4. 미확인

- cargo deny advisories(조사 단계에서는 오프라인이었다. check.sh에서 확인)
- Windows 앱 UI 동작, 한글 IME, Windows 10/11 데스크톱
- Windows krilld가 WSA 100xx 코드를 실제로 `Failure::Os`로 보내는지
