use crate::app_meta::CONFIG_DIR_NAME;

pub const CONFIG_FILE_NAME: &str = "config.json";

pub fn config_dir_name() -> &'static str {
    CONFIG_DIR_NAME
}

pub fn default_allow_multiple_playbacks() -> bool {
    false
}

/// Both tray settings default on. Where a session has no tray the icon simply
/// never appears and the close button keeps quitting, so defaulting on costs
/// those users nothing.
pub fn default_tray_setting() -> bool {
    true
}

pub fn default_auto_gain_target() -> f64 {
    -14.0
}

pub fn default_auto_gain_lookahead_ms() -> u32 {
    30
}

pub fn default_auto_gain_attack_ms() -> u32 {
    6
}

pub fn default_auto_gain_release_ms() -> u32 {
    150
}

#[cfg(test)]
mod tests {
    use super::default_allow_multiple_playbacks;

    #[test]
    fn multiple_playbacks_are_disabled_by_default() {
        assert!(!default_allow_multiple_playbacks());
    }
}
