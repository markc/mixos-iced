// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicitly unavailable player transport until a native media service owns it.
//! Output volume continues through the independent native audio controller.

#[derive(Debug)]
pub enum MediaError {
    Unavailable,
}
impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "media playback control is unavailable: no native media service or MPRIS adapter",
        )
    }
}
impl std::error::Error for MediaError {}

#[derive(Default)]
pub struct MediaController;
impl MediaController {
    pub fn new() -> Self {
        Self
    }
    pub fn play_pause(&self) -> Result<(), MediaError> {
        self.unavailable()
    }
    pub fn play(&self) -> Result<(), MediaError> {
        self.unavailable()
    }
    pub fn pause(&self) -> Result<(), MediaError> {
        self.unavailable()
    }
    pub fn stop(&self) -> Result<(), MediaError> {
        self.unavailable()
    }
    pub fn next(&self) -> Result<(), MediaError> {
        self.unavailable()
    }
    pub fn previous(&self) -> Result<(), MediaError> {
        self.unavailable()
    }
    fn unavailable(&self) -> Result<(), MediaError> {
        warn!("{}", MediaError::Unavailable);
        Err(MediaError::Unavailable)
    }
}
