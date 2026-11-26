//! Real-time checklist for the audio side of a session: "No memory
//! allocation or deallocation." in `AudioRenderer::render`, which runs the
//! player, asks the sources whether the playhead is ready, and publishes the
//! snapshot, while the session is stopped, starting, playing, jumping,
//! fading and panicking, while a setlist links one song to the next and
//! crossfades into a third, and while the mixer changes the gains of a
//! session whose players render more channels than its stream takes.
//!
//! This test binary installs a counting global allocator that counts only on
//! the test thread while it calls `render`; `unsafe` is needed only to
//! forward to the system allocator and stays in this test.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::{Arc, Mutex};

use app::{
    AudioOutput, AudioRenderer, Command, DeviceInfo, JumpTiming, OutputError, OutputRequest,
    OutputStream, Session, SessionOptions, SetlistCommand, SetlistSession, StreamHealth,
    StreamInfo,
};
use common::{IDLE, RATE, write_song, write_song_with};
use domain::{Bus, Routing, Setlist, SetlistItem, SongItem, Transition, Volume};
use library::Library;

struct Counting;

thread_local! {
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}

fn record() {
    let _ = COUNT.try_with(|count| {
        if let Some(n) = count.get() {
            count.set(Some(n + 1));
        }
    });
}

// SAFETY: every call forwards to `System` with the caller's arguments.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        record();
        // SAFETY: forwarded unchanged; `ptr` came from `System`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A port that hands the renderer to the test.
#[derive(Clone, Default)]
struct Capture {
    renderer: Arc<Mutex<Option<AudioRenderer>>>,
}

struct Captured {
    renderer: Arc<Mutex<Option<AudioRenderer>>>,
    info: StreamInfo,
}

impl AudioOutput for Capture {
    fn devices(&self) -> Result<Vec<DeviceInfo>, OutputError> {
        Ok(Vec::new())
    }

    fn open(
        &mut self,
        request: &OutputRequest,
        renderer: AudioRenderer,
    ) -> Result<Box<dyn OutputStream>, OutputError> {
        *self.renderer.lock().unwrap() = Some(renderer);
        Ok(Box::new(Captured {
            renderer: Arc::clone(&self.renderer),
            info: StreamInfo {
                device: "capture".to_owned(),
                device_id: "capture".to_owned(),
                sample_rate: request.sample_rate,
                buffer_frames: request.buffer_frames,
                channels: 2,
            },
        }))
    }
}

impl OutputStream for Captured {
    fn info(&self) -> &StreamInfo {
        &self.info
    }

    fn health(&self) -> StreamHealth {
        StreamHealth::Running
    }
}

impl Drop for Captured {
    fn drop(&mut self) {
        drop(self.renderer.lock().unwrap().take());
    }
}

#[test]
fn rendering_a_session_allocates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), RATE);
    let capture = Capture::default();
    let mut session = Session::open(
        dir.path(),
        Box::new(capture.clone()),
        SessionOptions {
            buffer_frames: 256,
            ..SessionOptions::default()
        },
    )
    .unwrap();
    let monitor = session.disk_monitor();
    let script = [
        (2, Command::Play),
        (40, Command::QueueJump { position: 3 }),
        (60, Command::SetJumpTiming(JumpTiming::NextBar)),
        (60, Command::QueueJump { position: 1 }),
        (200, Command::ToggleFade),
        (260, Command::LiveLoop),
        (300, Command::SetInfinite(true)),
        (340, Command::Pause),
        (350, Command::Play),
        (380, Command::BackToStart),
        (400, Command::Panic),
        (430, Command::Play),
    ];
    let mut out = vec![0.0f32; 2 * 256];
    let mut allocations = 0;
    let mut renderer = capture.renderer.lock().unwrap();
    let renderer = renderer.as_mut().unwrap();
    for buffer in 0..500 {
        for (_, command) in script.iter().filter(|(at, _)| *at == buffer) {
            session.send(*command).unwrap();
        }
        assert!(monitor.wait_idle(IDLE));
        COUNT.with(|count| count.set(Some(0)));
        renderer.render(&mut out);
        allocations += COUNT.with(|count| count.replace(None)).unwrap();
        session.poll();
    }
    assert_eq!(allocations, 0);
    assert!(session.poll().output_frame > 0, "the song played");
}

#[test]
fn rendering_a_routed_session_through_mixer_changes_allocates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), RATE);
    let capture = Capture::default();
    // The players render eight channels; the stream takes the two the
    // routing uses.
    let mut session = Session::open(
        dir.path(),
        Box::new(capture.clone()),
        SessionOptions {
            buffer_frames: 256,
            routing: Some(Routing::stereo(8)),
            ..SessionOptions::default()
        },
    )
    .unwrap();
    let monitor = session.disk_monitor();
    let db = Volume::from_db;
    let script = [
        (2, Command::Play),
        (
            40,
            Command::SetStemVolume {
                stem: 0,
                volume: db(-6.0),
            },
        ),
        (
            60,
            Command::SetBusMute {
                bus: Bus::Drums,
                mute: true,
            },
        ),
        (
            80,
            Command::SetStemBus {
                stem: 1,
                bus: Some(Bus::Vocals),
            },
        ),
        (100, Command::SetMasterVolume(db(-3.0))),
        (
            120,
            Command::SetStemSolo {
                stem: 1,
                solo: true,
            },
        ),
        (
            140,
            Command::SetBusVolume {
                bus: Bus::ClickGuide,
                volume: db(-6.0),
            },
        ),
        (
            160,
            Command::SetBusMute {
                bus: Bus::Drums,
                mute: false,
            },
        ),
    ];
    let mut out = vec![0.0f32; 2 * 256];
    let mut allocations = 0;
    let mut renderer = capture.renderer.lock().unwrap();
    let renderer = renderer.as_mut().unwrap();
    for buffer in 0..200 {
        for (_, command) in script.iter().filter(|(at, _)| *at == buffer) {
            session.send(*command).unwrap();
        }
        assert!(monitor.wait_idle(IDLE));
        COUNT.with(|count| count.set(Some(0)));
        renderer.render(&mut out);
        allocations += COUNT.with(|count| count.replace(None)).unwrap();
        session.poll();
    }
    assert_eq!(allocations, 0);
    assert!(out.iter().any(|&sample| sample != 0.0), "the song played");
}

#[test]
fn rendering_a_setlist_through_an_autolink_and_a_crossfade_allocates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    for (name, seed) in [("A", 0), ("B", 10), ("C", 20)] {
        write_song_with(&dir.path().join(format!("Songs/{name}.song")), RATE, seed);
    }
    let library = Library::new(dir.path());
    let item = |song: &str, transition| {
        SetlistItem::Song(SongItem {
            transition,
            ..SongItem::new(song)
        })
    };
    let mut setlist = Setlist::new("Allocations");
    setlist.items = vec![
        item("Songs/A.song", Transition::AutoLink),
        item("Songs/B.song", Transition::Crossfade),
        item("Songs/C.song", Transition::CueNext),
    ];
    let entry = library.save_setlist(&setlist).unwrap();
    let capture = Capture::default();
    let mut session = SetlistSession::open(
        &library,
        &entry.file,
        Box::new(capture.clone()),
        SessionOptions {
            buffer_frames: 256,
            ..SessionOptions::default()
        },
    )
    .unwrap();

    let mut out = vec![0.0f32; 2 * 256];
    let mut allocations = 0;
    let mut items = Vec::new();
    let mut blended = false;
    let mut renderer = capture.renderer.lock().unwrap();
    let renderer = renderer.as_mut().unwrap();
    // Three 14-second songs, the last two overlapping by five seconds.
    for buffer in 0..1_200 {
        if buffer == 2 {
            session.send(SetlistCommand::Item(Command::Play)).unwrap();
        }
        for monitor in session.disk_monitors() {
            assert!(monitor.wait_idle(IDLE));
        }
        COUNT.with(|count| count.set(Some(0)));
        renderer.render(&mut out);
        allocations += COUNT.with(|count| count.replace(None)).unwrap();
        let snapshot = session.poll();
        blended |= snapshot.setlist.blend.is_some();
        if items.last() != Some(&snapshot.setlist.current) {
            items.push(snapshot.setlist.current);
        }
    }
    assert_eq!(allocations, 0);
    assert_eq!(items, [0, 1, 2], "A linked to B, which crossfaded into C");
    assert!(blended, "the crossfade ran");
}
