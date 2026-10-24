//! Playing: the player, the media keys, and the visualizer's frames.

use super::*;

impl App {
    pub fn play_index(&mut self, index: usize) {
        let Some(entry) = self.playlist.get(index) else {
            return;
        };
        let media = media(entry, &self.library);
        self.requested = Some(media.id);
        self.skip_after = None;
        self.playlist.set_current(Some(index));
        self.request_cover(&media);
        self.player.play(media);
        self.playlist_dirty = true;
        // What follows this entry, not the one queued after the last.
        self.queue_next();
    }

    pub fn stop(&mut self) {
        self.requested = None;
        self.skip_after = None;
        self.player.stop();
    }

    pub fn play(&mut self) {
        match self.player.status().state {
            PlaybackState::Paused => self.player.set_paused(false),
            PlaybackState::Playing => {}
            PlaybackState::Stopped => {
                let index = self
                    .playlist
                    .current()
                    .or_else(|| self.playlist.selected_indices().next())
                    .unwrap_or(0);
                self.play_index(index);
            }
        }
    }

    /// Mutes, or plays at the volume from before. With the volume at 0 already, nothing.
    pub fn toggle_mute(&mut self) {
        match self.muted.take() {
            Some(volume) => self.player.set_volume(volume),
            None => {
                let volume = self.player.volume();
                if volume > 0.0 {
                    self.muted = Some(volume);
                    self.player.set_volume(0.0);
                }
            }
        }
    }

    /// Plays at `volume`, muted or not before.
    pub fn set_volume(&mut self, volume: f32) {
        self.muted = None;
        self.player.set_volume(volume);
    }

    pub fn toggle_pause(&mut self) {
        match self.player.status().state {
            PlaybackState::Playing => self.player.set_paused(true),
            _ => self.play(),
        }
    }

    pub fn step(&mut self, forward: bool) {
        let (shuffle, repeat) = (self.settings.shuffle, self.settings.repeat);
        let target = if forward {
            self.playlist.peek_next(shuffle, repeat)
        } else {
            self.playlist.peek_previous(shuffle, repeat)
        };
        if let Some(i) = target {
            self.play_index(i);
        }
    }

    pub fn seek(&mut self, fraction: f64) {
        if let Some(length) = self.player.status().track.and_then(|t| t.duration) {
            self.player.seek(length.mul_f64(fraction.clamp(0.0, 1.0)));
        }
    }

    pub fn seek_by(&mut self, seconds: f64) {
        let position = self.player.status().position.as_secs_f64();
        self.player
            .seek(Duration::from_secs_f64((position + seconds).max(0.0)));
    }

    pub fn toggle_shuffle(&mut self) {
        self.settings.shuffle = !self.settings.shuffle;
        self.queue_next();
    }

    pub fn toggle_repeat(&mut self) {
        self.settings.repeat = !self.settings.repeat;
        self.queue_next();
    }

    /// Tells the engine which track follows the current one, for gapless playback.
    pub(super) fn queue_next(&mut self) {
        let next = self
            .playlist
            .peek_next(self.settings.shuffle, self.settings.repeat)
            .and_then(|i| self.playlist.get(i))
            .map(|entry| media(entry, &self.library));
        self.armed = next.as_ref().map(|m| m.id);
        self.player.set_next(next);
    }

    /// Changes the equalizer and sends the result to the audio engine.
    pub fn edit_eq(&mut self, change: impl FnOnce(&mut EqualizerSettings)) {
        change(&mut self.settings.equalizer);
        self.player.set_equalizer(&self.settings.equalizer.engine());
    }

    /// A request from the system's media controls: media keys, Now Playing.
    pub fn handle_media_request(&mut self, request: MediaRequest) {
        match request {
            MediaRequest::Play => self.play(),
            MediaRequest::Pause => self.player.set_paused(true),
            MediaRequest::Toggle => self.toggle_pause(),
            MediaRequest::Next => self.step(true),
            MediaRequest::Previous => self.step(false),
            MediaRequest::Stop => self.stop(),
            MediaRequest::SeekBy(seconds) => self.seek_by(seconds),
            MediaRequest::SetPosition(position) => self.player.seek(position),
            MediaRequest::SetVolume(volume) => self.set_volume(volume as f32),
            // Added to Up Next and played, as for an address: what is queued stays.
            MediaRequest::OpenUri(uri) => match playlist_file::file_url_path(&uri) {
                Some(path) => {
                    let first = self.playlist.len();
                    self.add_paths(&[path]);
                    if self.playlist.len() > first {
                        self.play_index(first);
                    }
                }
                None => self.add_url(&uri),
            },
            MediaRequest::Raise => {
                let _ = self.view.ui.show();
            }
            MediaRequest::Quit => {
                let _ = slint::quit_event_loop();
            }
        }
    }

    /// A stream added by its address gets the station's name in the playlist.
    pub(super) fn name_station(&mut self, track: &TrackInfo) {
        let Some(station) = &track.station else {
            return;
        };
        let untitled = self
            .playlist
            .entries()
            .iter()
            .any(|e| e.path == track.path && e.title.is_none());
        if untitled {
            let tags = freya_media::TagInfo {
                title: Some(station.clone()),
                ..Default::default()
            };
            self.playlist.apply_tags(&track.path, &tags);
            self.playlist_dirty = true;
        }
    }

    /// How often to draw visualization frames, or `None` when there is nothing to draw:
    /// nothing plays, or the window cannot be seen.
    pub fn visualization_interval(&self) -> Option<Duration> {
        let fps = visualizer::frame_rate(
            self.settings.visualizer_fps,
            self.view.visualizer.active(),
            self.window_hidden,
            self.window_focused,
        )?;
        Some(visualizer::frame_interval(fps))
    }

    /// Draws one visualization frame.
    pub fn visualize(&mut self, dt: Duration) {
        let playing = self.player.status().state == PlaybackState::Playing;
        self.view.visualizer.frame(
            &self.view.ui,
            &self.player,
            playing,
            &self.settings.visualizer_mode,
            dt,
        );
    }
}
