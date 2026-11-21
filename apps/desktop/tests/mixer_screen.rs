//! S3 (F7): the Mixer screen without a window. A song of three stems opens
//! on the app's `FakeOutput`; each control sends its mixer command, the view
//! follows the session's mix, "Save mix to arrangement" writes `mix.json`
//! only when the mix changed, "Reset to song mix" returns every stem to its
//! defaults, and bus and master changes are saved once they settle. The
//! header and the strips show what the design canvas's Mixer artboard
//! shows: the song's place in a setlist, its arrangement, the time, the
//! play/pause state, and each stem named and drawn after its part. Every
//! folder is temporary.

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use app::SessionOptions;
use app::testing::FakeOutput;
use domain::{
    Arrangement, ArrangementMix, Bus, BusSettings, Routing, SectionMarker, SectionType, Setlist,
    SetlistItem, SongItem, SongPackage, Stem, StemRole, TempoPoint, TimeSignaturePoint, Volume,
};
use hound::{SampleFormat, WavSpec, WavWriter};
use library::{ConfigStore, Library};
use openvamp_desktop::action::UiAction;
use openvamp_desktop::audio_settings::AudioSettings;
use openvamp_desktop::mixer::{
    BUS_SAVE_DELAY, Layer, MixerAction, MixerPage, bus_icon, db_text, fader_position, fader_volume,
    stem_icon, stem_name,
};
use openvamp_desktop::show::{Show, Source, replace};

const RATE: u32 = 8_000;

/// How long a test waits for the audio and disk threads.
const WAIT: Duration = Duration::from_secs(10);

/// A two-bar song with a drums and a keys track and a guide.
fn write_song(dir: &Path) {
    write_song_titled(dir, "Rivers Rise");
}

/// [`write_song`] titled `title`.
fn write_song_titled(dir: &Path, title: &str) {
    write_song_with(
        dir,
        title,
        &[
            ("drums", "drums", StemRole::Track),
            ("keys", "keys", StemRole::Track),
            ("guide", "guide", StemRole::Guide),
        ],
    );
}

/// [`write_song`] titled `title` with `stems`, each an id, a part and a
/// role.
fn write_song_with(dir: &Path, title: &str, stems: &[(&str, &str, StemRole)]) {
    let bar = 2 * u64::from(RATE);
    let section = |id: &str, section_type, start| SectionMarker {
        id: id.to_owned(),
        section_type,
        number: None,
        start,
    };
    let package = SongPackage {
        format_version: 1,
        title: title.to_owned(),
        artist: None,
        sample_rate: RATE,
        key: None,
        tempo_map: vec![TempoPoint { at: 0, bpm: 120.0 }],
        time_signature_map: vec![TimeSignaturePoint {
            at: 0,
            numerator: 4,
            denominator: 4,
        }],
        stems: stems
            .iter()
            .map(|&(id, part, role)| Stem {
                id: id.to_owned(),
                file: format!("{id}.wav"),
                part: part.to_owned(),
                role,
            })
            .collect(),
        sections: vec![
            section("intro", SectionType::Intro, 0),
            section("outro", SectionType::Outro, bar),
        ],
        arrangements: vec![Arrangement {
            id: "default".to_owned(),
            name: "Default".to_owned(),
            sections: vec!["intro".to_owned(), "outro".to_owned()],
            infinite_loop: Vec::new(),
        }],
    };
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("song.json"), package.to_json().unwrap()).unwrap();
    let spec = WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    for &(id, _, _) in stems {
        let mut writer = WavWriter::create(dir.join(format!("{id}.wav")), spec).unwrap();
        for frame in 0..2 * bar {
            writer.write_sample((frame % 50) as i16 * 20).unwrap();
        }
        writer.finalize().unwrap();
    }
}

struct Bench {
    dir: tempfile::TempDir,
    show: Show,
    settings: AudioSettings,
    page: MixerPage,
}

impl Bench {
    fn new() -> Self {
        Self::on(FakeOutput::new())
    }

    fn on(fake: FakeOutput) -> Self {
        let dir = tempfile::tempdir().unwrap();
        write_song(&dir.path().join("Rivers Rise.song"));
        let settings = AudioSettings::load(Some(Self::store_in(dir.path())));
        let show = Self::open(dir.path(), &fake, &settings);
        Self {
            dir,
            show,
            settings,
            page: MixerPage::new(),
        }
    }

    fn store_in(dir: &Path) -> ConfigStore {
        ConfigStore::new(dir.join("config"))
    }

    fn open(dir: &Path, fake: &FakeOutput, settings: &AudioSettings) -> Show {
        let devices = app::AudioOutput::devices(fake).unwrap();
        let options: SessionOptions = settings.options(&devices);
        let mut loaded = None;
        let source = Source::Song(dir.join("Rivers Rise.song"));
        replace(
            &mut loaded,
            &source,
            &options,
            || Box::new(fake.clone()),
            |_| {},
        )
        .unwrap_or_else(|_| panic!("the song opens"));
        loaded.expect("the song is loaded")
    }

    fn song(&self) -> std::path::PathBuf {
        self.dir.path().join("Rivers Rise.song")
    }

    fn act(&mut self, action: MixerAction) {
        self.page.act(action, &mut self.show, Instant::now());
    }

    fn view(&self) -> openvamp_desktop::mixer::MixerScreen {
        self.page.view(Some(&self.show)).expect("a song is loaded")
    }

    fn mix(&self) -> ArrangementMix {
        self.show.snapshot().mixer.mix.clone()
    }
}

#[test]
fn the_fader_runs_from_silent_at_the_bottom_through_unity_to_plus_12_db_at_the_top() {
    assert_eq!(fader_position(Volume::SILENT), 0.0);
    assert_eq!(fader_volume(0.0), Volume::SILENT);
    assert_eq!(db_text(Volume::SILENT), "-inf");
    assert_eq!(fader_volume(1.0), Volume::from_db(12.0));
    assert_eq!(db_text(Volume::from_db(12.0)), "+12.0");
    assert_eq!(db_text(Volume::UNITY), "0.0");
    assert_eq!(db_text(Volume::from_db(-6.54)), "-6.5");
    assert_eq!(db_text(Volume::from_db(-0.04)), "0.0", "never -0.0");
    let unity = fader_position(Volume::UNITY);
    assert!(
        (0.45..0.55).contains(&unity),
        "0 dB sits near the middle: {unity}"
    );
    assert_eq!(fader_volume(unity), Volume::UNITY);
    let mut last = Volume::SILENT.db();
    for step in 1..=100 {
        let db = fader_volume(step as f32 / 100.0).db();
        assert!(db > last, "rises all the way: {db} after {last}");
        last = db;
    }
    for db in [-40.0, -12.5, -6.0, -0.5, 3.0, 11.9] {
        let volume = Volume::from_db(db);
        assert_eq!(fader_volume(fader_position(volume)), volume, "{db} dB");
    }
}

#[test]
fn the_stems_layer_shows_every_stem_with_its_bus_level_mute_and_solo() {
    let bench = Bench::new();

    let view = bench.view();

    assert_eq!(view.title, "Rivers Rise");
    assert_eq!(view.arrangement, "Arrangement: default");
    assert_eq!(view.layer, Layer::Stems);
    assert_eq!(view.summary, "3 stems · tap a bus to reassign");
    let stems: Vec<(&str, Bus, bool, &str, bool, bool, bool)> = view
        .stems
        .iter()
        .map(|stem| {
            (
                stem.name.as_str(),
                stem.bus,
                stem.default_bus,
                stem.db.as_str(),
                stem.mute,
                stem.solo,
                stem.silenced,
            )
        })
        .collect();
    assert_eq!(
        stems,
        [
            ("Drums", Bus::Drums, true, "0.0", false, false, false),
            ("Keys", Bus::KeysPads, true, "0.0", false, false, false),
            ("Guide", Bus::ClickGuide, true, "0.0", false, false, false),
        ]
    );
    assert!(!view.can_save, "nothing changed");
    assert!(!view.can_reset, "every stem is at its defaults");
    assert_eq!(view.notices, Vec::<String>::new());
}

#[test]
fn every_stem_control_sends_its_command_and_the_view_follows() {
    let mut bench = Bench::new();

    bench.act(MixerAction::StemFader {
        stem: 0,
        position: 0.25,
    });
    assert_eq!(bench.mix().stem("drums").volume, fader_volume(0.25));
    assert_eq!(bench.view().stems[0].db, db_text(fader_volume(0.25)));
    bench.act(MixerAction::StemUnity(0));
    assert_eq!(bench.mix().stem("drums").volume, Volume::UNITY);

    bench.act(MixerAction::ToggleStemMute(1));
    assert!(bench.mix().stem("keys").mute);
    assert!(bench.view().stems[1].mute && bench.view().stems[1].silenced);
    bench.act(MixerAction::ToggleStemMute(1));
    assert!(!bench.mix().stem("keys").mute);

    bench.act(MixerAction::ToggleStemSolo(1));
    let view = bench.view();
    assert!(view.stems[1].solo);
    assert!(view.stems[0].silenced, "another stem is soloed");
    assert!(!view.stems[2].silenced, "solo never silences the guide");
    bench.act(MixerAction::ToggleStemSolo(1));
    assert!(!bench.mix().stem("keys").solo);

    bench.act(MixerAction::StemBus {
        stem: 0,
        bus: Some(Bus::Vocals),
    });
    let drums = &bench.view().stems[0];
    assert_eq!((drums.bus, drums.default_bus), (Bus::Vocals, false));
    bench.act(MixerAction::StemBus { stem: 0, bus: None });
    let drums = &bench.view().stems[0];
    assert_eq!((drums.bus, drums.default_bus), (Bus::Drums, true));
    assert_eq!(bench.mix(), ArrangementMix::default());
}

#[test]
fn save_mix_is_enabled_only_once_the_mix_changed_and_writes_mix_json() {
    let mut bench = Bench::new();
    bench.act(MixerAction::StemFader {
        stem: 1,
        position: 0.3,
    });
    assert!(bench.view().can_save);

    bench.act(MixerAction::Save);

    let view = bench.view();
    assert!(!view.can_save, "the file holds the mix now");
    assert!(view.can_reset);
    let song = library::load_song(&bench.song()).unwrap().song;
    let saved = library::read_mix(&bench.song(), &song).mix;
    assert_eq!(saved.arrangement("default"), bench.mix());
    // Saving again with no change is not offered.
    bench.act(MixerAction::StemFader {
        stem: 1,
        position: 0.3,
    });
    assert!(!bench.view().can_save);
}

#[cfg(unix)]
#[test]
fn a_mix_the_song_folder_refuses_shows_why_and_stays_unsaved() {
    use std::os::unix::fs::PermissionsExt;

    let mut bench = Bench::new();
    bench.act(MixerAction::ToggleStemMute(0));
    let mode = |mode| fs::Permissions::from_mode(mode);

    fs::set_permissions(bench.song(), mode(0o555)).unwrap();
    bench.act(MixerAction::Save);
    fs::set_permissions(bench.song(), mode(0o755)).unwrap();

    let view = bench.view();
    assert!(view.can_save, "still unsaved");
    assert_eq!(view.notices.len(), 1, "{:?}", view.notices);
    assert!(
        view.notices[0].starts_with("The mix could not be saved: "),
        "{:?}",
        view.notices
    );
    bench.act(MixerAction::Save);
    assert!(!bench.view().can_save);
    assert_eq!(bench.view().notices, Vec::<String>::new());
}

#[test]
fn reset_to_song_mix_returns_every_stem_to_its_defaults() {
    let mut bench = Bench::new();
    bench.act(MixerAction::ToggleStemSolo(0));
    bench.act(MixerAction::StemBus {
        stem: 1,
        bus: Some(Bus::Guitars),
    });
    bench.act(MixerAction::BusFader {
        bus: Bus::Drums,
        position: 0.2,
    });
    assert!(bench.view().can_reset);

    bench.act(MixerAction::Reset);

    let view = bench.view();
    assert_eq!(bench.mix(), ArrangementMix::default());
    assert!(!view.can_reset);
    assert!(!view.can_save, "the song had no saved mix");
    assert_eq!(view.stems[1].bus, Bus::KeysPads);
    assert_eq!(
        view.buses[Bus::Drums.index()].volume,
        fader_volume(0.2),
        "bus levels are global and stay"
    );
}

#[test]
fn bus_and_master_controls_are_global_and_saved_once_they_settle() {
    let mut bench = Bench::new();
    let start = Instant::now();
    let act = |bench: &mut Bench, action| {
        bench.page.act(action, &mut bench.show, start);
    };
    act(
        &mut bench,
        MixerAction::BusFader {
            bus: Bus::Drums,
            position: 0.3,
        },
    );
    act(&mut bench, MixerAction::ToggleBusMute(Bus::Vocals));
    act(&mut bench, MixerAction::MasterFader(0.4));
    act(&mut bench, MixerAction::BusUnity(Bus::Bass));
    let view = bench.view();
    assert_eq!(view.buses[Bus::Drums.index()].volume, fader_volume(0.3));
    assert!(view.buses[Bus::Vocals.index()].mute);
    assert_eq!(view.master.volume, fader_volume(0.4));
    let expected = bench.show.bus_settings().clone();
    assert_ne!(expected, BusSettings::new());

    // Not yet: the changes have not settled.
    bench.page.tick(
        &mut bench.show,
        &mut bench.settings,
        start + BUS_SAVE_DELAY / 2,
    );
    let unsaved = AudioSettings::load(Some(Bench::store_in(bench.dir.path())));
    assert_eq!(unsaved.options(&[]).bus_settings, BusSettings::new());
    bench
        .page
        .tick(&mut bench.show, &mut bench.settings, start + BUS_SAVE_DELAY);

    assert!(!bench.show.snapshot().mixer.buses_changed);
    // The next session opens with them (on another device: this one is
    // still open).
    let settings = AudioSettings::load(Some(Bench::store_in(bench.dir.path())));
    let show = Bench::open(bench.dir.path(), &FakeOutput::new(), &settings);
    assert_eq!(show.snapshot().mixer.buses, expected);
    assert_eq!(show.bus_settings(), &expected);
}

#[test]
fn the_buses_layer_shows_each_bus_with_its_output_from_the_routing() {
    let fake = FakeOutput::without_devices().with_device("Interface", 8, None);
    let mut bench = Bench::on(fake);

    bench.act(MixerAction::ShowLayer(Layer::Buses));

    let view = bench.view();
    assert_eq!(view.layer, Layer::Buses);
    assert_eq!(view.outputs, "8 outputs");
    let buses: Vec<(Bus, &str, &str, bool)> = view
        .buses
        .iter()
        .map(|bus| (bus.bus, bus.output.as_str(), bus.db.as_str(), bus.mute))
        .collect();
    assert_eq!(
        buses,
        [
            (Bus::ClickGuide, "Out 1", "0.0", false),
            (Bus::Drums, "Out 2", "0.0", false),
            (Bus::Bass, "Out 3", "0.0", false),
            (Bus::KeysPads, "Out 5/6", "0.0", false),
            (Bus::Guitars, "Out 4", "0.0", false),
            (Bus::Vocals, "Out 7/8", "0.0", false),
        ]
    );
    assert_eq!(bench.show.snapshot().mixer.routing, Routing::preset(8));
    assert_eq!(
        (view.master.output.as_str(), view.master.db.as_str()),
        ("Out All", "0.0")
    );
    let icons: Vec<&str> = view.buses.iter().map(|bus| bus.icon).collect();
    assert_eq!(
        icons,
        [
            "headphones",
            "drum",
            "bass",
            "piano",
            "electric-guitar",
            "mic"
        ]
    );
}

#[test]
fn split_mode_shows_click_and_guide_on_output_1_and_the_rest_on_output_2() {
    let mut bench = Bench::new();
    let outputs = |bench: &Bench| -> Vec<String> {
        bench
            .view()
            .buses
            .into_iter()
            .map(|bus| bus.output)
            .collect()
    };
    // A 2-output device starts in plain stereo.
    assert_eq!(outputs(&bench), ["Out 1/2"; 6]);

    bench
        .show
        .set_routing(Routing {
            split_mode: true,
            ..Routing::preset(2)
        })
        .unwrap();
    bench.show.poll();

    assert_eq!(
        outputs(&bench),
        ["Out 1", "Out 2", "Out 2", "Out 2", "Out 2", "Out 2"]
    );
    assert_eq!(bench.view().outputs, "2 outputs");
}

#[test]
fn with_nothing_loaded_there_is_nothing_to_mix() {
    assert!(MixerPage::new().view(None).is_none());
    let _ = Duration::ZERO;
}

#[test]
fn a_bus_chip_opens_its_choices_and_a_choice_closes_them() {
    let mut bench = Bench::new();

    bench.act(MixerAction::OpenBusMenu(1));
    assert_eq!(bench.view().bus_menu, Some(1));
    bench.act(MixerAction::OpenBusMenu(1));
    assert_eq!(bench.view().bus_menu, None, "a second press closes them");
    bench.act(MixerAction::OpenBusMenu(1));
    bench.act(MixerAction::StemBus {
        stem: 1,
        bus: Some(Bus::Bass),
    });

    let view = bench.view();
    assert_eq!(view.bus_menu, None);
    assert_eq!(view.stems[1].bus, Bus::Bass);
}

#[test]
fn a_fader_drag_reads_the_cap_centre_along_the_padded_track() {
    use openvamp_desktop::mixer::{FADER_PADDING, fader_drag_position};

    // 200 points of travel between the paddings.
    let (top, height) = (100.0, 200.0 + 2.0 * FADER_PADDING);

    assert_eq!(fader_drag_position(top, height, top + FADER_PADDING), 1.0);
    assert_eq!(
        fader_drag_position(top, height, top + height - FADER_PADDING),
        0.0
    );
    assert_eq!(fader_drag_position(top, height, top + height / 2.0), 0.5);
    assert_eq!(fader_drag_position(top, height, top - 50.0), 1.0, "above");
    assert_eq!(fader_drag_position(top, height, top + height + 50.0), 0.0);
    let squeezed = fader_drag_position(top, 2.0 * FADER_PADDING, top + 3.0);
    assert!((0.0..=1.0).contains(&squeezed), "no travel: {squeezed}");
}

#[test]
fn a_strip_names_its_stem_after_the_part_in_title_case() {
    assert_eq!(stem_name("drums", "s1"), "Drums");
    assert_eq!(stem_name("synth bass", "s1"), "Synth Bass");
    assert_eq!(stem_name("BGVs", "s1"), "BGVs", "capitals stay");
    assert_eq!(stem_name("EG 1", "s1"), "EG 1");
    assert_eq!(stem_name("eg_1", "s1"), "Eg 1", "underscores are spaces");
    assert_eq!(stem_name("hi-hat", "s1"), "Hi-Hat");
    assert_eq!(
        stem_name("room mics (drums  live)", "s1"),
        "Room Mics (Drums Live)"
    );
    assert_eq!(stem_name("batería", "s1"), "Batería");
    assert_eq!(stem_name(" ", "lead_vox"), "Lead Vox", "no part: its id");
}

#[test]
fn a_strip_draws_the_instrument_its_part_names() {
    let track = |part: &str| stem_icon(StemRole::Track, part, "stem");
    // The design canvas's stems and the icon it draws for each.
    for (part, icon) in [
        ("Click", "metronome"),
        ("Drums", "drum"),
        ("Perc", "maraca"),
        ("Loop", "loop"),
        ("Bass", "bass"),
        ("Synth Bass", "sine"),
        ("Keys", "piano"),
        ("Pad", "sine"),
        ("Strings", "violin"),
        ("EG 1", "electric-guitar"),
        ("AG", "acoustic-guitar"),
        ("BGVs", "voices"),
        ("Choir", "voices"),
        ("FX", "sparkle"),
        ("Hi-Hat", "drum"),
        ("Percussion Loop 1", "maraca"),
        ("Sequence Arp 1/8", "loop"),
        ("Pad Ambient Swell", "sine"),
        ("Room Mics L/R (Drums Live)", "drum"),
        ("Lead Vocal Reference", "mic"),
        ("Spoken Word Intro", "speech"),
        ("Acoustic Guitar", "acoustic-guitar"),
        ("AcGtr", "acoustic-guitar"),
        ("Batería", "drum"),
        ("Track 7", "note"),
    ] {
        assert_eq!(track(part), icon, "{part}");
    }
    assert_eq!(stem_icon(StemRole::Click, "Track 1", "a"), "metronome");
    assert_eq!(stem_icon(StemRole::Guide, "Cues", "b"), "speech");
    assert_eq!(
        stem_icon(StemRole::Track, "Track 2", "kick_in"),
        "drum",
        "a part that names nothing: its id"
    );
    assert_eq!(bus_icon(Bus::Vocals), "mic");
}

#[test]
fn the_stems_layer_names_and_draws_each_stem_after_its_part() {
    let bench = Bench::new();

    let view = bench.view();

    let stems: Vec<(&str, &str)> = view
        .stems
        .iter()
        .map(|stem| (stem.name.as_str(), stem.icon))
        .collect();
    assert_eq!(
        stems,
        [("Drums", "drum"), ("Keys", "piano"), ("Guide", "speech")]
    );
}

#[test]
fn the_header_shows_the_time_and_whether_the_song_plays() {
    let fake = FakeOutput::new();
    let mut bench = Bench::on(fake.clone());

    let view = bench.view();
    assert_eq!(
        view.position, None,
        "a song alone has no place in a setlist"
    );
    assert_eq!(view.arrangement, "Arrangement: default");
    assert_eq!(
        (view.time.as_str(), view.total.as_str()),
        ("00:00", "00:04")
    );
    assert!(!view.playing);

    bench.show.send(UiAction::TogglePlay);
    let deadline = Instant::now() + WAIT;
    while !bench.view().playing && Instant::now() < deadline {
        fake.render(256);
        bench.show.poll();
    }
    assert!(bench.view().playing, "play shows pause");
}

#[test]
fn the_header_of_a_setlist_item_shows_its_place_and_the_arrangement_name() {
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path();
    write_song_titled(&library.join("Songs/One.song"), "First Light");
    write_song_titled(&library.join("Songs/Two.song"), "Rivers Rise");
    let mut setlist = Setlist::new("Sunday");
    setlist.items = ["Songs/One.song", "Songs/Two.song", "Songs/One.song"]
        .map(|song| SetlistItem::Song(SongItem::new(song)))
        .to_vec();
    let entry = Library::new(library).save_setlist(&setlist).unwrap();
    let source = Source::Setlist {
        library: library.to_owned(),
        file: entry.file,
    };
    let fake = FakeOutput::new();
    let mut loaded = None;
    replace(
        &mut loaded,
        &source,
        &SessionOptions::default(),
        || Box::new(fake.clone()),
        |_| {},
    )
    .unwrap_or_else(|_| panic!("the setlist opens"));
    let mut show = loaded.expect("the setlist is loaded");
    let page = MixerPage::new();
    assert_eq!(
        page.view(Some(&show)).unwrap().position.as_deref(),
        Some("Song 1 of 3")
    );

    show.send(UiAction::SelectItem(1));
    let deadline = Instant::now() + WAIT;
    let place = |show: &Show| page.view(Some(show)).unwrap().position;
    while place(&show).as_deref() != Some("Song 2 of 3") && Instant::now() < deadline {
        fake.render(256);
        show.poll();
    }

    let view = page.view(Some(&show)).unwrap();
    assert_eq!(view.title, "Rivers Rise");
    assert_eq!(view.position.as_deref(), Some("Song 2 of 3"));
    assert_eq!(
        view.arrangement, "Arrangement: Default",
        "the library names it"
    );
}

#[test]
fn stems_that_share_a_part_are_named_by_their_ids() {
    let dir = tempfile::tempdir().unwrap();
    let song = dir.path().join("Rivers Rise.song");
    write_song_with(
        &song,
        "Rivers Rise",
        &[
            ("EG 1", "electric guitar", StemRole::Track),
            ("eg_2", "Electric Guitar", StemRole::Track),
            ("bass_di", "bass", StemRole::Track),
        ],
    );
    let fake = FakeOutput::new();
    let mut loaded = None;
    replace(
        &mut loaded,
        &Source::Song(song),
        &SessionOptions::default(),
        || Box::new(fake.clone()),
        |_| {},
    )
    .unwrap_or_else(|_| panic!("the song opens"));

    let view = MixerPage::new().view(loaded.as_ref()).unwrap();

    let names: Vec<&str> = view.stems.iter().map(|stem| stem.name.as_str()).collect();
    assert_eq!(names, ["EG 1", "Eg 2", "Bass"]);
    let icons: Vec<&str> = view.stems.iter().map(|stem| stem.icon).collect();
    assert_eq!(icons, ["electric-guitar", "electric-guitar", "bass"]);
}
