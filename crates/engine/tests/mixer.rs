//! Mixer and router in the live player (F7.1, F7.3, F7.4, F8.2, F8.4):
//! per-stem gains that ramp, buses, and the routing of each bus to the
//! outputs of an N-channel device.
//!
//! Stems hold constant levels on known buses (`drums` on Drums, `keys` on
//! Keys & Pads, `guide` on Click & Guide), and a silent `click` stem turns
//! the generated click off unless a test wants it, so every output sample
//! says exactly what reached it.

mod common;

use common::{SongSpec, bits, mono, noise, plan, stereo};
use domain::{
    ArrangementMix, Bus, BusLevel, BusSettings, Destination, Routing, SectionType, Song, StemMix,
    StemRole, Volume, effective_gains,
};
use engine::live::{
    Command, Controller, Deck, GAIN_RAMP_MILLISECONDS, Gain, Handoff, LiveError, Player,
    PlayerConfig, Routes, memory_sources,
};
use engine::{MixSettings, StemAudio, render_offline};

const RATE: u32 = 8_000;
/// [`GAIN_RAMP_MILLISECONDS`] at [`RATE`]: 160 frames.
const RAMP: usize = (RATE as u64 * GAIN_RAMP_MILLISECONDS / 1_000) as usize;
const FRAMES: u64 = 32_000;

/// A one-section song of [`FRAMES`] frames with these stems.
fn song(stems: &[(&'static str, StemRole)]) -> Song {
    SongSpec::new(
        RATE,
        FRAMES,
        vec![("verse", SectionType::Verse, 0)],
        stems.to_vec(),
    )
    .build()
}

fn dc(id: &str, role: StemRole, value: f32) -> StemAudio {
    mono(id, role, vec![value; FRAMES as usize])
}

fn silent_click() -> StemAudio {
    dc("click", StemRole::Click, 0.0)
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

/// Renders exactly `frames` frames of `channels` interleaved channels.
fn render(player: &mut Player, channels: usize, frames: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(channels * frames);
    let mut chunk = vec![0.0f32; channels * 256];
    let mut left = frames;
    while left > 0 {
        let n = left.min(256);
        player.process(&mut chunk[..channels * n]);
        out.extend_from_slice(&chunk[..channels * n]);
        left -= n;
    }
    out
}

/// Channel `channel` (from 0) of frame `frame`.
fn at(out: &[f32], channels: usize, frame: usize, channel: usize) -> f32 {
    out[frame * channels + channel]
}

fn routes(entries: &[(Bus, Destination)], master: Option<Destination>) -> Routes {
    let mut routes = Routes::STEREO;
    for &(bus, destination) in entries {
        routes.buses[bus.index()] = destination;
    }
    routes.master = master;
    routes
}

fn send_gains(controller: &mut Controller, song: &Song, mix: &ArrangementMix, buses: &BusSettings) {
    for (stem, gain) in effective_gains(song.stems(), mix, buses).iter().enumerate() {
        controller
            .send(Command::SetStemGain {
                stem,
                gain: Gain::new(gain.gain),
            })
            .unwrap();
    }
}

#[test]
fn a_gain_change_ramps_over_its_length_with_no_larger_step() {
    assert_eq!(RAMP, 160, "20 ms at 8 kHz");
    let song = song(&[("drums", StemRole::Track), ("click", StemRole::Click)]);
    let (mut player, mut controller) = player(
        &song,
        vec![dc("drums", StemRole::Track, 0.5), silent_click()],
        2,
    );
    controller.send(Command::Play).unwrap();
    render(&mut player, 2, 4_000);
    controller
        .send(Command::SetStemGain {
            stem: 0,
            gain: Gain::new(0.25),
        })
        .unwrap();
    let out = render(&mut player, 2, 1_000);

    let largest = (0.5 - 0.125) / RAMP as f32;
    let mut previous = 0.5f32;
    for frame in 0..RAMP {
        let value = at(&out, 2, frame, 0);
        assert!(value < previous, "frame {frame} falls: {value}");
        assert!(
            previous - value <= largest * 1.000_1,
            "frame {frame}: step {}",
            previous - value
        );
        previous = value;
    }
    assert_eq!(
        at(&out, 2, RAMP - 1, 0),
        0.125,
        "the target on the ramp's last frame"
    );
    assert!((RAMP..1_000).all(|frame| at(&out, 2, frame, 0) == 0.125));
}

#[test]
fn a_gain_set_while_stopped_holds_from_the_first_frame() {
    let song = song(&[("drums", StemRole::Track), ("click", StemRole::Click)]);
    let (mut player, mut controller) = player(
        &song,
        vec![dc("drums", StemRole::Track, 0.5), silent_click()],
        2,
    );
    controller
        .send(Command::SetStemGain {
            stem: 0,
            gain: Gain::new(0.0),
        })
        .unwrap();
    controller.send(Command::Play).unwrap();
    let out = render(&mut player, 2, 1_000);
    assert!(out.iter().all(|&s| s == 0.0), "a muted stem never sounds");
}

#[test]
fn muting_a_bus_silences_its_stems_and_unmuting_restores_their_levels() {
    let song = song(&[
        ("drums", StemRole::Track),
        ("keys", StemRole::Track),
        ("click", StemRole::Click),
    ]);
    let (mut player, mut controller) = player(
        &song,
        vec![
            dc("drums", StemRole::Track, 0.25),
            dc("keys", StemRole::Track, 0.5),
            silent_click(),
        ],
        2,
    );
    let mix = ArrangementMix::default();
    let mut buses = BusSettings::new();
    controller.send(Command::Play).unwrap();
    assert_eq!(at(&render(&mut player, 2, 1_000), 2, 999, 0), 0.75);

    buses.set_level(
        Bus::Drums,
        BusLevel {
            volume: Volume::UNITY,
            mute: true,
        },
    );
    send_gains(&mut controller, &song, &mix, &buses);
    let muted = render(&mut player, 2, 1_000);
    assert!(
        (RAMP..1_000).all(|frame| at(&muted, 2, frame, 0) == 0.5),
        "drums silent"
    );

    buses.set_level(Bus::Drums, BusLevel::default());
    send_gains(&mut controller, &song, &mix, &buses);
    let restored = render(&mut player, 2, 1_000);
    assert!(
        (RAMP..1_000).all(|frame| at(&restored, 2, frame, 0) == 0.75),
        "drums back at their level"
    );
}

#[test]
fn solo_never_mutes_click_or_guide() {
    let song = song(&[
        ("drums", StemRole::Track),
        ("keys", StemRole::Track),
        ("guide", StemRole::Guide),
        ("click", StemRole::Click),
    ]);
    let (mut player, mut controller) = player(
        &song,
        vec![
            dc("drums", StemRole::Track, 0.25),
            dc("keys", StemRole::Track, 0.5),
            dc("guide", StemRole::Guide, 0.125),
            silent_click(),
        ],
        2,
    );
    let mut mix = ArrangementMix::default();
    mix.set_stem(
        "keys",
        StemMix {
            solo: true,
            ..StemMix::default()
        },
    );
    send_gains(&mut controller, &song, &mix, &BusSettings::new());
    controller.send(Command::Play).unwrap();
    let out = render(&mut player, 2, 1_000);
    assert!(
        (0..1_000).all(|frame| at(&out, 2, frame, 0) == 0.625),
        "keys and the guide play, the drums do not"
    );
}

#[test]
fn the_generated_click_follows_the_click_and_guide_bus() {
    let song = song(&[("drums", StemRole::Track)]);
    let silence = || vec![dc("drums", StemRole::Track, 0.0)];
    let (mut unity, mut control) = player(&song, silence(), 2);
    control.send(Command::Play).unwrap();
    let reference = render(&mut unity, 2, 16_000);
    assert!(reference.iter().any(|&s| s != 0.0), "the click plays");

    let mut buses = BusSettings::new();
    buses.set_level(
        Bus::ClickGuide,
        BusLevel {
            volume: Volume::from_db(-6.0),
            mute: false,
        },
    );
    let gain = buses.bus_gain(Bus::ClickGuide);
    let (mut quieter, mut control) = player(&song, silence(), 2);
    quieter.set_click_gain(gain);
    control.send(Command::Play).unwrap();
    let out = render(&mut quieter, 2, 16_000);
    for (got, unity) in out.iter().zip(&reference) {
        assert!(
            (got - unity * gain).abs() <= 1e-7,
            "{got} vs {}",
            unity * gain
        );
    }

    let (mut muted, mut control) = player(&song, silence(), 2);
    control.send(Command::Play).unwrap();
    render(&mut muted, 2, 2_000);
    buses.set_level(
        Bus::ClickGuide,
        BusLevel {
            volume: Volume::UNITY,
            mute: true,
        },
    );
    control
        .send(Command::SetClickGain(Gain::new(
            buses.bus_gain(Bus::ClickGuide),
        )))
        .unwrap();
    let out = render(&mut muted, 2, 14_000);
    assert!(
        out[2 * RAMP..].iter().all(|&s| s == 0.0),
        "a muted bus mutes the click"
    );

    // The click plays on Click & Guide's outputs.
    let (mut routed, mut control) = player(&song, silence(), 4);
    routed.set_routes(routes(&[(Bus::ClickGuide, Destination::Mono(3))], None));
    control.send(Command::Play).unwrap();
    let out = render(&mut routed, 4, 16_000);
    for frame in 0..16_000 {
        assert_eq!(at(&out, 4, frame, 0), 0.0);
        assert_eq!(at(&out, 4, frame, 1), 0.0);
        assert_eq!(at(&out, 4, frame, 3), 0.0);
        assert!((at(&out, 4, frame, 2) - at(&reference, 2, frame, 0)).abs() <= 1e-7);
    }
}

/// Drums in stereo (0.4 left, 0.2 right), keys in mono at 0.5, the guide in
/// mono at 0.125, and a silent click stem.
fn band() -> (Song, Vec<StemAudio>) {
    let song = song(&[
        ("drums", StemRole::Track),
        ("keys", StemRole::Track),
        ("guide", StemRole::Guide),
        ("click", StemRole::Click),
    ]);
    let stems = vec![
        stereo("drums", StemRole::Track, [0.4, 0.2].repeat(FRAMES as usize)),
        dc("keys", StemRole::Track, 0.5),
        dc("guide", StemRole::Guide, 0.125),
        silent_click(),
    ];
    (song, stems)
}

/// One frame of the band routed with `routes` on `channels` outputs.
fn routed_frame(channels: u16, routes: Routes) -> Vec<f32> {
    let (song, stems) = band();
    let (mut player, mut controller) = player(&song, stems, channels);
    player.set_routes(routes);
    controller.send(Command::Play).unwrap();
    let out = render(&mut player, usize::from(channels), 600);
    let n = usize::from(channels);
    out[500 * n..501 * n].to_vec()
}

fn close(got: &[f32], expected: &[f32]) {
    assert_eq!(got.len(), expected.len());
    for (channel, (got, expected)) in got.iter().zip(expected).enumerate() {
        assert!(
            (got - expected).abs() <= 1e-6,
            "output {}: {got} instead of {expected} ({got:?})",
            channel + 1
        );
    }
}

#[test]
fn a_bus_on_a_stereo_pair_plays_there_only() {
    let frame = routed_frame(
        8,
        routes(
            &[(Bus::Drums, Destination::Stereo { left: 3, right: 4 })],
            None,
        ),
    );
    close(&frame, &[0.625, 0.625, 0.4, 0.2, 0.0, 0.0, 0.0, 0.0]);
}

#[test]
fn a_mono_output_receives_the_mean_of_left_and_right() {
    let frame = routed_frame(8, routes(&[(Bus::Drums, Destination::Mono(5))], None));
    close(&frame, &[0.625, 0.625, 0.0, 0.0, 0.3, 0.0, 0.0, 0.0]);
}

#[test]
fn buses_sharing_an_output_are_summed() {
    let frame = routed_frame(
        8,
        routes(
            &[
                (Bus::Drums, Destination::Mono(2)),
                (Bus::KeysPads, Destination::Mono(2)),
                (Bus::ClickGuide, Destination::Mono(1)),
            ],
            None,
        ),
    );
    close(&frame, &[0.125, 0.8, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
}

#[test]
fn split_mode_on_two_outputs_puts_click_and_guide_left_and_the_rest_in_mono_right() {
    let split = Routing {
        split_mode: true,
        ..Routing::preset(2)
    };
    let frame = routed_frame(2, Routes::from_routing(&split));
    close(&frame, &[0.125, 0.8]);
}

#[test]
fn the_master_destination_carries_the_main_mix_without_click_and_guide() {
    let mut routing = Routing::stereo(8);
    routing.master = Some(Destination::Stereo { left: 7, right: 8 });
    let frame = routed_frame(8, Routes::from_routing(&routing));
    close(&frame, &[1.025, 0.825, 0.0, 0.0, 0.0, 0.0, 0.9, 0.7]);
}

#[test]
fn destinations_reach_the_top_of_32_outputs() {
    let frame = routed_frame(
        32,
        routes(
            &[
                (
                    Bus::Drums,
                    Destination::Stereo {
                        left: 31,
                        right: 32,
                    },
                ),
                (Bus::KeysPads, Destination::Mono(32)),
            ],
            None,
        ),
    );
    let mut expected = vec![0.0; 32];
    expected[0] = 0.125;
    expected[1] = 0.125;
    expected[30] = 0.4;
    expected[31] = 0.7;
    close(&frame, &expected);
}

#[test]
fn a_routing_change_while_playing_crossfades_over_the_gain_ramp() {
    let song = song(&[("keys", StemRole::Track), ("click", StemRole::Click)]);
    let (mut player, mut controller) = player(
        &song,
        vec![dc("keys", StemRole::Track, 0.5), silent_click()],
        4,
    );
    controller.send(Command::Play).unwrap();
    render(&mut player, 4, 3_000);
    controller
        .send(Command::SetRouting(routes(
            &[(Bus::KeysPads, Destination::Stereo { left: 3, right: 4 })],
            None,
        )))
        .unwrap();
    let out = render(&mut player, 4, 1_000);
    let largest = 0.5 / RAMP as f32;
    let (mut old, mut new) = (0.5f32, 0.0f32);
    for frame in 0..RAMP {
        let (from, to) = (at(&out, 4, frame, 0), at(&out, 4, frame, 2));
        assert!(
            old - from <= largest * 1.000_1 && from < old,
            "outputs 1/2 fade out, frame {frame}"
        );
        assert!(
            to - new <= largest * 1.000_1 && to > new,
            "outputs 3/4 fade in, frame {frame}"
        );
        assert!(
            (from + to - 0.5).abs() <= 1e-6,
            "the level holds, frame {frame}"
        );
        (old, new) = (from, to);
    }
    for frame in RAMP..1_000 {
        assert_eq!(&out[frame * 4..frame * 4 + 4], &[0.0, 0.0, 0.5, 0.5]);
    }
}

/// Song A: two sections of noise on two tracks, with the generated click.
fn noisy(frames: u64, seed: u64) -> (Song, Vec<StemAudio>) {
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

#[test]
fn unity_gains_and_the_stereo_routing_play_exactly_the_offline_render() {
    let (song, stems) = noisy(24_000, 3);
    let expected =
        render_offline(&plan(&song, "default"), &stems, &MixSettings::default()).unwrap();
    let unity = effective_gains(
        song.stems(),
        &ArrangementMix::default(),
        &BusSettings::new(),
    );
    for channels in [2u16, 8] {
        let (mut player, mut controller) = player(&song, stems.clone(), channels);
        player.set_stem_gains(&unity);
        player.set_click_gain(1.0);
        player.set_routes(Routes::from_routing(&Routing::stereo(channels)));
        controller.send(Command::Play).unwrap();
        // The same settings sent again while playing change nothing.
        let n = usize::from(channels);
        let mut out = render(&mut player, n, 5_000);
        for (stem, gain) in unity.iter().enumerate() {
            controller
                .send(Command::SetStemGain {
                    stem,
                    gain: Gain::new(gain.gain),
                })
                .unwrap();
        }
        controller
            .send(Command::SetClickGain(Gain::new(1.0)))
            .unwrap();
        controller
            .send(Command::SetRouting(Routes::from_routing(&Routing::stereo(
                channels,
            ))))
            .unwrap();
        out.extend(render(&mut player, n, 24_000 - 5_000));
        let left_right: Vec<f32> = out
            .chunks_exact(n)
            .flat_map(|frame| [frame[0], frame[1]])
            .collect();
        assert_eq!(bits(&left_right), bits(&expected), "{channels} outputs");
        assert!(
            out.chunks_exact(n)
                .all(|frame| frame[2..].iter().all(|&s| s == 0.0)),
            "outputs past 2 are silent"
        );
    }
}

fn deck_player(song: &(Song, Vec<StemAudio>), routes: Routes) -> Player {
    let (mut player, _controller) = player(&song.0, song.1.clone(), 8);
    player.set_routes(routes);
    player
}

fn run_deck(deck: &mut Deck, frames: usize) -> Vec<f32> {
    let mut out = Vec::new();
    let mut chunk = vec![0.0f32; 8 * 256];
    while out.len() < 8 * frames {
        deck.process(&mut chunk);
        out.extend_from_slice(&chunk);
    }
    out.truncate(8 * frames);
    out
}

fn alone(song: &(Song, Vec<StemAudio>), routes: Routes, frames: usize) -> Vec<f32> {
    let (mut player, mut controller) = player(&song.0, song.1.clone(), 8);
    player.set_routes(routes);
    controller.send(Command::Play).unwrap();
    render(&mut player, 8, frames)
}

#[test]
fn autolink_on_eight_outputs_starts_the_next_item_on_the_next_frame() {
    let routed = routes(
        &[
            (Bus::Drums, Destination::Stereo { left: 3, right: 4 }),
            (Bus::KeysPads, Destination::Mono(6)),
        ],
        None,
    );
    let (a, b) = (noisy(24_000, 3), noisy(20_000, 70));
    let expected_a = alone(&a, routed, 24_000);
    let expected_b = alone(&b, routed, 20_000);
    let (mut deck, mut control) = Deck::new(deck_player(&a, routed), 1, 256);
    control
        .cue(deck_player(&b, routed), 2, Handoff::AutoLink)
        .unwrap();
    control.send(Command::Play).unwrap();
    let out = run_deck(&mut deck, 44_500);
    assert_eq!(bits(&out[..8 * 24_000]), bits(&expected_a));
    assert_eq!(bits(&out[8 * 24_000..8 * 44_000]), bits(&expected_b));
    assert!(out[8 * 44_000..].iter().all(|&s| s == 0.0));
}

#[test]
fn a_crossfade_on_eight_outputs_blends_every_output() {
    let routed = routes(&[(Bus::KeysPads, Destination::Mono(5))], None);
    let steady = |value: f32| {
        let song = SongSpec::new(
            RATE,
            56_000,
            vec![("verse", SectionType::Verse, 0)],
            vec![("keys", StemRole::Track), ("click", StemRole::Click)],
        )
        .build();
        let stems = vec![
            mono("keys", StemRole::Track, vec![value; 56_000]),
            mono("click", StemRole::Click, vec![0.0; 56_000]),
        ];
        (song, stems)
    };
    let (a, b) = (steady(0.5), steady(-0.5));
    let blend = 40_000u64;
    let (mut deck, mut control) = Deck::new(deck_player(&a, routed), 1, 256);
    control
        .cue(
            deck_player(&b, routed),
            2,
            Handoff::Crossfade { frames: blend },
        )
        .unwrap();
    control.send(Command::Play).unwrap();
    let out = run_deck(&mut deck, 72_000);
    let start = 16_000usize;
    for t in 0..blend as usize {
        let gain = (t as f64 + 0.5) / blend as f64;
        let expected = (0.5 * (1.0 - gain) - 0.5 * gain) as f32;
        let frame = &out[(start + t) * 8..(start + t) * 8 + 8];
        assert!(
            (frame[4] - expected).abs() <= 1e-6,
            "output 5, blend frame {t}: {frame:?}"
        );
        assert!(frame.iter().enumerate().all(|(c, &s)| c == 4 || s == 0.0));
    }
    assert!(
        (56_000..72_000).all(|f| out[f * 8 + 4] == -0.5),
        "B alone after the blend"
    );
}

/// The largest change between two frames on any output of `out`.
fn largest_step(out: &[f32], channels: usize) -> (f32, usize, usize) {
    let mut largest = (0.0f32, 0, 0);
    for frame in 1..out.len() / channels {
        for channel in 0..channels {
            let step =
                (at(out, channels, frame, channel) - at(out, channels, frame - 1, channel)).abs();
            if step > largest.0 {
                largest = (step, frame, channel);
            }
        }
    }
    largest
}

/// A drums stem at a constant 0.1 on both sides, and a silent click stem.
fn steady_drums(channels: u16) -> (Player, Controller) {
    let song = song(&[("drums", StemRole::Track), ("click", StemRole::Click)]);
    player(
        &song,
        vec![dc("drums", StemRole::Track, 0.1), silent_click()],
        channels,
    )
}

#[test]
fn routing_changes_in_quick_succession_never_step_more_than_one_ramp_allows() {
    let (mut player, mut controller) = steady_drums(8);
    controller.send(Command::Play).unwrap();
    let mut out = render(&mut player, 8, 2_000);
    let pair = |left| {
        routes(
            &[(
                Bus::Drums,
                Destination::Stereo {
                    left,
                    right: left + 1,
                },
            )],
            None,
        )
    };
    controller.send(Command::SetRouting(pair(3))).unwrap();
    out.extend(render(&mut player, 8, 40));
    controller.send(Command::SetRouting(pair(5))).unwrap();
    out.extend(render(&mut player, 8, 80));
    controller
        .send(Command::SetRouting(Routes::STEREO))
        .unwrap();
    out.extend(render(&mut player, 8, 1_000));

    let bound = 0.1 / RAMP as f32 * 1.001;
    let (step, frame, channel) = largest_step(&out, 8);
    assert!(
        step <= bound,
        "output {} steps by {step} at frame {frame}; one ramp allows {bound}",
        channel + 1
    );
    let last = &out[out.len() - 8..];
    assert_eq!(
        last,
        &[0.1, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        "back on 1/2"
    );
}

#[test]
fn moving_a_stem_to_another_bus_while_playing_crossfades_between_their_outputs() {
    let (mut player, mut controller) = steady_drums(8);
    player.set_routes(routes(
        &[
            (Bus::Drums, Destination::Stereo { left: 3, right: 4 }),
            (Bus::Vocals, Destination::Stereo { left: 5, right: 6 }),
        ],
        None,
    ));
    controller.send(Command::Play).unwrap();
    let mut out = render(&mut player, 8, 2_000);
    controller
        .send(Command::SetStemBus {
            stem: 0,
            bus: Bus::Vocals,
        })
        .unwrap();
    out.extend(render(&mut player, 8, 40));
    // Moved again before the first move ended.
    controller
        .send(Command::SetStemBus {
            stem: 0,
            bus: Bus::Guitars,
        })
        .unwrap();
    out.extend(render(&mut player, 8, 1_000));

    let bound = 0.1 / RAMP as f32 * 1.001;
    let (step, frame, channel) = largest_step(&out, 8);
    assert!(
        step <= bound,
        "output {} steps by {step} at frame {frame}; one ramp allows {bound}",
        channel + 1
    );
    let last = &out[out.len() - 8..];
    assert_eq!(
        last,
        &[0.1, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        "on Guitars, which plays on 1/2"
    );
}

#[test]
fn moving_a_stem_between_buses_both_on_1_2_changes_no_sample() {
    let (song, stems) = noisy(24_000, 3);
    let expected =
        render_offline(&plan(&song, "default"), &stems, &MixSettings::default()).unwrap();
    let (mut player, mut controller) = player(&song, stems, 2);
    controller.send(Command::Play).unwrap();
    let mut out = render(&mut player, 2, 5_000);
    for (stem, bus) in [(0, Bus::Vocals), (1, Bus::Bass), (0, Bus::Guitars)] {
        controller.send(Command::SetStemBus { stem, bus }).unwrap();
        out.extend(render(&mut player, 2, 50));
    }
    out.extend(render(&mut player, 2, 24_000 - 5_150));
    assert_eq!(bits(&out), bits(&expected));
}

#[test]
fn a_gain_that_is_not_a_finite_non_negative_number_is_silence() {
    let steady = song(&[("drums", StemRole::Track), ("click", StemRole::Click)]);
    let click = song(&[("drums", StemRole::Track)]);
    for bad in [
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -0.5,
        -f32::MIN_POSITIVE,
    ] {
        assert_eq!(Gain::new(bad).get(), 0.0, "Gain::new({bad})");
        let (mut stem, mut controller) = player(
            &steady,
            vec![dc("drums", StemRole::Track, 0.5), silent_click()],
            2,
        );
        controller
            .send(Command::SetStemGain {
                stem: 0,
                gain: Gain::new(bad),
            })
            .unwrap();
        controller.send(Command::Play).unwrap();
        assert!(
            render(&mut stem, 2, 2_000).iter().all(|&s| s == 0.0),
            "a stem at Gain::new({bad}) is silent"
        );
        let (mut generated, mut controller) =
            player(&click, vec![dc("drums", StemRole::Track, 0.0)], 2);
        controller
            .send(Command::SetClickGain(Gain::new(bad)))
            .unwrap();
        controller.send(Command::Play).unwrap();
        assert!(
            render(&mut generated, 2, 16_000).iter().all(|&s| s == 0.0),
            "the click at Gain::new({bad}) is silent"
        );
    }
}

#[test]
fn a_player_renders_1_to_32_output_channels() {
    let song = song(&[("drums", StemRole::Track)]);
    let new = |channels: u16| {
        Player::new(
            &song,
            "default",
            &MixSettings::default(),
            &PlayerConfig {
                channels,
                ..PlayerConfig::new(RATE, 256)
            },
            memory_sources(vec![dc("drums", StemRole::Track, 0.5)]).unwrap(),
        )
        .map(|(player, _controller)| player.channels())
    };
    for channels in [0, 33] {
        assert_eq!(
            new(channels).err(),
            Some(LiveError::ChannelsOutOfRange { channels }),
            "{channels} channels"
        );
    }
    assert_eq!(new(1), Ok(1));
    assert_eq!(new(32), Ok(32));
}

/// Drums steady at 0.5 under a silent click stem.
fn steady_song() -> (Song, Vec<StemAudio>) {
    let song = song(&[("drums", StemRole::Track), ("click", StemRole::Click)]);
    (
        song,
        vec![dc("drums", StemRole::Track, 0.5), silent_click()],
    )
}

/// A song whose only sound is the generated click.
fn click_only() -> (Song, Vec<StemAudio>) {
    let song = song(&[("drums", StemRole::Track)]);
    (song, vec![dc("drums", StemRole::Track, 0.0)])
}

/// Keys steady at 0.25 and bass at 0.125 under a silent click stem: 0.375
/// on outputs 1 and 2 at unity.
fn two_stems() -> (Song, Vec<StemAudio>) {
    let song = song(&[
        ("keys", StemRole::Track),
        ("bass", StemRole::Track),
        ("click", StemRole::Click),
    ]);
    let stems = vec![
        dc("keys", StemRole::Track, 0.25),
        dc("bass", StemRole::Track, 0.125),
        silent_click(),
    ];
    (song, stems)
}

/// Outputs 1 and 2 of every frame of `out` (eight channels).
fn main_pair(out: &[f32]) -> Vec<[f32; 2]> {
    out.chunks_exact(8)
        .map(|frame| [frame[0], frame[1]])
        .collect()
}

#[test]
fn the_cued_item_follows_routing_and_click_gain_changes() {
    let (a, b) = (steady_song(), click_only());
    let routed = routes(&[(Bus::ClickGuide, Destination::Mono(5))], None);
    let expected = {
        let (mut player, mut controller) = player(&b.0, b.1.clone(), 8);
        player.set_routes(routed);
        player.set_click_gain(0.5);
        controller.send(Command::Play).unwrap();
        render(&mut player, 8, 16_000)
    };
    assert!(expected.iter().any(|&s| s != 0.0), "B's click plays");

    let (mut deck, mut control) = Deck::new(deck_player(&a, Routes::STEREO), 1, 256);
    control
        .cue(deck_player(&b, Routes::STEREO), 2, Handoff::AutoLink)
        .unwrap();
    control.send(Command::Play).unwrap();
    run_deck(&mut deck, 1_024);
    // While A plays and B waits.
    control.send(Command::SetRouting(routed)).unwrap();
    control.send(Command::SetClickGain(Gain::new(0.5))).unwrap();
    let out = run_deck(&mut deck, FRAMES as usize - 1_024 + 16_000);
    assert_eq!(deck.status().item, 2, "B followed on");
    assert_eq!(
        bits(&out[8 * (FRAMES as usize - 1_024)..]),
        bits(&expected),
        "B starts on output 5 with its click at half"
    );
}

#[test]
fn an_item_leaving_in_a_crossfade_follows_routing_and_click_gain_changes() {
    // A: drums at 0.5 and the generated click; B: silence.
    let a = click_only();
    let a = (a.0, vec![dc("drums", StemRole::Track, 0.5)]);
    let b = (
        steady_song().0,
        vec![dc("drums", StemRole::Track, 0.0), silent_click()],
    );
    let (mut deck, mut control) = Deck::new(deck_player(&a, Routes::STEREO), 1, 256);
    control
        .cue(
            deck_player(&b, Routes::STEREO),
            2,
            Handoff::Crossfade { frames: 8_000 },
        )
        .unwrap();
    control.send(Command::Play).unwrap();
    // The blend runs from 24 000 to A's end at 32 000.
    let sent = 25_600;
    run_deck(&mut deck, sent);
    assert_eq!(deck.status().leaving, 1, "A is leaving under B");
    control
        .send(Command::SetRouting(routes(
            &[(Bus::Drums, Destination::Stereo { left: 3, right: 4 })],
            None,
        )))
        .unwrap();
    control.send(Command::SetClickGain(Gain::new(0.0))).unwrap();
    let out = run_deck(&mut deck, FRAMES as usize - sent);
    for frame in RAMP..FRAMES as usize - sent {
        let heard = &out[8 * frame..8 * frame + 8];
        assert_eq!(
            [heard[0], heard[1], heard[4], heard[5], heard[6], heard[7]],
            [0.0; 6],
            "frame {}: A's drums moved to 3/4 and its click went silent",
            sent + frame
        );
    }
    let frame = 28_000 - sent;
    let drums = at(&out, 8, frame, 2);
    assert!(drums > 0.0, "A's drums fade out on 3/4");
    assert_eq!(at(&out, 8, frame, 3), drums);
}

#[test]
fn a_command_for_the_cued_item_reaches_it_and_not_the_current_one() {
    let (a, b) = (steady_song(), steady_song());
    let (mut deck, mut control) = Deck::new(deck_player(&a, Routes::STEREO), 1, 256);
    control
        .cue(deck_player(&b, Routes::STEREO), 2, Handoff::AutoLink)
        .unwrap();
    control.send(Command::Play).unwrap();
    let mut out = run_deck(&mut deck, 1_024);
    control
        .send_cued(Command::SetStemGain {
            stem: 0,
            gain: Gain::new(0.25),
        })
        .unwrap();
    out.extend(run_deck(&mut deck, FRAMES as usize));
    let heard = main_pair(&out);
    assert!(
        heard[..FRAMES as usize]
            .iter()
            .all(|&pair| pair == [0.5, 0.5]),
        "A plays on at unity"
    );
    assert!(
        heard[FRAMES as usize..]
            .iter()
            .all(|&pair| pair == [0.125, 0.125]),
        "B starts at the gain it was sent"
    );
}

#[test]
fn a_command_for_the_cued_item_is_dropped_when_nothing_is_cued() {
    let mute = Command::SetStemGain {
        stem: 0,
        gain: Gain::new(0.0),
    };
    let (mut deck, mut control) = Deck::new(deck_player(&steady_song(), Routes::STEREO), 1, 256);
    // Nothing was ever cued.
    control.send_cued(mute).unwrap();
    // The cued item was sent back.
    control
        .cue(
            deck_player(&steady_song(), Routes::STEREO),
            2,
            Handoff::AutoLink,
        )
        .unwrap();
    control.uncue().unwrap();
    control.send_cued(mute).unwrap();
    // Neither waits for the next cue.
    control
        .cue(
            deck_player(&steady_song(), Routes::STEREO),
            3,
            Handoff::AutoLink,
        )
        .unwrap();
    control.send(Command::Play).unwrap();
    let out = run_deck(&mut deck, FRAMES as usize + 1_024);
    assert_eq!(deck.status().item, 3);
    assert!(
        main_pair(&out).iter().all(|&pair| pair == [0.5, 0.5]),
        "the current item and item 3 play at unity"
    );
}

#[test]
fn a_cued_gain_change_sent_as_the_handoff_happens_reaches_the_item_it_was_for() {
    // Sent once the deck made B current, before the control thread knew:
    // just after an AutoLink, and inside a crossfade.
    for (handoff, sent) in [
        (Handoff::AutoLink, 32_256),
        (Handoff::Crossfade { frames: 2_048 }, 30_976),
    ] {
        let (mut deck, mut control) =
            Deck::new(deck_player(&steady_song(), Routes::STEREO), 1, 256);
        control
            .cue(deck_player(&two_stems(), Routes::STEREO), 2, handoff)
            .unwrap();
        control.send(Command::Play).unwrap();
        run_deck(&mut deck, sent);
        assert_eq!(deck.status().item, 2, "{handoff:?}: B is current");
        control
            .send_cued(Command::SetStemGain {
                stem: 1,
                gain: Gain::new(0.5),
            })
            .unwrap();
        let out = run_deck(&mut deck, 2_048);
        assert_eq!(
            main_pair(&out).last(),
            Some(&[0.3125, 0.3125]),
            "{handoff:?}: B's bass went to half"
        );
    }
}

#[test]
fn a_command_sent_to_an_item_reaches_it_wherever_the_deck_holds_it_and_no_other() {
    let mute_drums = Command::SetStemGain {
        stem: 0,
        gain: Gain::new(0.0),
    };
    // Inside a crossfade, A (item 1) is leaving under B: muting A's drums
    // leaves what a silent A would, and B untouched.
    let silent_a = (
        steady_song().0,
        vec![dc("drums", StemRole::Track, 0.0), silent_click()],
    );
    let crossfade = Handoff::Crossfade { frames: 2_048 };
    let deck_from = |a: &(Song, Vec<StemAudio>)| {
        let (deck, mut control) = Deck::new(deck_player(a, Routes::STEREO), 1, 256);
        control
            .cue(deck_player(&two_stems(), Routes::STEREO), 2, crossfade)
            .unwrap();
        control.send(Command::Play).unwrap();
        (deck, control)
    };
    let (mut reference, _control) = deck_from(&silent_a);
    let expected = run_deck(&mut reference, 33_024);
    let (mut deck, mut control) = deck_from(&steady_song());
    let sent = 30_976;
    let mut out = run_deck(&mut deck, sent);
    assert_eq!(deck.status().leaving, 1, "A is leaving");
    control.send_to(1, mute_drums).unwrap();
    out.extend(run_deck(&mut deck, 2_048));
    let from = 8 * (sent + RAMP);
    assert_eq!(bits(&out[from..]), bits(&expected[from..]));

    // After an AutoLink, A is gone: nothing else takes its command.
    let (mut deck, mut control) = Deck::new(deck_player(&steady_song(), Routes::STEREO), 1, 256);
    control
        .cue(
            deck_player(&two_stems(), Routes::STEREO),
            2,
            Handoff::AutoLink,
        )
        .unwrap();
    control.send(Command::Play).unwrap();
    run_deck(&mut deck, 32_256);
    control.send_to(1, mute_drums).unwrap();
    let out = run_deck(&mut deck, 1_024);
    assert!(
        main_pair(&out).iter().all(|&pair| pair == [0.375, 0.375]),
        "B plays at unity"
    );
}
