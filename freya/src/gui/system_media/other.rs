//! No media controls of our own yet on this system.

use std::sync::mpsc::Sender;
use std::time::Duration;

use freya_audio::PlaybackState;

use super::{MediaRequest, Metadata};

pub struct Controls;

impl Controls {
    pub fn new(_window: &slint::Window, _requests: Sender<MediaRequest>) -> Option<Self> {
        None
    }

    pub fn set_metadata(&mut self, _metadata: &Metadata) {}

    pub fn set_playback(&mut self, _state: PlaybackState, _position: Duration) {}

    pub fn set_volume(&mut self, _volume: f32) {}
}
