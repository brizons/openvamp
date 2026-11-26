//! F8.5 through the `app` API: the buffer size changes while a session
//! plays, by opening the stream again, without rebuilding the players.

mod common;

use std::time::Duration;

use app::testing::FakeOutput;
use app::{
    Command, DeviceStatus, MAX_BUFFER_FRAMES, RuntimeError, SessionOptions, SetlistCommand,
    SetlistSession, TransportState,
};
use common::{bits, offline, open, play_for, start, step, write_song, write_song_with};
use domain::{DEFAULT_SEAM_CROSSFADE, Setlist, SetlistItem, SongItem, Transition};
use library::Library;

#[test]
fn the_buffer_size_changes_while_playing_and_the_song_goes_on_from_where_it_held() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), common::RATE);
    let expected = offline(dir.path(), "default", DEFAULT_SEAM_CROSSFADE);
    let fake = FakeOutput::new();
    let mut session = open(
        dir.path(),
        &fake,
        SessionOptions {
            buffer_frames: 64,
            ..SessionOptions::default()
        },
    );
    let (first, _) = start(&mut session, &fake, 64);
    let (before, _) = play_for(&mut session, &fake, 64, 20_000);
    let heard = [first, before].concat();
    assert_eq!(
        bits(&heard),
        bits(&expected[..heard.len()]),
        "sample-exact up to the change"
    );

    session.set_buffer_frames(MAX_BUFFER_FRAMES).unwrap();
    assert_eq!(fake.opened().len(), 1, "the stream ramps out first");
    let monitor = session.disk_monitor();
    let mut old = Vec::new();
    while fake.opened().len() == 1 {
        assert!(old.len() < 2 * 64 * 64, "the stream opens again");
        old.extend(step(&mut session, &fake, &monitor, 64).0);
    }
    assert_eq!(&old[old.len() - 2..], [0.0, 0.0], "silent when it closes");
    assert_eq!(fake.opened()[1].buffer_frames, MAX_BUFFER_FRAMES);
    let snapshot = session.poll();
    let DeviceStatus::Connected(info) = &snapshot.device else {
        panic!("the device plays");
    };
    assert_eq!(info.buffer_frames, MAX_BUFFER_FRAMES);
    assert_eq!(info.latency().as_secs_f64(), 1_024.0 / 8_000.0, "128 ms");
    let held = snapshot.source_frame;

    // Past the ramp back in, the song plays sample-exact from where the
    // ramp out held it: no frame lost or repeated.
    let mut last = (Vec::new(), snapshot);
    for _ in 0..4 {
        last = step(&mut session, &fake, &monitor, MAX_BUFFER_FRAMES as usize);
    }
    let (out, snapshot) = last;
    assert_eq!(snapshot.transport, TransportState::Playing, "it plays on");
    let end = 2 * snapshot.source_frame as usize;
    assert!(end / 2 > held as usize + 1_024);
    assert_eq!(bits(&out), bits(&expected[end - out.len()..end]));

    // The same size again opens nothing.
    session.set_buffer_frames(MAX_BUFFER_FRAMES).unwrap();
    assert_eq!(fake.opened().len(), 2);
}

#[test]
fn a_buffer_size_outside_64_to_1024_frames_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    write_song(dir.path(), common::RATE);
    let fake = FakeOutput::new();
    let mut session = open(dir.path(), &fake, SessionOptions::default());
    for frames in [0, 63, 1_025] {
        let error = session.set_buffer_frames(frames).unwrap_err();
        assert_eq!(error, RuntimeError::BufferSize { frames });
        assert_eq!(session.poll().last_error, Some(error));
    }
    assert_eq!(
        RuntimeError::BufferSize { frames: 32 }.to_string(),
        "the buffer size must be 64 to 1024 frames, not 32"
    );
    assert_eq!(fake.opened().len(), 1, "nothing opened again");
}

#[test]
fn a_setlist_changes_the_buffer_size_for_the_items_that_follow_too() {
    let dir = tempfile::tempdir().unwrap();
    write_song_with(&dir.path().join("first"), common::RATE, 0);
    write_song_with(&dir.path().join("second"), common::RATE, 9);
    let library = Library::new(dir.path());
    let mut setlist = Setlist::new("Sunday");
    setlist.items = ["first", "second"]
        .into_iter()
        .map(|path| {
            SetlistItem::Song(SongItem {
                transition: Transition::AutoLink,
                ..SongItem::new(path)
            })
        })
        .collect();
    let file = library.save_setlist(&setlist).unwrap().file;
    let fake = FakeOutput::new();
    let mut session = SetlistSession::open(
        &library,
        &file,
        Box::new(fake.clone()),
        SessionOptions::default(),
    )
    .unwrap();
    session.send(SetlistCommand::Item(Command::Play)).unwrap();
    session.set_buffer_frames(512).unwrap();
    assert_eq!(fake.opened()[1].buffer_frames, 512);
    let second = offline(
        &dir.path().join("second"),
        "default",
        DEFAULT_SEAM_CROSSFADE,
    );
    let mut heard = Vec::new();
    for _ in 0..1_000 {
        for monitor in session.disk_monitors() {
            assert!(monitor.wait_idle(Duration::from_secs(10)));
        }
        let out = fake.render(512).expect("a stream plays");
        if session.poll().setlist.current == 1 {
            heard.extend(out);
            if heard.len() >= second.len() / 2 {
                break;
            }
        }
    }
    assert!(
        heard.len() >= second.len() / 2,
        "the second song plays at 512 frames"
    );
    assert!(heard.iter().any(|&sample| sample != 0.0));
}
