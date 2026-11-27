//! Source-time grid queries used by live playback: the next bar line and the
//! click ticks inside a source range follow the same grid, with the same
//! rounding, as the clicks `compile` puts in a plan.

mod common;

use common::{RATE, SongSpec, info, marker};
use domain::{
    BarBeat, BeatGrid, ClickEvent, ClickLevel, CompileOptions, DraftStem, MAX_SONG_HOURS,
    MapChange, RenderPlan, SectionType, Song, SongDraft, SourceClick, StoredFormat, TempoPoint,
    TimeSignature, TimeSignaturePoint, compile, validate,
};

const BAR: u64 = 96_000;

fn plan(song: &Song, arrangement: &str) -> RenderPlan {
    compile(song, arrangement, &CompileOptions::default()).expect("arrangement compiles")
}

/// Sections off the bar grid, a mid-bar tempo change, and two meter changes.
fn changing_spec() -> SongSpec {
    SongSpec::new(
        14 * BAR,
        vec![
            marker("a", SectionType::Intro, None, 0),
            marker("b", SectionType::Verse, Some(1), 2 * BAR + 13),
            marker("c", SectionType::Chorus, Some(1), 2 * BAR + 40_000),
            marker("d", SectionType::Bridge, None, 7 * BAR),
            marker("e", SectionType::Outro, None, 11 * BAR + 99),
        ],
    )
    .tempo(&[(0, 120.0), (6 * BAR + 777, 87.25)])
    .meters(&[(0, 4, 4), (3 * BAR, 3, 4), (9 * BAR + 5, 6, 8)])
}

/// Clicks of `range` moved so the range starts at output sample `out_start`.
fn mapped(grid: &BeatGrid, range: std::ops::Range<u64>, out_start: u64) -> Vec<ClickEvent> {
    let src_start = range.start;
    grid.clicks_in(range)
        .iter()
        .map(|click| ClickEvent {
            at: out_start + (click.at - src_start),
            level: click.level,
        })
        .collect()
}

struct Rng(u64);

impl Rng {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound
    }
}

#[test]
fn clicks_in_the_whole_song_equal_the_default_plan_clicks() {
    let song = changing_spec().build();
    let grid = BeatGrid::new(&song);
    let plan = plan(&song, "default");

    assert_eq!(mapped(&grid, 0..song.length(), 0), plan.clicks);
    assert!(grid.clicks_in(5..5).is_empty());
    assert!(grid.clicks_in(song.length()..u64::MAX).is_empty());
}

#[test]
fn clicks_in_any_segment_equal_compiles_clicks_for_that_segment() {
    let base = changing_spec().package();
    let ids = ["a", "b", "c", "d", "e"];
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    for case in 0..200 {
        let count = 1 + rng.next(8) as usize;
        let order: Vec<&str> = (0..count).map(|_| ids[rng.next(5) as usize]).collect();
        let mut package = base.clone();
        package.arrangements.push(common::arrangement("x", &order));
        let song = validate(&package, &[info("drums", RATE, 14 * BAR)])
            .unwrap()
            .song;
        let grid = BeatGrid::new(&song);
        let plan = plan(&song, "x");

        let mut expected = Vec::new();
        for segment in &plan.segments {
            let range = segment.src_start..segment.src_start + segment.len;
            expected.extend(mapped(&grid, range, segment.out_start));
        }
        assert_eq!(expected, plan.clicks, "case {case} {order:?}");
    }
}

#[test]
fn clicks_in_a_partial_range_keep_only_the_ticks_inside_it() {
    let song = changing_spec().build();
    let grid = BeatGrid::new(&song);
    let all = plan(&song, "default").clicks;
    let mut rng = Rng(7);
    for _ in 0..500 {
        let start = rng.next(song.length());
        let end = start + rng.next(3 * BAR);
        let expected: Vec<SourceClick> = all
            .iter()
            .filter(|click| (start..end).contains(&click.at))
            .map(|click| SourceClick {
                at: click.at,
                level: click.level,
            })
            .collect();
        assert_eq!(
            grid.clicks_in(start..end),
            expected.as_slice(),
            "{start}..{end}"
        );
    }
}

#[test]
fn next_bar_line_is_the_next_downbeat_tick_across_tempo_and_meter_changes() {
    let song = changing_spec().build();
    let grid = BeatGrid::new(&song);
    let downbeats: Vec<u64> = plan(&song, "default")
        .clicks
        .iter()
        .filter(|click| click.level == ClickLevel::Downbeat)
        .map(|click| click.at)
        .collect();
    // 3 bars of 4/4, bars of 3/4 from the meter change, a longer bar after
    // the tempo change, then 6/8 bars from 9 * BAR + 5.
    assert_eq!(&downbeats[..4], &[0, BAR, 2 * BAR, 3 * BAR]);
    assert_eq!(downbeats[4], 3 * BAR + 72_000);
    assert!(
        downbeats.contains(&(9 * BAR + 5)),
        "6/8 restarts on its point"
    );

    for &bar in &downbeats {
        assert_eq!(
            grid.next_bar_line(bar),
            Some(bar),
            "a bar line is its own next"
        );
    }
    for pair in downbeats.windows(2) {
        assert_eq!(grid.next_bar_line(pair[0] + 1), Some(pair[1]));
        assert_eq!(grid.next_bar_line(pair[1] - 1), Some(pair[1]));
    }
    let last = *downbeats.last().unwrap();
    assert_eq!(
        grid.next_bar_line(last + 1),
        None,
        "no bar line after the last"
    );
    assert_eq!(grid.next_bar_line(u64::MAX), None);
}

#[test]
fn bar_and_beat_count_from_one_at_the_song_start() {
    let song = changing_spec().build();
    let grid = BeatGrid::new(&song);

    assert_eq!(grid.bar_and_beat(0), BarBeat { bar: 1, beat: 1 });
    assert_eq!(grid.bar_and_beat(23_999), BarBeat { bar: 1, beat: 1 });
    assert_eq!(grid.bar_and_beat(24_000), BarBeat { bar: 1, beat: 2 });
    assert_eq!(grid.bar_and_beat(BAR - 1), BarBeat { bar: 1, beat: 4 });
    assert_eq!(grid.bar_and_beat(BAR), BarBeat { bar: 2, beat: 1 });
    // The 3/4 bar after the meter change: bar 4, three beats.
    assert_eq!(
        grid.bar_and_beat(3 * BAR + 48_000),
        BarBeat { bar: 4, beat: 3 }
    );
    assert_eq!(
        grid.bar_and_beat(3 * BAR + 72_000),
        BarBeat { bar: 5, beat: 1 }
    );
    // Bars keep counting across the 6/8 point.
    let six_eight = grid.next_bar_line(9 * BAR).unwrap();
    assert_eq!(six_eight, 9 * BAR + 5);
    let before = grid.bar_and_beat(six_eight - 1);
    assert_eq!(
        grid.bar_and_beat(six_eight),
        BarBeat {
            bar: before.bar + 1,
            beat: 1
        }
    );
}

#[test]
fn from_maps_gives_the_grid_of_a_song_with_those_maps() {
    let song = changing_spec().build();

    let grid = BeatGrid::from_maps(
        song.sample_rate(),
        song.length(),
        song.tempo_map(),
        song.time_signature_map(),
    );

    assert_eq!(grid, Some(BeatGrid::new(&song)));
}

#[test]
fn bar_lines_and_beat_lines_are_where_each_bar_and_each_beat_starts() {
    let tempo = [TempoPoint { at: 0, bpm: 120.0 }];
    let meter = [TimeSignaturePoint {
        at: 0,
        numerator: 4,
        denominator: 4,
    }];
    let grid = BeatGrid::from_maps(RATE, 2 * BAR + 1, &tempo, &meter).unwrap();

    assert_eq!(grid.bar_lines(), [0, BAR, 2 * BAR]);
    let beats: Vec<u64> = (0..=8).map(|beat| beat * 24_000).collect();
    assert_eq!(grid.beat_lines(), beats.as_slice());

    // Across tempo and meter changes they are the plan's downbeat and beat
    // ticks.
    let song = changing_spec().build();
    let grid = BeatGrid::new(&song);
    let ticks = |levels: &[ClickLevel]| -> Vec<u64> {
        plan(&song, "default")
            .clicks
            .iter()
            .filter(|click| levels.contains(&click.level))
            .map(|click| click.at)
            .collect()
    };
    assert_eq!(grid.bar_lines(), ticks(&[ClickLevel::Downbeat]).as_slice());
    assert_eq!(
        grid.beat_lines(),
        ticks(&[ClickLevel::Downbeat, ClickLevel::Beat]).as_slice()
    );
    assert!(
        grid.bar_lines().contains(&(9 * BAR + 5)),
        "6/8 restarts a bar"
    );
}

#[test]
fn from_maps_refuses_maps_that_break_the_package_rules() {
    let tempo = [TempoPoint { at: 0, bpm: 120.0 }];
    let meter = [TimeSignaturePoint {
        at: 0,
        numerator: 4,
        denominator: 4,
    }];
    let late_tempo = [TempoPoint { at: 5, bpm: 120.0 }];
    let slow = [TempoPoint { at: 0, bpm: 0.0 }];
    let no_beats = [TimeSignaturePoint {
        at: 0,
        numerator: 0,
        denominator: 4,
    }];
    let odd_note = [TimeSignaturePoint {
        at: 0,
        numerator: 4,
        denominator: 3,
    }];
    let too_long = MAX_SONG_HOURS * 3_600 * u64::from(RATE) + 1;

    assert!(BeatGrid::from_maps(RATE, BAR, &tempo, &meter).is_some());
    assert_eq!(BeatGrid::from_maps(0, BAR, &tempo, &meter), None);
    assert_eq!(BeatGrid::from_maps(RATE, too_long, &tempo, &meter), None);
    assert_eq!(BeatGrid::from_maps(RATE, BAR, &[], &meter), None);
    assert_eq!(BeatGrid::from_maps(RATE, BAR, &tempo, &[]), None);
    assert_eq!(BeatGrid::from_maps(RATE, BAR, &late_tempo, &meter), None);
    assert_eq!(BeatGrid::from_maps(RATE, BAR, &slow, &meter), None);
    assert_eq!(BeatGrid::from_maps(RATE, BAR, &tempo, &no_beats), None);
    assert_eq!(BeatGrid::from_maps(RATE, BAR, &tempo, &odd_note), None);
}

#[test]
fn the_existing_queries_still_agree_with_the_bar_and_beat_lines() {
    let song = changing_spec().build();
    let grid = BeatGrid::new(&song);

    for (index, &line) in grid.bar_lines().iter().enumerate() {
        assert_eq!(grid.next_bar_line(line), Some(line));
        assert_eq!(
            grid.bar_and_beat(line),
            BarBeat {
                bar: index as u64 + 1,
                beat: 1
            }
        );
    }
    for pair in grid.beat_lines().windows(2) {
        let (beat, next) = (grid.bar_and_beat(pair[0]), grid.bar_and_beat(pair[1]));
        assert!(
            next == BarBeat {
                bar: beat.bar,
                beat: beat.beat + 1
            } || next
                == BarBeat {
                    bar: beat.bar + 1,
                    beat: 1
                },
            "{} -> {}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn a_draft_still_places_each_change_on_the_bar_lines_of_the_maps_before_it() {
    let mut draft = SongDraft::new(
        "Song",
        vec![DraftStem::new(
            "Drums.wav",
            StoredFormat::Wav,
            RATE,
            60 * 48_000,
        )],
    );
    draft.changes = vec![
        MapChange {
            bar: 7,
            bpm: None,
            time_signature: Some(TimeSignature {
                numerator: 3,
                denominator: 4,
            }),
        },
        MapChange {
            bar: 5,
            bpm: Some(90.0),
            time_signature: None,
        },
    ];

    let package = draft.to_package().unwrap().package;

    let length = draft.length_frames();
    let start = [TimeSignaturePoint {
        at: 0,
        numerator: 4,
        denominator: 4,
    }];
    let before_tempo =
        BeatGrid::from_maps(RATE, length, &[TempoPoint { at: 0, bpm: 120.0 }], &start).unwrap();
    let before_meter = BeatGrid::from_maps(RATE, length, &package.tempo_map, &start).unwrap();
    assert_eq!(package.tempo_map[1].at, before_tempo.bar_lines()[4]);
    assert_eq!(
        package.time_signature_map[1].at,
        before_meter.bar_lines()[6]
    );
    assert_eq!(
        (package.tempo_map[1].at, package.time_signature_map[1].at),
        (384_000, 640_000)
    );
}
