//! A setlist loads its next item off the control thread: a song folder whose
//! read hangs (a slow, sleeping or unplugged volume) never holds up a poll,
//! Panic or the transport, and the item follows on once it loads.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use app::testing::FakeOutput;
use app::{
    Command, OpenGate, SessionOptions, SetlistCommand, SetlistProblem, SetlistSession,
    SetlistSnapshot, TransportState,
};
use common::{IDLE, RATE, bits, offline, write_song_with};
use domain::{DEFAULT_SEAM_CROSSFADE, Setlist, SetlistItem, SongItem, Transition};
use library::{DiskMonitor, Library};

const BUFFER: usize = 256;

/// The longest a poll or a command may take while a read hangs: far less
/// than [`HOLD_LIMIT`], so a call that waited for the read fails.
const PROMPT: Duration = Duration::from_millis(500);

/// The longest the [`Valve`] holds a read, so a call that waits for it
/// fails its test instead of hanging it.
const HOLD_LIMIT: Duration = Duration::from_secs(10);

/// Holds the open of one song folder, as a volume that does not answer
/// would, until released (at most [`HOLD_LIMIT`]).
#[derive(Clone, Default)]
struct Valve {
    state: Arc<(Mutex<ValveState>, Condvar)>,
}

#[derive(Default)]
struct ValveState {
    released: bool,
    /// Reads held right now.
    holding: usize,
    /// A read was let through only because it was held too long.
    timed_out: bool,
}

impl Valve {
    /// The hook that holds every open of the folder ending in `held`.
    fn gate(&self, held: &'static str) -> OpenGate {
        let state = Arc::clone(&self.state);
        Arc::new(move |dir: &Path| {
            if !dir.ends_with(held) {
                return;
            }
            let (lock, changed) = &*state;
            let mut valve = lock.lock().unwrap();
            valve.holding += 1;
            changed.notify_all();
            let (mut valve, wait) = changed
                .wait_timeout_while(valve, HOLD_LIMIT, |valve| !valve.released)
                .unwrap();
            valve.holding -= 1;
            valve.timed_out |= wait.timed_out();
        })
    }

    /// Waits until a read is held.
    fn wait_held(&self) {
        let (lock, changed) = &*self.state;
        let (valve, _) = changed
            .wait_timeout_while(lock.lock().unwrap(), IDLE, |valve| valve.holding == 0)
            .unwrap();
        assert!(valve.holding > 0, "the read is held");
    }

    /// How many reads are held right now.
    fn holding(&self) -> usize {
        self.state.0.lock().unwrap().holding
    }

    /// Whether a read is held right now, never let through by the limit.
    fn still_holding(&self) -> bool {
        let valve = self.state.0.lock().unwrap();
        valve.holding > 0 && !valve.timed_out
    }

    fn release(&self) {
        let (lock, changed) = &*self.state;
        lock.lock().unwrap().released = true;
        changed.notify_all();
    }
}

struct Shelf {
    dir: tempfile::TempDir,
    library: Library,
}

impl Shelf {
    /// A library with three 14 s songs, A, B and C.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let library = Library::new(dir.path());
        for (song, seed) in [("A", 0), ("B", 10), ("C", 20)] {
            write_song_with(&dir.path().join(format!("Songs/{song}.song")), RATE, seed);
        }
        Self { dir, library }
    }

    fn path(&self, song: &str) -> PathBuf {
        self.dir.path().join(format!("Songs/{song}.song"))
    }

    /// Opens the setlist of `items` (song name, transition) on `fake`, with
    /// `valve` holding the open of song B.
    fn open(
        &self,
        items: &[(&str, Transition)],
        fake: &FakeOutput,
        valve: &Valve,
    ) -> SetlistSession {
        let mut setlist = Setlist::new("Sunday");
        setlist.items = items
            .iter()
            .map(|&(song, transition)| {
                SetlistItem::Song(SongItem {
                    transition,
                    ..SongItem::new(format!("Songs/{song}.song"))
                })
            })
            .collect();
        let file = self.library.save_setlist(&setlist).unwrap().file;
        let options = SessionOptions {
            buffer_frames: BUFFER as u32,
            open_gate: Some(valve.gate("B.song")),
            ..SessionOptions::default()
        };
        SetlistSession::open(&self.library, &file, Box::new(fake.clone()), options)
            .expect("the setlist opens")
    }
}

/// Runs `call` and checks it came back within [`PROMPT`].
fn promptly<T>(what: &str, call: impl FnOnce() -> T) -> T {
    let started = Instant::now();
    let value = call();
    let took = started.elapsed();
    assert!(took < PROMPT, "{what} took {took:?} while a read hangs");
    value
}

/// One callback once `disks` are idle, then a poll that must come back
/// within [`PROMPT`]. The session's own monitors are not asked for: they
/// wait for an item loading.
fn step(
    session: &mut SetlistSession,
    fake: &FakeOutput,
    disks: &[DiskMonitor],
) -> (Vec<f32>, SetlistSnapshot) {
    for disk in disks {
        assert!(disk.wait_idle(IDLE), "the disk thread keeps up");
    }
    let out = fake.render(BUFFER).expect("a stream plays");
    (out, promptly("a poll", || session.poll()))
}

#[test]
fn poll_panic_and_the_transport_answer_at_once_while_the_next_items_read_hangs() {
    let shelf = Shelf::new();
    let fake = FakeOutput::new();
    let valve = Valve::default();
    let mut session = shelf.open(
        &[("A", Transition::AutoLink), ("B", Transition::CueNext)],
        &fake,
        &valve,
    );
    let disks = session.disk_monitors();

    promptly("the first poll", || session.poll());
    valve.wait_held();
    promptly("play", || session.send(SetlistCommand::Item(Command::Play))).unwrap();
    let mut snapshot = step(&mut session, &fake, &disks).1;
    for _ in 0..20 {
        snapshot = step(&mut session, &fake, &disks).1;
    }
    assert_eq!(snapshot.song.transport, TransportState::Playing);
    assert_eq!(snapshot.setlist.current, 0);
    assert_eq!(snapshot.setlist.next, Some(1));
    assert!(!snapshot.setlist.next_ready, "B is still loading");

    promptly("pause", || {
        session.send(SetlistCommand::Item(Command::Pause))
    })
    .unwrap();
    for _ in 0..8 {
        snapshot = step(&mut session, &fake, &disks).1;
    }
    assert_eq!(snapshot.song.transport, TransportState::Paused);
    promptly("play", || session.send(SetlistCommand::Item(Command::Play))).unwrap();
    for _ in 0..8 {
        snapshot = step(&mut session, &fake, &disks).1;
    }
    assert_eq!(snapshot.song.transport, TransportState::Playing);
    promptly("panic", || {
        session.send(SetlistCommand::Item(Command::Panic))
    })
    .unwrap();
    let (out, _) = step(&mut session, &fake, &disks);
    assert!(out[..64].iter().any(|&s| s != 0.0), "A ramps out");
    for _ in 0..40 {
        snapshot = step(&mut session, &fake, &disks).1;
        if snapshot.song.transport == TransportState::Stopped {
            break;
        }
    }
    assert_eq!(snapshot.song.transport, TransportState::Stopped);
    assert!(
        valve.still_holding(),
        "everything above ran while B's read hung"
    );

    // Once B's read comes back it is prepared and cued as before.
    valve.release();
    let ready = (0..200).any(|_| {
        for disk in session.disk_monitors() {
            assert!(disk.wait_idle(IDLE));
        }
        session.poll().setlist.next_ready
    });
    assert!(ready, "B is cued once its read comes back");
}

#[test]
fn an_autolink_whose_next_item_loads_late_starts_it_once_loaded_and_says_so() {
    let shelf = Shelf::new();
    let a = offline(&shelf.path("A"), "default", DEFAULT_SEAM_CROSSFADE);
    let b = offline(&shelf.path("B"), "default", DEFAULT_SEAM_CROSSFADE);
    let fake = FakeOutput::new();
    let valve = Valve::default();
    let mut session = shelf.open(
        &[("A", Transition::AutoLink), ("B", Transition::CueNext)],
        &fake,
        &valve,
    );
    let disks = session.disk_monitors();
    session.send(SetlistCommand::Item(Command::Play)).unwrap();

    // A plays to its end while B's read hangs.
    let mut snapshot = step(&mut session, &fake, &disks).1;
    for _ in 0..(a.len() / 2 + 2_048) / BUFFER {
        snapshot = step(&mut session, &fake, &disks).1;
    }
    assert!(valve.still_holding(), "A ended while B's read hung");
    assert_eq!(snapshot.setlist.current, 0, "A stays current until B loads");
    assert_eq!(snapshot.setlist.next, Some(1), "B is the item it moves to");
    assert!(!snapshot.setlist.next_ready, "B is loading");
    assert_eq!(snapshot.song.transport, TransportState::Stopped);
    assert_eq!(
        snapshot.setlist.problem,
        Some(SetlistProblem::LateLink { item: 1 })
    );

    // B's read comes back: B starts as soon as it is buffered, from its
    // start.
    valve.release();
    let mut played = Vec::new();
    for _ in 0..200 {
        let disks = session.disk_monitors();
        let (out, snapshot) = step(&mut session, &fake, &disks);
        if snapshot.setlist.current == 1 && snapshot.song.output_frame > 0 {
            played = out;
            break;
        }
        assert!(out.iter().all(|&s| s == 0.0), "nothing plays before B");
    }
    assert!(!played.is_empty(), "B started");
    while played.len() < 2 * 20_000 {
        let disks = session.disk_monitors();
        let (out, snapshot) = step(&mut session, &fake, &disks);
        played.extend(out);
        assert_eq!(snapshot.song.transport, TransportState::Playing);
        assert_eq!(
            snapshot.setlist.problem,
            Some(SetlistProblem::LateLink { item: 1 })
        );
    }
    assert_eq!(
        bits(&played[..2 * 20_000]),
        bits(&b[..2 * 20_000]),
        "B plays from its start"
    );
}

#[test]
fn next_while_the_next_item_loads_answers_at_once_and_selects_it_once_loaded() {
    let shelf = Shelf::new();
    let fake = FakeOutput::new();
    let valve = Valve::default();
    let mut session = shelf.open(
        &[("A", Transition::CueNext), ("B", Transition::CueNext)],
        &fake,
        &valve,
    );
    let disks = session.disk_monitors();
    session.poll();
    valve.wait_held();

    promptly("next", || session.send(SetlistCommand::Next)).unwrap();
    let snapshot = step(&mut session, &fake, &disks).1;
    assert_eq!(snapshot.setlist.current, 0, "A stays current until B loads");
    assert_eq!(snapshot.setlist.next, Some(1));
    assert!(!snapshot.setlist.next_ready);
    // Play while B loads plays B once it has, as the last transport
    // command does for an item the device reopens for.
    promptly("play", || session.send(SetlistCommand::Item(Command::Play))).unwrap();
    for _ in 0..8 {
        let (out, snapshot) = step(&mut session, &fake, &disks);
        assert!(out.iter().all(|&s| s == 0.0), "A does not start");
        assert_eq!(snapshot.setlist.current, 0);
    }
    assert!(valve.still_holding());

    valve.release();
    let mut snapshot = session.poll();
    for _ in 0..200 {
        let disks = session.disk_monitors();
        snapshot = step(&mut session, &fake, &disks).1;
        if snapshot.song.output_frame > 0 {
            break;
        }
    }
    assert_eq!(snapshot.setlist.current, 1, "B is selected once loaded");
    assert_eq!(snapshot.song.transport, TransportState::Playing);
    assert_eq!(snapshot.setlist.problem, None);
}

#[test]
fn a_selected_item_whose_load_fails_reports_why_and_stays_unselected() {
    let shelf = Shelf::new();
    let fake = FakeOutput::new();
    let valve = Valve::default();
    let mut session = shelf.open(
        &[("A", Transition::CueNext), ("B", Transition::CueNext)],
        &fake,
        &valve,
    );
    let disks = session.disk_monitors();
    session.poll();
    valve.wait_held();
    promptly("next", || session.send(SetlistCommand::Next)).unwrap();

    // The volume comes back without B's song.json.
    std::fs::remove_file(shelf.path("B").join("song.json")).unwrap();
    valve.release();
    let mut snapshot = session.poll();
    for _ in 0..200 {
        snapshot = step(&mut session, &fake, &disks).1;
        if snapshot.setlist.problem.is_some() {
            break;
        }
    }
    let Some(SetlistProblem::Prepare { item: 1, error }) = &snapshot.setlist.problem else {
        panic!("B's failure is reported: {:?}", snapshot.setlist.problem);
    };
    assert!(error.contains("song.json"), "it says why: {error}");
    assert_eq!(snapshot.setlist.current, 0, "A stays selected");
    assert_eq!(snapshot.song.transport, TransportState::Stopped);
    assert!(!snapshot.setlist.next_ready);
}

#[test]
fn selecting_back_and_forth_on_a_hung_folder_reads_it_once_and_the_newest_selection_wins() {
    let shelf = Shelf::new();
    let fake = FakeOutput::new();
    let valve = Valve::default();
    let mut session = shelf.open(
        &[
            ("A", Transition::CueNext),
            ("B", Transition::CueNext),
            ("C", Transition::CueNext),
        ],
        &fake,
        &valve,
    );
    session.poll();
    valve.wait_held();

    // B's folder does not answer; the operator goes back and forth.
    for _ in 0..6 {
        promptly("select B", || session.send(SetlistCommand::Select(1))).unwrap();
        promptly("select C", || session.send(SetlistCommand::Select(2))).unwrap();
        promptly("a poll", || session.poll());
    }
    promptly("select B", || session.send(SetlistCommand::Select(1))).unwrap();
    promptly("a poll", || session.poll());
    assert_eq!(
        valve.holding(),
        1,
        "B is read once, however often it is picked"
    );

    // C is picked last, then B's read comes back: it does not land.
    promptly("select C", || session.send(SetlistCommand::Select(2))).unwrap();
    assert_eq!(session.poll().setlist.current, 2);
    assert!(valve.still_holding());
    valve.release();
    let mut snapshot = session.poll();
    for _ in 0..20 {
        for disk in session.disk_monitors() {
            assert!(disk.wait_idle(IDLE));
        }
        snapshot = session.poll();
    }
    assert_eq!(
        snapshot.setlist.current, 2,
        "C, picked last, stays selected"
    );
    assert_eq!(snapshot.setlist.problem, None);

    // B selected again now loads and is selected.
    session.send(SetlistCommand::Select(1)).unwrap();
    let mut snapshot = session.poll();
    for _ in 0..200 {
        if snapshot.setlist.current == 1 {
            break;
        }
        for disk in session.disk_monitors() {
            assert!(disk.wait_idle(IDLE));
        }
        snapshot = session.poll();
    }
    assert_eq!(snapshot.setlist.current, 1);
    assert_eq!(valve.holding(), 0);
}
