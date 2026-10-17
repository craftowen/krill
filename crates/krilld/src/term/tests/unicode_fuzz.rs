//! MK-056의 빠른 회귀 검사. 시간 제한 PTY → 네이티브 렌더 검사는 krill-app example에서 실행한다.

#[path = "../../../../krill-term/tests/support/unicode_fuzz.rs"]
mod corpus;

use super::*;

#[test]
fn unicode_byte_mutations_and_every_utf8_split() {
    let look = fresh();
    let mut rng = corpus::Generator::new(corpus::SEED);
    for iteration in 0..512 {
        let bytes = rng.case(iteration);
        let mut whole = Engine::new(WinSize { cols: 17, rows: 5 });
        let mut split = Engine::new(WinSize { cols: 17, rows: 5 });
        whole.feed(&bytes, Feed::Replay, &look);
        for byte in &bytes {
            split.feed(std::slice::from_ref(byte), Feed::Replay, &look);
        }
        whole.end_sync(Feed::Replay, &look);
        split.end_sync(Feed::Replay, &look);
        assert_eq!(
            whole.screen(),
            split.screen(),
            "case {iteration}: {bytes:02x?}"
        );
        let screen = split.screen();
        assert_eq!(screen.rows.len(), usize::from(screen.lines));
        assert!(
            screen
                .rows
                .iter()
                .all(|row| row.len() <= usize::from(screen.cols))
        );
    }
}
