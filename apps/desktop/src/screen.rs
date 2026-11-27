//! The Perform screen's container: owns the session of the song or setlist
//! that plays, polls it on a timer, turns pointer and key events into
//! view-model actions, and hands a [`PerformView`] (and, for a setlist, a
//! [`openvamp_desktop::strip::SetlistView`]) to the presentational code in
//! [`crate::ui`], with the song's [`SongFacts`] read from its `song.json`
//! on a background thread ([`openvamp_desktop::facts`]).

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use app::{AudioOutput, SessionOptions};
use domain::SetlistItem;
use gpui::{
    AnyWindowHandle, App, AppContext, Context, FocusHandle, KeyDownEvent, KeyUpEvent, Modifiers,
    ModifiersChangedEvent, PathPromptOptions, PromptButton, PromptLevel, Render, ScrollHandle,
    Subscription, Task, Window, prelude::*,
};
use io_cpal::CpalOutput;
use library::ConfigStore;
use openvamp_desktop::action::{Intent, Page, UiAction, disarms_removal};
use openvamp_desktop::audio_settings::AudioSettings;
use openvamp_desktop::ending::{Dialog, EndRequest, Ending, control_acts, end_request};
use openvamp_desktop::facts::{self, FactsCache};
use openvamp_desktop::hold::PanicHold;
use openvamp_desktop::keys::{HeldKeys, KeyAction, KeyPress, Mods, Typed, is_panic_key, typed};
use openvamp_desktop::library_page::{Dragged, LibraryPage, Opened, library_to_open};
use openvamp_desktop::mixer::{MixerAction, MixerPage, UnsavedMix, restore_mix, unsaved_mix};
use openvamp_desktop::output_page::{OutputAction, OutputPage};
use openvamp_desktop::reorder::Half;
use openvamp_desktop::show::{
    LoadedSetlist, OpenFailure, Prepared, Replaced, Show, Source, close, reopen_previous,
    replace_prepared,
};
use openvamp_desktop::song_import::STOP_PATIENCE;
use openvamp_desktop::tick::Timed;
use openvamp_desktop::view::{PerformView, SongFacts};

use crate::library_ui::{DraggedItem, DraggedSetlist};
use crate::ui::DraggedTile;
use crate::{library_ui, mixer_ui, output_ui, song_editor_ui, ui};

/// How often the session is polled: about 30 Hz (Decision 3). The timer
/// keeps running while the window is hidden, since a waiting play starts
/// only through `Session::poll` (L16). A poll asks for a frame while the
/// song moves or a meter reads above silence
/// ([`openvamp_desktop::mixer::needs_frames`]), else only when it changed
/// what the screens show ([`Timed::turn`]).
const POLL_EVERY: Duration = Duration::from_millis(33);

/// The longest a quit waits for a song import's save, cancelled, to report
/// that it stopped ([`Perform::end_once_saved`]).
const QUIT_PATIENCE: Duration = Duration::from_secs(10);

/// The Perform screen.
pub struct Perform {
    focus: FocusHandle,
    loaded: Option<Show>,
    /// Problems to show, one line each: why the last pick did not open (the
    /// song that was playing goes on), or how the song it replaced closed.
    problems: Vec<String>,
    hold: PanicHold,
    keys: HeldKeys,
    /// Whether the question before ending the show is on screen.
    asking: bool,
    /// The screen of the side navigation on show.
    page: Page,
    /// The library folder the Library screen shows, once one is open.
    library: Option<LibraryPage>,
    /// How [`Self::library`] was opened, which decides whether it follows
    /// the setlist on Perform.
    opened: Opened,
    /// Why the last library folder picked did not open.
    library_problem: Option<String>,
    /// How the show ends once the save it cancelled has stopped
    /// ([`Self::end_once_saved`]), and the window it ends in.
    ending_after_save: Option<(Ending, AnyWindowHandle)>,
    /// Where the Library screen's song list is scrolled, so a song just
    /// imported scrolls into view.
    songs_scroll: ScrollHandle,
    /// The saved routings and bus settings every session opens with.
    settings: AudioSettings,
    /// The Mixer screen's own state.
    mixer: MixerPage,
    /// The Audio output screen's own state.
    output: OutputPage,
    /// The facts of the song folders read, which the song header and the
    /// bars on Perform show; never read while drawing.
    facts: FactsCache,
    /// Counts the picks: only the newest one opens once it has prepared
    /// ([`Self::open`]).
    opening: u64,
    _poll: Task<()>,
    _quit: Subscription,
    _deactivated: Subscription,
    _blurred: Subscription,
}

impl Perform {
    /// The screen, with `path` open when one is given: a song folder, or a
    /// setlist file ([`Source::for_path`]).
    pub fn new(path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        let poll = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL_EVERY).await;
                if this.update(cx, |screen, cx| screen.tick(cx)).is_err() {
                    break;
                }
            }
        });
        // GPUI gives the future a quit handler returns 100 ms, but calls the
        // handler itself at once: a song being saved is cancelled here, and
        // the handler waits until the save has stopped and removed its
        // hidden folder ([`STOP_PATIENCE`] at most), so the process never
        // ends in the middle of a save. Quits the app can ask about first
        // come here once the save has stopped ([`Self::end_once_saved`]);
        // this covers the others (the Dock, logging out).
        let quit = cx.on_app_quit(|screen, _| {
            if let Some(library) = &mut screen.library
                && !library.stop_save(STOP_PATIENCE)
            {
                eprintln!(
                    "warning: the song being saved did not stop within {} s; a later import removes its hidden folder from the library",
                    STOP_PATIENCE.as_secs()
                );
            }
            screen.close_song();
            async {}
        });
        // A release that happens while the window is inactive, or while
        // something else has the focus, never reaches the screen.
        let deactivated = cx.observe_window_activation(window, |screen, window, cx| {
            if !window.is_window_active() {
                screen.focus_lost(cx);
            }
        });
        let blurred = cx.on_blur(&focus, window, |screen, _, cx| screen.focus_lost(cx));
        let mut screen = Self {
            focus,
            loaded: None,
            problems: Vec::new(),
            hold: PanicHold::new(),
            keys: HeldKeys::new(),
            asking: false,
            page: Page::Perform,
            library: None,
            opened: Opened::Followed,
            library_problem: None,
            ending_after_save: None,
            songs_scroll: ScrollHandle::new(),
            settings: AudioSettings::load(ConfigStore::from_environment()),
            mixer: MixerPage::new(),
            output: OutputPage::new(),
            facts: FactsCache::new(),
            opening: 0,
            _poll: poll,
            _quit: quit,
            _deactivated: deactivated,
            _blurred: blurred,
        };
        if let Some(path) = path {
            screen.open_path(&path, cx);
        }
        screen
    }

    /// Stops the song or setlist with its ramp, sending a Stop the player
    /// refuses again within [`openvamp_desktop::song::STOP_WAIT`], then
    /// closes the session and its device; what went wrong goes to stderr.
    pub fn close_song(&mut self) {
        // A pick still preparing never opens after the show ended.
        self.opening += 1;
        self.save_waiting_buses();
        if let Some(show) = self.loaded.take() {
            // A buffer size or routing waiting for the device is not kept.
            self.output.song_closed();
            if let Some(problem) = close(show, thread::sleep).problem() {
                eprintln!("warning: {problem}");
            }
        }
    }

    /// Quits or closes the window: at once when nothing plays and nothing
    /// is being saved (the song, if any, stops with its ramp first); while a
    /// song plays or is being saved, only once the person confirms
    /// ([`end_request`]), and after a save, only once it has stopped
    /// ([`Self::end_once_saved`]). Returns whether it ended at once, which
    /// lets the window close.
    fn end_show(&mut self, ending: Ending, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let transport = self.loaded.as_mut().map(|show| {
            show.poll();
            show.snapshot().transport
        });
        let saving = self.library.as_ref().is_some_and(LibraryPage::saving);
        match end_request(transport, saving, self.controls_wait()) {
            EndRequest::EndNow => {
                self.close_song();
                if ending == Ending::Quit {
                    cx.quit();
                }
                true
            }
            EndRequest::Ask => {
                self.ask_before_ending(ending, saving, window, cx);
                false
            }
            EndRequest::Wait => false,
        }
    }

    /// Whether the controls wait: the question before ending the show is
    /// on screen, or the app ends once the save it cancelled has stopped.
    fn controls_wait(&self) -> bool {
        self.asking || self.ending_after_save.is_some()
    }

    /// Ends the show as `ending` says, once nothing is being saved: the
    /// song stops at once, a save under way is cancelled, and the app quits
    /// (or the window closes) only when the save reports that it stopped
    /// and removed its hidden folder ([`Self::run_save`]), or after
    /// [`QUIT_PATIENCE`] at the latest, so a drive that hangs never keeps
    /// the app open; the quit handler then still waits for the save.
    fn end_once_saved(&mut self, ending: Ending, window: &mut Window, cx: &mut Context<Self>) {
        self.close_song();
        let Some(library) = self.library.as_mut().filter(|library| library.saving()) else {
            match ending {
                Ending::Quit => cx.quit(),
                Ending::CloseWindow => window.remove_window(),
            }
            return;
        };
        library.stop_save(Duration::ZERO);
        self.ending_after_save = Some((ending, window.window_handle()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(QUIT_PATIENCE).await;
            let _ = this.update(cx, |screen, cx| screen.finish_ending(cx));
        })
        .detach();
    }

    /// Carries out the ending [`Self::end_once_saved`] put off, if any.
    fn finish_ending(&mut self, cx: &mut Context<Self>) {
        let Some((ending, window)) = self.ending_after_save.take() else {
            return;
        };
        self.close_song();
        match ending {
            Ending::Quit => cx.quit(),
            Ending::CloseWindow => cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, _| window.remove_window());
            }),
        }
    }

    fn ask_before_ending(
        &mut self,
        ending: Ending,
        saving: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let question = if saving {
            ending.saving_question(Dialog::HERE)
        } else {
            ending.question(Dialog::HERE)
        };
        let buttons = question.answers.map(|answer| {
            if answer.keeps_playing {
                PromptButton::cancel(answer.label)
            } else {
                PromptButton::new(answer.label)
            }
        });
        let answer = window.prompt(
            PromptLevel::Warning,
            question.message,
            Some(question.detail),
            &buttons,
            cx,
        );
        self.asking = true;
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await.ok();
            let _ = this.update_in(cx, |screen, window, cx| {
                screen.asking = false;
                if question.ends(answer) {
                    screen.end_once_saved(ending, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens what `path` names ([`Source::for_path`]); one that cannot be
    /// opened says why.
    fn open_path(&mut self, path: &Path, cx: &mut Context<Self>) {
        match Source::for_path(path) {
            Ok(source) => self.open(&source, cx),
            Err(problem) => self.problems = vec![problem],
        }
    }

    /// Opens `source` in place of what plays ([`Self::open_with`]).
    fn open(&mut self, source: &Source, cx: &mut Context<Self>) {
        self.open_with(source, None, cx);
    }

    /// Opens `source` in place of what plays: it prepares on the background
    /// executor ([`Source::prepare`]) while what plays goes on, Panic
    /// included, since reading its files can wait seconds for a slow,
    /// sleeping or unplugged volume. Once it has, unless a newer pick came
    /// meanwhile, it takes the place of what plays ([`replace_prepared`]): a
    /// pick that does not open leaves the current song or setlist loaded and
    /// playing, and the reasons show on screen; otherwise what plays stops
    /// with its ramp and closes its device before the pick opens the
    /// device. `unsaved`, a mix not saved yet, plays again once it opens.
    fn open_with(&mut self, source: &Source, unsaved: Option<UnsavedMix>, cx: &mut Context<Self>) {
        // The saved routing of the device it opens on, and the bus settings,
        // with the last bus changes in them.
        self.save_waiting_buses();
        let listed = CpalOutput::new().devices();
        let (options, _) = self.settings.open_options(&listed);
        self.opening += 1;
        let ticket = self.opening;
        let pick = source.clone();
        let used = options.clone();
        self.prepare_then(
            ticket,
            source.clone(),
            options,
            cx,
            move |screen, prepared, cx| {
                screen.prepared(ticket, &pick, &used, prepared, unsaved, cx);
            },
        );
    }

    /// Prepares `source` with `options` on the background executor, then
    /// hands what it gave to `then` on the screen, unless a newer pick came
    /// meanwhile: what that pick left is dropped on the background
    /// executor, since dropping it waits for its disk thread.
    fn prepare_then(
        &mut self,
        ticket: u64,
        source: Source,
        options: SessionOptions,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Result<Prepared, OpenFailure>, &mut Context<Self>) + 'static,
    ) {
        let background = cx.background_executor().clone();
        let prepare = background.spawn(async move { source.prepare(&options) });
        cx.spawn(async move |this, cx| {
            let prepared = prepare.await;
            let _ = this.update(cx, |screen, cx| {
                if screen.opening == ticket {
                    then(screen, prepared, cx);
                } else {
                    background.spawn(async move { drop(prepared) }).detach();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The pick `source`, prepared with `used`, takes the place of what
    /// plays ([`replace_prepared`]). When the settings it opens with changed
    /// while it prepared (a device plugged in, a bus moved), it prepares
    /// again with them first. When its device does not start, the show it
    /// replaced prepares again and opens, stopped at the start.
    fn prepared(
        &mut self,
        ticket: u64,
        source: &Source,
        used: &SessionOptions,
        prepared: Result<Prepared, OpenFailure>,
        unsaved: Option<UnsavedMix>,
        cx: &mut Context<Self>,
    ) {
        self.save_waiting_buses();
        let listed = CpalOutput::new().devices();
        let (options, fallback) = self.settings.open_options(&listed);
        if !same_settings(&options, used) {
            cx.background_executor()
                .spawn(async move { drop(prepared) })
                .detach();
            self.open_with(source, unsaved, cx);
            return;
        }
        let replaced = prepared.and_then(|prepared| {
            replace_prepared(
                &mut self.loaded,
                source,
                prepared,
                Box::new(CpalOutput::new()),
                thread::sleep,
            )
        });
        let mut replaced = match replaced {
            Ok(replaced) => replaced,
            Err(error) => {
                self.problems = error.lines();
                self.problems
                    .insert(0, format!("Could not open {}:", source.describe()));
                self.notice_fallback(fallback);
                return;
            }
        };
        // What plays closed: a buffer size or routing waiting for its
        // device is not kept.
        self.output.song_closed();
        // Its song.json may have changed: read every folder again.
        self.facts.forget();
        self.hold = PanicHold::new();
        let Some(previous) = replaced.to_reopen().cloned() else {
            self.opened(&replaced, fallback, unsaved);
            return;
        };
        self.prepare_then(ticket, previous, options, cx, move |screen, prepared, _| {
            reopen_previous(&mut screen.loaded, &mut replaced, prepared, || {
                Box::new(CpalOutput::new())
            });
            screen.opened(&replaced, fallback, unsaved);
        });
    }

    /// A pick opened (or the show it replaced opened again): its problems
    /// show, and `unsaved` plays again when it is the same song.
    fn opened(
        &mut self,
        replaced: &Replaced,
        fallback: Option<String>,
        unsaved: Option<UnsavedMix>,
    ) {
        self.problems = replaced.problems();
        for problem in &self.problems {
            eprintln!("warning: {problem}");
        }
        self.notice_fallback(fallback);
        if let (Some(show), Some(unsaved)) = (&mut self.loaded, &unsaved) {
            restore_mix(show, unsaved);
        }
    }

    /// Not the device asked for: say which one plays instead.
    fn notice_fallback(&mut self, fallback: Option<String>) {
        if let Some(notice) = fallback {
            eprintln!("warning: {notice}");
            self.problems.push(notice);
        }
    }

    /// The window lost focus (Cmd-Tab, a system dialog, the folder
    /// picker): a panic hold ends without panic, and every key counts as
    /// up, since their releases go elsewhere.
    fn focus_lost(&mut self, cx: &mut Context<Self>) {
        self.hold.cancel();
        self.keys.forget_all();
        cx.notify();
    }

    /// One turn of the timer: moves on everything that changes with time
    /// alone and draws again only when it changed what the screens show
    /// ([`Timed::turn`]), then starts reading the facts of a song that
    /// shows when they are wanted ([`Self::read_facts`]).
    fn tick(&mut self, cx: &mut Context<Self>) {
        let timed = Timed {
            show: self.loaded.as_mut(),
            hold: &mut self.hold,
            mixer: &mut self.mixer,
            output: &mut self.output,
            settings: &mut self.settings,
            library: self.library.as_mut(),
        };
        if timed.turn(Instant::now()) {
            cx.notify();
        }
        self.read_facts(cx);
    }

    /// Starts reading the facts of the song on show when the cache wants
    /// them ([`FactsCache::start_read`]), on the background executor, so a
    /// volume slow to answer holds up no frame and no control; the screen
    /// draws again when they arrive. A click-only tile has none to read.
    fn read_facts(&mut self, cx: &mut Context<Self>) {
        let Some((folder, _)) = self.loaded.as_ref().and_then(Show::mix_file) else {
            return;
        };
        if !self.facts.start_read(&folder, Instant::now()) {
            return;
        }
        let read = cx.background_executor().spawn({
            let folder = folder.clone();
            async move { facts::read(&folder) }
        });
        cx.spawn(async move |this, cx| {
            let read = read.await;
            let _ = this.update(cx, |screen, cx| {
                screen.facts.finish(folder, read, Instant::now());
                cx.notify();
            });
        })
        .detach();
    }

    /// Carries out what the presentational code or a key asks for; while
    /// the question before ending the show is open, the controls do
    /// nothing ([`control_acts`]).
    pub fn handle(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        if !control_acts(intent, self.controls_wait()) {
            return;
        }
        // A pending "Click again to remove …" never outlives leaving the
        // Library screen or any control outside its remove controls.
        if disarms_removal(intent)
            && let Some(library) = &mut self.library
        {
            library.disarm();
        }
        let now = Instant::now();
        match intent {
            Intent::Ui(action) => {
                if let Some(show) = &mut self.loaded {
                    show.send(action);
                    show.poll();
                }
            }
            Intent::PanicDown => self.hold.press(now),
            Intent::PanicUp => {
                if self.hold.release(now)
                    && let Some(show) = &mut self.loaded
                {
                    show.send(UiAction::Panic);
                }
            }
            Intent::OpenSong => self.prompt_for(Pick::Song, window, cx),
            Intent::OpenSetlist => self.prompt_for(Pick::Setlist, window, cx),
            Intent::OpenLibrary => self.prompt_for(Pick::Library, window, cx),
            Intent::ImportStems => self.prompt_for(Pick::Import, window, cx),
            Intent::ShowPage(page) => self.show_page(page),
            Intent::Library(action) => {
                // The setlist on Perform cannot be removed.
                let playing = self.playing_file().map(str::to_owned);
                if let Some(library) = &mut self.library {
                    library.set_playing(playing.as_deref());
                    library.act(action);
                }
                self.follow_library_moves();
                self.run_save(cx);
            }
            Intent::PerformSetlist => self.perform_setlist(cx),
            Intent::DismissProblems => self.problems.clear(),
        }
        cx.notify();
    }

    /// Asks for a song folder or a setlist file, then opens it.
    fn prompt_for(&mut self, pick: Pick, _window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(pick.options());
        cx.spawn(async move |this, cx| {
            let chosen = match paths.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Err(error)) => {
                    let message = format!("The folder picker failed: {error}");
                    eprintln!("error: {message}");
                    let _ = this.update(cx, |screen, cx| {
                        screen.problems = vec![message];
                        cx.notify();
                    });
                    None
                }
                _ => None,
            };
            if let Some(path) = chosen {
                let _ = this.update(cx, |screen, cx| {
                    match pick {
                        Pick::Song => screen.open(&Source::Song(path), cx),
                        Pick::Setlist => match Source::picked_setlist(&path) {
                            Ok(source) => screen.open(&source, cx),
                            Err(problem) => screen.problems = vec![problem],
                        },
                        Pick::Library => screen.open_library(&path, None, Opened::Picked),
                        Pick::Import => screen.import_stems(&path, cx),
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Shows `page`. The Library screen opens on the library of the
    /// setlist that plays, with that setlist in the editor, when no library
    /// is open yet.
    fn show_page(&mut self, page: Page) {
        self.page = page;
        if page == Page::Output {
            self.output
                .set_devices(CpalOutput::new().devices().unwrap_or_default());
        }
        if page != Page::Library {
            return;
        }
        let playing = match &self.loaded {
            Some(Show::Setlist(setlist)) => {
                Some((setlist.library.as_path(), setlist.file.as_str()))
            }
            _ => None,
        };
        let open = self
            .library
            .as_ref()
            .map(|library| (library.root(), self.opened));
        if let Some((root, file)) = library_to_open(open, playing) {
            self.open_library(&root, Some(&file), Opened::Followed);
        }
    }

    /// Lets the setlist on Perform follow its file when the Library screen
    /// moved it (a rename, or a file named by hand moved on its first
    /// change), so it opens again from where it now is and stays marked.
    fn follow_library_moves(&mut self) {
        let Some(library) = &mut self.library else {
            return;
        };
        let root = library.root().to_owned();
        for (from, to) in library.take_moves() {
            if let Some(show) = &mut self.loaded {
                show.follow_move(&root, &from, &to);
            }
        }
    }

    /// Carries out what a Mixer screen control asks for; nothing while the
    /// question before ending the show is open.
    pub fn mixer_act(&mut self, action: MixerAction, cx: &mut Context<Self>) {
        if self.controls_wait() {
            return;
        }
        if let Some(show) = &mut self.loaded {
            self.mixer.act(action, show, Instant::now());
            cx.notify();
        }
    }

    /// Carries out what an Audio output screen control asks for; nothing
    /// while the question before ending the show is open. Another device
    /// opens the song again on it.
    pub fn output_act(&mut self, action: OutputAction, cx: &mut Context<Self>) {
        if self.controls_wait() {
            return;
        }
        let reopen = self.output.act(
            action,
            self.loaded.as_mut(),
            &mut self.settings,
            Instant::now(),
        );
        if reopen && let Some(show) = &self.loaded {
            // A mix not saved yet carries onto the new device.
            let unsaved = unsaved_mix(show);
            let source = show.source();
            self.open_with(&source, unsaved, cx);
        }
        if let Some(show) = &mut self.loaded {
            show.poll();
        }
        cx.notify();
    }

    /// Saves bus and master changes that wait for their delay, before the
    /// song that has them closes.
    fn save_waiting_buses(&mut self) {
        if self.mixer.buses_waiting()
            && let Some(show) = &mut self.loaded
        {
            self.mixer.save_buses(show, &mut self.settings);
        }
    }

    /// What the song playing's `song.json` says, as far as it has been read
    /// ([`FactsCache::facts`]); for a setlist's click-only tile, its tempo
    /// and meter. It reads nothing: until the facts arrive, or while they
    /// cannot be read, the screen shows what the session says.
    fn song_facts(&self) -> SongFacts {
        let Some(show) = &self.loaded else {
            return SongFacts::default();
        };
        match show.mix_file() {
            Some((folder, _)) => self.facts.facts(&folder),
            None => current_click(show).unwrap_or_default(),
        }
    }

    /// The setlist playing, when one does.
    fn loaded_setlist(&mut self) -> Option<&mut LoadedSetlist> {
        self.loaded.as_mut().and_then(Show::setlist_mut)
    }

    /// A drag of a strip tile starts.
    pub fn strip_drag_started(&mut self, cx: &mut Context<Self>) {
        if let Some(setlist) = self.loaded_setlist() {
            setlist.start_drag();
            cx.notify();
        }
    }

    /// A drag of strip tile `from` is over tile `over`, in its `half`.
    pub fn strip_drag_over(
        &mut self,
        from: usize,
        over: usize,
        half: Half,
        cx: &mut Context<Self>,
    ) {
        if let Some(setlist) = self.loaded_setlist() {
            setlist.drag_over(from, over, half);
            cx.notify();
        }
    }

    /// A drag of a strip tile left the tiles: its line goes until it is
    /// over a tile again.
    pub fn strip_drag_left(&mut self, cx: &mut Context<Self>) {
        if let Some(setlist) = self.loaded_setlist()
            && setlist.cancel_drag()
        {
            cx.notify();
        }
    }

    /// Strip tile `from` was dropped on the tiles: the setlist moves it
    /// while it plays and saves the new order, which the Library screen
    /// reads again.
    pub fn strip_drop(&mut self, from: usize, cx: &mut Context<Self>) {
        let Some(setlist) = self.loaded_setlist() else {
            return;
        };
        setlist.drop_dragged(from);
        let saves = setlist.take_saves();
        let root = setlist.library.clone();
        if let Some(library) = &mut self.library
            && library.root() == root
        {
            for (from, to) in saves {
                library.setlist_saved(&from, &to);
            }
        }
        cx.notify();
    }

    /// A strip tile dropped anywhere but the tiles: nothing moves.
    fn strip_drag_cancelled(&mut self, cx: &mut Context<Self>) {
        if let Some(setlist) = self.loaded_setlist() {
            setlist.cancel_drag();
            cx.notify();
        }
    }

    /// A drag of a row starts.
    pub fn library_drag_started(&mut self, cx: &mut Context<Self>) {
        if let Some(library) = &mut self.library {
            library.start_drag();
            cx.notify();
        }
    }

    /// A drag of `dragged` is over row `over` of its list, in its `half`.
    pub fn library_drag_over(
        &mut self,
        dragged: Dragged,
        over: usize,
        half: Half,
        cx: &mut Context<Self>,
    ) {
        if let Some(library) = &mut self.library {
            library.drag_over(dragged, over, half);
            cx.notify();
        }
    }

    /// The drag of `dragged` was dropped on its list.
    pub fn library_drop(&mut self, dragged: Dragged, cx: &mut Context<Self>) {
        if let Some(library) = &mut self.library {
            library.drop_dragged(dragged);
        }
        self.follow_library_moves();
        cx.notify();
    }

    /// A drag left its list: its line goes until it is over a row again.
    pub fn library_drag_left(&mut self, cx: &mut Context<Self>) {
        if let Some(library) = &mut self.library
            && library.cancel_drag()
        {
            cx.notify();
        }
    }

    /// A drag ended anywhere but its list: nothing moves.
    fn library_drag_cancelled(&mut self, cx: &mut Context<Self>) {
        if let Some(library) = &mut self.library {
            library.cancel_drag();
            cx.notify();
        }
    }

    /// Imports the folder or ZIP at `source` into the library on the
    /// Library screen: it is read on the background executor, since a
    /// slow, sleeping or unplugged drive can take seconds, and the Song
    /// editor opens with what it read, or the Library says why it cannot
    /// be imported.
    fn import_stems(&mut self, source: &Path, cx: &mut Context<Self>) {
        let Some(library) = &mut self.library else {
            return;
        };
        let job = library.import_from(source);
        let probe = cx.background_executor().spawn(async move { job.run() });
        cx.spawn(async move |this, cx| {
            let done = probe.await;
            let _ = this.update(cx, |screen, cx| {
                if let Some(library) = &mut screen.library {
                    library.probed(done);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Folders or files dropped on "Import stems": the first one is
    /// imported, as if it was picked.
    pub fn import_dropped(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if self.controls_wait() {
            return;
        }
        if let Some(source) = paths.first() {
            self.import_stems(source, cx);
        }
        cx.notify();
    }

    /// Runs the save "Save song" started, if any, on the background
    /// executor: it copies and transcodes every stem, which takes seconds,
    /// while the timer draws its progress ([`Timed::turn`]). Once it ends,
    /// the Library lists the new song and scrolls to it, or the Song
    /// editor says why it was not saved.
    fn run_save(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.library.as_mut().and_then(LibraryPage::take_save) else {
            return;
        };
        let save = cx.background_executor().spawn(async move { job.run() });
        cx.spawn(async move |this, cx| {
            let done = save.await;
            let _ = this.update(cx, |screen, cx| {
                if let Some(library) = &mut screen.library {
                    library.saved(done);
                    if let Some(row) = library.imported_row() {
                        screen.songs_scroll.scroll_to_item(row);
                    }
                }
                // A quit put off until the save stopped goes ahead now.
                if !screen.library.as_ref().is_some_and(LibraryPage::saving) {
                    screen.finish_ending(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the library folder `root` on the Library screen, with `file`
    /// in the editor when given.
    fn open_library(&mut self, root: &Path, file: Option<&str>, opened: Opened) {
        match LibraryPage::open(root, file) {
            Ok(library) => {
                self.library = Some(library);
                self.opened = opened;
                self.library_problem = None;
            }
            Err(error) => {
                self.library_problem = Some(format!("Could not open {}: {error}", root.display()));
            }
        }
    }

    /// Opens the setlist in the Library screen's editor on the Perform
    /// screen, as Open setlist… does.
    fn perform_setlist(&mut self, cx: &mut Context<Self>) {
        let Some(library) = &self.library else {
            return;
        };
        let Some(file) = library.selected_file() else {
            return;
        };
        let source = Source::Setlist {
            library: library.root().to_owned(),
            file: file.to_owned(),
        };
        self.open(&source, cx);
        self.page = Page::Perform;
    }

    /// The setlist file open on the Perform screen, when it is in the
    /// library the Library screen shows.
    fn playing_file(&self) -> Option<&str> {
        let library = self.library.as_ref()?;
        match &self.loaded {
            Some(Show::Setlist(setlist)) if setlist.library == library.root() => {
                Some(setlist.file.as_str())
            }
            _ => None,
        }
    }

    /// A key while a field of the Library screen has the focus: it types
    /// into the field, except the shortcuts ([`typed`]).
    fn type_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let press = KeyPress {
            key: &event.keystroke.key,
            mods: mods(&event.keystroke.modifiers),
        };
        cx.stop_propagation();
        match typed(&press, event.keystroke.key_char.as_deref()) {
            Typed::Line(key) => {
                if let Some(library) = &mut self.library {
                    library.type_key(key);
                }
                self.follow_library_moves();
            }
            Typed::Paste => {
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                if let (Some(library), Some(text)) = (&mut self.library, text) {
                    library.paste(&text);
                }
            }
            Typed::Key(KeyAction::Quit) => {
                self.end_show(Ending::Quit, window, cx);
                return;
            }
            Typed::Key(KeyAction::OpenSong) => self.handle(Intent::OpenSong, window, cx),
            Typed::Key(_) | Typed::Ignored => {}
        }
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.page == Page::Library && self.library.as_ref().is_some_and(LibraryPage::takes_keys)
        {
            self.type_key(event, window, cx);
            return;
        }
        let press = KeyPress {
            key: &event.keystroke.key,
            mods: mods(&event.keystroke.modifiers),
        };
        // A held key repeats; only its first press acts, so a held space
        // does not toggle play over and over.
        let intent = match self.keys.press(&press, event.is_held) {
            Some(KeyAction::Ui(action)) => Intent::Ui(action),
            Some(KeyAction::PanicDown) => Intent::PanicDown,
            Some(KeyAction::OpenSong) => Intent::OpenSong,
            Some(KeyAction::Quit) => {
                if let Some(library) = &mut self.library {
                    library.disarm();
                }
                self.end_show(Ending::Quit, window, cx);
                return;
            }
            None => return,
        };
        cx.stop_propagation();
        self.handle(intent, window, cx);
    }

    fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.keys.modifiers_changed(mods(&event.modifiers));
    }

    fn key_up(&mut self, event: &KeyUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.keys.release(&event.keystroke.key);
        if is_panic_key(&event.keystroke.key) {
            self.handle(Intent::PanicUp, window, cx);
        }
    }
}

/// The facts of the click-only tile `show`'s setlist is on, when it is on
/// one.
fn current_click(show: &Show) -> Option<SongFacts> {
    let setlist = show.setlist()?;
    let item = setlist
        .session
        .info()
        .items
        .get(setlist.snapshot.setlist.current)?;
    let SetlistItem::Click(click) = &item.item else {
        return None;
    };
    let meter = click.time_signature;
    Some(SongFacts::click(
        click.bpm,
        meter.numerator,
        meter.denominator,
    ))
}

/// The modifiers the keyboard rules read; Shift is left out.
fn mods(modifiers: &Modifiers) -> Mods {
    Mods {
        control: modifiers.control,
        alt: modifiers.alt,
        platform: modifiers.platform,
        function: modifiers.function,
    }
}

/// What a picker asks for.
#[derive(Debug, Clone, Copy)]
enum Pick {
    /// A song folder.
    Song,
    /// A setlist file in the Setlists folder of a library folder.
    Setlist,
    /// A library folder.
    Library,
    /// A folder or ZIP file of stems to import.
    Import,
}

impl Pick {
    /// The picker that asks for it.
    fn options(self) -> PathPromptOptions {
        let (files, directories, prompt) = match self {
            Pick::Song => (false, true, "Open song"),
            Pick::Setlist => (true, false, "Open setlist"),
            Pick::Library => (false, true, "Open library"),
            Pick::Import => (true, true, "Import stems"),
        };
        PathPromptOptions {
            files,
            directories,
            multiple: false,
            prompt: Some(prompt.into()),
        }
    }
}

impl Render for Perform {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facts = if self.page == Page::Perform {
            self.song_facts()
        } else {
            SongFacts::default()
        };
        let body = match (self.page, &self.loaded) {
            (Page::Library, _) => match &self.library {
                Some(library) => match library.song_editor() {
                    Some(editor) => {
                        song_editor_ui::song_editor(&editor, window.viewport_size(), cx)
                            .into_any_element()
                    }
                    None => {
                        let view = library.view(self.playing_file());
                        library_ui::library(&view, &self.songs_scroll, cx).into_any_element()
                    }
                },
                None => {
                    library_ui::no_library(self.library_problem.as_deref(), cx).into_any_element()
                }
            },
            (Page::Perform, Some(show)) => {
                let view = PerformView::with_facts(show.snapshot(), show.song(), &facts);
                let setlist = show.setlist().map(LoadedSetlist::view);
                let mixer = self.mixer.view(Some(show));
                ui::perform(
                    &view,
                    setlist.as_ref(),
                    mixer.as_ref(),
                    window.viewport_size(),
                    self.hold.fill(Instant::now()),
                    &self.problems,
                    cx,
                )
                .into_any_element()
            }
            (Page::Perform, None) => ui::no_song(&self.problems, cx).into_any_element(),
            (Page::Output, show) => {
                let view = self.output.view(show.as_ref(), &self.settings);
                output_ui::output(&view, cx).into_any_element()
            }
            (Page::Mixer, show) => match self.mixer.view(show.as_ref()) {
                Some(view) => mixer_ui::mixer(&view, cx).into_any_element(),
                None => mixer_ui::no_mixer().into_any_element(),
            },
        };
        ui::root(&self.focus, cx)
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            // A row dropped anywhere but its list moves nothing.
            .on_drop(cx.listener(|screen, _: &DraggedItem, _, cx| {
                screen.library_drag_cancelled(cx);
            }))
            .on_drop(cx.listener(|screen, _: &DraggedSetlist, _, cx| {
                screen.library_drag_cancelled(cx);
            }))
            .on_drop(cx.listener(|screen, _: &DraggedTile, _, cx| {
                screen.strip_drag_cancelled(cx);
            }))
            .child(ui::nav(self.page, cx))
            .child(ui::main().child(body))
            .when(self.controls_wait(), |root| root.child(ui::shield()))
    }
}

/// Opens the Perform window for `path` (a song folder or a setlist file),
/// if any. An error means no window opened, and so no song or device
/// either.
pub fn open_window(path: Option<PathBuf>, cx: &mut App) -> gpui::Result<()> {
    let bounds = gpui::Bounds::centered(None, gpui::size(gpui::px(1366.), gpui::px(900.)), cx);
    let options = gpui::WindowOptions {
        window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
        titlebar: Some(gpui::TitlebarOptions {
            title: Some("OpenVamp — Perform".into()),
            ..Default::default()
        }),
        window_min_size: Some(gpui::size(gpui::px(960.), gpui::px(640.))),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| {
        let screen = cx.new(|cx| Perform::new(path, window, cx));
        // Closing the window while a song plays asks first; otherwise the
        // song stops with its ramp before the window closes.
        let closing = screen.downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            closing
                .update(cx, |screen, cx| {
                    screen.end_show(Ending::CloseWindow, window, cx)
                })
                .unwrap_or(true)
        });
        screen
    })?;
    Ok(())
}

/// Whether a show prepared with `used` opens as one prepared with `options`
/// would: on the same device, with the same buffer size, routing and bus
/// settings.
fn same_settings(options: &SessionOptions, used: &SessionOptions) -> bool {
    options.device == used.device
        && options.buffer_frames == used.buffer_frames
        && options.routing == used.routing
        && options.bus_settings == used.bus_settings
}

#[cfg(test)]
mod tests {
    use super::Pick;

    #[test]
    fn each_picker_asks_for_one_folder_or_file_of_its_kind() {
        let asks = |pick: Pick| {
            let options = pick.options();
            (
                options.files,
                options.directories,
                options.multiple,
                options.prompt.map(|prompt| prompt.to_string()),
            )
        };
        let prompt = |text: &str| Some(text.to_owned());
        assert_eq!(asks(Pick::Song), (false, true, false, prompt("Open song")));
        assert_eq!(
            asks(Pick::Setlist),
            (true, false, false, prompt("Open setlist"))
        );
        assert_eq!(
            asks(Pick::Library),
            (false, true, false, prompt("Open library"))
        );
        // A folder of stems, or a ZIP file of them.
        assert_eq!(
            asks(Pick::Import),
            (true, true, false, prompt("Import stems"))
        );
    }
}
