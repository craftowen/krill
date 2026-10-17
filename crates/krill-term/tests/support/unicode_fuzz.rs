//! MK-056의 결정적 바이트 변이 코퍼스. 제품 코드에 포함하지 않는다.

pub const SEED: u64 = 0x4d4b_0056_2026_1015;

pub const CORPUS: &[&[u8]] = &[
    "한글 가나다 힣 ㄱㅏ \u{1112}\u{1161}\u{11ab}\r\n".as_bytes(),
    "👩🏽‍💻👨‍👩‍👧‍👦🏳️‍🌈🇰🇷1️⃣☕️❤︎❤️".as_bytes(),
    "a\u{301}\u{308}\u{200d}\u{fe0f}한\u{302}\u{20e3}".as_bytes(),
    "\u{1100}\u{1161}\u{11a8}\u{115f}\u{1160}\u{3164}".as_bytes(),
    b"\xf0\x9f\x91\xe3\x80\xed\xa0\x80\xc0\xaf\xff\x80",
    "\x1b[1;3;4;9m한👩🏽‍💻\x1b[0m".as_bytes(),
    "\x1b[38;2;255;0;99m한\x1b[48;5;196m🙂\x1b[0m".as_bytes(),
    "\x1b]8;;https://example.invalid/한\x1b\\링크👩‍💻\x1b]8;;\x1b\\".as_bytes(),
    "\x1b]2;제목🙂\x07\x1b[6n\x1b]11;?\x07".as_bytes(),
    "\x1b[?2026h동기🙂\x1b[?2026l".as_bytes(),
    "\x1b[?1049h대체🇰🇷\x1b[?1049l".as_bytes(),
    b"\x1b[2J\x1b[H\x1b[2;4r\x1b[999C\x1b[3P\x1b[2@\x1b[0m",
    b"\x1b[?2031h\x1b[?2031$p\x1b[?2031l\x1b[?2031$p",
    b"\x1bPunterminated\x1b\\\x1b]52;c;?\x07\x18\x1a",
    b"\x1b[>1u\x1b[<u", // 스택 overflow는 별도 OWE-474. 여기서는 짝을 이룬 키보드 모드.
    b"\r\n\t\x08\x1bM\x1b[1S\x1b[1T",
];

#[derive(Debug)]
pub struct Generator(u64);

impl Generator {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next(&mut self, bound: usize) -> usize {
        // xorshift64: 의도적인 wrapping 없이 재현 가능한 테스트 전용 PRNG.
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % u64::try_from(bound).unwrap_or(1)).unwrap_or(0)
    }

    pub fn case(&mut self, iteration: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        for _ in 0..=self.next(8) {
            bytes.extend_from_slice(CORPUS[self.next(CORPUS.len())]);
        }
        // 첫 16개는 모든 fixture를 무변이로 실행한다.
        if let Ok(index) = usize::try_from(iteration)
            && let Some(seed) = CORPUS.get(index)
        {
            return seed.to_vec();
        }
        for _ in 0..=self.next(8) {
            let pos = self.next(bytes.len() + 1);
            match self.next(5) {
                0 => bytes.insert(pos, u8::try_from(self.next(256)).unwrap_or(0)),
                1 if pos < bytes.len() => {
                    bytes.remove(pos);
                }
                2 if pos < bytes.len() => bytes[pos] ^= 1 << self.next(8),
                3 => bytes.truncate(pos),
                _ => {
                    let fragment = CORPUS[self.next(CORPUS.len())];
                    bytes.splice(pos..pos, fragment.iter().copied());
                }
            }
        }
        bytes
    }
}
