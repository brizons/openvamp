//! S2: the fixed list of instrument parts, and the part a stem's file name
//! gives it by the rules table that also gives its bus (F1.2).

use domain::{Bus, DEFAULT_BUS, Part, StemRole, bus_for_name, part_for_file, part_for_name};

/// Every part in menu order, with its id, display name and bus.
const PARTS: [(Part, &str, &str, Bus); 23] = [
    (Part::Click, "click", "Click", Bus::ClickGuide),
    (Part::Guide, "guide", "Guide", Bus::ClickGuide),
    (Part::Drums, "drums", "Drums", Bus::Drums),
    (Part::Percussion, "percussion", "Percussion", Bus::Drums),
    (Part::Loop, "loop", "Loop", Bus::Drums),
    (Part::Bass, "bass", "Bass", Bus::Bass),
    (Part::SynthBass, "synth-bass", "Synth bass", Bus::Bass),
    (Part::Keys, "keys", "Keys", Bus::KeysPads),
    (Part::Piano, "piano", "Piano", Bus::KeysPads),
    (Part::Organ, "organ", "Organ", Bus::KeysPads),
    (Part::Synth, "synth", "Synth", Bus::KeysPads),
    (Part::Pad, "pad", "Pad", Bus::KeysPads),
    (Part::Strings, "strings", "Strings", Bus::KeysPads),
    (Part::Brass, "brass", "Brass", Bus::KeysPads),
    (Part::Woodwinds, "woodwinds", "Woodwinds", Bus::KeysPads),
    (Part::Bells, "bells", "Bells", Bus::KeysPads),
    (Part::Fx, "fx", "FX", Bus::KeysPads),
    (Part::Other, "other", "Other", Bus::KeysPads),
    (
        Part::ElectricGuitar,
        "electric-guitar",
        "Electric guitar",
        Bus::Guitars,
    ),
    (
        Part::AcousticGuitar,
        "acoustic-guitar",
        "Acoustic guitar",
        Bus::Guitars,
    ),
    (Part::Guitar, "guitar", "Guitar", Bus::Guitars),
    (Part::Vocals, "vocals", "Vocals", Bus::Vocals),
    (
        Part::BackingVocals,
        "backing-vocals",
        "Backing vocals",
        Bus::Vocals,
    ),
];

#[test]
fn a_stem_named_eg_1_is_an_electric_guitar_on_guitars() {
    // F1 acceptance: "A stem named `EG 1.wav` is assigned to the
    // electric-guitar part and bus without user action."
    for file in [
        "EG 1.wav",
        "stems/EG 1.wav",
        r"Audio\EG 1.flac",
        "Eg1.wav",
        "eg_1.mp3",
    ] {
        let part = part_for_file(file);
        assert_eq!(part, Part::ElectricGuitar, "{file:?}");
        assert_eq!(part.id(), "electric-guitar");
        assert_eq!(part.bus(), Bus::Guitars);
    }
    for name in ["EG 1", "Eg1", "eg_1"] {
        assert_eq!(part_for_name(name), Some(Part::ElectricGuitar), "{name:?}");
        assert_eq!(bus_for_name(name), Some(Bus::Guitars), "{name:?}");
    }
}

#[test]
fn every_part_round_trips_through_its_id() {
    for (part, id, name, _) in PARTS {
        assert_eq!(part.id(), id);
        assert_eq!(Part::from_id(id), Some(part), "{id:?}");
        assert_eq!(part.name(), name);
        assert_eq!(part.to_string(), name);
    }
}

#[test]
fn unknown_ids_name_no_part() {
    for id in [
        "",
        "Electric-Guitar",
        "electric guitar",
        "electricGuitar",
        "electric_guitar",
        "Drums",
        " drums",
        "guitars",
        "eg",
        "keysPads",
    ] {
        assert_eq!(Part::from_id(id), None, "{id:?} is no part id");
    }
}

#[test]
fn every_part_belongs_to_its_bus() {
    for (part, _, _, bus) in PARTS {
        assert_eq!(part.bus(), bus, "{part}");
    }
    assert_eq!(Part::Other.bus(), DEFAULT_BUS);
}

#[test]
fn only_the_click_and_guide_parts_change_the_stem_role() {
    assert_eq!(Part::Click.role(), StemRole::Click);
    assert_eq!(Part::Guide.role(), StemRole::Guide);
    for (part, _, _, _) in PARTS {
        if !matches!(part, Part::Click | Part::Guide) {
            assert_eq!(part.role(), StemRole::Track, "{part}");
        }
    }
}

#[test]
fn a_file_naming_no_instrument_is_other() {
    for file in [
        "Track 12.wav",
        "stems/Stereo Mix.flac",
        r"Audio\1 2 3.wav",
        "---.wav",
        ".wav",
        "",
        // Only the file's own name counts, not the folders above it.
        "Keys/Track 7.wav",
    ] {
        assert_eq!(part_for_file(file), Part::Other, "{file:?}");
    }
    assert_eq!(part_for_name("Track 12"), None);
    // The extension never names a part, and a named file keeps its part.
    assert_eq!(part_for_file("Guitars/Bass.wav"), Part::Bass);
    assert_eq!(part_for_file("stems/Batería 2.flac"), Part::Drums);
}

#[test]
fn two_word_names_pick_the_precise_part() {
    for (name, part) in [
        ("Electric Guitar", Part::ElectricGuitar),
        ("Electric Gtr 2", Part::ElectricGuitar),
        ("Guitarra Eléctrica", Part::ElectricGuitar),
        ("GUITARRA ELECTRICA", Part::ElectricGuitar),
        ("Acoustic Guitar", Part::AcousticGuitar),
        ("Acoustic Gtrs", Part::AcousticGuitar),
        ("AcGtr", Part::AcousticGuitar),
        ("Guitarra Acústica", Part::AcousticGuitar),
        ("Synth Bass", Part::SynthBass),
        ("SynthBass", Part::SynthBass),
        ("Bass Synth", Part::SynthBass),
        ("Sub Bass", Part::SynthBass),
        ("Bajo Sinte", Part::SynthBass),
        ("Bajo Sintetizador", Part::SynthBass),
        ("Bass Guitar", Part::Bass),
        ("Bass Drum", Part::Drums),
        ("Voz Guía", Part::Vocals),
        ("Backing Vocals", Part::BackingVocals),
        // The words alone give the less precise part.
        ("Guitar", Part::Guitar),
        ("Guitarra", Part::Guitar),
        ("Electric", Part::ElectricGuitar),
        ("Bass", Part::Bass),
        ("Synth", Part::Synth),
    ] {
        assert_eq!(part_for_name(name), Some(part), "{name:?}");
        assert_eq!(bus_for_name(name), Some(part.bus()), "{name:?}");
    }
}

#[test]
fn part_all_lists_the_parts_grouped_by_bus_in_mixer_order() {
    assert_eq!(Part::ALL, PARTS.map(|(part, _, _, _)| part), "menu order");
    let buses: Vec<Bus> = Part::ALL.iter().map(|part| part.bus()).collect();
    let mut grouped = buses.clone();
    grouped.sort_by_key(|bus| bus.index());
    assert_eq!(buses, grouped, "parts are grouped by bus in Bus::ALL order");
    for bus in Bus::ALL {
        assert!(buses.contains(&bus), "{bus} has a part");
    }
}

#[test]
fn the_part_and_the_bus_come_from_the_same_rules() {
    for (name, part) in [
        ("Click Track", Part::Click),
        ("Metrónomo", Part::Click),
        ("Count In", Part::Guide),
        ("Guía", Part::Guide),
        ("Kick In", Part::Drums),
        ("Toms", Part::Drums),
        ("Room Mics L/R (Drums Live)", Part::Drums),
        ("Shaker", Part::Percussion),
        ("Percusión", Part::Percussion),
        ("Loop", Part::Loop),
        ("Secuencias", Part::Loop),
        ("Bajo Eléctrico", Part::Bass),
        ("Rhodes", Part::Keys),
        ("Guide Me O Thou - Piano", Part::Piano),
        ("Órgano", Part::Organ),
        ("Colchones", Part::Pad),
        ("Cuerdas", Part::Strings),
        ("Metales", Part::Brass),
        ("Saxo", Part::Woodwinds),
        ("Campanas", Part::Bells),
        ("Riser FX", Part::Fx),
        ("Gtr Loop", Part::Guitar),
        ("EG 2 Lead (Swells & Ambient)", Part::ElectricGuitar),
        ("AG", Part::AcousticGuitar),
        ("Requinto", Part::Guitar),
        ("Lead Vocal", Part::Vocals),
        ("Guide Vox", Part::Vocals),
        ("BGVs", Part::BackingVocals),
        ("Coros", Part::BackingVocals),
        ("Armonías", Part::BackingVocals),
    ] {
        assert_eq!(part_for_name(name), Some(part), "{name:?}");
        assert_eq!(bus_for_name(name), part_for_name(name).map(Part::bus));
    }
}

#[test]
fn every_stored_part_id_names_its_own_part() {
    // A stem's part is stored by its id in `song.json`'s `part` string, which
    // the rules table reads back.
    for (part, id, name, bus) in PARTS {
        if part == Part::Other {
            continue;
        }
        assert_eq!(part_for_name(id), Some(part), "{id:?}");
        assert_eq!(part_for_name(name), Some(part), "{name:?}");
        assert_eq!(bus_for_name(id), Some(bus), "{id:?}");
    }
    assert_eq!(part_for_name("other"), None, "other names no instrument");
}
