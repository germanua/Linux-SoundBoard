use std::sync::Arc;

use parking_lot::Mutex;

pub struct ConfigBuilder {
    config: linux_soundboard::config::Config,
}

impl ConfigBuilder {
    pub fn new() -> Self {
        Self {
            config: linux_soundboard::config::Config::default(),
        }
    }

    pub fn build(self) -> Arc<Mutex<linux_soundboard::config::Config>> {
        Arc::new(Mutex::new(self.config))
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}
