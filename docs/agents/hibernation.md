# MK-081 Hibernation (자동 종료 제거됨)

0.3에서 유휴 세션 자동 종료(`hibernation.enabled` opt-in, 30분 타이머)를 제거했다(2026-11-03). Krill은 세션을 스스로 끝내지 않는다. 상태 디렉터리에 남은 `hibernation.enabled` 파일과 `settings.toml`의 `hibernation_enabled` 키는 무시하며 지우지 않는다.

남은 것은 재개 경로뿐이다. 이전 버전이 종료하면서 남긴 `hibernated/<세션 ID>`(0600) 표식이 있는 pane을 앱이 열면, `FEAT_HIBERNATION`(`1 << 51`)과 `FEAT_RESUME`을 함께 협상한 경우에만 `Request::Hibernation`으로 같은 대화를 재개한다. 같은 세션을 두 창에서 열어도 한 번만 재개한다. 재개 실행이 실패하면 오류를 반환하고 표식을 보존해 다음 재시도에서 이어간다. 일반 종료·일반 콜드 화면은 자동 실행하지 않는다.

## 재현·회귀 검증

- `cargo nextest run --locked -p krilld -p krill-proto -p krill-app -E 'test(hibernation)'`: 프로토콜 fixture, 기능 협상, 표식 없는 세션 비재개, 계정 전환 뒤 실행 당시 계정으로 재개.
- `krilld`의 `hibernation_auto_resume_keeps_conversation_and_is_idempotent`: 임시 상태 디렉터리와 가짜 Codex 실행 파일, 실제 holder·krilld로 표식이 있는 세션을 같은 대화 ID·cwd·권한으로 재개하고 중복 요청이 새 프로세스를 추가하지 않는지 확인한다.
