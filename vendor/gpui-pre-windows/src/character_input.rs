//! Track character-preferred key presses by Win32 virtual-key identity.
//! The logical key can change when Shift is released before an OEM/digit key.
use std::collections::BTreeSet;

#[derive(Default)]
pub(crate) struct CharacterInputKeys(BTreeSet<u16>);

impl CharacterInputKeys {
    pub(crate) fn press(&mut self, key: u16, repeat: bool, prefer_character: bool) {
        if !repeat {
            self.0.remove(&key);
        }
        if prefer_character {
            self.0.insert(key);
        }
    }

    pub(crate) fn release(&mut self, key: u16) -> bool {
        self.0.remove(&key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifted_altgr_release_uses_virtual_key_after_modifiers_change() {
        let mut keys = CharacterInputKeys::default();
        keys.press(0x32, false, true); // shifted VK_2: @ on this layout
        keys.press(0x32, true, false); // modifiers changed during repeat
        assert!(keys.release(0x32)); // unshifted VK_2: 2
        assert!(!keys.release(0x32));
        keys.press(0x32, false, false); // genuine Ctrl+Alt action
        assert!(!keys.release(0x32));
    }

    #[test]
    fn simultaneous_oem_keys_and_new_presses_do_not_swallow_actions() {
        let mut keys = CharacterInputKeys::default();
        keys.press(0xba, false, true);
        keys.press(0xbb, false, true);
        keys.press(0x41, false, false);
        assert!(!keys.release(0x41));
        assert!(keys.release(0xbb));
        assert!(keys.release(0xba));
        // Focus can leave and return before the key is released. Keep the
        // pending physical identity; a fresh keydown clears any missed release.
        keys.press(0xba, false, true);
        assert!(keys.release(0xba));
        keys.press(0xba, false, true);
        keys.press(0xba, false, false); // missed release followed by new action
        assert!(!keys.release(0xba));
    }
}
