//! S3: two players on the audio thread. A `Deck` plays the current setlist
//! item, holds the next one cued, and hands over at the current item's end:
//! AutoLink on the next frame, Crossfade as a linear blend over the frames
//! before the end. Players it no longer needs go back to the control
//! thread.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::{SongSpec, bits, mono, noise, onsets, plan, stereo};
use domain::{SectionType, Song, StemRole, TimeSignature};
use engine::live::{
    Command, DECLICK_FRAMES, Deck, DeckController, Handoff, MemorySource, Player, PlayerConfig,
    StemSource, Transport, memory_sources,
};
use engine::{MixSettings, StemAudio, render_offline};

const RATE: u32 = 8_000;
/// −1 dBFS.
const PEAK: f32 = 0.891_250_9;
/// Five seconds at [`RATE`].
const BLEND: u64 = 5 * RATE as u64;

/// Song A: 3 s, two sections, noise on a mono and a stereo track.
fn song_a() -> (Song, Vec<StemAudio>) {
    let frames = 24_000;
    let song = SongSpec::new(
        RATE,
        frames,
        vec![
            ("intro", SectionType::Intro, 0),
            ("verse", SectionType::Verse, 12_000),
        ],
        vec![("drums", StemRole::Track), ("keys", StemRole::Track)],
    )
    .build();
    let stems = vec![
        mono("drums", StemRole::Track, noise(1, frames as usize)),
        stereo("keys", StemRole::Track, noise(2, 2 * frames as usize)),
    ];
    (song, stems)
}

/// Song B: 2.5 s, two sections, other noise.
fn song_b() -> (Song, Vec<StemAudio>) {
    let frames = 20_000;
    let song = SongSpec::new(
        RATE,
        frames,
        vec![
            ("chorus", SectionType::Chorus, 0),
            ("outro", SectionType::Outro, 10_000),
        ],
        vec![("bass", StemRole::Track)],
    )
    .build();
    let stems = vec![mono("bass", StemRole::Track, noise(7, frames as usize))];
    (song, stems)
}

/// A 7-second song whose output is `track` on both channels, plus `guide`:
/// a silent click stem replaces the generated click, so the song peaks at
/// exactly `track + guide`.
fn steady(track: f32, guide: f32) -> (Song, Vec<StemAudio>) {
    let frames = 56_000;
    let song = SongSpec::new(
        RATE,
        frames,
        vec![("verse", SectionType::Verse, 0)],
        vec![
            ("track", StemRole::Track),
            ("guide", StemRole::Guide),
            ("click", StemRole::Click),
        ],
    )
    .build();
    let n = frames as usize;
    let stems = vec![
        mono("track", StemRole::Track, vec![track; n]),
        mono("guide", StemRole::Guide, vec![guide; n]),
        mono("click", StemRole::Click, vec![0.0; n]),
    ];
    (song, stems)
}

fn player(song: &Song, stems: Vec<StemAudio>, max_frames: usize) -> Player {
    let (player, _controller) = Player::new(
        song,
        "default",
        &MixSettings::default(),
        &PlayerConfig::new(RATE, max_frames),
        memory_sources(stems).unwrap(),
    )
    .unwrap();
    player
}

fn offline(song: &Song, stems: &[StemAudio]) -> Vec<f32> {
    render_offline(&plan(song, "default"), stems, &MixSettings::default()).unwrap()
}

/// Renders `frames` frames through `deck` in buffers of `buffer` frames.
fn run(deck: &mut Deck, buffer: usize, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(2 * frames);
    let mut chunk = vec![0.0f32; 2 * buffer];
    while out.len() < 2 * frames {
        deck.process(&mut chunk);
        out.extend_from_slice(&chunk);
    }
    out.truncate(2 * frames);
    out
}

fn left(out: &[f32], frame: u64) -> f32 {
    out[2 * frame as usize]
}

fn deck_of(song: &(Song, Vec<StemAudio>), buffer: usize) -> (Deck, DeckController) {
    Deck::new(player(&song.0, song.1.clone(), buffer), 1, buffer)
}

#[test]
fn a_deck_with_one_item_plays_exactly_what_its_player_plays() {
    let a = song_a();
    let buffer = 256;
    let (mut alone, mut controller) = Player::new(
        &a.0,
        "default",
        &MixSettings::default(),
        &PlayerConfig::new(RATE, buffer),
        memory_sources(a.1.clone()).unwrap(),
    )
    .unwrap();
    let (mut deck, mut control) = deck_of(&a, buffer);
    let script = [
        (0, Command::Play),
        (5, Command::QueueJump { position: 1 }),
        (40, Command::ToggleFade),
        (60, Command::BackToStart),
    ];
    let mut expected = Vec::new();
    let mut played = Vec::new();
    let mut chunk = vec![0.0f32; 2 * buffer];
    for index in 0..120 {
        for (_, command) in script.iter().filter(|(at, _)| *at == index) {
            controller.send(*command).unwrap();
            control.send(*command).unwrap();
        }
        alone.process(&mut chunk);
        expected.extend_from_slice(&chunk);
        deck.process(&mut chunk);
        played.extend_from_slice(&chunk);
    }
    assert_eq!(bits(&played), bits(&expected));
    assert_eq!(deck.status().current, alone.status());
    assert_eq!(deck.applied_messages(), script.len() as u64);
}

#[test]
fn autolink_starts_the_cued_item_on_the_frame_after_the_current_one_ends() {
    let (a, b) = (song_a(), song_b());
    let expected_a = offline(&a.0, &a.1);
    let expected_b = offline(&b.0, &b.1);
    let (la, lb) = (expected_a.len() / 2, expected_b.len() / 2);
    for buffer in [1, 7, 256, 1_000] {
        let (mut deck, mut control) = deck_of(&a, buffer);
        control
            .cue(player(&b.0, b.1.clone(), buffer), 2, Handoff::AutoLink)
            .unwrap();
        control.send(Command::Play).unwrap();
        let out = run(&mut deck, buffer, la + lb + 500);

        assert_eq!(
            bits(&out[..2 * la]),
            bits(&expected_a),
            "A plays to its last frame, {la} (buffer {buffer})"
        );
        assert_eq!(
            bits(&out[2 * la..2 * (la + lb)]),
            bits(&expected_b),
            "B's first frame is frame {la}, right after A's last (buffer {buffer})"
        );
        assert!(out[2 * (la + lb)..].iter().all(|&s| s == 0.0));
        let status = deck.status();
        assert_eq!(status.item, 2, "B is the current item");
        assert_eq!(status.cued, None);
        assert_eq!(status.finished, 1, "B ended with nothing cued");
        assert_eq!(status.finished_item, Some(2));
        assert!(control.returned().is_some(), "A comes back to be dropped");
        assert!(control.returned().is_none());
    }
}

#[test]
fn a_crossfade_blends_linearly_over_the_last_frames_of_the_current_item() {
    let (a, b) = (steady(PEAK, 0.0), steady(-PEAK, 0.0));
    let la = a.0.length();
    let start = la - BLEND;
    let (mut deck, mut control) = deck_of(&a, 333);
    control
        .cue(
            player(&b.0, b.1.clone(), 333),
            2,
            Handoff::Crossfade { frames: BLEND },
        )
        .unwrap();
    control.send(Command::Play).unwrap();
    let out = run(&mut deck, 333, (start + b.0.length() + 1_000) as usize);

    for frame in 0..start {
        assert_eq!(
            left(&out, frame),
            PEAK,
            "A alone before the blend, frame {frame}"
        );
    }
    for t in 0..BLEND {
        let gain = (t as f64 + 0.5) / BLEND as f64;
        let expected = f64::from(PEAK) * (1.0 - gain) - f64::from(PEAK) * gain;
        let got = f64::from(left(&out, start + t));
        assert!(
            (got - expected).abs() < 1e-6,
            "blend frame {t}: {got} instead of {expected}"
        );
    }
    let end = start + b.0.length();
    for frame in la..end {
        assert_eq!(
            left(&out, frame),
            -PEAK,
            "B alone after the blend, frame {frame}"
        );
    }
    assert!((end..end + 1_000).all(|frame| left(&out, frame) == 0.0));
    assert_eq!(deck.status().item, 2);
    assert!(control.returned().is_some(), "A comes back after the blend");
}

#[test]
fn a_crossfade_of_two_songs_peaking_at_minus_1_dbfs_stays_below_0_dbfs() {
    let (a, b) = (steady(PEAK, 0.0), steady(PEAK, 0.0));
    let la = a.0.length();
    let (mut deck, mut control) = deck_of(&a, 256);
    control
        .cue(
            player(&b.0, b.1.clone(), 256),
            2,
            Handoff::Crossfade { frames: BLEND },
        )
        .unwrap();
    control.send(Command::Play).unwrap();
    let mut blend_seen = None;
    let mut out = Vec::new();
    let mut chunk = vec![0.0f32; 512];
    while out.len() < 2 * (la + b.0.length() - BLEND) as usize {
        deck.process(&mut chunk);
        out.extend_from_slice(&chunk);
        blend_seen = blend_seen.or(deck.status().blend);
    }
    let peak = out.iter().fold(0.0f32, |peak, &s| peak.max(s.abs()));
    assert!(peak < 1.0, "the blend peaks at {peak}, over 0 dBFS");
    assert!(
        peak <= PEAK * (1.0 + 1e-6),
        "the blend never adds up: {peak}"
    );
    let blend = blend_seen.expect("a blend ran");
    assert_eq!(
        (blend.from, blend.frames),
        (1, BLEND),
        "five seconds from A"
    );
}

#[test]
fn pausing_during_a_crossfade_holds_both_items_and_the_blend() {
    let (a, b) = (steady(PEAK, 0.0), steady(-PEAK, 0.0));
    let (mut deck, mut control) = deck_of(&a, 100);
    control
        .cue(
            player(&b.0, b.1.clone(), 100),
            2,
            Handoff::Crossfade { frames: BLEND },
        )
        .unwrap();
    control.send(Command::Play).unwrap();
    run(&mut deck, 100, (a.0.length() - BLEND + 4_000) as usize);
    let before = deck.status().blend.expect("the blend runs").played;
    assert_eq!(before, 4_000);

    control.send(Command::Pause).unwrap();
    let ramp = run(&mut deck, 100, 300);
    assert!(ramp[..2 * 200].iter().any(|&s| s != 0.0), "both ramp out");
    let held = deck.status().blend.expect("the blend holds").played;
    assert_eq!(held, before + DECLICK_FRAMES, "the blend follows the ramp");
    let silent = run(&mut deck, 100, 5_000);
    assert!(silent.iter().all(|&s| s == 0.0));
    assert_eq!(deck.status().blend.unwrap().played, held);
    assert!(control.returned().is_none(), "A is still in the blend");

    control.send(Command::Play).unwrap();
    run(&mut deck, 100, (BLEND - held + 100) as usize);
    assert_eq!(deck.status().blend, None, "the blend finished");
    assert!(control.returned().is_some());
    assert_eq!(deck.status().current.transport, Transport::Playing);
}

#[test]
fn the_end_fade_silences_the_tracks_on_the_last_frame_while_the_guide_plays_on() {
    let (song, stems) = steady(0.5, 0.25);
    let length = song.length();
    let fade = BLEND;
    let mut player = player(&song, stems, 256);
    player.set_end_fade(Some(fade));
    let (mut deck, mut control) = Deck::new(player, 1, 256);
    control.send(Command::Play).unwrap();
    let out = run(&mut deck, 256, length as usize + 500);

    for frame in 0..length - fade {
        assert_eq!(
            left(&out, frame),
            0.75,
            "full level before the fade, frame {frame}"
        );
    }
    for i in 0..fade {
        let track = 0.5 * (1.0 - (i + 1) as f64 / fade as f64);
        let got = f64::from(left(&out, length - fade + i));
        assert!((got - (track + 0.25)).abs() < 1e-6, "fade frame {i}: {got}");
    }
    assert_eq!(
        left(&out, length - 1),
        0.25,
        "the tracks are silent on the last frame"
    );
    assert!((length..length + 500).all(|frame| left(&out, frame) == 0.0));
    assert_eq!(deck.status().current.transport, Transport::Stopped);
}

fn click_tile(bars: Option<u32>) -> Player {
    let tile = Song::click_only(
        RATE,
        120.0,
        TimeSignature {
            numerator: 4,
            denominator: 4,
        },
        bars,
    )
    .unwrap();
    let (mut player, _controller) = Player::new(
        &tile,
        "default",
        &MixSettings::default(),
        &PlayerConfig::new(RATE, 256),
        Vec::new(),
    )
    .unwrap();
    player.set_infinite_at_start(bars.is_none());
    player
}

/// 120 bpm in 4/4 at 8 kHz: a beat is 4 000 frames, a bar 16 000.
const BAR: u64 = 16_000;

#[test]
fn a_click_tile_with_a_bar_count_plays_that_many_bars_then_ends() {
    let (mut deck, mut control) = Deck::new(click_tile(Some(2)), 1, 256);
    control.send(Command::Play).unwrap();
    let out = run(&mut deck, 256, 3 * BAR as usize);
    let beats: Vec<u64> = (0..8).map(|beat| beat * 4_000).collect();
    assert_eq!(onsets(&out), beats, "eight beats, then silence");
    let status = deck.status();
    assert_eq!(status.current.transport, Transport::Stopped);
    assert_eq!((status.finished, status.finished_item), (1, Some(1)));
}

#[test]
fn a_click_tile_without_a_bar_count_repeats_its_bar_even_after_a_stop() {
    let (mut deck, mut control) = Deck::new(click_tile(None), 1, 256);
    control.send(Command::Play).unwrap();
    let out = run(&mut deck, 256, 5 * BAR as usize);
    let beats: Vec<u64> = (0..20).map(|beat| beat * 4_000).collect();
    assert_eq!(onsets(&out), beats, "the bar repeats");
    assert_eq!(deck.status().current.transport, Transport::Playing);
    assert!(deck.status().current.infinite);

    control.send(Command::Stop).unwrap();
    run(&mut deck, 256, 1_000);
    control.send(Command::Play).unwrap();
    run(&mut deck, 256, 3 * BAR as usize);
    let status = deck.status();
    assert_eq!(
        status.current.transport,
        Transport::Playing,
        "it loops again"
    );
    assert!(status.current.infinite);
    assert_eq!(status.finished, 0);
}

#[test]
fn selecting_while_playing_ramps_the_current_item_out_and_stops_at_the_new_one() {
    let (a, b) = (song_a(), song_b());
    let expected_b = offline(&b.0, &b.1);
    let (mut deck, mut control) = deck_of(&a, 64);
    control.send(Command::Play).unwrap();
    run(&mut deck, 64, 5_000);

    control.select(player(&b.0, b.1.clone(), 64), 2).unwrap();
    let ramp = run(&mut deck, 64, 1_000);
    let last = (0..1_000).rev().find(|&f| ramp[2 * f] != 0.0).unwrap();
    assert!(
        last < DECLICK_FRAMES as usize,
        "A ramps out within the declick"
    );
    assert!(
        last > DECLICK_FRAMES as usize / 2,
        "A ramps rather than cuts"
    );
    let status = deck.status();
    assert_eq!(status.item, 2);
    assert_eq!(status.current.transport, Transport::Stopped);
    assert_eq!(status.current.output_frame, 0, "B waits at its start");
    assert_eq!(status.leaving, 0);
    assert!(control.returned().is_some(), "A comes back once silent");

    control.send(Command::Play).unwrap();
    let played = run(&mut deck, 64, expected_b.len() / 2);
    assert_eq!(bits(&played), bits(&expected_b), "B plays from its start");
}

#[test]
fn an_item_with_a_stop_handoff_ends_stopped_and_its_cued_item_can_be_selected() {
    let (a, b) = (song_a(), song_b());
    let expected_b = offline(&b.0, &b.1);
    let (mut deck, mut control) = deck_of(&a, 256);
    control
        .cue(player(&b.0, b.1.clone(), 256), 2, Handoff::Stop)
        .unwrap();
    control.send(Command::Play).unwrap();
    let out = run(&mut deck, 256, a.0.length() as usize + 2_000);
    let la = a.0.length() as usize;
    assert!(out[2 * la..].iter().all(|&s| s == 0.0), "nothing follows A");
    let status = deck.status();
    assert_eq!((status.item, status.cued), (1, Some(2)));
    assert_eq!((status.finished, status.finished_item), (1, Some(1)));
    assert_eq!(status.current.transport, Transport::Stopped);

    control.select_cued().unwrap();
    run(&mut deck, 256, 10);
    let status = deck.status();
    assert_eq!((status.item, status.cued), (2, None));
    assert!(control.returned().is_some(), "A comes back");
    control.send(Command::Play).unwrap();
    let played = run(&mut deck, 256, expected_b.len() / 2);
    assert_eq!(bits(&played), bits(&expected_b));
}

#[test]
fn an_uncued_item_comes_back_and_the_current_one_just_ends() {
    let (a, b) = (song_a(), song_b());
    let (mut deck, mut control) = deck_of(&a, 256);
    control
        .cue(player(&b.0, b.1.clone(), 256), 2, Handoff::AutoLink)
        .unwrap();
    control.uncue().unwrap();
    control.send(Command::Play).unwrap();
    let out = run(&mut deck, 256, a.0.length() as usize + 2_000);
    assert!(control.returned().is_some(), "B comes back unplayed");
    let la = a.0.length() as usize;
    assert!(out[2 * la..].iter().all(|&s| s == 0.0));
    assert_eq!(deck.status().item, 1);
}

#[test]
fn the_operators_settings_reach_the_cued_item_and_nothing_else_does() {
    let (a, b) = (song_a(), song_b());
    let (mut deck, mut control) = deck_of(&a, 256);
    control
        .cue(player(&b.0, b.1.clone(), 256), 2, Handoff::AutoLink)
        .unwrap();
    control
        .send(Command::SetJumpTiming(engine::live::JumpTiming::NextBar))
        .unwrap();
    control.send(Command::SetInfinite(true)).unwrap();
    control.send(Command::SetInfinite(false)).unwrap();
    control
        .send(Command::SetFadeDuration { frames: 1_000 })
        .unwrap();
    control.send(Command::Play).unwrap();
    run(&mut deck, 256, a.0.length() as usize + 10);
    let status = deck.status();
    assert_eq!(status.item, 2, "B followed on");
    assert_eq!(
        status.current.jump_timing,
        engine::live::JumpTiming::NextBar
    );
    control.send(Command::ToggleFade).unwrap();
    run(&mut deck, 256, 2_000);
    assert_eq!(
        deck.status().current.fade.state,
        engine::live::FadeState::Out,
        "B took the 1 000-frame fade"
    );
}

#[test]
fn selecting_during_a_crossfade_ramps_both_items_out_at_the_levels_they_had() {
    let (a, b) = (steady(PEAK, 0.0), steady(PEAK, 0.0));
    let c = song_b();
    let expected_c = offline(&c.0, &c.1);
    let (mut deck, mut control) = deck_of(&a, 64);
    control
        .cue(
            player(&b.0, b.1.clone(), 64),
            2,
            Handoff::Crossfade { frames: BLEND },
        )
        .unwrap();
    control.send(Command::Play).unwrap();
    run(&mut deck, 64, (a.0.length() - BLEND / 2) as usize);
    assert!(deck.status().blend.is_some(), "half way through the blend");

    control.select(player(&c.0, c.1.clone(), 64), 3).unwrap();
    let ramp = run(&mut deck, 64, 1_000);
    // The two levels still sum to the full one; the stop ramp's first step
    // takes 1/256 off it.
    let first = f64::from(left(&ramp, 0));
    let step = f64::from(PEAK) * (1.0 - 1.0 / DECLICK_FRAMES as f64);
    assert!((first - step).abs() < 1e-5, "no jump in level: {first}");
    assert!(
        (0..1_000).all(|frame| left(&ramp, frame).abs() <= PEAK * (1.0 + 1e-6)),
        "the two ramps never add up"
    );
    let last = (0..1_000).rev().find(|&f| left(&ramp, f) != 0.0).unwrap();
    assert!(last < DECLICK_FRAMES, "both ramp out within the declick");
    let status = deck.status();
    assert_eq!((status.item, status.blend, status.leaving), (3, None, 0));
    assert!(control.returned().is_some() && control.returned().is_some());

    control.send(Command::Play).unwrap();
    let played = run(&mut deck, 64, expected_c.len() / 2);
    assert_eq!(bits(&played), bits(&expected_c), "C plays from its start");
}

/// A two-section song of `frames` frames, noise on two tracks.
fn song_len(frames: u64, seed: u64) -> (Song, Vec<StemAudio>) {
    let song = SongSpec::new(
        RATE,
        frames,
        vec![
            ("intro", SectionType::Intro, 0),
            ("verse", SectionType::Verse, frames / 2),
        ],
        vec![("drums", StemRole::Track), ("keys", StemRole::Track)],
    )
    .build();
    let stems = vec![
        mono("drums", StemRole::Track, noise(seed, frames as usize)),
        stereo(
            "keys",
            StemRole::Track,
            noise(seed + 1, 2 * frames as usize),
        ),
    ];
    (song, stems)
}

/// A click tile's player and its length.
fn click_player(bars: Option<u32>, buffer: usize) -> (Player, u64) {
    let click = Song::click_only(
        RATE,
        120.0,
        TimeSignature {
            numerator: 4,
            denominator: 4,
        },
        bars,
    )
    .unwrap();
    let (mut player, _controller) = Player::new(
        &click,
        "default",
        &MixSettings::default(),
        &PlayerConfig::new(RATE, buffer),
        Vec::new(),
    )
    .unwrap();
    player.set_infinite_at_start(bars.is_none());
    (player, click.length())
}

fn first_diff(a: &[f32], b: &[f32]) -> Option<usize> {
    a.iter()
        .zip(b)
        .position(|(x, y)| x.to_bits() != y.to_bits())
        .map(|i| i / 2)
}

/// A memory source whose `is_ready` follows a shared flag, to hold a jump
/// target back.
struct Gated {
    inner: MemorySource,
    open: Arc<AtomicBool>,
}

impl StemSource for Gated {
    fn id(&self) -> &str {
        self.inner.id()
    }
    fn role(&self) -> StemRole {
        self.inner.role()
    }
    fn read(&mut self, at: u64, out: &mut [f32]) -> bool {
        self.inner.read(at, out)
    }
    fn is_ready(&mut self, at: u64, frames: u64) -> bool {
        self.open.load(Ordering::SeqCst) && self.inner.is_ready(at, frames)
    }
    fn prefetch(&mut self, playhead: u64, targets: &[u64]) {
        self.inner.prefetch(playhead, targets);
    }
}

#[derive(Clone)]
struct Probes {
    open: Arc<AtomicBool>,
}

impl Probes {
    fn new() -> Self {
        Self {
            open: Arc::new(AtomicBool::new(true)),
        }
    }

    /// A player whose sources are all gated by [`Self::open`].
    fn player(&self, song: &(Song, Vec<StemAudio>), max_frames: usize) -> Player {
        let sources: Vec<Box<dyn StemSource>> = song
            .1
            .iter()
            .cloned()
            .map(|stem| {
                Box::new(Gated {
                    inner: MemorySource::new(stem).unwrap(),
                    open: Arc::clone(&self.open),
                }) as Box<dyn StemSource>
            })
            .collect();
        let (player, _controller) = Player::new(
            &song.0,
            "default",
            &MixSettings::default(),
            &PlayerConfig::new(RATE, max_frames),
            sources,
        )
        .unwrap();
        player
    }
}

/// a panic in the last half second of an item with an AutoLink cue
/// must not start the next item.
#[test]
fn a_panic_near_the_end_does_not_start_the_autolinked_item() {
    let buffer = 256;
    let a = song_len(24_000, 3);
    let b = song_len(24_000, 70);
    let (mut deck, mut control) = Deck::new(player(&a.0, a.1.clone(), buffer), 1, buffer);
    control
        .cue(player(&b.0, b.1.clone(), buffer), 2, Handoff::AutoLink)
        .unwrap();
    control.send(Command::Play).unwrap();
    // Play to 2 000 frames before A's end (panic takes 4 000 frames).
    let _ = run(&mut deck, buffer, 22_016);
    control.send(Command::Panic).unwrap();
    let after = run(&mut deck, buffer, 16_000);
    let status = deck.status();
    let loud_after = after[2 * 6_000..].iter().filter(|s| s.abs() > 0.0).count();
    assert_eq!(
        (status.item, status.current.transport, loud_after),
        (1, Transport::Stopped, 0),
        "after a panic, B started: item {} {:?}, {} non-zero samples",
        status.item,
        status.current.transport,
        loud_after
    );
}

/// a stop in the last DECLICK frames of an item with an AutoLink cue
/// must not start the next item.
#[test]
fn a_stop_or_pause_near_the_end_does_not_start_the_autolinked_item() {
    let buffer = 64;
    let a = song_len(24_000, 3);
    let b = song_len(24_000, 70);
    for (name, command) in [("stop", Command::Stop), ("pause", Command::Pause)] {
        let (mut deck, mut control) = Deck::new(player(&a.0, a.1.clone(), buffer), 1, buffer);
        control
            .cue(player(&b.0, b.1.clone(), buffer), 2, Handoff::AutoLink)
            .unwrap();
        control.send(Command::Play).unwrap();
        let _ = run(&mut deck, buffer, 24_000 - 128);
        control.send(command).unwrap();
        let after = run(&mut deck, buffer, 4_000);
        let status = deck.status();
        let loud = after[2 * 1_000..].iter().filter(|s| s.abs() > 0.0).count();
        assert_eq!(
            (status.item, loud),
            (1, 0),
            "{name} 128 frames before the end: item {} {:?}, {} non-zero samples after",
            status.item,
            status.current.transport,
            loud
        );
    }
}

/// a live loop of the last section deferred for its audio at the end:
/// A ends where it would, so B should start on the next frame.
#[test]
fn a_deferred_loop_at_the_end_keeps_autolink_sample_exact() {
    let buffer = 256;
    let a = song_len(24_000, 3);
    let b = song_len(9_000, 70);
    let (oa, ob) = (offline(&a.0, &a.1), offline(&b.0, &b.1));
    let probes = Probes::new();
    let (mut deck, mut control) = Deck::new(probes.player(&a, buffer), 1, buffer);
    control
        .cue(player(&b.0, b.1.clone(), buffer), 2, Handoff::AutoLink)
        .unwrap();
    control.send(Command::Play).unwrap();
    let mut out = run(&mut deck, buffer, 20_480);
    control.send(Command::LiveLoop).unwrap();
    probes.open.store(false, Ordering::SeqCst); // the loop target is not ready
    out.extend(run(&mut deck, buffer, 24_000));
    let mut expected = oa.clone();
    expected.extend_from_slice(&ob);
    expected.resize(out.len(), 0.0);
    let diff = first_diff(&out, &expected);
    let b_first = (0..out.len() / 2).find(|&f| f >= 24_000 && out[2 * f] != 0.0);
    assert_eq!(
        diff, None,
        "A ends at frame 24000; B's first sample landed at {b_first:?}"
    );
}

/// P10: a crossfade into a click tile shorter than the blend (a 2-bar
/// count-in, 4 s) that AutoLinks into the next song: the song should start
/// on the frame after the tile's last.
#[test]
fn autolink_out_of_a_short_item_inside_a_crossfade_is_sample_exact() {
    let buffer = 256;
    let a = song_len(56_000, 3);
    let c = song_len(20_000, 90);
    let oc = offline(&c.0, &c.1);
    let (tile, lt) = click_player(Some(2), buffer); // 32 000 frames
    let (mut deck, mut control) = Deck::new(player(&a.0, a.1.clone(), buffer), 1, buffer);
    control
        .cue(tile, 2, Handoff::Crossfade { frames: 40_000 })
        .unwrap();
    control.send(Command::Play).unwrap();
    let mut out = Vec::new();
    let mut chunk = vec![0.0f32; 2 * buffer];
    let mut c_cued = false;
    while out.len() < 2 * 80_000 {
        deck.process(&mut chunk);
        out.extend_from_slice(&chunk);
        if !c_cued && deck.status().item == 2 {
            control
                .cue(player(&c.0, c.1.clone(), buffer), 3, Handoff::AutoLink)
                .unwrap();
            c_cued = true;
        }
        while control.returned().is_some() {}
    }
    // The tile starts at 16 000 and ends after 32 000 frames: C's first
    // frame belongs at 48 000. A's own end is 56 000.
    let tile_end = 16_000 + lt as usize;
    let c_at = (0..80_000).find(|&f| {
        out[2 * f..2 * f + 2 * 1_000]
            .iter()
            .zip(&oc[..2 * 1_000])
            .all(|(x, y)| (x - y).abs() < 0.3)
            && f > 40_000
    });
    assert_eq!(c_at, Some(tile_end), "C starts on the frame after the tile");
}

#[test]
fn a_player_with_another_channel_count_is_refused_with_an_error() {
    let (a, b) = (song_a(), song_b());
    let (mut deck, mut control) = deck_of(&a, 256);
    let wide = || {
        let (player, _controller) = Player::new(
            &b.0,
            "default",
            &MixSettings::default(),
            &PlayerConfig {
                channels: 8,
                ..PlayerConfig::new(RATE, 256)
            },
            memory_sources(b.1.clone()).unwrap(),
        )
        .unwrap();
        player
    };
    let mismatch = engine::live::DeckError::ChannelMismatch { deck: 2, player: 8 };
    assert_eq!(control.cue(wide(), 2, Handoff::AutoLink), Err(mismatch));
    assert_eq!(control.select(wide(), 3), Err(mismatch));
    assert!(mismatch.to_string().contains("8"), "{mismatch}");

    // Nothing reached the deck: A plays on alone.
    control.send(Command::Play).unwrap();
    let out = run(&mut deck, 256, 2_000);
    assert!(out.iter().any(|&s| s != 0.0));
    let status = deck.status();
    assert_eq!((status.item, status.cued), (1, None));
}
