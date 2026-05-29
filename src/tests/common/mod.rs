// Shared integration-test toolbox: each test binary includes `mod common` but
// uses only the helpers it needs, so unused-in-this-binary items and re-exports
// are expected rather than a defect.
#![allow(dead_code, unused_imports)]

pub mod audio_mock;
pub mod config_fixture;
pub mod temp_dir;

pub use audio_mock::{FakeAudioPlayer, PlayCall};
pub use config_fixture::ConfigBuilder;
pub use temp_dir::TempConfigDir;
