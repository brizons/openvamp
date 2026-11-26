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
    player_with(song, stems, channels, 256)
}

/// A player whose largest buffer is `max_frames`.
fn player_with(
    song: &Song,
    stems: Vec<StemAudio>,
    channels: u16,
    max_frames: usize,
) -> (Player, Controller) {
    Player::new(
        song,
        "default",
        &MixSettings::default(),
        &PlayerConfig {
            channels,
            ..PlayerConfig::new(RATE, max_frames)
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

/// Keys alone (noise, Keys & Pads) under a silent click stem, in two
/// sections, so a jump has tails to mix.
fn keys_alone() -> (Song, Vec<StemAudio>) {
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
    (song, stems)
}

/// The plain stereo path, and the bus path on eight outputs.
fn both_paths() -> [(&'static str, u16, Routes); 2] {
    [
        ("plain", 2, Routes::STEREO),
        (
            "eight outputs",
            8,
            Routes::from_routing(&Routing::preset(8)),
        ),
    ]
}

#[test]
fn a_stem_crossfading_into_a_seam_reads_its_sum_as_its_bus_and_the_master_do() {
    for (path, channels, routes) in both_paths() {
        let (song, stems) = keys_alone();
        let (mut player, mut controller) = player(&song, stems, channels);
        player.set_routes(routes);
        controller.send(Command::Play).unwrap();
        let mut out = vec![0.0f32; usize::from(channels) * 64];
        for call in 0..200 {
            // Back to the start mid-song: the stem's tail and its first
            // section sum through the seam crossfade.
            if call == 100 {
                controller.send(Command::BackToStart).unwrap();
            }
            player.process(&mut out);
            let meters = player.status().meters;
            assert_eq!(meters.stems[0], meters.master, "{path}: call {call}");
            assert_eq!(
                meters.stems[0],
                bus(&meters, Bus::KeysPads),
                "{path}: call {call}"
            );
        }
        assert!(player.status().meters.master[0] > 0.0, "{path}");
    }
}

/// Whether `a` and `b` are within one float step of each other.
fn within_a_step(a: [f32; 2], b: [f32; 2]) -> bool {
    a.iter()
        .zip(&b)
        .all(|(a, b)| (a - b).abs() <= f32::EPSILON * a.abs().max(b.abs()))
}

#[test]
fn a_stem_moving_between_buses_reads_as_the_master_through_the_move() {
    for (path, channels, routes) in both_paths() {
        let (song, stems) = keys_alone();
        let (mut player, mut controller) = player(&song, stems, channels);
        player.set_routes(routes);
        controller.send(Command::Play).unwrap();
        let mut out = vec![0.0f32; usize::from(channels) * 64];
        for call in 0..60 {
            // To Vocals and back before the first move ends: the stem's
            // frames split between two buses, in proportions that sum to
            // one up to a float step.
            match call {
                20 => controller
                    .send(Command::SetStemBus {
                        stem: 0,
                        bus: Bus::Vocals,
                    })
                    .unwrap(),
                21 => controller
                    .send(Command::SetStemBus {
                        stem: 0,
                        bus: Bus::KeysPads,
                    })
                    .unwrap(),
                _ => {}
            }
            player.process(&mut out);
            let meters = player.status().meters;
            assert!(
                within_a_step(meters.stems[0], meters.master),
                "{path}: call {call}: stem {:?}, master {:?}",
                meters.stems[0],
                meters.master
            );
        }
        let meters = player.status().meters;
        assert_eq!(
            meters.stems[0], meters.master,
            "{path}: once the move ended"
        );
    }
}

/// Three sections of stems whose level steps every 333 frames, with the
/// generated click on.
fn varying() -> (Song, Vec<StemAudio>) {
    let frames = 60_000;
    let song = SongSpec::new(
        RATE,
        frames as u64,
        vec![
            ("intro", SectionType::Intro, 0),
            ("verse", SectionType::Verse, 20_000),
            ("chorus", SectionType::Chorus, 40_000),
        ],
        vec![
            ("drums", StemRole::Track),
            ("keys", StemRole::Track),
            ("guide", StemRole::Guide),
        ],
    )
    .build();
    let shaped = |seed: u64, count: usize| -> Vec<f32> {
        common::noise(seed, count)
            .iter()
            .enumerate()
            .map(|(i, value)| value * (((i / 333) % 7) as f32 / 7.0 + 0.05))
            .collect()
    };
    let stems = vec![
        mono("drums", StemRole::Track, shaped(7, frames)),
        stereo("keys", StemRole::Track, shaped(8, 2 * frames)),
        mono("guide", StemRole::Guide, shaped(9, frames)),
    ];
    (song, stems)
}

#[test]
fn the_meters_do_not_depend_on_the_buffer_size() {
    // Readings every 4096 frames, which every buffer size reaches; the
    // commands go at the same frames too.
    let checkpoint = 4_096;
    let checkpoints = 20;
    // The first reading after the routing change.
    let routed = 13;
    let paths = [
        (2, None),
        (8, Some(Routing::preset(8))),
        (1, Some(Routing::preset(1))),
    ];
    for (channels, routing) in paths {
        for max_frames in [256, 4_096] {
            let mut reference: Option<Vec<Meters>> = None;
            for buffer in [32, 64, 256, 1_024, 4_096] {
                let (song, stems) = varying();
                let (mut player, mut controller) = player_with(&song, stems, channels, max_frames);
                if let Some(routing) = &routing {
                    player.set_routes(Routes::from_routing(routing));
                }
                let mut out = vec![0.0f32; usize::from(channels) * buffer];
                let mut readings = Vec::new();
                for frame in (0..checkpoint * checkpoints).step_by(buffer) {
                    if frame % checkpoint == 0 {
                        let command = match frame / checkpoint {
                            0 => Some(Command::Play),
                            3 => Some(Command::QueueJump { position: 0 }),
                            5 => Some(Command::SetStemGain {
                                stem: 1,
                                gain: Gain::new(0.3),
                            }),
                            7 => Some(Command::Pause),
                            9 => Some(Command::Play),
                            11 => Some(Command::SetStemBus {
                                stem: 0,
                                bus: Bus::Guitars,
                            }),
                            12 => Some(Command::SetRouting(Routes::STEREO)),
                            15 => Some(Command::Stop),
                            17 => Some(Command::Play),
                            _ => None,
                        };
                        if let Some(command) = command {
                            controller.send(command).unwrap();
                        }
                    }
                    player.process(&mut out);
                    if (frame + buffer) % checkpoint == 0 {
                        readings.push(player.status().meters);
                    }
                }
                assert_eq!(readings.len(), checkpoints);
                let Some(reference) = &reference else {
                    reference = Some(readings);
                    continue;
                };
                let case = format!("{channels} channels, max {max_frames}, buffer {buffer}");
                for (k, (expected, got)) in reference.iter().zip(&readings).enumerate() {
                    assert_eq!(got.stems, expected.stems, "{case}: stems at {k}");
                    assert_eq!(got.buses, expected.buses, "{case}: buses at {k}");
                    // The master sums the buses: right after a routing
                    // change, where the audio itself may differ by a float
                    // step between buffer sizes, it may too.
                    if k == routed {
                        assert!(
                            within_a_step(got.master, expected.master),
                            "{case}: master at {k}: {:?} vs {:?}",
                            got.master,
                            expected.master
                        );
                    } else {
                        assert_eq!(got.master, expected.master, "{case}: master at {k}");
                    }
                }
            }
            assert!(
                reference
                    .iter()
                    .flatten()
                    .any(|meters| meters.master[0] > 0.0),
                "{channels} channels: something played"
            );
        }
    }
}
