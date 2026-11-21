//! Level meters in the live player (T6b): the peak of every stem after its
//! gain, of every bus, and of the master, per side, published with the
//! status. The stems hold constant levels, powers of two apart, so every
//! peak is exact whatever the order the signal sums in.

mod common;

use common::{SongSpec, mono, stereo};
use domain::{Bus, Routing, SectionType, Song, StemRole};
use engine::live::{
    Command, Controller, Gain, MAX_METERED_STEMS, METER_WINDOW_MILLISECONDS, Meters, Player,
    PlayerConfig, Routes, memory_sources,
};
use engine::{MixSettings, StemAudio};

const RATE: u32 = 8_000;
const FRAMES: usize = 32_000;
/// [`METER_WINDOW_MILLISECONDS`] at [`RATE`]: 400 frames.
const WINDOW: usize = (RATE as u64 * METER_WINDOW_MILLISECONDS / 1_000) as usize;

/// Drums (Drums bus) at 0.5 in mono, keys (Keys & Pads) at 0.25 left and
/// 0.125 right, the guide (Click & Guide) at 0.0625, and a silent click
/// stem, so the generated click is off.
fn band() -> (Song, Vec<StemAudio>) {
    let song = SongSpec::new(
        RATE,
        FRAMES as u64,
        vec![("verse", SectionType::Verse, 0)],
        vec![
            ("drums", StemRole::Track),
            ("keys", StemRole::Track),
            ("guide", StemRole::Guide),
            ("click", StemRole::Click),
        ],
    )
    .build();
    let stems = vec![
        mono("drums", StemRole::Track, vec![0.5; FRAMES]),
        stereo("keys", StemRole::Track, [0.25, 0.125].repeat(FRAMES)),
        mono("guide", StemRole::Guide, vec![0.0625; FRAMES]),
        mono("click", StemRole::Click, vec![0.0; FRAMES]),
    ];
    (song, stems)
}

fn player(song: &Song, stems: Vec<StemAudio>, channels: u16) -> (Player, Controller) {
    Player::new(
        song,
        "default",
        &MixSettings::default(),
        &PlayerConfig {
            channels,
            ..PlayerConfig::new(RATE, 256)
        },
        memory_sources(stems).unwrap(),
    )
    .unwrap()
}

/// Renders `frames` frames in callbacks of 256.
fn render(player: &mut Player, channels: u16, frames: usize) {
    let mut out = vec![0.0f32; usize::from(channels) * 256];
    let mut left = frames;
    while left > 0 {
        let n = left.min(256);
        player.process(&mut out[..usize::from(channels) * n]);
        left -= n;
    }
}

fn bus(meters: &Meters, bus: Bus) -> [f32; 2] {
    meters.buses[bus.index()]
}

#[test]
fn every_stem_bus_and_the_master_read_their_peak_after_the_gains() {
    let split = Routing {
        split_mode: true,
        ..Routing::preset(2)
    };
    // The plain stereo path, the bus path, split mode and the mono fold:
    // the meters read the buses as they reach them, before the router.
    let paths = [
        ("plain", 2, Routes::STEREO),
        (
            "eight outputs",
            8,
            Routes::from_routing(&Routing::preset(8)),
        ),
        ("split mode", 2, Routes::from_routing(&split)),
        ("mono fold", 1, Routes::from_routing(&Routing::preset(1))),
    ];
    for (path, channels, routes) in paths {
        let (song, stems) = band();
        let (mut player, mut controller) = player(&song, stems, channels);
        player.set_routes(routes);
        controller
            .send(Command::SetStemGain {
                stem: 0,
                gain: Gain::new(0.5),
            })
            .unwrap();
        controller.send(Command::Play).unwrap();
        render(&mut player, channels, 2_048);

        let meters = player.status().meters;
        assert_eq!(meters.stems[0], [0.25, 0.25], "{path}: drums at half");
        assert_eq!(meters.stems[1], [0.25, 0.125], "{path}: keys, per side");
        assert_eq!(meters.stems[2], [0.0625, 0.0625], "{path}: the guide");
        assert_eq!(meters.stems[3], [0.0, 0.0], "{path}: the silent click");
        assert!(
            meters.stems[4..].iter().all(|&peak| peak == [0.0, 0.0]),
            "{path}: no stem past the song's"
        );
        assert_eq!(bus(&meters, Bus::Drums), [0.25, 0.25], "{path}");
        assert_eq!(bus(&meters, Bus::KeysPads), [0.25, 0.125], "{path}");
        assert_eq!(bus(&meters, Bus::ClickGuide), [0.0625, 0.0625], "{path}");
        for silent in [Bus::Bass, Bus::Guitars, Bus::Vocals] {
            assert_eq!(bus(&meters, silent), [0.0, 0.0], "{path}: {silent}");
        }
        assert_eq!(meters.master, [0.5625, 0.4375], "{path}: every bus summed");

        // A muted stem (gain 0) reads silence, and so does its bus.
        controller
            .send(Command::SetStemGain {
                stem: 1,
                gain: Gain::new(0.0),
            })
            .unwrap();
        render(&mut player, channels, 3 * WINDOW + 512);
        let meters = player.status().meters;
        assert_eq!(meters.stems[1], [0.0, 0.0], "{path}: muted keys");
        assert_eq!(bus(&meters, Bus::KeysPads), [0.0, 0.0], "{path}");
        assert_eq!(meters.master, [0.3125, 0.3125], "{path}");
    }
}

#[test]
fn a_block_reads_its_own_peak_at_once_and_holds_it_one_to_two_windows() {
    let loud_at = 1_000;
    let mut drums = vec![0.125; FRAMES];
    drums[loud_at] = 0.75;
    let song = SongSpec::new(
        RATE,
        FRAMES as u64,
        vec![("verse", SectionType::Verse, 0)],
        vec![("drums", StemRole::Track), ("click", StemRole::Click)],
    )
    .build();
    let stems = vec![
        mono("drums", StemRole::Track, drums),
        mono("click", StemRole::Click, vec![0.0; FRAMES]),
    ];
    let (mut player, mut controller) = player(&song, stems, 2);
    controller.send(Command::Play).unwrap();
    render(&mut player, 2, loud_at);
    assert_eq!(player.status().meters.stems[0], [0.125, 0.125]);
    // The block holding the loud frame reads it.
    render(&mut player, 2, 1);
    assert_eq!(player.status().meters.stems[0], [0.75, 0.75]);
    assert_eq!(player.status().meters.master, [0.75, 0.75]);
    // Held for a whole window at least.
    render(&mut player, 2, WINDOW - 1);
    assert_eq!(player.status().meters.stems[0], [0.75, 0.75]);
    // Gone after two windows and a callback.
    render(&mut player, 2, WINDOW + 256 + 1);
    assert_eq!(player.status().meters.stems[0], [0.125, 0.125]);
}

#[test]
fn a_stopped_or_paused_player_reads_silence() {
    let (song, stems) = band();
    let (mut player, mut controller) = player(&song, stems, 2);
    render(&mut player, 2, 1_024);
    assert_eq!(player.status().meters, Meters::SILENT, "stopped");

    controller.send(Command::Play).unwrap();
    render(&mut player, 2, 1_024);
    assert_ne!(player.status().meters, Meters::SILENT, "playing");
    controller.send(Command::Pause).unwrap();
    render(&mut player, 2, 2 * WINDOW + 1_024);
    assert_eq!(player.status().meters, Meters::SILENT, "paused");
}

#[test]
fn stems_past_the_maximum_are_mixed_and_reach_their_bus_but_are_not_metered() {
    let count = MAX_METERED_STEMS + 6;
    let ids: Vec<&'static str> = (0..count)
        .map(|i| &*Box::leak(format!("pad{i}").into_boxed_str()))
        .collect();
    let mut stems: Vec<(&'static str, StemRole)> =
        ids.iter().map(|&id| (id, StemRole::Track)).collect();
    stems.push(("click", StemRole::Click));
    let song = SongSpec::new(
        RATE,
        FRAMES as u64,
        vec![("verse", SectionType::Verse, 0)],
        stems,
    )
    .build();
    let level = 1.0 / 128.0;
    let mut audio: Vec<StemAudio> = ids
        .iter()
        .map(|&id| mono(id, StemRole::Track, vec![level; FRAMES]))
        .collect();
    audio.push(mono("click", StemRole::Click, vec![0.0; FRAMES]));
    let (mut player, mut controller) = player(&song, audio, 2);
    controller.send(Command::Play).unwrap();
    render(&mut player, 2, 2_048);

    let meters = player.status().meters;
    assert_eq!(meters.stems.len(), MAX_METERED_STEMS);
    assert!(meters.stems.iter().all(|&peak| peak == [level, level]));
    let all = count as f32 * level;
    assert_eq!(
        bus(&meters, Bus::KeysPads),
        [all, all],
        "every pad on its bus"
    );
    assert_eq!(meters.master, [all, all]);
}

#[test]
fn a_stem_crossfading_into_a_seam_reads_its_sum_as_its_bus_and_the_master_do() {
    let song = SongSpec::new(
        RATE,
        FRAMES as u64,
        vec![
            ("intro", SectionType::Intro, 0),
            ("verse", SectionType::Verse, 16_000),
        ],
        vec![("keys", StemRole::Track), ("click", StemRole::Click)],
    )
    .build();
    let stems = vec![
        stereo("keys", StemRole::Track, common::noise(5, 2 * FRAMES)),
        mono("click", StemRole::Click, vec![0.0; FRAMES]),
    ];
    let (mut player, mut controller) = player(&song, stems, 2);
    controller.send(Command::Play).unwrap();
    let mut out = vec![0.0f32; 2 * 64];
    for call in 0..200 {
        // Back to the start mid-song: the stem's tail and its first section
        // sum through the seam crossfade.
        if call == 100 {
            controller.send(Command::BackToStart).unwrap();
        }
        player.process(&mut out);
        let meters = player.status().meters;
        assert_eq!(meters.stems[0], meters.master, "call {call}");
        assert_eq!(meters.stems[0], bus(&meters, Bus::KeysPads), "call {call}");
    }
    assert!(player.status().meters.master[0] > 0.0);
}
