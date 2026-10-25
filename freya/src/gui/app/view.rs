//! What the skin is told: the state written into it on every refresh, the log, and About.

use super::*;

impl App {
    /// Writes current state into the skin.
    pub fn refresh(&mut self) {
        let chosen = self.server_chosen();
        let station_rows = self.stations.rows(self.playing_station().as_deref());
        let playing_stream = self.playing_stream();
        let view = &mut self.view;
        let ui = &view.ui;
        let status = self.player.status();
        view.visualizer.update(
            ui,
            &self.player,
            status.state == PlaybackState::Playing && !self.window_hidden,
            &self.settings.visualizer_mode,
            !self.window_hidden,
        );
        view.visualizer.show_fps(ui, self.settings.visualizer_fps);
        let mut values = skin_state::player_values(
            &status,
            self.track.as_ref(),
            &self.playlist,
            self.player.volume(),
        );
        values.push(("muted", Value::Bool(self.muted.is_some())));
        values.push(("error", skin_state::string(&self.error)));
        view.player_out.write(ui, values);
        if let Some(media) = &mut self.system_media {
            let track = status.track.as_deref().or(self.track.as_deref());
            media.update(
                &status,
                track,
                self.cover_file.as_deref(),
                self.player.volume(),
            );
        }
        if view.playlist_out.available {
            if self.playlist_dirty {
                view.rows.sync(&self.playlist);
                self.playlist_dirty = false;
            }
            let values = skin_state::playlist_values(
                &self.playlist,
                self.settings.shuffle,
                self.settings.repeat,
            );
            view.playlist_out.write(ui, values);
        }
        let eq = &self.settings.equalizer;
        if view.eq_out.available {
            if view.eq_bands_shown != Some(eq.bands_db) {
                let values: Vec<Value> = eq
                    .bands_db
                    .iter()
                    .map(|&db| Value::Number(f64::from(db)))
                    .collect();
                let _ = ui.set_global_property(
                    "Equalizer",
                    "bands",
                    Value::Model(ModelRc::new(VecModel::from(values))),
                );
                view.eq_bands_shown = Some(eq.bands_db);
            }
            view.eq_out.write(
                ui,
                vec![
                    ("enabled", Value::Bool(eq.enabled)),
                    ("preamp", Value::Number(f64::from(eq.preamp_db))),
                    ("preset-name", skin_state::string(eq.preset_name())),
                ],
            );
        }
        if view.library_out.available {
            let playing = status
                .track
                .as_ref()
                .filter(|_| status.state != PlaybackState::Stopped)
                .map(|track| (track.path.as_path(), track.start));
            self.library.set_playing(playing);
            let folders = self.library.folders();
            self.library
                .show_folders(ui, &folders, self.folders_draft.as_deref());
            view.library_out.write(ui, self.library.values());
        }
        if view.playlists_out.available {
            let shown = self.shown_playlist;
            let synced = Some((self.playlists.version(), shown));
            if view.playlists_synced != synced {
                let all: Vec<(String, &[Entry], Option<SystemTime>)> = (0..self.playlists.len())
                    .filter_map(|i| Some((self.playlists.get(i)?, self.playlists.created(i))))
                    .map(|(p, created)| (p.name.clone(), p.entries.as_slice(), created))
                    .collect();
                self.library.set_playlists(&all);
                let playlist = shown.and_then(|i| self.playlists.get(i));
                self.library.show_playlist(
                    playlist.map(|p| p.name.as_str()),
                    playlist.map_or(&[][..], |p| &p.entries),
                );
                view.playlist_list_rows.sync(self.playlists.rows());
                let tracks = shown
                    .and_then(|i| self.playlists.get(i))
                    .map(|p| {
                        p.entries
                            .iter()
                            .enumerate()
                            .map(|(i, e)| skin_state::track_row(i, e, false))
                            .collect()
                    })
                    .unwrap_or_default();
                view.playlist_track_rows.sync(tracks);
                view.playlists_synced = synced;
            }
            let summary = shown.map(|i| self.playlists.summary(i)).unwrap_or_default();
            view.playlists_out.write(
                ui,
                vec![
                    ("shown", Value::Number(shown.map_or(-1.0, |i| i as f64))),
                    ("summary", skin_state::string(summary)),
                    ("error", skin_state::string(&self.playlists_error)),
                    ("page", self.library.playlist_page_value()),
                    ("draft", self.library.playlist_draft_value()),
                ],
            );
        }
        if view.radio_out.available {
            view.station_rows.sync(station_rows);
            let mut values = vec![("error", skin_state::string(&self.radio_error))];
            values.extend(self.radio.values());
            view.radio_out.write(ui, values);
            let saved: Vec<String> = self.stations.iter().map(|s| s.url.clone()).collect();
            let shown = (
                (self.radio.version(), self.radio_logos.version()),
                saved,
                playing_stream.clone(),
            );
            if view.directory_shown.as_ref() != Some(&shown) {
                let saved: HashSet<String> = shown.1.iter().cloned().collect();
                let streams = &self.station_streams;
                let playing = |url: &str| {
                    playing_stream.as_deref().is_some_and(|stream| {
                        url == stream || streams.get(url).is_some_and(|s| s == stream)
                    })
                };
                let far_only = self.library.elsewhere();
                let logos = &mut self.radio_logos;
                let rows = self
                    .radio
                    .rows(&saved, &playing, &mut |url| logos.get(url, far_only));
                let [found, shelves, tags, chosen] = &mut view.directory_rows;
                found.sync(rows.found);
                shelves.sync(rows.shelves);
                tags.sync(rows.tags);
                chosen.sync(rows.chosen);
                view.directory_shown = Some(shown);
            }
        }
        if !view.icon_set {
            view.icon_set = app_icon::set_window_icon(ui.window());
        }
        let title_bar = view.title_bar.update(ui);
        let lastfm = self.lastfm.status();
        match lastfm.state {
            lastfm::State::Connected => self.settings.lastfm_user = Some(lastfm.user.clone()),
            lastfm::State::Disconnected => self.settings.lastfm_user = None,
            // Unreadable keeps the user: the session may be readable next time.
            _ => {}
        }
        let (remote, name, address) = self.library.source_shown();
        let (state, error) = self.library.server_state();
        let mut server = vec![
            ("remote", Value::Bool(remote)),
            ("name", skin_state::string(name)),
            ("address", skin_state::string(address)),
            ("state", skin_state::string(state)),
            ("error", skin_state::string(error)),
            (
                "network",
                Value::Bool(self.library.server_settings().is_none_or(|s| s.network)),
            ),
            ("chosen", Value::Bool(chosen)),
            (
                "start-at-login",
                Value::Bool(
                    self.library
                        .server_settings()
                        .is_none_or(|s| s.start_at_login),
                ),
            ),
            ("here", Value::Bool(self.server_here)),
        ];
        server.extend(self.server.values());
        view.server_out.write(ui, server);
        view.lastfm_out.write(
            ui,
            vec![
                ("state", skin_state::string(lastfm.state.name())),
                ("user", skin_state::string(&lastfm.user)),
                ("scrobbling", Value::Bool(self.settings.lastfm_scrobbling)),
                ("waiting", Value::Number(lastfm.waiting as f64)),
                ("error", skin_state::string(&lastfm.error)),
            ],
        );
        if view.updates_out.available {
            let mut values = self.updater.values();
            values.push((
                "check-automatically",
                Value::Bool(self.settings.check_for_updates),
            ));
            view.updates_out.write(ui, values);
        }
        let activities = self.activities();
        let log_level = self.log_level;
        let view = &mut self.view;
        let ui = &view.ui;
        view.activity_rows.sync(activities);
        // Only while shown: a scan logs a lot.
        let log_shown = matches!(
            ui.get_global_property("App", "view"),
            Ok(Value::String(shown)) if shown == "activity"
        );
        let logged = (log_level, crate::log_capture::count());
        if log_shown && view.log_shown != Some(logged) {
            view.log_shown = Some(logged);
            view.log_rows.sync(log_rows(log_level));
        }
        view.app_out.write(
            ui,
            vec![
                (
                    "window-buttons-width",
                    Value::Number(f64::from(title_bar.buttons_width)),
                ),
                ("window-controls", Value::Bool(title_bar.controls)),
                ("window-maximized", Value::Bool(ui.window().is_maximized())),
                ("theme", skin_state::string(&self.settings.theme)),
                (
                    "equalizer-visible",
                    Value::Bool(self.settings.equalizer_visible),
                ),
                ("can-go-back", Value::Bool(self.history.can_go_back())),
                ("can-go-forward", Value::Bool(self.history.can_go_forward())),
                ("log-level", skin_state::string(level_name(log_level))),
            ],
        );
    }

    /// Shows what was logged at `level` ("debug", "info", "warning" or "error") or worse.
    pub fn set_log_level(&mut self, level: &str) {
        if let Some(level) = level_of(level) {
            self.log_level = level;
        }
    }

    /// What Freya does in the background, for `App.activities`.
    pub(super) fn activities(&self) -> Vec<Value> {
        let mut activities = self.library.activities();
        let lastfm = self.lastfm.status();
        if lastfm.waiting > 0 {
            let detail = if lastfm.error.is_empty() {
                format!("{} waiting", lastfm.waiting)
            } else {
                lastfm.error.clone()
            };
            activities.push(("Sending plays to Last.fm".to_owned(), detail));
        }
        if let update::State::Downloading { release, .. } = self.updater.state() {
            activities.push(("Downloading an update".to_owned(), release.tag_name.clone()));
        }
        activities
            .into_iter()
            .map(|(title, detail)| {
                Value::Struct(slint_interpreter::Struct::from_iter([
                    ("title".to_owned(), skin_state::string(title)),
                    ("detail".to_owned(), skin_state::string(detail)),
                ]))
            })
            .collect()
    }

    pub fn show_about(&mut self) {
        if self.about.is_none() {
            match freya_skin::about_window() {
                Ok(window) => {
                    let _ = window
                        .set_property("version", skin_state::string(env!("CARGO_PKG_VERSION")));
                    self.about = Some(window);
                }
                Err(e) => {
                    log::warn!("could not open About: {e}");
                    return;
                }
            }
        }
        if let Some(about) = &self.about {
            let _ = about.show();
        }
    }
}

/// What was logged at `level` or worse, for `App.log`.
pub(super) fn log_rows(level: log::Level) -> Vec<Value> {
    crate::log_capture::entries(level, LOG_SHOWN)
        .into_iter()
        .map(|entry| {
            Value::Struct(slint_interpreter::Struct::from_iter([
                ("time".to_owned(), skin_state::string(&entry.time)),
                (
                    "level".to_owned(),
                    skin_state::string(level_name(entry.level)),
                ),
                ("message".to_owned(), skin_state::string(&entry.message)),
            ]))
        })
        .collect()
}

/// How many entries of the log the Activity page shows at most.
pub(super) const LOG_SHOWN: usize = 1_000;

pub(super) fn level_name(level: log::Level) -> &'static str {
    match level {
        log::Level::Error => "error",
        log::Level::Warn => "warning",
        log::Level::Info => "info",
        log::Level::Debug | log::Level::Trace => "debug",
    }
}

pub(super) fn level_of(name: &str) -> Option<log::Level> {
    Some(match name {
        "error" => log::Level::Error,
        "warning" => log::Level::Warn,
        "info" => log::Level::Info,
        "debug" => log::Level::Debug,
        _ => return None,
    })
}
