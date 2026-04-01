use super::error::InitError;

pub fn init_config() -> Result<crate::config::Config, InitError> {
    crate::config::Config::load().map_err(|e| InitError::Config(e.to_string()))
}

pub fn validate_config(config: &crate::config::Config) -> Vec<String> {
    let mut warnings = Vec::new();

    for sound in &config.sounds {
        if !std::path::Path::new(&sound.path).exists() {
            warnings.push(format!(
                "Sound '{}' has missing file: {}",
                sound.name, sound.path
            ));
        }
    }

    let mut hotkey_set = std::collections::HashSet::new();
    for sound in &config.sounds {
        if let Some(ref hotkey) = sound.hotkey {
            if !hotkey_set.insert(hotkey.clone()) {
                warnings.push(format!("Duplicate hotkey '{}' found in sounds", hotkey));
            }
        }
    }

    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_config_empty_is_valid() {
        let config = crate::config::Config::default();
        let warnings = validate_config(&config);
        assert!(warnings.is_empty());
    }
}
