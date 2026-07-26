use std::{
    env, io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
        mpsc::{Receiver, Sender},
    },
    thread,
};

use ::image as image_crate;
use futures::{SinkExt, Stream};
use iced::{
    Element, Event, Subscription, Task,
    event::Status,
    keyboard::{self, key},
    widget::{image, operation},
    window,
};
use ringboard_sdk::{
    ClientError,
    core::{Error as CoreError, protocol::RingKind},
    search::cancellation_token,
    ui_actor::{
        Command, CommandError, Message as ControllerMessage, SearchKind, UiEntry, UiEntryCache,
        controller,
    },
};

use crate::{
    message::{ImageKind, Message},
    state::{ActiveTab, State},
    utils::{decode_image_async, load_server_config_async, save_server_config_async},
};

/// One request → decode → cache pipeline for images. The app holds two
/// instances (thumbnails and full-res detail images) that differ only in
/// how the decoded image is post-processed; keeping the mechanics in one
/// type means fixes to requesting/eviction can't miss a twin copy.
#[derive(Default)]
pub struct ImagePipeline {
    cache: std::collections::HashMap<u64, image::Handle>,
    pending: std::collections::HashSet<u64>,
}

impl ImagePipeline {
    pub fn get(&self, id: u64) -> Option<&image::Handle> {
        self.cache.get(&id)
    }

    /// Asks the controller for the entry's image unless it's already cached
    /// or in flight.
    fn request(&mut self, id: u64, requests: &Sender<Command>) {
        if !self.pending.contains(&id) && !self.cache.contains_key(&id) {
            self.pending.insert(id);
            let _ = requests.send(Command::LoadImage(id));
        }
    }

    /// Claims an arrived response: true if this pipeline was waiting on it.
    fn claim(&mut self, id: u64) -> bool {
        self.pending.remove(&id)
    }

    fn insert(&mut self, id: u64, handle: image::Handle) {
        self.cache.insert(id, handle);
    }

    fn remove(&mut self, id: u64) {
        self.cache.remove(&id);
        self.pending.remove(&id);
    }

    fn retain(&mut self, live: &std::collections::HashSet<u64>) {
        self.cache.retain(|id, _| live.contains(id));
        self.pending.retain(|id| live.contains(id));
    }

    fn clear(&mut self) {
        self.cache.clear();
        self.pending.clear();
    }
}

/// The application model plus communication channels (TEA Model).
pub struct RingboardApp {
    pub requests: Sender<Command>,
    pub state: State,
    /// Downscaled row-preview thumbnails, kept for every visible image entry.
    pub thumbnails: ImagePipeline,
    /// Full-resolution images, fetched lazily only for the entry currently
    /// shown in the detail panel (evicted when it closes) so we're not
    /// holding a full-res decode in memory for every image in the list.
    pub detail_images: ImagePipeline,
    /// Whether closing the window should hide it (resuming instantly on the
    /// next launch) instead of exiting the process, mirroring the egui
    /// client's background behavior.
    daemon: bool,
    /// Tells the `maintain_single_instance` background thread to stop when
    /// the app is really exiting (not just hiding).
    stop: Arc<AtomicBool>,
    /// This window's id, resolved once at boot; needed to hide/show/focus it.
    window_id: Option<window::Id>,
}

/// A one-off `window::resize` after the window is already showing turned out
/// to corrupt text rendering in this iced version (a real render glitch, not
/// just a timing/positioning one — investigated and confirmed via manual
/// testing, not just theorized), so the window is sized once, fixed, at
/// creation (see `main`) instead of adapting to the actual monitor. Kept in
/// the middle of the min/max range so it's reasonable on both small and
/// large screens without ever needing a post-launch resize.
pub const WINDOW_MIN_SIZE: iced::Size = iced::Size::new(400.0, 480.0);
pub const WINDOW_DEFAULT_SIZE: iced::Size = iced::Size::new(600.0, 650.0);
pub const WINDOW_MAX_SIZE: iced::Size = iced::Size::new(800.0, 900.0);

/// Bridges the background controller thread's blocking `Receiver` into an
/// async stream, so the UI is woken only when a message actually arrives
/// instead of polling on a timer.
fn controller_messages(responses: Receiver<ControllerMessage>) -> impl Stream<Item = Message> {
    iced::stream::channel(8, async move |mut output| {
        thread::spawn(move || {
            while let Ok(msg) = responses.recv() {
                // `send` (not `try_send`) so a momentary burst (e.g. many
                // image loads after the first page loads) applies
                // backpressure instead of erroring out and permanently
                // killing this forwarder thread.
                if futures::executor::block_on(output.send(Message::Controller(Arc::new(msg))))
                    .is_err()
                {
                    break;
                }
            }
        });
    })
}

/// Bridges the `maintain_single_instance` background thread's wake signal
/// into an async stream.
fn wake_messages(rx: Receiver<()>) -> impl Stream<Item = Message> {
    iced::stream::channel(1, async move |mut output| {
        thread::spawn(move || {
            while rx.recv().is_ok() {
                if futures::executor::block_on(output.send(Message::WakeRequested)).is_err() {
                    break;
                }
            }
        });
    })
}

/// `keyboard::listen()` only delivers events the widget tree *ignored*. The
/// always-focused search input captures Left/Right itself (to move its text
/// cursor), so without this they'd never reach `handle_key_event` at all.
/// Only forwards the ones that were actually captured, so nothing is ever
/// delivered twice.
fn captured_arrow_key(event: Event, status: Status, _window: window::Id) -> Option<Message> {
    if status != Status::Captured {
        return None;
    }
    match event {
        Event::Keyboard(
            event @ keyboard::Event::KeyPressed {
                key: key::Key::Named(key::Named::ArrowLeft | key::Named::ArrowRight),
                ..
            },
        ) => Some(Message::KeyEvent(event)),
        _ => None,
    }
}

impl RingboardApp {
    /// Initialize the model and spawn the background controller thread.
    pub fn boot(startup_token: Option<crate::startup::Token>) -> (Self, Task<Message>) {
        let (command_sender, command_receiver) = mpsc::channel();
        let (response_sender, response_receiver) = mpsc::sync_channel(8);
        let requests = command_sender;

        thread::spawn(move || {
            controller(&command_receiver, |m| {
                response_sender.send(m).map_err(|_| ())
            });
        });

        let daemon = env::var_os("RINGBOARD_NO_DAEMON").is_none();
        let stop = Arc::new(AtomicBool::new(false));
        let (wake_tx, wake_rx) = mpsc::sync_channel(1);
        if daemon {
            let stop = stop.clone();
            thread::spawn(move || {
                if let Err(e) =
                    crate::startup::maintain_single_instance(&stop, startup_token, move || {
                        let _ = wake_tx.send(());
                    })
                {
                    eprintln!("Single-instance background thread failed: {e}");
                }
            });
        }

        let state = State::new();
        let app = Self {
            requests,
            state,
            thumbnails: ImagePipeline::default(),
            detail_images: ImagePipeline::default(),
            daemon,
            stop,
            window_id: None,
        };

        let controller_stream = Task::stream(controller_messages(response_receiver));
        // Populates `window_id` as early as possible, ahead of the first
        // `window::Event` — needed for hide/show/focus.
        let mut tasks = vec![
            controller_stream,
            window::latest().map(Message::WindowIdResolved),
        ];
        if daemon {
            tasks.push(Task::stream(wake_messages(wake_rx)));
        }
        (app, Task::batch(tasks))
    }

    // `&self` is required to match `iced`'s `title` function-pointer signature.
    #[allow(clippy::unused_self)]
    pub fn title(&self) -> String {
        format!("Ringboard v{}", env!("CARGO_PKG_VERSION"))
    }

    /// The TEA update function: (Model, Msg) -> (Model, Cmd).
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Controller(msg) => self.handle_incoming_controller_message(msg),
            Message::KeyEvent(event) => self.handle_key_event(event),
            Message::WindowEvent(id, event) => self.handle_window_event(id, &event),
            Message::WindowIdResolved(id) => {
                if let Some(id) = id {
                    self.window_id.get_or_insert(id);
                }
                Task::none()
            }
            Message::WakeRequested => self.wake(),
            Message::ImageDecoded(id, kind, result) => self.handle_image_decoded(id, kind, result),
            Message::SearchChanged(query) => self.handle_search_changed(query),
            Message::SearchKindToggled => self.toggle_search_kind(),
            Message::TabSelected(tab) => self.select_tab(tab),
            Message::TabNext => self.cycle_tab(1),
            Message::TabPrev => self.cycle_tab(-1),
            Message::PinnedToggled => {
                self.state.ui.pinned_expanded = !self.state.ui.pinned_expanded;
                Task::none()
            }
            Message::EntryClicked(id) | Message::FavPaste(id) => {
                self.state.ui.input_active = false;
                self.paste(id)
            }
            Message::FavoriteToggled(id) => self.toggle_favorite(id),
            Message::DeleteEntry(id) => self.delete(id),
            Message::DetailRequested(id) => self.open_detail(id),
            Message::DetailClosed => {
                if let Some(id) = self.state.ui.details_requested {
                    self.detail_images.remove(id);
                }
                self.state.ui.details_requested = None;
                self.state.ui.detailed_entry = None;
                Task::none()
            }
            Message::SearchInputFocusRequested => {
                self.state.ui.input_active = true;
                operation::focus(crate::widgets::search_input_id())
            }
            Message::MoveFavoriteUp(id) => self.move_favorite_up(id),
            Message::MoveFavoriteDown(id) => self.move_favorite_down(id),

            Message::Refresh => self.refresh(),
            Message::DismissError => {
                self.state.ui.last_error = None;
                Task::none()
            }
            Message::EntryHovered(id) => {
                self.state.ui.hovered_id = id;
                Task::none()
            }
            Message::SettingsLoaded(result) => {
                match result {
                    Ok((main, favorites)) => {
                        self.state.settings.max_main_entries = main.to_string();
                        self.state.settings.max_favorite_entries = favorites.to_string();
                    }
                    Err(e) => self.state.settings.status = Some(Err(e)),
                }
                self.state.settings.loaded = true;
                Task::none()
            }
            Message::SettingsMaxMainChanged(value) => {
                self.state.settings.max_main_entries = value;
                Task::none()
            }
            Message::SettingsMaxFavoritesChanged(value) => {
                self.state.settings.max_favorite_entries = value;
                Task::none()
            }
            Message::SettingsSaveRequested => self.save_settings(),
            Message::SettingsSaved(result) => {
                self.state.settings.saving = false;
                self.state.settings.status = Some(
                    result.map(|()| "Saved. Restart the Ringboard server to apply.".to_string()),
                );
                Task::none()
            }
            Message::SettingsGcBytesChanged(value) => {
                self.state.settings.gc_max_wasted_bytes = value;
                Task::none()
            }
            Message::SettingsGcRequested => self.run_gc(),
        }
    }

    /// The TEA view function: Model -> Html/Element.
    pub fn view(&self) -> Element<'_, Message> {
        crate::widgets::main_view(self)
    }

    /// The TEA subscriptions: Model -> Subscriptions.
    // `&self` is required to match `iced`'s `subscription` function-pointer signature.
    #[allow(clippy::unused_self)]
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            keyboard::listen().map(Message::KeyEvent),
            window::events().map(|(id, event)| Message::WindowEvent(id, event)),
            iced::event::listen_with(captured_arrow_key),
        ])
    }

    // ------------------------------------------------------------------
    // Query / read-only helpers (safe to call from view)
    // ------------------------------------------------------------------

    /// Return the entries currently visible based on search state.
    pub fn active_entries(&self) -> &[UiEntry] {
        if self.state.ui.query.is_empty() {
            &self.state.entries.loaded_entries
        } else {
            &self.state.entries.search_results
        }
    }

    /// Whether the entry currently passes the active tab's filter.
    fn tab_matches(&self, e: &UiEntry) -> bool {
        match self.state.ui.active_tab {
            ActiveTab::All => true,
            ActiveTab::Text => matches!(
                e.cache,
                UiEntryCache::Text { .. } | UiEntryCache::HighlightedText { .. }
            ),
            ActiveTab::Images => matches!(e.cache, UiEntryCache::Image),
            ActiveTab::Favorites => e.entry.ring() == RingKind::Favorites,
            ActiveTab::Settings => false,
        }
    }

    /// Return entries filtered by the active tab.
    pub fn filtered_entries(&self) -> Vec<&UiEntry> {
        self.active_entries()
            .iter()
            .filter(|e| self.tab_matches(e))
            .collect()
    }

    /// Like `!filtered_entries().is_empty()`, without building the list.
    pub fn has_visible_entries(&self) -> bool {
        self.active_entries().iter().any(|e| self.tab_matches(e))
    }

    /// Whether the list is split into Favorites/Recent sections. Only the
    /// unfiltered All tab has sections; searches and the other tabs render
    /// one flat list. Every consumer of the section split (rendering,
    /// keyboard navigation, scroll positioning, status counts) must go
    /// through this and [`Self::partitioned_entries`] so they can't diverge.
    pub fn show_sections(&self) -> bool {
        self.state.ui.query.is_empty() && self.state.ui.active_tab == ActiveTab::All
    }

    /// Splits the filtered list into (pinned favorites, unpinned recents).
    pub fn partitioned_entries(&self) -> (Vec<&UiEntry>, Vec<&UiEntry>) {
        self.filtered_entries()
            .into_iter()
            .partition(|e| e.entry.ring() == RingKind::Favorites)
    }

    /// Looks an entry up by id across the loaded and search lists.
    fn find_entry(&self, id: u64) -> Option<&UiEntry> {
        self.state
            .entries
            .loaded_entries
            .iter()
            .chain(self.state.entries.search_results.iter())
            .find(|e| e.entry.id() == id)
    }

    /// Navigation order: pinned first (if shown), then recent/filtered.
    pub fn nav_entries(&self) -> Vec<&UiEntry> {
        if self.show_sections() {
            let (mut pinned, mut unpinned) = self.partitioned_entries();
            pinned.append(&mut unpinned);
            pinned
        } else {
            self.filtered_entries()
        }
    }

    pub const fn current_highlight_id(&self) -> Option<u64> {
        if self.state.ui.query.is_empty() {
            self.state.ui.highlighted_id
        } else {
            self.state.ui.search_highlighted_id
        }
    }

    // ------------------------------------------------------------------
    // Commands / side effects (only called from update)
    // ------------------------------------------------------------------

    fn paste(&mut self, id: u64) -> Task<Message> {
        self.state.ui.pending_search_token.take();
        let _ = self.requests.send(Command::Paste(id));
        Task::none()
    }

    fn paste_text(&mut self, id: u64) -> Task<Message> {
        self.state.ui.pending_search_token.take();
        let _ = self.requests.send(Command::PasteText(id));
        Task::none()
    }

    fn move_favorite_up(&mut self, id: u64) -> Task<Message> {
        let (pinned, _) = self.partitioned_entries();
        if let Some(pos) = pinned.iter().position(|e| e.entry.id() == id)
            && pos > 0
        {
            let prev_id = pinned[pos - 1].entry.id();
            let _ = self.requests.send(Command::Swap(id, prev_id));
            self.state.ui.highlighted_id = Some(id);
            return self.refresh_entries();
        }
        Task::none()
    }

    fn move_favorite_down(&mut self, id: u64) -> Task<Message> {
        let (pinned, _) = self.partitioned_entries();
        if let Some(pos) = pinned.iter().position(|e| e.entry.id() == id)
            && pos + 1 < pinned.len()
        {
            let next_id = pinned[pos + 1].entry.id();
            let _ = self.requests.send(Command::Swap(id, next_id));
            self.state.ui.highlighted_id = Some(id);
            return self.refresh_entries();
        }
        Task::none()
    }

    fn toggle_favorite(&mut self, id: u64) -> Task<Message> {
        let cmd = {
            let entry = self.find_entry(id);
            match entry.map(|e| e.entry.ring()) {
                Some(RingKind::Favorites) => Command::Unfavorite(id),
                _ => Command::Favorite(id),
            }
        };
        let _ = self.requests.send(cmd);
        self.refresh_entries()
    }

    fn delete(&mut self, id: u64) -> Task<Message> {
        let _ = self.requests.send(Command::Delete(id));
        if self.state.ui.query.is_empty() {
            self.state.ui.highlighted_id = None;
        } else {
            self.state.ui.search_highlighted_id = None;
        }
        self.refresh_entries()
    }

    fn open_detail(&mut self, id: u64) -> Task<Message> {
        if self.state.ui.details_requested != Some(id) {
            self.state.ui.details_requested = Some(id);
            self.state.ui.detailed_entry = None;
            let entry = self.find_entry(id);
            let has_text = entry.is_some_and(|e| e.cache.is_text());
            let is_image = entry.is_some_and(|e| matches!(e.cache, UiEntryCache::Image));
            let _ = self.requests.send(Command::GetDetails {
                id,
                with_text: has_text,
            });
            if is_image {
                self.detail_images.request(id, &self.requests);
            }
            return self.scroll_to_entry(id);
        }
        Task::none()
    }

    fn handle_window_event(&mut self, id: window::Id, event: &window::Event) -> Task<Message> {
        self.window_id.get_or_insert(id);
        match event {
            window::Event::Focused => Task::none(),
            // `window::gain_focus` is a no-op under Wayland (winit has no
            // xdg_activation-token plumbing for it), so a `toggle` invocation
            // can only reliably bring the window back via the minimize ->
            // unminimize transition, which compositors do focus as a side
            // effect. Auto-hiding on focus loss guarantees the window is
            // always either focused or truly minimized, so it never gets
            // stuck visible-but-unfocused where toggle can't recover it.
            window::Event::Unfocused if self.daemon => self.hide_window(id),
            window::Event::CloseRequested => {
                if self.daemon {
                    self.hide_window(id)
                } else {
                    self.exit()
                }
            }
            _ => Task::none(),
        }
    }

    /// Hides the window instead of exiting, if running as a background
    /// daemon; otherwise exits the process for real. Used for Escape and
    /// after a successful paste (the window's native close button is
    /// handled directly in `handle_window_event`, using the id the
    /// `CloseRequested` event itself carries).
    fn close_or_hide(&mut self) -> Task<Message> {
        let Some(id) = self.window_id.filter(|_| self.daemon) else {
            return self.exit();
        };
        self.hide_window(id)
    }

    fn hide_window(&mut self, id: window::Id) -> Task<Message> {
        self.state.reset();
        self.thumbnails.clear();
        self.detail_images.clear();
        // Minimizing is respected far more consistently across window
        // managers than `Mode::Hidden` (e.g. GNOME/mutter's client-side
        // decoration frame doesn't reliably follow `set_visible(false)`).
        window::minimize(id, true)
    }

    fn exit(&self) -> Task<Message> {
        self.stop.store(true, Ordering::Relaxed);
        crate::startup::cleanup();
        std::process::exit(0);
    }

    /// Called when another `toggle` invocation asked us to wake up.
    fn wake(&mut self) -> Task<Message> {
        let Some(id) = self.window_id else {
            return Task::none();
        };
        Task::batch([
            window::minimize(id, false),
            window::gain_focus(id),
            self.refresh_entries(),
        ])
    }

    fn toggle_detail(&self) -> Task<Message> {
        if let Some(id) = self.current_highlight_id() {
            if self.state.ui.details_requested == Some(id) {
                return Task::done(Message::DetailClosed);
            }
            return Task::done(Message::DetailRequested(id));
        }
        Task::none()
    }

    fn select_tab(&mut self, tab: ActiveTab) -> Task<Message> {
        self.state.ui.active_tab = tab;
        self.state.ui.highlighted_id = None;
        self.state.ui.search_highlighted_id = None;
        if tab == ActiveTab::Settings && !self.state.settings.loaded {
            return Task::perform(load_server_config_async(), Message::SettingsLoaded);
        }
        Task::none()
    }

    fn save_settings(&mut self) -> Task<Message> {
        let Ok(max_main) = self.state.settings.max_main_entries.trim().parse() else {
            self.state.settings.status =
                Some(Err("Max main entries must be a positive number".into()));
            return Task::none();
        };
        let Ok(max_favorites) = self.state.settings.max_favorite_entries.trim().parse() else {
            self.state.settings.status =
                Some(Err("Max favorite entries must be a positive number".into()));
            return Task::none();
        };

        self.state.settings.saving = true;
        self.state.settings.status = None;
        Task::perform(
            save_server_config_async(max_main, max_favorites),
            Message::SettingsSaved,
        )
    }

    fn run_gc(&mut self) -> Task<Message> {
        let Ok(max_wasted_bytes) = self.state.settings.gc_max_wasted_bytes.trim().parse() else {
            self.state.settings.status =
                Some(Err("Max wasted bytes must be a non-negative number".into()));
            return Task::none();
        };

        self.state.settings.running_gc = true;
        self.state.settings.status = None;
        let _ = self
            .requests
            .send(Command::GarbageCollect { max_wasted_bytes });
        Task::none()
    }

    fn cycle_tab(&mut self, delta: i8) -> Task<Message> {
        let tabs = ActiveTab::ALL;
        let current = tabs
            .iter()
            .position(|t| *t == self.state.ui.active_tab)
            .unwrap_or(0);
        let len = isize::try_from(tabs.len()).unwrap_or(0);
        let current = isize::try_from(current).unwrap_or(0);
        let next = (current + isize::from(delta)).rem_euclid(len);
        let next = usize::try_from(next).unwrap_or(0);
        self.select_tab(tabs[next])
    }

    fn toggle_search_kind(&mut self) -> Task<Message> {
        self.state.ui.search_kind = match self.state.ui.search_kind {
            SearchKind::Plain => SearchKind::Regex,
            SearchKind::Regex => SearchKind::Mime,
            SearchKind::Mime => SearchKind::Plain,
        };
        if self.state.ui.query.is_empty() {
            Task::none()
        } else {
            self.send_search()
        }
    }

    fn refresh(&mut self) -> Task<Message> {
        self.state.ui.last_error.take();
        self.state.ui.highlighted_id = None;
        self.state.ui.search_highlighted_id = None;
        self.thumbnails.clear();
        self.detail_images.clear();
        self.refresh_entries()
    }

    fn refresh_entries(&mut self) -> Task<Message> {
        self.state.ui.last_error.take();
        let _ = self.requests.send(Command::LoadFirstPage);
        if self.state.ui.query.is_empty() {
            Task::none()
        } else {
            self.send_search()
        }
    }

    fn send_search(&mut self) -> Task<Message> {
        let (source, sink) = cancellation_token();
        let _ = self.requests.send(Command::Search {
            query: self.state.ui.query.clone().into(),
            kind: self.state.ui.search_kind,
            token: source,
        });
        self.state.ui.pending_search_token = Some(sink);
        Task::none()
    }

    fn handle_search_changed(&mut self, query: String) -> Task<Message> {
        self.state.ui.last_error = None;
        if query.is_empty() {
            self.state.ui.query = String::new();
            self.state.entries.search_results = Box::default();
            self.state.ui.search_highlighted_id = None;
            self.state.ui.pending_search_token = None;
            return Task::none();
        }
        self.state.ui.query = query;
        self.send_search()
    }

    fn handle_image_decoded(
        &mut self,
        id: u64,
        kind: ImageKind,
        result: Result<image_crate::DynamicImage, String>,
    ) -> Task<Message> {
        match result {
            Ok(img) => {
                let img = match kind {
                    // `thumbnail` preserves aspect ratio (with a >= 1px
                    // floor) and never upscales, so no manual ratio math.
                    ImageKind::Thumbnail => img.thumbnail(320, u32::MAX),
                    // Kept at full resolution: only ever decoded for the
                    // entry currently open in the detail panel.
                    ImageKind::Detail => img,
                };
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let handle = image::Handle::from_rgba(w, h, rgba.into_raw());
                match kind {
                    ImageKind::Thumbnail => self.thumbnails.insert(id, handle),
                    ImageKind::Detail => {
                        self.detail_images.insert(id, handle);
                        if self.state.ui.details_requested == Some(id) {
                            // Aspect ratio may differ from the thumbnail
                            // fallback; keep the row in view after it resizes.
                            return self.scroll_to_entry(id);
                        }
                    }
                }
            }
            Err(e) => {
                self.state.ui.last_error = Some(CommandError::Core(CoreError::Io {
                    error: io::Error::other(e),
                    context: "image decode".into(),
                }));
            }
        }
        Task::none()
    }

    // ------------------------------------------------------------------
    // Controller message handling
    // ------------------------------------------------------------------

    fn handle_incoming_controller_message(&mut self, msg: Arc<ControllerMessage>) -> Task<Message> {
        let Some(msg) = Arc::into_inner(msg) else {
            return Task::none();
        };
        if let ControllerMessage::LoadedImage { id, image } = msg {
            // Route this response to whichever pipeline is waiting on it
            // (both request via the same `Command::LoadImage`).
            let kind = if self.detail_images.claim(id) {
                ImageKind::Detail
            } else if self.thumbnails.claim(id) {
                ImageKind::Thumbnail
            } else {
                return Task::none();
            };
            return Task::perform(decode_image_async(id, image), move |(id, result)| {
                Message::ImageDecoded(id, kind, result)
            });
        }
        self.handle_controller_message(msg)
    }

    fn handle_controller_message(&mut self, msg: ControllerMessage) -> Task<Message> {
        match msg {
            ControllerMessage::FatalDbOpen(e) => {
                self.state.ui.fatal_error = Some(ClientError::Core(e));
                Task::none()
            }
            ControllerMessage::Error(e) => {
                self.state.settings.running_gc = false;
                self.state
                    .ui
                    .pending_search_token
                    .take_if(|token| token.is_done());
                self.state.ui.last_error = Some(e);
                Task::none()
            }
            ControllerMessage::LoadedFirstPage {
                entries: new_entries,
                default_focused_id,
            } => {
                if self.state.ui.highlighted_id.is_none() {
                    self.state.ui.highlighted_id =
                        default_focused_id.or_else(|| new_entries.first().map(|e| e.entry.id()));
                }
                self.request_images(&new_entries);
                self.state.entries.loaded_entries = new_entries;
                self.prune_image_caches();
                Task::none()
            }
            ControllerMessage::EntryDetails { id, result } => {
                if self.state.ui.details_requested == Some(id) {
                    self.state.ui.detailed_entry = result.ok();
                    // The row likely just grew (one-liner -> full text); keep
                    // it in view instead of letting it drift off-screen.
                    return self.scroll_to_entry(id);
                }
                Task::none()
            }
            ControllerMessage::SearchResults(new_entries) => {
                // No pending token means the search was cancelled (Escape or a
                // cleared query); applying its late results would resurface the
                // old query's hits — and top-hit highlight — under whatever the
                // user types next.
                if self.state.ui.pending_search_token.is_none() {
                    return Task::none();
                }
                self.state
                    .ui
                    .pending_search_token
                    .take_if(|token| token.is_done());
                self.state.ui.search_highlighted_id = new_entries.first().map(|e| e.entry.id());
                self.request_images(&new_entries);
                self.state.entries.search_results = new_entries;
                self.prune_image_caches();
                Task::none()
            }
            ControllerMessage::FavoriteChange(id) => {
                if self.state.ui.query.is_empty() {
                    self.state.ui.highlighted_id = Some(id);
                } else {
                    self.state.ui.search_highlighted_id = Some(id);
                }
                self.thumbnails.remove(id);
                self.refresh_entries()
            }
            ControllerMessage::Deleted(id) => {
                // Composite ids (ring + index) are reused once a slot is freed;
                // a cached thumbnail left under this id would be shown for
                // whatever entry lands in the slot next.
                self.thumbnails.remove(id);
                self.detail_images.remove(id);
                self.refresh_entries()
            }
            ControllerMessage::LoadedImage { .. } => Task::none(),
            ControllerMessage::Pasted => self.close_or_hide(),
            ControllerMessage::Swapped => self.refresh_entries(),
            ControllerMessage::GarbageCollected { bytes_freed } => {
                self.state.settings.running_gc = false;
                self.state.settings.status = Some(Ok(format!("Freed {bytes_freed} bytes.")));
                self.refresh_entries()
            }
        }
    }

    // ------------------------------------------------------------------
    // Keyboard handling
    // ------------------------------------------------------------------

    fn handle_key_event(&mut self, event: keyboard::Event) -> Task<Message> {
        match event {
            keyboard::Event::ModifiersChanged(modifiers) => {
                self.state.ui.ctrl_held = modifiers.control();
                Task::none()
            }
            keyboard::Event::KeyReleased { modifiers, .. } => {
                self.state.ui.ctrl_held = modifiers.control();
                Task::none()
            }
            keyboard::Event::KeyPressed { key, modifiers, .. } => {
                self.handle_key_pressed(key, modifiers)
            }
        }
    }

    fn handle_key_pressed(
        &mut self,
        key: keyboard::Key,
        modifiers: keyboard::Modifiers,
    ) -> Task<Message> {
        let current_id = self.current_highlight_id();
        let mut new_id = current_id;
        let mut set_pinned_expanded: Option<bool> = None;

        match &key {
            key::Key::Named(key::Named::Enter) => {
                if self.state.ui.input_active {
                    return Task::none();
                }
                if let Some(id) = current_id {
                    return self.paste(id);
                }
                return Task::none();
            }
            key::Key::Named(key::Named::Escape) => {
                if self.state.ui.input_active {
                    self.state.ui.input_active = false;
                    self.state.ui.query = String::new();
                    self.state.entries.search_results = Box::default();
                    self.state.ui.highlighted_id = self.nav_entries().first().map(|e| e.entry.id());
                    self.state.ui.search_highlighted_id = None;
                    self.state.ui.last_error = None;
                    self.state.ui.pending_search_token = None;
                    return Task::none();
                }
                if self.state.ui.details_requested.is_some() {
                    return Task::done(Message::DetailClosed);
                }
                if !self.state.ui.query.is_empty() {
                    self.state.ui.query = String::new();
                    self.state.entries.search_results = Box::default();
                    self.state.ui.highlighted_id = None;
                    self.state.ui.search_highlighted_id = None;
                    self.state.ui.last_error = None;
                    self.state.ui.pending_search_token = None;
                    return Task::none();
                }
                return self.close_or_hide();
            }
            key::Key::Named(key::Named::ArrowDown) if modifiers.control() && modifiers.shift() => {
                if let Some(id) = current_id {
                    return self.move_favorite_down(id);
                }
                return Task::none();
            }
            key::Key::Named(key::Named::ArrowUp) if modifiers.control() && modifiers.shift() => {
                if let Some(id) = current_id {
                    return self.move_favorite_up(id);
                }
                return Task::none();
            }
            key::Key::Named(key::Named::ArrowDown) if !modifiers.control() => {
                if self.state.ui.input_active {
                    self.state.ui.input_active = false;
                }
                let nav = self.nav_entries();
                let show_sections = self.show_sections();
                new_id = Self::next_id(&nav, current_id);
                if show_sections && !self.state.ui.pinned_expanded && !new_id.is_none() {
                    let (pinned, _unpinned) = self.partitioned_entries();
                    if new_id.is_some_and(|id| pinned.iter().any(|e| e.entry.id() == id)) {
                        set_pinned_expanded = Some(true);
                    }
                }
                // If wrapped from last entry back to first, focus the search bar instead
                if !modifiers.control() && self.state.ui.query.is_empty() {
                    let nav = self.nav_entries();
                    if current_id.is_some()
                        && new_id.is_some()
                        && new_id == nav.first().map(|e| e.entry.id())
                        && current_id != nav.first().map(|e| e.entry.id())
                    {
                        self.state.ui.input_active = true;
                        return operation::focus(crate::widgets::search_input_id());
                    }
                }
            }
            key::Key::Named(key::Named::ArrowUp) if !modifiers.control() => {
                let nav = self.nav_entries();
                let show_sections = self.show_sections();
                new_id = Self::prev_id(&nav, current_id);
                if show_sections && !self.state.ui.pinned_expanded && !new_id.is_none() {
                    let (pinned, _unpinned) = self.partitioned_entries();
                    if new_id.is_some_and(|id| pinned.iter().any(|e| e.entry.id() == id)) {
                        set_pinned_expanded = Some(true);
                    }
                }
                // If at the first entry, focus the search input instead
                if !modifiers.control() && self.state.ui.query.is_empty() {
                    let nav = self.nav_entries();
                    if current_id.is_some() && nav.first().map(|e| e.entry.id()) == current_id {
                        self.state.ui.input_active = true;
                        return operation::focus(crate::widgets::search_input_id());
                    }
                }
            }
            key::Key::Named(key::Named::ArrowLeft) => {
                let show_sections = self.show_sections();
                let (pinned, unpinned) = if show_sections {
                    self.partitioned_entries()
                } else {
                    (Vec::new(), Vec::new())
                };
                let on_pinned_entry =
                    current_id.is_some_and(|id| pinned.iter().any(|e| e.entry.id() == id));
                if show_sections && !pinned.is_empty() && on_pinned_entry {
                    set_pinned_expanded = Some(false);
                    new_id = unpinned.first().map(|e| e.entry.id());
                } else if self.state.ui.query.is_empty()
                    && let Some(id) = current_id
                    && self.state.ui.details_requested == Some(id)
                {
                    // Only when there's no query: with text in the search box,
                    // Left/Right arrive here too (forwarded after the input
                    // moves its cursor) and must not double as detail toggles.
                    return Task::done(Message::DetailClosed);
                }
            }
            key::Key::Named(key::Named::ArrowRight) => {
                let show_sections = self.show_sections();
                let (pinned, _unpinned) = if show_sections {
                    self.partitioned_entries()
                } else {
                    (Vec::new(), Vec::new())
                };
                let on_collapsed_pinned_entry = !self.state.ui.pinned_expanded
                    && current_id.is_some_and(|id| pinned.iter().any(|e| e.entry.id() == id));
                if show_sections && !pinned.is_empty() && on_collapsed_pinned_entry {
                    set_pinned_expanded = Some(true);
                } else if self.state.ui.query.is_empty()
                    && let Some(id) = current_id
                    && self.state.ui.details_requested != Some(id)
                    && self.entry_has_extra_detail(id)
                {
                    return Task::done(Message::DetailRequested(id));
                }
            }
            key::Key::Named(key::Named::Tab) if modifiers.control() => {
                return if modifiers.shift() {
                    Task::done(Message::TabPrev)
                } else {
                    Task::done(Message::TabNext)
                };
            }
            key::Key::Character(c) => {
                let s = c.as_str();

                // Ctrl+V paste focused entry (only when input is NOT active)
                if modifiers.control()
                    && !modifiers.shift()
                    && s.eq_ignore_ascii_case("v")
                    && !self.state.ui.input_active
                    && let Some(id) = self.current_highlight_id()
                {
                    return self.paste(id);
                }
                // Ctrl+Shift+V text-mode paste focused entry (only when input is NOT active)
                if modifiers.control()
                    && modifiers.shift()
                    && s.eq_ignore_ascii_case("v")
                    && !self.state.ui.input_active
                    && let Some(id) = self.current_highlight_id()
                {
                    return self.paste_text(id);
                }

                if modifiers.control() {
                    if s.eq_ignore_ascii_case("r") {
                        return Task::done(Message::Refresh);
                    }
                    if s.eq_ignore_ascii_case("d") {
                        return self.toggle_detail();
                    }
                    if let Some(digit) = s.chars().next().and_then(|c| c.to_digit(10)) {
                        let idx = digit as usize;
                        if modifiers.shift() {
                            // Ctrl+Shift+digit: paste favorite
                            let (pinned, _) = self.partitioned_entries();
                            if let Some(entry) = pinned.get(idx) {
                                return self.paste(entry.entry.id());
                            }
                        } else {
                            // Ctrl+digit: paste recent (nav order)
                            let nav = self.nav_entries();
                            if let Some(entry) = nav.get(idx) {
                                return self.paste(entry.entry.id());
                            }
                        }
                    }
                    return Task::none();
                }

                if modifiers.alt() {
                    if s.eq_ignore_ascii_case("x") || s.eq_ignore_ascii_case("m") {
                        return self.toggle_search_kind();
                    }
                    if let Some(digit) = s.chars().next().and_then(|c| c.to_digit(10))
                        && (1..=u32::try_from(ActiveTab::ALL.len()).unwrap_or(u32::MAX))
                            .contains(&digit)
                    {
                        return Task::done(Message::TabSelected(
                            ActiveTab::ALL[digit as usize - 1],
                        ));
                    }
                    return Task::none();
                }

                // Typing a character when input is inactive: activate search and focus it
                if !modifiers.control() && !modifiers.alt() && !self.state.ui.input_active {
                    self.state.ui.input_active = true;
                    self.state.ui.query.push_str(s);
                    // Trigger search for the new query
                    let task = self.send_search();
                    return Task::batch([
                        task,
                        operation::focus(crate::widgets::search_input_id()),
                    ]);
                }

                return Task::none();
            }
            _ => return Task::none(),
        }

        // Deactivate search input when navigating to an entry
        if new_id.is_some() && new_id != current_id {
            self.state.ui.input_active = false;
        }

        // Apply navigation changes.
        if self.state.ui.query.is_empty() {
            self.state.ui.highlighted_id = new_id;
        } else {
            self.state.ui.search_highlighted_id = new_id;
        }
        if let Some(expanded) = set_pinned_expanded {
            self.state.ui.pinned_expanded = expanded;
        }

        if new_id.is_some() && new_id != current_id {
            self.scroll_to_highlighted()
        } else {
            Task::none()
        }
    }

    fn entry_has_extra_detail(&self, id: u64) -> bool {
        self.find_entry(id)
            .is_some_and(crate::widgets::entry_has_extra_detail)
    }

    /// Keeps the highlighted entry roughly in view after keyboard
    /// navigation.
    fn scroll_to_highlighted(&self) -> Task<Message> {
        let Some(id) = self.current_highlight_id() else {
            return Task::none();
        };
        self.scroll_to_entry(id)
    }

    /// A rough relative weight for how tall a row's rendered height is,
    /// used to convert its position in the list into a scroll fraction.
    /// Rows aren't uniform height (collapsed text/image previews vs. an
    /// expanded detail panel), so a plain `index / count` fraction badly
    /// misjudges where a big expanded row (e.g. a detail image, capped at
    /// ~400px vs. a normal ~40px row) actually sits, and can scroll it
    /// clean out of view.
    fn row_weight(&self, entry: &UiEntry) -> f32 {
        let expanded = self.state.ui.details_requested == Some(entry.entry.id());
        match entry.cache {
            UiEntryCache::Image if expanded => 10.0,
            UiEntryCache::Text { .. } | UiEntryCache::HighlightedText { .. } if expanded => 8.0,
            UiEntryCache::Image => 1.6,
            _ => 1.0,
        }
    }

    /// Keeps a specific entry roughly in view, e.g. after it's expanded (or
    /// its expanded content resizes) rather than only on keyboard
    /// navigation. See [`Self::row_weight`] for why this is weighted
    /// instead of a plain index fraction.
    fn scroll_to_entry(&self, id: u64) -> Task<Message> {
        let render_order: Vec<&UiEntry> = if self.show_sections() {
            let (mut pinned, mut unpinned) = self.partitioned_entries();
            if self.state.ui.pinned_expanded {
                pinned.append(&mut unpinned);
                pinned
            } else {
                unpinned
            }
        } else {
            self.filtered_entries()
        };

        let Some(idx) = render_order.iter().position(|e| e.entry.id() == id) else {
            return Task::none();
        };

        let weights: Vec<f32> = render_order.iter().map(|e| self.row_weight(e)).collect();
        let total: f32 = weights.iter().sum();
        let before: f32 = weights[..idx].iter().sum();
        let fraction = if total <= f32::EPSILON {
            0.0
        } else {
            (before / total).clamp(0.0, 1.0)
        };

        operation::snap_to(
            crate::widgets::entry_list_id(),
            operation::RelativeOffset {
                x: 0.0,
                y: fraction,
            },
        )
    }

    fn request_images(&mut self, entries: &[UiEntry]) {
        for entry in entries {
            if matches!(entry.cache, UiEntryCache::Image) {
                self.thumbnails.request(entry.entry.id(), &self.requests);
            }
        }
    }

    /// Drops cached thumbnails for entries no longer in the loaded or search
    /// lists. Ids are reused once ring slots are freed, so a stale cache hit
    /// would display the previous entry's image for new content — and without
    /// pruning the cache grows with every entry ever seen.
    fn prune_image_caches(&mut self) {
        let live: std::collections::HashSet<u64> = self
            .state
            .entries
            .loaded_entries
            .iter()
            .chain(self.state.entries.search_results.iter())
            .map(|e| e.entry.id())
            .collect();
        self.thumbnails.retain(&live);
    }

    fn next_id(nav: &[&UiEntry], current_id: Option<u64>) -> Option<u64> {
        current_id.map_or_else(
            || nav.first().map(|e| e.entry.id()),
            |id| {
                let idx = nav.iter().position(|e| e.entry.id() == id);
                if idx == Some(nav.len().saturating_sub(1)) || idx.is_none() {
                    nav.first().map(|e| e.entry.id())
                } else {
                    idx.and_then(|i| i.checked_add(1))
                        .and_then(|i| nav.get(i))
                        .map(|e| e.entry.id())
                }
            },
        )
    }

    fn prev_id(nav: &[&UiEntry], current_id: Option<u64>) -> Option<u64> {
        current_id.map_or_else(
            || nav.last().map(|e| e.entry.id()),
            |id| {
                let idx = nav.iter().position(|e| e.entry.id() == id);
                if idx == Some(0) || idx.is_none() {
                    nav.last().map(|e| e.entry.id())
                } else {
                    idx.and_then(|i| i.checked_sub(1))
                        .and_then(|i| nav.get(i))
                        .map(|e| e.entry.id())
                }
            },
        )
    }
}
