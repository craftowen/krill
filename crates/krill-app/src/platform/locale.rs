//! Windows preferred UI language lookup; the buffer decoder is tested on every host.

#[cfg(windows)]
pub(crate) fn preferred_locale() -> Option<String> {
    super::windows::preferred_locale()
}

pub(super) fn first_preferred_locale(buffer: &[u16]) -> Option<String> {
    if !buffer.ends_with(&[0, 0]) {
        return None;
    }
    let first = buffer.split(|unit| *unit == 0).next()?;
    if first.is_empty() {
        return None;
    }
    String::from_utf16(first).ok()
}

#[cfg(test)]
mod tests {
    use super::first_preferred_locale;

    #[test]
    fn windows_locale_uses_first_preference_without_skipping_to_korean() {
        for (languages, expected) in [
            ("en-US\0ko-KR\0\0", "en-US"),
            ("ko-KR\0en-US\0\0", "ko-KR"),
            ("ja-JP\0ko-KR\0en-US\0\0", "ja-JP"),
        ] {
            let buffer: Vec<u16> = languages.encode_utf16().collect();
            assert_eq!(first_preferred_locale(&buffer).as_deref(), Some(expected));
        }
    }

    #[test]
    fn windows_locale_rejects_empty_truncated_or_invalid_utf16_buffers() {
        for buffer in [
            &[][..],
            &[0, 0],
            &[101, 110],
            &[101, 110, 0],
            &[0xd800, 0, 0],
        ] {
            assert_eq!(first_preferred_locale(buffer), None);
        }
    }

    proptest::proptest! {
        #[test]
        fn windows_locale_decoder_handles_arbitrary_buffers(buffer in proptest::collection::vec(proptest::num::u16::ANY, 0..1024)) {
            let _ = first_preferred_locale(&buffer);
        }
    }
}
