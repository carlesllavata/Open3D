// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether the OS asks for fewer animations: Windows' "Animation effects" switched off, macOS'
//! "Reduce motion", a Linux or BSD desktop's animations setting. The UI asks about once a second
//! while it draws (`Host::reduce_motion`), so a change shows without a restart.

/// Does the OS ask for fewer animations? Animations stay on where it can't be told.
#[cfg(windows)]
pub fn reduced() -> bool {
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(r"Control Panel\Desktop")
        .and_then(|desktop| desktop.get_raw_value("UserPreferencesMask"))
        .is_ok_and(|mask| animations_off(&mask.bytes))
}

/// Windows keeps `SPI_GETCLIENTAREAANIMATION` (the switch in Settings, and what browsers follow
/// for `prefers-reduced-motion`) as bit 33 of `UserPreferencesMask`. Reading the registry stands
/// in for `SystemParametersInfoW`, which needs `unsafe`. A mask too short for the bit says nothing.
#[cfg(any(windows, test))]
fn animations_off(mask: &[u8]) -> bool {
    mask.get(4).is_some_and(|byte| byte & 0b10 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_animation_setting_is_bit_33_of_the_mask() {
        // Windows' default mask, then one from a machine with "Animation effects" switched off.
        assert!(!animations_off(&[0x9E, 0x1E, 0x07, 0x80, 0x12, 0x00, 0x00, 0x00]));
        assert!(animations_off(&[0x90, 0x12, 0x03, 0x80, 0x10, 0x00, 0x00, 0x00]));
    }

    #[test]
    fn a_short_mask_leaves_animations_on() {
        assert!(!animations_off(&[]));
        assert!(!animations_off(&[0x90, 0x12, 0x03, 0x80]));
    }
}
