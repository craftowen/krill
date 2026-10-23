//! Windows preferred UI language lookup; the buffer decoder is tested on every host.
#![cfg_attr(
    windows,
    expect(
        unsafe_code,
        reason = "Win32 preferred UI language FFI (CONVENTIONS U1)"
    )
)]

#[cfg(windows)]
pub(crate) fn preferred_locale() -> Option<String> {
    use windows_sys::Win32::Globalization::{GetUserPreferredUILanguages, MUI_LANGUAGE_NAME};

    let mut count = 0;
    let mut length = 0;
    // SAFETY: count and length are writable u32 values; a null buffer with zero
    // length requests the required UTF-16 buffer size without writing language data.
    let ok = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &raw mut count,
            std::ptr::null_mut(),
            &raw mut length,
        )
    };
    if ok == 0 || count == 0 || length == 0 {
        return None;
    }
    let mut buffer = vec![0; usize::try_from(length).ok()?];
    // SAFETY: buffer has length writable UTF-16 units and lives through the call;
    // count and length are valid output pointers, and length initially holds its capacity.
    let ok = unsafe {
        GetUserPreferredUILanguages(
            MUI_LANGUAGE_NAME,
            &raw mut count,
            buffer.as_mut_ptr(),
            &raw mut length,
        )
    };
    if ok == 0 || count == 0 {
        return None;
    }
    first_preferred_locale(buffer.get(..usize::try_from(length).ok()?)?)
}

fn first_preferred_locale(buffer: &[u16]) -> Option<String> {
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
