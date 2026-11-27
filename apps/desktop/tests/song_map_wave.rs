//! S5 (F2): the waveform on the Perform song map, without a window, on
//! temporary song folders. The read runs as the job the screen hands to
//! its background executor; here it runs on the test's thread. Each
//! position's bars show its own source frames, played bars are opaque and
//! bars ahead faded, and until the waveform is read the map is as before.
//! The cached overview is read whatever the transport does; one is built
//! from the stems only while the transport is stopped or paused, so a song
//! that plays never competes with a build for its drive.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use app::{
    DeviceStatus, FadeState, FadeView, JumpKind, JumpTiming, MixerView, PositionInfo, QueuedView,
    Snapshot, SongInfo, StreamInfo, TransportState,
};
use domain::{
    Arrangement, SectionMarker, SectionType, SongPackage, Stem, StemRole, TempoPoint,
    TimeSignaturePoint,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::{Overview, OverviewError};
use openvamp_desktop::map_wave::{MapWaveform, SongWave, SongWaveError};
use openvamp_desktop::view::{
    AHEAD_OPACITY, MAP_WAVE_HIGH, MAP_WAVE_LOW, PerformView, SectionKind, SongFacts,
};

const RATE: u32 = 8_000;

/// A bar at 120 BPM in 4/4: four half-second beats.
const BAR: u64 = 2 * RATE as u64;

/// The song's bars.
const BARS: u64 = 16;

/// The level of the keys stem in bar `index`, counted from 0: louder bar
/// after bar, so every bar of the song reads differently.
fn level(index: u64) -> f32 {
    0.1 + 0.05 * index as f32
}

/// A mono 16-bit WAV of `BARS` bars, each sample of bar `i` at `levels(i)`.
fn wav(path: &Path, levels: impl Fn(u64) -> f32) {
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec).unwrap();
    for frame in 0..BARS * BAR {
        let value = (levels(frame / BAR) * f32::from(i16::MAX)) as i16;
        writer.write_sample(value).unwrap();
    }
    writer.finalize().unwrap();
}

/// The song's sections: (id, label, the bar it starts at).
const SECTIONS: [(&str, &str, u64); 4] = [
    ("intro", "Intro", 1),
    ("verse", "Verse 1", 5),
    ("chorus", "Chorus", 9),
    ("outro", "Outro", 13),
];

/// "First Light": 16 bars at 120 BPM in 4/4, an intro (bar 1), Verse 1
/// (bar 5), a chorus (bar 9) and an outro (bar 13), 4 bars each. A keys
/// stem gets louder bar by bar; the click is loud and not in the waveform.
fn package() -> SongPackage {
    let marker = |id: &str, section_type, number, bar: u64| SectionMarker {
        id: id.to_owned(),
        section_type,
        number,
        start: (bar - 1) * BAR,
    };
    SongPackage {
        format_version: 1,
        title: "First Light".to_owned(),
        artist: None,
        sample_rate: RATE,
        key: Some("G".to_owned()),
        tempo_map: vec![TempoPoint { at: 0, bpm: 120.0 }],
        time_signature_map: vec![TimeSignaturePoint {
            at: 0,
            numerator: 4,
            denominator: 4,
        }],
        stems: vec![
            Stem {
                id: "keys".to_owned(),
                file: "keys.wav".to_owned(),
                part: "keys".to_owned(),
                role: StemRole::Track,
            },
            Stem {
                id: "click".to_owned(),
                file: "click.wav".to_owned(),
                part: "click".to_owned(),
                role: StemRole::Click,
            },
        ],
        sections: vec![
            marker("intro", SectionType::Intro, None, 1),
            marker("verse", SectionType::Verse, Some(1), 5),
            marker("chorus", SectionType::Chorus, None, 9),
            marker("outro", SectionType::Outro, None, 13),
        ],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: SECTIONS.iter().map(|(id, _, _)| (*id).to_owned()).collect(),
            infinite_loop: Vec::new(),
        }],
    }
}

/// Song folders in a temporary directory.
struct Bench {
    dir: tempfile::TempDir,
}

impl Bench {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// A song folder `name` holding "First Light" and its stems.
    fn song(&self, name: &str) -> PathBuf {
        let folder = self.dir.path().join(name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("song.json"), package().to_json().unwrap()).unwrap();
        wav(&folder.join("keys.wav"), level);
        wav(&folder.join("click.wav"), |_| 0.9);
        folder
    }

    /// A song folder `name` whose `song.json` lists stems it does not hold.
    fn broken(&self, name: &str) -> PathBuf {
        let folder = self.dir.path().join(name);
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("song.json"), package().to_json().unwrap()).unwrap();
        folder
    }
}

/// The song on show in `folder`, stopped, reads its waveform: its cache,
/// else a build from its stems; every job is run.
fn read<'a>(waves: &'a mut MapWaveform, folder: &Path) -> &'a SongWave {
    let mut jobs = 0;
    while let Some(job) = waves.show(Some(folder), TransportState::Stopped) {
        waves.finish(job.run());
        jobs += 1;
        assert!(jobs <= 2, "a cache read, then a build at most");
    }
    waves.wave(Some(folder)).expect("the waveform is read")
}

/// The overview of the song in `folder` built and cached, as the Song
/// editor or an earlier stop leaves it.
fn cache(folder: &Path) {
    read(&mut MapWaveform::new(), folder);
    assert!(library::load_overview(folder).is_some());
}

/// The transport states that play or ramp.
const PLAYING: [TransportState; 5] = [
    TransportState::Starting,
    TransportState::Playing,
    TransportState::Pausing,
    TransportState::Stopping,
    TransportState::Panicking,
];

/// "First Light" playing the sections `arrangement` (ids) in that order,
/// as the session describes it.
fn song(arrangement: &[&str]) -> SongInfo {
    let positions = arrangement
        .iter()
        .map(|id| {
            let &(id, label, bar) = SECTIONS
                .iter()
                .find(|(section, _, _)| section == id)
                .expect("a section of the song");
            PositionInfo {
                section_id: id.to_owned(),
                label: label.to_owned(),
                start_frame: (bar - 1) * BAR,
                length_frames: 4 * BAR,
                start_bar: bar,
            }
        })
        .collect();
    SongInfo {
        title: "First Light".to_owned(),
        artist: None,
        sample_rate: RATE,
        arrangement: "default".to_owned(),
        positions,
        warnings: Vec::new(),
    }
}

/// Playing `song` at `frames` past the start of position `position`.
fn at(song: &SongInfo, position: usize, frames: u64) -> Snapshot {
    let info = &song.positions[position];
    Snapshot {
        transport: TransportState::Playing,
        output_frame: 0,
        source_frame: info.start_frame + frames,
        bar: info.start_bar + frames / BAR,
        beat: 1,
        position,
        section: info.label.clone(),
        queued: None,
        jump_timing: JumpTiming::SectionEnd,
        infinite: false,
        fade: FadeView {
            state: FadeState::In,
            level: 1.0,
            duration: Duration::from_secs(3),
        },
        device: DeviceStatus::Connected(StreamInfo {
            device: "Speakers".to_owned(),
            device_id: "host:speakers".to_owned(),
            sample_rate: RATE,
            buffer_frames: 256,
            channels: 2,
        }),
        mixer: MixerView::default(),
        underruns: 0,
        decode_errors: 0,
        last_error: None,
    }
}

/// What `song.json` says of "First Light".
fn facts() -> SongFacts {
    SongFacts::from_manifest(&package().to_json().unwrap()).expect("the manifest parses")
}

/// The frames of source bars `bars` (counted from 0), one range each.
fn source_bars(bars: Range<u64>) -> Vec<Range<u64>> {
    bars.map(|bar| bar * BAR..(bar + 1) * BAR).collect()
}

/// How tall the song map draws a bar of `frames`.
fn height(overview: &Overview, frames: Range<u64>) -> f32 {
    let level = overview.range_level(frames) / overview.peak();
    MAP_WAVE_LOW + (MAP_WAVE_HIGH - MAP_WAVE_LOW) * level
}

fn close(actual: f32, expected: f64) {
    assert!(
        (f64::from(actual) - expected).abs() < 1e-6,
        "{actual} is not {expected}"
    );
}

#[test]
fn each_position_draws_one_bar_per_musical_bar_from_its_own_audio_a_repeat_included() {
    let bench = Bench::new();
    let folder = bench.song("First Light.song");
    let mut waves = MapWaveform::new();
    let wave = read(&mut waves, &folder);
    // The overview the job cached, built from the stems by the library.
    let overview = library::load_overview(&folder).expect("the job cached it");
    assert!(overview.peak() > 0.0);
    let long = song(&["intro", "verse", "chorus", "chorus", "outro"]);

    let view = PerformView::with_wave(&at(&long, 0, 0), &long, &facts(), Some(wave));

    let bars = &view.map.wave;
    let positions: Vec<usize> = bars.iter().map(|bar| bar.position).collect();
    let expected: Vec<usize> = (0..5).flat_map(|position| [position; 4]).collect();
    assert_eq!(positions, expected, "4 bars in each position's block");
    // The chorus plays twice: both show its own audio, bars 9 to 12.
    let sources: Vec<Range<u64>> = bars.iter().map(|bar| bar.source.clone()).collect();
    let expected: Vec<Range<u64>> = [0..4, 4..8, 8..12, 8..12, 12..16]
        .into_iter()
        .flat_map(source_bars)
        .collect();
    assert_eq!(sources, expected);
    let heights: Vec<f32> = bars.iter().map(|bar| bar.height).collect();
    let expected: Vec<f32> = sources
        .iter()
        .map(|frames| height(&overview, frames.clone()))
        .collect();
    assert_eq!(heights, expected);
    assert_eq!(heights[8..12], heights[12..16], "the same audio twice");
    assert!(heights[0] < heights[1], "louder bar by bar");
    assert!(
        (heights[19] - MAP_WAVE_HIGH).abs() < 1e-6,
        "the loudest bar"
    );
    assert!(heights.iter().all(|&h| h >= MAP_WAVE_LOW));
    let kinds: Vec<SectionKind> = bars.iter().map(|bar| bar.kind).collect();
    assert_eq!(
        kinds[3..9],
        [
            SectionKind::Neutral,
            SectionKind::Verse,
            SectionKind::Verse,
            SectionKind::Verse,
            SectionKind::Verse,
            SectionKind::Chorus
        ]
    );
    // 20 bars of equal length across the arrangement, each block's bars
    // filling it from its start.
    for (index, bar) in bars.iter().enumerate() {
        close(bar.start, index as f64 / 20.0);
        close(bar.width, 1.0 / 20.0);
    }
    for block in &view.map.blocks {
        let inside: Vec<_> = bars
            .iter()
            .filter(|bar| bar.position == block.position)
            .collect();
        close(inside[0].start, f64::from(block.start));
        let width: f32 = inside.iter().map(|bar| bar.width).sum();
        close(width, f64::from(block.width));
    }
}

#[test]
fn a_reordered_arrangement_shows_each_sections_audio_where_it_plays() {
    let bench = Bench::new();
    let folder = bench.song("First Light.song");
    let mut waves = MapWaveform::new();
    let wave = read(&mut waves, &folder);
    let overview = library::load_overview(&folder).unwrap();
    let reordered = song(&["chorus", "intro", "verse", "outro"]);

    let view = PerformView::with_wave(&at(&reordered, 0, 0), &reordered, &facts(), Some(wave));

    let sources: Vec<Range<u64>> = view.map.wave.iter().map(|bar| bar.source.clone()).collect();
    let expected: Vec<Range<u64>> = [8..12, 0..4, 4..8, 12..16]
        .into_iter()
        .flat_map(source_bars)
        .collect();
    assert_eq!(sources, expected, "the chorus first, then the intro");
    let heights: Vec<f32> = view.map.wave.iter().map(|bar| bar.height).collect();
    let chorus: Vec<f32> = source_bars(8..12)
        .into_iter()
        .map(|frames| height(&overview, frames))
        .collect();
    assert_eq!(heights[0..4], chorus);
    assert!(
        heights[0] > heights[4],
        "the chorus is louder than the intro"
    );
    assert_eq!(view.map.wave[0].kind, SectionKind::Chorus);
    assert_eq!(view.map.wave[4].kind, SectionKind::Neutral);
}

#[test]
fn a_section_that_starts_inside_a_bar_cuts_that_bar_where_it_starts() {
    let bench = Bench::new();
    let folder = bench.song("First Light.song");
    let mut waves = MapWaveform::new();
    let wave = read(&mut waves, &folder);
    let overview = library::load_overview(&folder).unwrap();
    // The verse snapped to the third beat of bar 5.
    let mut song = song(&["intro", "verse"]);
    let half = 4 * BAR + BAR / 2;
    song.positions[0].length_frames = half;
    song.positions[1].start_frame = half;
    song.positions[1].length_frames = 8 * BAR - half;

    let view = PerformView::with_wave(&at(&song, 0, 0), &song, &facts(), Some(wave));

    let sources: Vec<(usize, Range<u64>)> = view
        .map
        .wave
        .iter()
        .map(|bar| (bar.position, bar.source.clone()))
        .collect();
    let mut expected: Vec<(usize, Range<u64>)> =
        source_bars(0..4).into_iter().map(|bar| (0, bar)).collect();
    expected.extend([(0, 4 * BAR..half), (1, half..5 * BAR)]);
    expected.extend(source_bars(5..8).into_iter().map(|bar| (1, bar)));
    assert_eq!(sources, expected);
    let cut = &view.map.wave[4];
    close(cut.start, 4.0 / 8.0);
    close(cut.width, 0.5 / 8.0);
    assert_eq!(cut.height, height(&overview, 4 * BAR..half));
}

#[test]
fn bars_the_playhead_has_reached_are_opaque_and_bars_ahead_faded() {
    let bench = Bench::new();
    let folder = bench.song("First Light.song");
    let mut waves = MapWaveform::new();
    let wave = read(&mut waves, &folder);
    let default = song(&["intro", "verse", "chorus", "outro"]);
    let opacities = |snapshot: &Snapshot, song: &SongInfo| -> Vec<f32> {
        PerformView::with_wave(snapshot, song, &facts(), Some(wave))
            .map
            .wave
            .iter()
            .map(|bar| bar.opacity)
            .collect()
    };

    // Halfway into bar 6: bars 1 to 6 are played, the rest ahead.
    let expected: Vec<f32> = [1.0; 6].into_iter().chain([AHEAD_OPACITY; 10]).collect();
    assert_eq!(
        opacities(&at(&default, 1, BAR + BAR / 2), &default),
        expected
    );
    // At the start nothing is played.
    assert_eq!(
        opacities(&at(&default, 0, 0), &default),
        [AHEAD_OPACITY; 16]
    );
    // In play order: the second chorus just started, so the first one and
    // its own first bar are played, though they show the same audio.
    let long = song(&["intro", "verse", "chorus", "chorus", "outro"]);
    let expected: Vec<f32> = [1.0; 13].into_iter().chain([AHEAD_OPACITY; 7]).collect();
    assert_eq!(opacities(&at(&long, 3, 1), &long), expected);
}

#[test]
fn the_map_shows_its_blocks_until_the_waveform_is_read_or_when_it_cannot_be() {
    let bench = Bench::new();
    let folder = bench.song("First Light.song");
    let broken = bench.broken("Broken.song");
    let default = song(&["intro", "verse", "chorus", "outro"]);
    let snapshot = at(&default, 1, BAR);
    let before = PerformView::with_facts(&snapshot, &default, &facts());
    assert!(before.map.wave.is_empty());
    let shown = |waves: &MapWaveform, folder: &Path| {
        PerformView::with_wave(&snapshot, &default, &facts(), waves.wave(Some(folder)))
    };
    let mut waves = MapWaveform::new();

    // The cache is read first: there is none, the map is as before.
    let job = waves
        .show(Some(&folder), TransportState::Stopped)
        .expect("it reads the cache");
    assert!(!job.builds());
    assert_eq!(shown(&waves, &folder), before);
    let done = job.run();
    assert!(matches!(done.result(), Err(SongWaveError::NotCached)));
    assert!(!waves.finish(done));
    assert_eq!(shown(&waves, &folder), before);
    // Stopped, it builds one from the stems.
    let job = waves
        .show(Some(&folder), TransportState::Stopped)
        .expect("it builds");
    assert!(job.builds());
    assert_eq!(shown(&waves, &folder), before);
    assert!(waves.finish(job.run()));
    assert!(waves.wave(Some(&folder)).is_some());

    // A song whose stems cannot be read has none, and is not read again
    // while it shows.
    let job = waves.show(Some(&broken), TransportState::Stopped).unwrap();
    assert!(!waves.finish(job.run()));
    let job = waves.show(Some(&broken), TransportState::Stopped).unwrap();
    let done = job.run();
    assert!(matches!(done.result(), Err(SongWaveError::Overview(_))));
    assert!(!waves.finish(done), "the map does not change");
    assert_eq!(shown(&waves, &broken), before);
    assert!(waves.show(Some(&broken), TransportState::Stopped).is_none());
}

#[test]
fn another_song_on_show_cancels_the_build_of_the_last_and_its_late_result_is_ignored() {
    let bench = Bench::new();
    let first = bench.song("First Light.song");
    let second = bench.song("Second.song");
    let mut waves = MapWaveform::new();

    // The first song's build is cancelled before it starts: it builds
    // nothing.
    let job = waves.show(Some(&first), TransportState::Stopped).unwrap();
    waves.finish(job.run());
    let cancelled = waves.show(Some(&first), TransportState::Stopped).unwrap();
    assert!(cancelled.builds());
    let job = waves
        .show(Some(&second), TransportState::Stopped)
        .expect("the second song reads its own");
    let done = cancelled.run();
    assert!(matches!(
        done.result(),
        Err(SongWaveError::Overview(OverviewError::Cancelled))
    ));
    assert!(!waves.finish(done));
    assert_eq!(library::load_overview(&first), None, "nothing was built");
    waves.finish(job.run());
    read(&mut waves, &second);

    // A result that comes back once another song shows is ignored.
    cache(&first);
    let late = waves.show(Some(&first), TransportState::Stopped).unwrap();
    let done = late.run();
    assert!(
        done.result().is_ok(),
        "it ran before the second song showed"
    );
    let job = waves.show(Some(&second), TransportState::Stopped).unwrap();
    assert!(!waves.finish(done));
    assert_eq!(waves.wave(Some(&first)), None);
    assert_eq!(waves.wave(Some(&second)), None, "its own is still read");
    assert!(waves.finish(job.run()));
    assert!(waves.wave(Some(&second)).is_some());
    assert_eq!(waves.wave(Some(&first)), None, "only the song on show");
}

#[test]
fn a_setlist_item_change_reads_the_next_songs_waveform_and_an_open_reads_again() {
    let bench = Bench::new();
    let first = bench.song("First Light.song");
    let second = bench.song("Second.song");
    let mut waves = MapWaveform::new();
    read(&mut waves, &first);

    // The same song on show reads nothing again.
    assert!(waves.show(Some(&first), TransportState::Stopped).is_none());
    assert!(waves.wave(Some(&first)).is_some());
    // The setlist moves on to the next song: a new read.
    read(&mut waves, &second);
    // A click-only tile has no waveform.
    assert!(waves.show(None, TransportState::Stopped).is_none());
    assert_eq!(waves.wave(None), None);
    // Back to a song: read again.
    read(&mut waves, &first);
    // A song or setlist opens: its song.json may have changed.
    waves.forget();
    assert_eq!(waves.wave(Some(&first)), None);
    read(&mut waves, &first);
}

#[test]
fn no_build_starts_while_the_song_plays_or_ramps() {
    for transport in PLAYING {
        let bench = Bench::new();
        let folder = bench.song("First Light.song");
        let mut waves = MapWaveform::new();

        // The cache is read, and there is none.
        let job = waves
            .show(Some(&folder), transport)
            .expect("the cache is read whatever plays");
        assert!(!job.builds(), "{transport:?}");
        assert!(!waves.finish(job.run()));
        // No build while it plays: the map shows its blocks.
        assert!(
            waves.show(Some(&folder), transport).is_none(),
            "{transport:?}"
        );
        assert!(waves.show(Some(&folder), transport).is_none());
        assert_eq!(waves.wave(Some(&folder)), None);
        assert_eq!(library::load_overview(&folder), None, "nothing was built");
    }
}

#[test]
fn a_build_under_way_is_cancelled_when_play_starts_and_leaves_no_cache() {
    let bench = Bench::new();
    let folder = bench.song("First Light.song");
    let mut waves = MapWaveform::new();
    let job = waves.show(Some(&folder), TransportState::Stopped).unwrap();
    waves.finish(job.run());
    let build = waves.show(Some(&folder), TransportState::Stopped).unwrap();
    assert!(build.builds());

    // Play is pressed while it builds.
    assert!(
        waves
            .show(Some(&folder), TransportState::Starting)
            .is_none()
    );

    let done = build.run();
    assert!(matches!(
        done.result(),
        Err(SongWaveError::Overview(OverviewError::Cancelled))
    ));
    assert!(!waves.finish(done));
    assert_eq!(library::load_overview(&folder), None, "no partial cache");
    assert_eq!(waves.wave(Some(&folder)), None);
    assert!(waves.show(Some(&folder), TransportState::Playing).is_none());
}

#[test]
fn the_build_starts_once_the_transport_stops_or_pauses() {
    for (stop, wait) in [
        (TransportState::Stopped, TransportState::Stopping),
        (TransportState::Paused, TransportState::Pausing),
    ] {
        let bench = Bench::new();
        let folder = bench.song("First Light.song");
        let mut waves = MapWaveform::new();
        let job = waves.show(Some(&folder), TransportState::Playing).unwrap();
        waves.finish(job.run());
        // A build cancelled by play starts again at the next stop.
        assert!(waves.show(Some(&folder), stop).unwrap().builds());
        assert!(waves.show(Some(&folder), TransportState::Playing).is_none());
        assert!(waves.show(Some(&folder), wait).is_none(), "{wait:?}");

        let build = waves.show(Some(&folder), stop).expect("it builds now");
        assert!(build.builds());
        assert!(waves.finish(build.run()));
        assert!(waves.wave(Some(&folder)).is_some());
        assert!(library::load_overview(&folder).is_some(), "it is cached");
        assert!(waves.show(Some(&folder), TransportState::Playing).is_none());
        assert!(waves.wave(Some(&folder)).is_some(), "play keeps it");
    }
}

#[test]
fn a_cached_overview_shows_while_the_song_plays() {
    let bench = Bench::new();
    let folder = bench.song("First Light.song");
    cache(&folder);
    let default = song(&["intro", "verse", "chorus", "outro"]);
    let mut waves = MapWaveform::new();

    let job = waves
        .show(Some(&folder), TransportState::Playing)
        .expect("the cache is read while it plays");
    assert!(!job.builds());
    assert!(waves.finish(job.run()));

    let view = PerformView::with_wave(
        &at(&default, 1, BAR),
        &default,
        &facts(),
        waves.wave(Some(&folder)),
    );
    assert_eq!(view.map.wave.len(), 16);
}

#[test]
fn the_blocks_ruler_jump_and_playhead_stay_as_they_were_with_the_waveform() {
    let bench = Bench::new();
    let folder = bench.song("First Light.song");
    let mut waves = MapWaveform::new();
    let wave = read(&mut waves, &folder);
    let long = song(&["intro", "verse", "chorus", "chorus", "outro"]);
    let mut snapshot = at(&long, 1, BAR);
    snapshot.queued = Some(QueuedView {
        target: 3,
        target_label: "Chorus".to_owned(),
        kind: JumpKind::Jump,
        executes_at: 2 * BAR,
        frames_until: BAR,
        deferrals: 0,
    });

    let before = PerformView::with_facts(&snapshot, &long, &facts());
    let mut shown = PerformView::with_wave(&snapshot, &long, &facts(), Some(wave));

    assert_eq!(shown.map.wave.len(), 20);
    let ruler: Vec<Option<&str>> = shown
        .map
        .blocks
        .iter()
        .map(|block| block.ruler.as_deref())
        .collect();
    assert_eq!(
        ruler,
        [Some("1"), Some("5"), Some("9"), Some("13"), Some("17")]
    );
    assert!(shown.map.blocks[1].is_now && shown.map.blocks[3].is_queued);
    // The playhead 5 bars in of 20, the jump at the end of the verse.
    close(shown.map.playhead, 5.0 / 20.0);
    close(shown.map.jump.as_ref().unwrap().at, 6.0 / 20.0);
    assert_eq!(shown.map.blocks, before.map.blocks);
    assert_eq!(shown.map.playhead, before.map.playhead);
    assert_eq!(shown.map.jump, before.map.jump);
    shown.map.wave.clear();
    assert_eq!(shown, before, "everything else as without it");
}
