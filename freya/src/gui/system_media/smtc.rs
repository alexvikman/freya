//! Windows: media keys and the media overlay, through SystemMediaTransportControls. Windows
//! ties them to a window, so Freya opens them for its own.

use std::path::Path;
use std::sync::mpsc::Sender;
use std::time::Duration;

use freya_audio::PlaybackState;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::WinitWindowAccessor;
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    MediaPlaybackStatus, MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    SystemMediaTransportControls, SystemMediaTransportControlsButton,
    SystemMediaTransportControlsButtonPressedEventArgs,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Storage::StorageFile;
use windows::Storage::Streams::RandomAccessStreamReference;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::WinRT::ISystemMediaTransportControlsInterop;
use windows::core::{HSTRING, Ref, Result, factory};

use super::{MediaRequest, Metadata, send};

/// How far the overlay's fast-forward and rewind buttons move, in seconds.
const SEEK_STEP: f64 = 10.0;

pub struct Controls {
    controls: SystemMediaTransportControls,
    /// What takes Freya's handlers off again.
    button_token: i64,
    position_token: i64,
    /// The length of what plays: the overlay's timeline needs it with every position.
    duration: Duration,
}

impl Controls {
    pub fn new(window: &slint::Window, requests: Sender<MediaRequest>) -> Option<Self> {
        let hwnd = window
            .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
                RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
                _ => None,
            })
            .flatten()?;
        Self::create(HWND(hwnd as *mut _), requests)
            .inspect_err(|e| log::warn!("no system media controls: {e}"))
            .ok()
    }

    fn create(hwnd: HWND, requests: Sender<MediaRequest>) -> Result<Self> {
        let controls = controls_for(hwnd)?;
        controls.SetIsEnabled(true)?;
        controls.SetIsPlayEnabled(true)?;
        controls.SetIsPauseEnabled(true)?;
        controls.SetIsStopEnabled(true)?;
        controls.SetIsNextEnabled(true)?;
        controls.SetIsPreviousEnabled(true)?;
        controls.SetIsFastForwardEnabled(true)?;
        controls.SetIsRewindEnabled(true)?;

        // Windows calls these on a thread of its own.
        let button_token = controls.ButtonPressed(&TypedEventHandler::new({
            let requests = requests.clone();
            move |_, args: Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
                if let Some(request) = button_request(args.ok()?.Button()?) {
                    send(&requests, request);
                }
                Ok(())
            }
        }))?;
        let position_token = controls.PlaybackPositionChangeRequested(&TypedEventHandler::new(
            move |_, args: Ref<PlaybackPositionChangeRequestedEventArgs>| {
                let position = args.ok()?.RequestedPlaybackPosition()?;
                send(&requests, MediaRequest::SetPosition(position.into()));
                Ok(())
            },
        ))?;
        Ok(Self {
            controls,
            button_token,
            position_token,
            duration: Duration::ZERO,
        })
    }

    pub fn set_metadata(&mut self, metadata: &Metadata) {
        if let Err(e) = self.show_metadata(metadata) {
            log::debug!("system media controls: {e}");
        }
    }

    pub fn set_playback(&mut self, state: PlaybackState, position: Duration) {
        if let Err(e) = self.show_playback(state, position) {
            log::debug!("system media controls: {e}");
        }
    }

    /// Nothing to show: Windows's controls have no volume of their own.
    pub fn set_volume(&mut self, _volume: f32) {}

    fn show_metadata(&mut self, metadata: &Metadata) -> Result<()> {
        self.duration = metadata.duration.unwrap_or_default();
        let updater = self.controls.DisplayUpdater()?;
        updater.ClearAll()?;
        updater.SetType(MediaPlaybackType::Music)?;
        let music = updater.MusicProperties()?;
        if let Some(title) = &metadata.title {
            music.SetTitle(&HSTRING::from(title))?;
        }
        if let Some(artist) = &metadata.artist {
            music.SetArtist(&HSTRING::from(artist))?;
        }
        if let Some(album) = &metadata.album {
            music.SetAlbumTitle(&HSTRING::from(album))?;
        }
        if let Some(cover) = &metadata.cover {
            // Without the cover the rest is still worth showing.
            match thumbnail(cover) {
                Ok(thumbnail) => updater.SetThumbnail(&thumbnail)?,
                Err(e) => log::debug!("no cover for the media overlay: {e}"),
            }
        }
        updater.Update()
    }

    fn show_playback(&self, state: PlaybackState, position: Duration) -> Result<()> {
        self.controls.SetPlaybackStatus(match state {
            PlaybackState::Stopped => MediaPlaybackStatus::Stopped,
            PlaybackState::Paused => MediaPlaybackStatus::Paused,
            PlaybackState::Playing => MediaPlaybackStatus::Playing,
        })?;
        let end = TimeSpan::from(self.duration);
        let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
        timeline.SetStartTime(TimeSpan::default())?;
        timeline.SetMinSeekTime(TimeSpan::default())?;
        timeline.SetEndTime(end)?;
        timeline.SetMaxSeekTime(end)?;
        timeline.SetPosition(position.into())?;
        self.controls.UpdateTimelineProperties(&timeline)
    }
}

impl Drop for Controls {
    /// Takes Freya's handlers off and hides the overlay.
    fn drop(&mut self) {
        let _ = self.controls.RemoveButtonPressed(self.button_token);
        let _ = self
            .controls
            .RemovePlaybackPositionChangeRequested(self.position_token);
        let _ = self.controls.SetIsEnabled(false);
    }
}

#[allow(
    unsafe_code,
    reason = "asking Windows for the media controls of the window winit created"
)]
fn controls_for(hwnd: HWND) -> Result<SystemMediaTransportControls> {
    let interop = factory::<SystemMediaTransportControls, ISystemMediaTransportControlsInterop>()?;
    // SAFETY: `hwnd` is Freya's open window, on this thread.
    unsafe { interop.GetForWindow(hwnd) }
}

fn button_request(button: SystemMediaTransportControlsButton) -> Option<MediaRequest> {
    Some(match button {
        SystemMediaTransportControlsButton::Play => MediaRequest::Play,
        SystemMediaTransportControlsButton::Pause => MediaRequest::Pause,
        SystemMediaTransportControlsButton::Stop => MediaRequest::Stop,
        SystemMediaTransportControlsButton::Next => MediaRequest::Next,
        SystemMediaTransportControlsButton::Previous => MediaRequest::Previous,
        SystemMediaTransportControlsButton::FastForward => MediaRequest::SeekBy(SEEK_STEP),
        SystemMediaTransportControlsButton::Rewind => MediaRequest::SeekBy(-SEEK_STEP),
        _ => return None,
    })
}

/// The cover image file as the overlay's thumbnail. WinRT opens files asynchronously; the
/// cover is a small file in Freya's cache, so this waits for it.
fn thumbnail(path: &Path) -> Result<RandomAccessStreamReference> {
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path))?.join()?;
    RandomAccessStreamReference::CreateFromFile(&file)
}
