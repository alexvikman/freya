//! macOS: media keys, the Now Playing widget and headphone buttons, through the MediaPlayer
//! framework. Its remote command center sends the requests; its info center shows what plays.

#![allow(
    unsafe_code,
    reason = "every MediaPlayer call is unsafe in its bindings; each is a plain call on the \
              shared command or info center, with the argument types Apple documents"
)]

use std::path::Path;
use std::ptr::NonNull;
use std::sync::mpsc::Sender;
use std::time::Duration;

use block2::RcBlock;
use freya_audio::PlaybackState;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSImage;
use objc2_core_foundation::CGSize;
use objc2_foundation::{NSMutableDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPChangePlaybackPositionCommandEvent, MPMediaItemArtwork, MPMediaItemPropertyAlbumTitle,
    MPMediaItemPropertyArtist, MPMediaItemPropertyArtwork, MPMediaItemPropertyPlaybackDuration,
    MPMediaItemPropertyTitle, MPNowPlayingInfoCenter, MPNowPlayingInfoPropertyElapsedPlaybackTime,
    MPNowPlayingInfoPropertyPlaybackRate, MPNowPlayingPlaybackState, MPRemoteCommand,
    MPRemoteCommandCenter, MPRemoteCommandEvent, MPRemoteCommandHandlerStatus,
};

use super::{MediaRequest, Metadata, send};

pub struct Controls {
    /// Each command Freya handles, with the target that takes its handler off again.
    commands: Vec<(Retained<MPRemoteCommand>, Retained<AnyObject>)>,
    /// What the Now Playing widget shows; the position is added to it as it changes.
    info: Retained<NSMutableDictionary<NSString, AnyObject>>,
}

impl Controls {
    pub fn new(_window: &slint::Window, requests: Sender<MediaRequest>) -> Option<Self> {
        let center = unsafe { MPRemoteCommandCenter::sharedCommandCenter() };
        let simple = |command, request: MediaRequest| {
            handle(command, &requests, move |_| Some(request.clone()))
        };
        let commands = unsafe {
            vec![
                simple(center.playCommand(), MediaRequest::Play),
                simple(center.pauseCommand(), MediaRequest::Pause),
                simple(center.togglePlayPauseCommand(), MediaRequest::Toggle),
                simple(center.stopCommand(), MediaRequest::Stop),
                simple(center.nextTrackCommand(), MediaRequest::Next),
                simple(center.previousTrackCommand(), MediaRequest::Previous),
                handle(
                    Retained::into_super(center.changePlaybackPositionCommand()),
                    &requests,
                    |event| {
                        let event = event.downcast_ref::<MPChangePlaybackPositionCommandEvent>()?;
                        let seconds = event.positionTime().max(0.0);
                        Some(MediaRequest::SetPosition(Duration::from_secs_f64(seconds)))
                    },
                ),
            ]
        };
        Some(Self {
            commands,
            info: NSMutableDictionary::new(),
        })
    }

    pub fn set_metadata(&mut self, metadata: &Metadata) {
        let info = NSMutableDictionary::<NSString, AnyObject>::new();
        let texts = unsafe {
            [
                (MPMediaItemPropertyTitle, &metadata.title),
                (MPMediaItemPropertyArtist, &metadata.artist),
                (MPMediaItemPropertyAlbumTitle, &metadata.album),
            ]
        };
        for (key, text) in texts {
            if let Some(text) = text {
                info.insert(key, &NSString::from_str(text));
            }
        }
        if let Some(duration) = metadata.duration {
            let seconds = NSNumber::new_f64(duration.as_secs_f64());
            info.insert(unsafe { MPMediaItemPropertyPlaybackDuration }, &seconds);
        }
        if let Some(artwork) = metadata.cover.as_deref().and_then(artwork) {
            info.insert(unsafe { MPMediaItemPropertyArtwork }, &artwork);
        }
        self.info = info;
        self.show();
    }

    pub fn set_playback(&mut self, state: PlaybackState, position: Duration) {
        let (state, rate) = match state {
            PlaybackState::Stopped => (MPNowPlayingPlaybackState::Stopped, 0.0),
            PlaybackState::Paused => (MPNowPlayingPlaybackState::Paused, 0.0),
            PlaybackState::Playing => (MPNowPlayingPlaybackState::Playing, 1.0),
        };
        // The widget moves the position along by itself at this rate.
        let elapsed = NSNumber::new_f64(position.as_secs_f64());
        let rate = NSNumber::new_f64(rate);
        unsafe {
            self.info
                .insert(MPNowPlayingInfoPropertyElapsedPlaybackTime, &elapsed);
            self.info
                .insert(MPNowPlayingInfoPropertyPlaybackRate, &rate);
        }
        self.show();
        // macOS cannot tell by itself whether Freya plays; media keys go to the app that last
        // said it does.
        unsafe { MPNowPlayingInfoCenter::defaultCenter().setPlaybackState(state) };
    }

    /// Nothing to show: macOS's controls have no volume of their own.
    pub fn set_volume(&mut self, _volume: f32) {}

    fn show(&self) {
        unsafe { MPNowPlayingInfoCenter::defaultCenter().setNowPlayingInfo(Some(&self.info)) };
    }
}

impl Drop for Controls {
    /// Takes Freya's handlers off the commands and clears the widget.
    fn drop(&mut self) {
        unsafe {
            for (command, target) in &self.commands {
                command.removeTarget(Some(target));
            }
            let center = MPNowPlayingInfoCenter::defaultCenter();
            center.setNowPlayingInfo(None);
            center.setPlaybackState(MPNowPlayingPlaybackState::Stopped);
        }
    }
}

/// Enables `command` and sends what `request` makes of each of its events to Freya. Returns
/// the command with its target, to take the handler off again.
fn handle(
    command: Retained<MPRemoteCommand>,
    requests: &Sender<MediaRequest>,
    request: impl Fn(&MPRemoteCommandEvent) -> Option<MediaRequest> + 'static,
) -> (Retained<MPRemoteCommand>, Retained<AnyObject>) {
    let requests = requests.clone();
    let handler = RcBlock::new(move |event: NonNull<MPRemoteCommandEvent>| {
        // SAFETY: the framework passes an event that is valid while the handler runs.
        match request(unsafe { event.as_ref() }) {
            Some(request) => {
                send(&requests, request);
                MPRemoteCommandHandlerStatus::Success
            }
            None => MPRemoteCommandHandlerStatus::CommandFailed,
        }
    });
    // The command keeps its own copy of the block until the target is removed.
    let target = unsafe {
        command.setEnabled(true);
        command.addTargetWithHandler(&handler)
    };
    (command, target)
}

/// The cover image file as Now Playing artwork.
fn artwork(path: &Path) -> Option<Retained<MPMediaItemArtwork>> {
    let path = NSString::from_str(path.to_str()?);
    let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &path)?;
    let size = image.size();
    // The widget asks for the image at the size it shows; macOS scales it. The block owns
    // the image, so the pointer it returns stays valid.
    let image_at = RcBlock::new(move |_size: CGSize| NonNull::from(&*image));
    let artwork = unsafe {
        MPMediaItemArtwork::initWithBoundsSize_requestHandler(
            MPMediaItemArtwork::alloc(),
            size,
            &image_at,
        )
    };
    Some(artwork)
}
