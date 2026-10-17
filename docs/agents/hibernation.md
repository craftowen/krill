# MK-081 Hibernation

기본값은 꺼짐이다. 상태 디렉터리(`KRILL_STATE_DIR`, 기본 경로는 ARCHITECTURE §4)의 `hibernation.enabled` 빈 파일을 만들면 켜지고, 지우면 다음 검사부터 자동 종료를 중단한다. 설정 → 에이전트 → '유휴 세션 동면'에서도 켜고 끌 수 있다(OWE-655). 앱은 이 토글을 같은 파일로 반영하고, 사람이 직접 만든 파일은 설정 파일 값이 꺼짐이어도 지우지 않고 켜짐으로 읽는다(끄려면 토글을 끄거나 파일을 지운다). 꺼도 이미 잠든 pane은 열 때 자동 재개한다.

확정 hook `done`을 관측한 뒤 단조 시계로 30분을 기다린다. 같은 실제 cwd를 가진 실행 중 세션을 한 묶음으로 검사한다. 한 pane이라도 안전하지 않으면 모두 살아 있다. 새 완료 hook, 출력, 연결 복귀·해제는 대기 시간을 다시 요구하며, 완료 후 입력은 **다음 완료 hook까지 계속 종료를 막는다**. 시간 경과만으로 입력 보호를 풀지 않는다. 입력·출력·연결 잠금을 묶음 전체에서 잡은 뒤 최종 검사해 새 attach와 종료가 엇갈리지 않게 한다.

## Orca 안전 조건 8가지 대조

근거: 조사에 사용된 Orca 문서 `docs/site/content/docs/agents/hibernation.mdx`의 “What gets hibernated”. 문서 조건을 독립 구현했으며 Orca 코드는 차용하지 않았다.

| Orca 조건 | Krill 대응 | 보수적 제한 / 검증 |
|---|---|---|
| 턴 완료, 사용자 응답 대기 아님 | `Source::Hook`, `Confidence::High`, `Done`만 허용 | waiting·blocked·working·unknown·exited 및 화면 추정 완료는 거절; 가짜 시계 회귀 테스트 |
| 활성 작업 공간·화면이 아님 | 같은 runtime cwd의 모든 pane에 화면 viewer와 모든 IPC attachment가 없어야 함 | 같은 cwd의 일반 셸도 완료 조건을 충족하지 못하면 묶음 전체를 보호 |
| 완료 이후 키 입력 없음 | PTY 쓰기 진입부터 입력 보호. 새로 수락한 완료 hook에서만 기준 갱신 | 중복 hook은 보호를 지우지 않고, 늦게 수거한 완료 hook보다 최신 입력도 보호 |
| 같은 대화를 재개할 수 있는 에이전트 | MK-052가 지원하는 Claude·Codex 실행 파일과 유효한 저장 대화 ID 필요 | 다른 provider·ID 없음·설정 손상은 종료하지 않음 |
| 유휴 창 이상 경과 | 완료 관측 후 30분 + 원본 출력/연결 복귀·해제 뒤 30분 | 프로세스 재기동·enable 전환 시 시간은 다시 셈; 30분 경계 테스트 |
| 모바일에서 조작 중 아님 | raw·screen, interactive·observer 구분 없이 attachment 하나라도 있으면 거절 | 현재 모바일 API 없음. 향후 모바일 연결도 이 장부에 등록해야 함 |
| 미정산 Dispatch 없음 | **MK-100 전 Dispatch 모델 미지원** | 외부 Orca Dispatch를 질의하지 않는다. Krill 자체 Dispatch를 도입할 때 authoritative settlement 연결 전에는 해당 pane 종료를 허용하면 안 됨 |
| 살아 있는 subagent/teammate roster 없음 | Task/Agent/spawn_agent/team 생성 도구·Subagent/Teammate 이벤트를 보면 프로세스 수명 전체에서 veto. 커널 프로세스 목록에서 자식이 하나라도 있으면 거절 | 종료 이벤트로 roster 해제를 추정하지 않음. 구 메타데이터·데몬 재전달 세션·손상 payload는 미확인으로 veto. MCP helper 등도 자식이면 종료를 막음 |

Dispatch 미지원의 위 대응과 보수적 roster 정책, 기본 꺼짐은 잠정 결정이다. provider 내부 동작을 완전히 관측하지 못하는 환경에서 opt-in을 제품 기본값으로 바꾸면 안 된다.

## 종료·복구

종료 전에 `hibernated/<세션 ID>`(0600) 표식을 남기고 holder v3의 `Terminate`를 보낸다. holder의 fd·세그먼트·실행 메타데이터를 지우지 않는다. `Exited` 뒤에도 기록과 표식은 남는다. 앱이 pane을 열 때 `FEAT_HIBERNATION`(`1 << 51`)과 `FEAT_RESUME`을 함께 협상한 경우에만 자동 재개를 요청한다. 타이머와 재개를 직렬화해 같은 세션을 두 창에서 열어도 한 번만 재개한다. 종료가 아직 진행 중이거나 재개 실행이 실패하면 오류를 반환하고 표식을 보존해 다음 재시도에서 이어간다. 일반 종료·일반 콜드 화면은 자동 실행하지 않는다.

데몬이 종료 요청 전후에 죽었는데 holder가 **살아 있는** 세션을 재전달하면 미확인 종료를 재실행하지 않고 표식을 취소해 그 프로세스를 보존한다. 이미 끝난 세션의 표식은 재기동 후에도 남아 자동 재개한다. 재개 도중 크래시의 전체 원자성은 기존 MK-052 범위에 따른다.

## 재현·회귀 검증

- `cargo nextest run --locked -p krilld -p krill-proto -p krill-app -E 'test(hibernation)'`: 단조 시계 30분 경계, 상태/입력/foreground 보호, 묶음 전체 gate, 표식 보존, 프로토콜 fixture, 기능 협상 및 자동 재개.
- `krilld`의 `hibernation_auto_resume_keeps_conversation_and_is_idempotent`: 임시 상태 디렉터리와 가짜 Codex 실행 파일, 실제 holder·krilld를 사용해 종료 후 같은 대화 ID·cwd·권한으로 재개하며 중복 요청이 새 프로세스를 추가하지 않는지 확인. 실제 유료 CLI는 호출하지 않는다.
- `scripts/check.sh`: 포맷·clippy·전체 nextest·doctest·deny 최종 게이트.
