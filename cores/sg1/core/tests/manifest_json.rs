//! The `cartridge-manifest-v1` JSON this core writes and reads, held as
//! literals: the manifest detection gives for each kind of cartridge, the
//! bytes a parsed manifest is written back as, the region list a manifest's
//! plan describes, and the text of each error a manifest can give. A change
//! to the manifest code that moves a byte of any of them fails here.

use retro_core::cartridge::describe_map_plan;
use sg1_core::cartridge::{
    build_with_manifest, detected_manifest, manifest::manifest_to_map_plan, parse_manifest,
};

/// A Genesis ROM of `size` bytes with a valid header: `title` in both title
/// fields, and SRAM at `$200001-$20FFFF` when `with_sram`.
fn genesis_rom(size: usize, title: &str, with_sram: bool) -> Vec<u8> {
    let mut rom = vec![0u8; size.max(0x200)];
    rom[0x100..0x110].copy_from_slice(b"SEGA MEGA DRIVE ");
    rom[0x110..0x120].copy_from_slice(b"(C)TEST 2025.JAN");
    let title = format!("{title:<48}");
    rom[0x120..0x150].copy_from_slice(&title.as_bytes()[..48]);
    rom[0x150..0x180].copy_from_slice(&title.as_bytes()[..48]);
    rom[0x183..0x190].copy_from_slice(b"GM T-00000-00");
    rom[0x190..0x1A0].copy_from_slice(b"J               ");
    rom[0x1A0..0x1A4].copy_from_slice(&0u32.to_be_bytes());
    rom[0x1A4..0x1A8].copy_from_slice(&((size - 1) as u32).to_be_bytes());
    if with_sram {
        rom[0x1A8] = 0xE0;
        rom[0x1A9..0x1AC].copy_from_slice(&0x200001u32.to_be_bytes()[1..]);
        rom[0x1AC..0x1B0].copy_from_slice(&0x20FFFFu32.to_be_bytes());
    }
    rom[0x1F0..0x1F3].copy_from_slice(b"UE ");
    rom[0x200] = 0xA5;
    rom
}

/// The same with the SRAM range in the second header format (`"RA"` at
/// `$1B0`), as Landstalker's.
fn genesis_rom_ra(size: usize, title: &str) -> Vec<u8> {
    let mut rom = genesis_rom(size, title, false);
    rom[0x1B0] = b'R';
    rom[0x1B1] = b'A';
    rom[0x1B2] = 0xF8;
    rom[0x1B3] = 0x20;
    rom[0x1B4..0x1B8].copy_from_slice(&0x200001u32.to_be_bytes());
    rom[0x1B8..0x1BC].copy_from_slice(&0x203FFFu32.to_be_bytes());
    rom
}

/// The same with the SVP's `"SV"` tag at `$1C8`.
fn genesis_rom_sv(size: usize, title: &str) -> Vec<u8> {
    let mut rom = genesis_rom(size, title, false);
    rom[0x1C8] = b'S';
    rom[0x1C9] = b'V';
    rom
}

#[test]
fn the_manifest_detection_gives_is_written_as_before() {
    let cases: [(&str, Vec<u8>); 7] = [
        ("detected: plain", genesis_rom(0x80000, "PLAIN GAME", false)),
        (
            "detected: plain with SRAM",
            genesis_rom(0x200000, "RPG WITH SAVE", true),
        ),
        (
            "detected: plain with SRAM in RA format",
            genesis_rom_ra(0x100000, "RA SAVE"),
        ),
        (
            "detected: SEGA mapper",
            genesis_rom(0x500000, "BIG GAME", false),
        ),
        (
            "detected: SVP by title",
            genesis_rom(0x200000, "VIRTUA RACING", false),
        ),
        (
            "detected: SVP by tag",
            genesis_rom_sv(0x200000, "Virtua Racing"),
        ),
        ("detected: too small for a header", vec![0u8; 0x100]),
    ];
    for (name, rom) in cases {
        let json = serde_json::to_string(&detected_manifest(&rom)).unwrap();
        check(name, &json);
    }
}

#[test]
fn the_manifest_detection_gives_is_written_pretty_as_before() {
    let json =
        serde_json::to_string_pretty(&detected_manifest(&genesis_rom(0x200000, "SAVES", true)))
            .unwrap();
    check("detected: pretty", &json);
}

/// A manifest that uses every key the format has.
const EVERY_KEY: &str = r#"{
    "$schema": "cartridge-manifest-v1",
    "core": "sg1",
    "version": "1.0",
    "rom": { "sha256": "ab12", "size": 1048576, "title": "EVERY KEY", "region": "USA" },
    "mapping": {
        "preset": "custom",
        "sram_size": 8192,
        "slots": [
            {
                "addr_range": ["0x000000", "0x3FFFFF"],
                "target": { "kind": "Rom", "base_offset": 16 },
                "mask": "0xFFFFF"
            },
            {
                "addr_range": ["0x200000", "0x201FFF"],
                "target": { "kind": "Sram" }
            },
            {
                "addr_range": ["0x400000", "0x40FFFF"],
                "target": { "kind": "OpenBus" }
            }
        ]
    },
    "chips": [
        {
            "id": 2,
            "kind": "sega",
            "mapping": {
                "addr_range": ["0x300000", "0x37FFFF"],
                "target": { "kind": "ChipMmio", "chip_id": 9, "base": 256 },
                "mask": "0x7FFFF"
            },
            "observes": ["0xA130F0-0xA130FF"]
        },
        {
            "id": 3,
            "kind": "jcart",
            "mapping": {
                "addr_range": ["0x38FFFE", "0x38FFFF"],
                "target": { "kind": "Sram", "base_offset": 64 }
            }
        }
    ],
    "metadata": { "note": "kept as it is", "list": [1, 2, 3] }
}"#;

#[test]
fn a_parsed_manifest_is_written_back_as_before() {
    let manifest = parse_manifest(EVERY_KEY).unwrap();
    check(
        "written: every key, compact",
        &serde_json::to_string(&manifest).unwrap(),
    );
    check(
        "written: every key, pretty",
        &serde_json::to_string_pretty(&manifest).unwrap(),
    );
    let minimal = r#"{"core":"sg1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"plain"},"unknown":{"a":1}}"#;
    check(
        "written: minimal, an unknown key dropped",
        &serde_json::to_string(&parse_manifest(minimal).unwrap()).unwrap(),
    );
}

/// A manifest with `preset`, `sram_size` and the chips of `chips`.
fn preset_manifest(preset: &str, sram_size: usize, chips: &str) -> String {
    format!(
        r#"{{"core":"sg1","version":"1.0","rom":{{"sha256":"","size":2097152}},"mapping":{{"preset":"{preset}","sram_size":{sram_size}}},"chips":[{chips}]}}"#
    )
}

#[test]
fn each_preset_gives_the_regions_it_gave() {
    let rom = genesis_rom(0x200000, "PRESETS", false);
    let svp = r#"{"id":1,"kind":"svp","mapping":{"addr_range":["0x300000","0x31FFFF"],"target":{"kind":"ChipMmio","chip_id":1,"base":0}}}"#;
    let cases = [
        ("plain", preset_manifest("plain", 0, "")),
        ("plain with SRAM", preset_manifest("plain", 0x4000, "")),
        ("sega", preset_manifest("sega", 0, "")),
        ("ssf2 with SRAM", preset_manifest("ssf2", 0x2000, "")),
        ("codemasters", preset_manifest("codemasters", 0, "")),
        ("eeprom", preset_manifest("eeprom", 0, "")),
        ("jcart", preset_manifest("jcart", 0x2000, "")),
        ("svp", preset_manifest("svp", 0, svp)),
        ("custom", preset_manifest("custom", 0, svp)),
        ("every key", EVERY_KEY.to_string()),
    ];
    for (name, json) in cases {
        let manifest = parse_manifest(&json).unwrap();
        let plan = manifest_to_map_plan(&manifest, &rom).unwrap();
        let regions = serde_json::to_string(&describe_map_plan(&plan)).unwrap();
        check(
            &format!("regions: {name}"),
            &format!("{} {regions}", plan.name),
        );
    }
}

#[test]
fn each_error_reads_as_before() {
    let rom = genesis_rom(0x80000, "ERRORS", false);
    let parse = |json: &str| parse_manifest(json).err().map(|e| e.to_string());
    let plan = |json: &str| {
        manifest_to_map_plan(&parse_manifest(json).unwrap(), &rom)
            .err()
            .map(|e| e.to_string())
    };
    let build = |json: &str| {
        build_with_manifest(rom.clone(), &parse_manifest(json).unwrap())
            .err()
            .map(|e| e.to_string())
    };
    let slot = |range: &str, mask: &str| {
        format!(
            r#"{{"core":"sg1","version":"1.0","rom":{{"sha256":"","size":4}},"mapping":{{"preset":"custom","slots":[{{"addr_range":{range},"target":{{"kind":"Rom"}}{mask}}}]}}}}"#
        )
    };
    let chip = |kind: &str, observes: &str| {
        format!(
            r#"{{"core":"sg1","version":"1.0","rom":{{"sha256":"","size":4}},"mapping":{{"preset":"custom"}},"chips":[{{"id":0,"kind":"{kind}","mapping":{{"addr_range":["0x300000","0x30FFFF"],"target":{{"kind":"ChipMmio","chip_id":0}}}},"observes":{observes}}}]}}"#
        )
    };
    let cases: Vec<(&str, Option<String>)> = vec![
        ("broken JSON", parse("{ not json }")),
        (
            "no mapping",
            parse(r#"{"core":"sg1","version":"1.0","rom":{"sha256":"","size":4}}"#),
        ),
        (
            "a size that is not a number",
            parse(
                r#"{"core":"sg1","version":"1.0","rom":{"sha256":"","size":"big"},"mapping":{"preset":"plain"}}"#,
            ),
        ),
        (
            "a chip with no kind",
            parse(
                r#"{"core":"sg1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"plain"},"chips":[{"id":1}]}"#,
            ),
        ),
        (
            "another version",
            parse(
                r#"{"core":"sg1","version":"2.0","rom":{"sha256":"","size":4},"mapping":{"preset":"plain"}}"#,
            ),
        ),
        (
            "SRAM over the limit",
            parse(
                r#"{"core":"sg1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"plain","sram_size":4194305}}"#,
            ),
        ),
        (
            "an unknown preset",
            plan(
                r#"{"core":"sg1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"weird"}}"#,
            ),
        ),
        ("a bad slot start", plan(&slot(r#"["0xZZ","0x10"]"#, ""))),
        ("a bad slot end", plan(&slot(r#"["0x0","123456789"]"#, ""))),
        (
            "a bad mask",
            plan(&slot(r#"["0x0","0x10"]"#, r#","mask":"0xG""#)),
        ),
        (
            "a bad chip window",
            plan(
                r#"{"core":"sg1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"custom"},"chips":[{"id":0,"kind":"svp","mapping":{"addr_range":["0x300000","x"],"target":{"kind":"OpenBus"}}}]}"#,
            ),
        ),
        ("an unknown chip kind", build(&chip("zzz", "[]"))),
        (
            "a malformed observed range",
            build(&chip("sega", r#"["0xA130F0-0xA130FF","0x2-0x1"]"#)),
        ),
    ];
    for (name, actual) in cases {
        check(
            &format!("error: {name}"),
            actual.as_deref().unwrap_or("(no error)"),
        );
    }
}

/// Hold `actual` to the literal of `name`.
fn check(name: &str, actual: &str) {
    assert_eq!(actual, expected(name), "{name}");
}

/// The literals each case is held to.
fn expected(name: &str) -> &'static str {
    match name {
        "written: every key, compact" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"ab12\",\"size\":1048576,\"title\":\"EVERY KEY\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"custom\",\"sram_size\":8192,\"slots\":[{\"addr_range\":[\"0x000000\",\"0x3FFFFF\"],\"target\":{\"kind\":\"Rom\",\"base_offset\":16},\"mask\":\"0xFFFFF\"},{\"addr_range\":[\"0x200000\",\"0x201FFF\"],\"target\":{\"kind\":\"Sram\",\"base_offset\":0},\"mask\":null},{\"addr_range\":[\"0x400000\",\"0x40FFFF\"],\"target\":{\"kind\":\"OpenBus\"},\"mask\":null}]},\"chips\":[{\"id\":2,\"kind\":\"sega\",\"mapping\":{\"addr_range\":[\"0x300000\",\"0x37FFFF\"],\"target\":{\"kind\":\"ChipMmio\",\"chip_id\":9,\"base\":256},\"mask\":\"0x7FFFF\"},\"observes\":[\"0xA130F0-0xA130FF\"]},{\"id\":3,\"kind\":\"jcart\",\"mapping\":{\"addr_range\":[\"0x38FFFE\",\"0x38FFFF\"],\"target\":{\"kind\":\"Sram\",\"base_offset\":64},\"mask\":null},\"observes\":[]}],\"metadata\":{\"list\":[1,2,3],\"note\":\"kept as it is\"}}",
        "written: every key, pretty" => "{\n  \"$schema\": \"cartridge-manifest-v1\",\n  \"core\": \"sg1\",\n  \"version\": \"1.0\",\n  \"rom\": {\n    \"sha256\": \"ab12\",\n    \"size\": 1048576,\n    \"title\": \"EVERY KEY\",\n    \"region\": \"USA\"\n  },\n  \"mapping\": {\n    \"preset\": \"custom\",\n    \"sram_size\": 8192,\n    \"slots\": [\n      {\n        \"addr_range\": [\n          \"0x000000\",\n          \"0x3FFFFF\"\n        ],\n        \"target\": {\n          \"kind\": \"Rom\",\n          \"base_offset\": 16\n        },\n        \"mask\": \"0xFFFFF\"\n      },\n      {\n        \"addr_range\": [\n          \"0x200000\",\n          \"0x201FFF\"\n        ],\n        \"target\": {\n          \"kind\": \"Sram\",\n          \"base_offset\": 0\n        },\n        \"mask\": null\n      },\n      {\n        \"addr_range\": [\n          \"0x400000\",\n          \"0x40FFFF\"\n        ],\n        \"target\": {\n          \"kind\": \"OpenBus\"\n        },\n        \"mask\": null\n      }\n    ]\n  },\n  \"chips\": [\n    {\n      \"id\": 2,\n      \"kind\": \"sega\",\n      \"mapping\": {\n        \"addr_range\": [\n          \"0x300000\",\n          \"0x37FFFF\"\n        ],\n        \"target\": {\n          \"kind\": \"ChipMmio\",\n          \"chip_id\": 9,\n          \"base\": 256\n        },\n        \"mask\": \"0x7FFFF\"\n      },\n      \"observes\": [\n        \"0xA130F0-0xA130FF\"\n      ]\n    },\n    {\n      \"id\": 3,\n      \"kind\": \"jcart\",\n      \"mapping\": {\n        \"addr_range\": [\n          \"0x38FFFE\",\n          \"0x38FFFF\"\n        ],\n        \"target\": {\n          \"kind\": \"Sram\",\n          \"base_offset\": 64\n        },\n        \"mask\": null\n      },\n      \"observes\": []\n    }\n  ],\n  \"metadata\": {\n    \"list\": [\n      1,\n      2,\n      3\n    ],\n    \"note\": \"kept as it is\"\n  }\n}",
        "written: minimal, an unknown key dropped" => "{\"$schema\":null,\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"\",\"size\":4,\"title\":null,\"region\":null},\"mapping\":{\"preset\":\"plain\",\"sram_size\":0,\"slots\":[]},\"chips\":[],\"metadata\":null}",
        "error: broken JSON" => "invalid JSON: key must be a string at line 1 column 3",
        "error: no mapping" => "invalid JSON: missing field `mapping` at line 1 column 59",
        "error: a size that is not a number" => "invalid JSON: invalid type: string \"big\", expected u64 at line 1 column 61",
        "error: a chip with no kind" => "invalid JSON: missing field `kind` at line 1 column 105",
        "error: another version" => "unsupported manifest version: 2.0",
        "error: SRAM over the limit" => "sram_size 4194305 is larger than the 4194304-byte limit",
        "error: an unknown preset" => "unsupported preset: weird",
        "error: a bad slot start" => "invalid hex in field 'slots[0].addr_range[0]': 0xZZ",
        "error: a bad slot end" => "invalid hex in field 'slots[0].addr_range[1]': 123456789",
        "error: a bad mask" => "invalid hex in field 'slots[0].mask': 0xG",
        "error: a bad chip window" => "invalid hex in field 'chips[0].mapping.addr_range[1]': x",
        "error: an unknown chip kind" => "unknown coprocessor chip kind: zzz",
        "error: a malformed observed range" => "invalid hex in field 'chips[0].observes[1]': 0x2-0x1",
        "regions: plain" => "plain [{\"start\":0,\"end\":4194303,\"label\":\"ROM\",\"detail\":\"mask=0x1FFFFF\"}]",
        "regions: plain with SRAM" => "plain [{\"start\":2097152,\"end\":2113535,\"label\":\"SRAM\",\"detail\":\"\"},{\"start\":0,\"end\":4194303,\"label\":\"ROM\",\"detail\":\"mask=0x1FFFFF\"}]",
        "regions: sega" => "sega [{\"start\":0,\"end\":4194303,\"label\":\"Chip 0\",\"detail\":\"\"}]",
        "regions: ssf2 with SRAM" => "ssf2 [{\"start\":2097152,\"end\":2105343,\"label\":\"SRAM\",\"detail\":\"\"},{\"start\":0,\"end\":4194303,\"label\":\"Chip 0\",\"detail\":\"\"}]",
        "regions: codemasters" => "codemasters [{\"start\":0,\"end\":4194303,\"label\":\"Chip 0\",\"detail\":\"\"}]",
        "regions: eeprom" => "eeprom [{\"start\":2097153,\"end\":2097153,\"label\":\"Chip 0\",\"detail\":\"\"},{\"start\":0,\"end\":4194303,\"label\":\"ROM\",\"detail\":\"mask=0x1FFFFF\"}]",
        "regions: jcart" => "jcart [{\"start\":3735550,\"end\":3735551,\"label\":\"Chip 0\",\"detail\":\"\"},{\"start\":4194302,\"end\":4194303,\"label\":\"Chip 0\",\"detail\":\"base=0x40\"},{\"start\":2097152,\"end\":2105343,\"label\":\"SRAM\",\"detail\":\"\"},{\"start\":0,\"end\":4194303,\"label\":\"ROM\",\"detail\":\"mask=0x1FFFFF\"}]",
        "regions: svp" => "svp [{\"start\":10571776,\"end\":10571787,\"label\":\"Chip 1\",\"detail\":\"base=0x100000\"},{\"start\":3145728,\"end\":3276799,\"label\":\"Chip 1\",\"detail\":\"\"},{\"start\":3735552,\"end\":3801087,\"label\":\"Chip 1\",\"detail\":\"base=0x200000\"},{\"start\":3801088,\"end\":3866623,\"label\":\"Chip 1\",\"detail\":\"base=0x300000\"},{\"start\":0,\"end\":2097151,\"label\":\"ROM\",\"detail\":\"mask=0x1FFFFF\"},{\"start\":3145728,\"end\":3276799,\"label\":\"Chip 1\",\"detail\":\"\"}]",
        "regions: custom" => "custom [{\"start\":3145728,\"end\":3276799,\"label\":\"Chip 1\",\"detail\":\"\"}]",
        "regions: every key" => "custom [{\"start\":0,\"end\":4194303,\"label\":\"ROM\",\"detail\":\"base=0x10, mask=0x0FFFFF\"},{\"start\":2097152,\"end\":2105343,\"label\":\"SRAM\",\"detail\":\"\"},{\"start\":4194304,\"end\":4259839,\"label\":\"OpenBus\",\"detail\":\"\"},{\"start\":3145728,\"end\":3670015,\"label\":\"Chip 2\",\"detail\":\"base=0x100, mask=0x07FFFF\"},{\"start\":3735550,\"end\":3735551,\"label\":\"Chip 3\",\"detail\":\"base=0x40\"}]",
        "detected: plain" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"9de4adc6c5a7fc02b7a3888d8c00263ab9a2c32eb0ee145ac3ce9346274bba1c\",\"size\":524288,\"title\":\"PLAIN GAME\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"plain\",\"sram_size\":0,\"slots\":[]},\"chips\":[],\"metadata\":null}",
        "detected: plain with SRAM" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"a520fc120e0e598ca2350e76a6116de25ce586fcae722bb86ecfab8cde7c21a8\",\"size\":2097152,\"title\":\"RPG WITH SAVE\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"plain\",\"sram_size\":65536,\"slots\":[]},\"chips\":[],\"metadata\":null}",
        "detected: plain with SRAM in RA format" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"316d5a70bc136a33447d347188c749ef331849eed07437c4e0e20d8c87c2a1be\",\"size\":1048576,\"title\":\"RA SAVE\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"plain\",\"sram_size\":16384,\"slots\":[]},\"chips\":[],\"metadata\":null}",
        "detected: SEGA mapper" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"3a65af7fd51b53b00783849f26b159dc95bd86227ec7a201d637bd27f8800ada\",\"size\":5242880,\"title\":\"BIG GAME\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"sega\",\"sram_size\":0,\"slots\":[]},\"chips\":[],\"metadata\":null}",
        "detected: SVP by title" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"0a586bd7d73dac779cafa92c6fda646d3c3c78d9eb48a96acab5dae50e2c90af\",\"size\":2097152,\"title\":\"VIRTUA RACING\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"svp\",\"sram_size\":0,\"slots\":[]},\"chips\":[],\"metadata\":null}",
        "detected: SVP by tag" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"0b9174ba83fb4023ec62fb72215d9cff28f319b6fa96e79749b780c4d238d8b9\",\"size\":2097152,\"title\":\"Virtua Racing\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"svp\",\"sram_size\":0,\"slots\":[]},\"chips\":[],\"metadata\":null}",
        "detected: too small for a header" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"sg1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"5341e6b2646979a70e57653007a1f310169421ec9bdd9f1a5648f75ade005af1\",\"size\":256,\"title\":null,\"region\":null},\"mapping\":{\"preset\":\"plain\",\"sram_size\":0,\"slots\":[]},\"chips\":[],\"metadata\":null}",
        "detected: pretty" => "{\n  \"$schema\": \"cartridge-manifest-v1\",\n  \"core\": \"sg1\",\n  \"version\": \"1.0\",\n  \"rom\": {\n    \"sha256\": \"5d785df6de19a7588bda392693f74f4355a29065a6feeeb2a66379e4e79ece3a\",\n    \"size\": 2097152,\n    \"title\": \"SAVES\",\n    \"region\": \"USA\"\n  },\n  \"mapping\": {\n    \"preset\": \"plain\",\n    \"sram_size\": 65536,\n    \"slots\": []\n  },\n  \"chips\": [],\n  \"metadata\": null\n}",
        _ => panic!("no literal for {name}"),
    }
}
