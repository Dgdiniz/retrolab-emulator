//! The `cartridge-manifest-v1` JSON this core writes and reads, held as
//! literals: the manifest detection gives for each kind of cartridge, the
//! bytes a parsed manifest is written back as, the plan each preset's
//! manifest gives, and the text of each error a manifest can give. A change
//! to the manifest code that moves a byte of any of them fails here.

use snes1_core::{
    detected_manifest, manifest_to_map_plan, parse_manifest, CartridgeBuilder, MapPlan,
    MAP_MODE_EXHIROM,
};

/// A ROM of `size` bytes with a header at `offset`: `map_mode`, the
/// cartridge type, the ROM and SRAM size codes, a reset vector at `$8000`.
fn cart(
    size: usize,
    offset: usize,
    map_mode: u8,
    cartridge_type: u8,
    rom_code: u8,
    sram_code: u8,
) -> Vec<u8> {
    let mut rom = vec![0u8; size];
    rom[offset + 0x10..offset + 0x10 + 21].copy_from_slice(b"MANIFEST TEST ROM    ");
    rom[offset + 0x25] = map_mode;
    rom[offset + 0x26] = cartridge_type;
    rom[offset + 0x27] = rom_code;
    rom[offset + 0x28] = sram_code;
    rom[offset + 0x29] = 0x01;
    rom[offset + 0x2A] = 0x01;
    let checksum: u16 = 0x1234;
    rom[offset + 0x2C..offset + 0x2E].copy_from_slice(&(!checksum).to_le_bytes());
    rom[offset + 0x2E..offset + 0x30].copy_from_slice(&checksum.to_le_bytes());
    rom[offset + 0x4C..offset + 0x4E].copy_from_slice(&0x8000u16.to_le_bytes());
    rom
}

fn lorom() -> Vec<u8> {
    cart(0x10_0000, 0x7FB0, 0x30, 0x02, 0x0A, 0x03)
}

#[test]
fn the_manifest_detection_gives_is_written_as_before() {
    let mut blank_title = cart(0x10_0000, 0x7FB0, 0x20, 0x00, 0x0A, 0x00);
    blank_title[0x7FC0..0x7FC0 + 21].copy_from_slice(&[b' '; 21]);
    let mut copier_headed = vec![0u8; 512];
    copier_headed.extend(lorom());
    let cases: [(&str, Vec<u8>); 14] = [
        ("detected: lorom", lorom()),
        (
            "detected: hirom",
            cart(0x40_0000, 0xFFB0, 0x21, 0x00, 0x0C, 0x00),
        ),
        (
            "detected: exhirom",
            cart(
                0x60_0000,
                0x40_FFB0,
                0x30 | MAP_MODE_EXHIROM,
                0x02,
                0x0D,
                0x01,
            ),
        ),
        (
            "detected: sdd1",
            cart(0x40_0000, 0x7FB0, 0x32, 0x45, 0x0C, 0x03),
        ),
        (
            "detected: spc7110",
            cart(0x40_0000, 0xFFB0, 0x3A, 0xF5, 0x0C, 0x03),
        ),
        (
            "detected: spc7110 with its clock",
            cart(0x40_0000, 0xFFB0, 0x3A, 0xF9, 0x0C, 0x03),
        ),
        (
            "detected: sa1",
            cart(0x40_0000, 0x7FB0, 0x23, 0x35, 0x0C, 0x05),
        ),
        (
            "detected: superfx",
            cart(0x10_0000, 0x7FB0, 0x20, 0x13, 0x0A, 0x00),
        ),
        (
            "detected: cx4",
            cart(0x20_0000, 0x7FB0, 0x20, 0xF3, 0x0B, 0x00),
        ),
        (
            "detected: dsp1 on lorom",
            cart(0x10_0000, 0x7FB0, 0x20, 0x03, 0x0A, 0x00),
        ),
        (
            "detected: dsp1 on hirom",
            cart(0x10_0000, 0xFFB0, 0x31, 0x05, 0x0A, 0x00),
        ),
        ("detected: a blank title", blank_title),
        ("detected: a copier header", copier_headed),
        ("detected: no header", vec![0u8; 0x1000]),
    ];
    for (name, rom) in cases {
        let json = serde_json::to_string(&detected_manifest(&rom)).unwrap();
        check(name, &json);
    }
}

#[test]
fn the_manifest_detection_gives_is_written_pretty_as_before() {
    let json = serde_json::to_string_pretty(&detected_manifest(&lorom())).unwrap();
    check("detected: pretty", &json);
}

/// A manifest that uses every key the format has.
const EVERY_KEY: &str = r#"{
    "$schema": "cartridge-manifest-v1",
    "core": "snes1",
    "version": "1.0",
    "rom": { "sha256": "ab12", "size": 1048576, "title": "EVERY KEY", "region": "USA" },
    "mapping": {
        "preset": "custom",
        "fastrom": true,
        "sram_size": 8192,
        "slots": [
            {
                "bank_range": ["0x80", "0xFF"],
                "offset_range": ["0x8000", "0xFFFF"],
                "target": { "kind": "Rom", "base_offset": 16 },
                "speed": "Fast",
                "mask": "0xFFFFF",
                "bank_stride": "0x10000"
            },
            {
                "bank_range": ["0x70", "0x7D"],
                "offset_range": ["0x0000", "0x7FFF"],
                "target": { "kind": "Sram" },
                "speed": "Slow"
            },
            {
                "bank_range": ["0x00", "0x3F"],
                "offset_range": ["0x6000", "0x6FFF"],
                "target": { "kind": "OpenBus" },
                "speed": "XSlow"
            }
        ]
    },
    "chips": [
        {
            "id": 5,
            "kind": "sdd1",
            "mapping": {
                "bank_range": ["0xC0", "0xFF"],
                "offset_range": ["0x0000", "0xFFFF"],
                "target": { "kind": "ChipMmio", "chip_id": 9, "base": 256 },
                "speed": "Slow"
            },
            "observes": ["0x4300-0x437F"]
        },
        {
            "id": 1,
            "kind": "dsp1",
            "mapping": {
                "bank_range": ["0x30", "0x3F"],
                "offset_range": ["0x8000", "0xFFFF"],
                "target": { "kind": "Rom", "base_offset": 64 },
                "speed": "Slow",
                "mask": "0x3FFF"
            }
        }
    ],
    "controllers": { "port1": "pad", "port2": null },
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
    let minimal = r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"hirom","unknown":5},"weird":[1,2]}"#;
    check(
        "written: minimal, unknown keys dropped",
        &serde_json::to_string(&parse_manifest(minimal).unwrap()).unwrap(),
    );
}

/// A plan as text, slot by slot: its window, target, mask, stride and
/// speed.
fn plan_text(plan: &MapPlan) -> String {
    let slots: Vec<String> = plan
        .slots
        .iter()
        .map(|slot| {
            format!(
                "{:02X}-{:02X}:{:04X}-{:04X} {:?} mask {:?} stride {:?} {:?}",
                slot.bank_range.start(),
                slot.bank_range.end(),
                slot.offset_range.start(),
                slot.offset_range.end(),
                slot.target,
                slot.mask,
                slot.bank_stride,
                slot.speed
            )
        })
        .collect();
    format!("{}: {}", plan.name, slots.join("; "))
}

/// A manifest with `preset`, the FastROM flag and `sram_size`.
fn preset_manifest(preset: &str, fastrom: bool, sram_size: usize) -> String {
    format!(
        r#"{{"core":"snes1","version":"1.0","rom":{{"sha256":"","size":1048576}},"mapping":{{"preset":"{preset}","fastrom":{fastrom},"sram_size":{sram_size}}}}}"#
    )
}

#[test]
fn each_preset_gives_the_plan_it_gave() {
    let rom = vec![0u8; 0x10_0000];
    let cases = [
        ("lorom", preset_manifest("lorom", false, 0)),
        (
            "lorom fast with SRAM",
            preset_manifest("lorom", true, 0x2000),
        ),
        ("hirom", preset_manifest("hirom", false, 0x800)),
        ("exhirom", preset_manifest("exhirom", true, 0)),
        ("sdd1", preset_manifest("sdd1", true, 0x2000)),
        ("spc7110", preset_manifest("spc7110", true, 0x2000)),
        ("sa1", preset_manifest("sa1", false, 0x8000)),
        ("superfx", preset_manifest("superfx", false, 0x8000)),
        ("cx4", preset_manifest("cx4", false, 0)),
        ("custom", preset_manifest("custom", false, 0)),
        ("every key", EVERY_KEY.to_string()),
    ];
    for (name, json) in cases {
        let manifest = parse_manifest(&json).unwrap();
        let plan = manifest_to_map_plan(&manifest, &rom).unwrap();
        check(&format!("plan: {name}"), &plan_text(&plan));
    }
}

#[test]
fn each_error_reads_as_before() {
    let rom = vec![0u8; 0x10_0000];
    let parse = |json: &str| parse_manifest(json).err().map(|e| e.to_string());
    let plan = |json: &str| {
        manifest_to_map_plan(&parse_manifest(json).unwrap(), &rom)
            .err()
            .map(|e| e.to_string())
    };
    let build = |json: &str| {
        CartridgeBuilder::new(&lorom())
            .with_manifest(parse_manifest(json).unwrap())
            .skip_sha256_check()
            .build()
            .err()
            .map(|e| e.to_string())
    };
    let slot = |bank: &str, offset: &str, speed: &str, more: &str| {
        format!(
            r#"{{"core":"snes1","version":"1.0","rom":{{"sha256":"","size":4}},"mapping":{{"preset":"custom","slots":[{{"bank_range":{bank},"offset_range":{offset},"target":{{"kind":"Rom"}},"speed":"{speed}"{more}}}]}}}}"#
        )
    };
    let chip = |kind: &str, observes: &str| {
        format!(
            r#"{{"core":"snes1","version":"1.0","rom":{{"sha256":"","size":4}},"mapping":{{"preset":"lorom"}},"chips":[{{"id":5,"kind":"{kind}","mapping":{{"bank_range":["0xC0","0xFF"],"offset_range":["0x0000","0xFFFF"],"target":{{"kind":"ChipMmio","chip_id":5}},"speed":"Slow"}},"observes":{observes}}}]}}"#
        )
    };
    let banks = r#"["0x00","0x3F"]"#;
    let offsets = r#"["0x8000","0xFFFF"]"#;
    let cases: Vec<(&str, Option<String>)> = vec![
        ("broken JSON", parse("{ not json }")),
        (
            "no mapping",
            parse(r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":4}}"#),
        ),
        (
            "a size that is not a number",
            parse(
                r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":"big"},"mapping":{"preset":"lorom"}}"#,
            ),
        ),
        (
            "an SRAM size that is not a number",
            parse(
                r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"lorom","sram_size":"x"}}"#,
            ),
        ),
        (
            "a FastROM flag that is not a boolean",
            parse(
                r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"lorom","fastrom":"yes"}}"#,
            ),
        ),
        (
            "a controller that is not a name",
            parse(
                r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"lorom"},"controllers":{"port1":5}}"#,
            ),
        ),
        (
            "a chip with no kind",
            parse(
                r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"lorom"},"chips":[{"id":1}]}"#,
            ),
        ),
        (
            "another version",
            parse(
                r#"{"core":"snes1","version":"2.0","rom":{"sha256":"","size":4},"mapping":{"preset":"lorom"}}"#,
            ),
        ),
        (
            "an unknown preset",
            plan(
                r#"{"core":"snes1","version":"1.0","rom":{"sha256":"","size":4},"mapping":{"preset":"weird"}}"#,
            ),
        ),
        (
            "a bad bank",
            plan(&slot(r#"["0x00","0x100"]"#, offsets, "Slow", "")),
        ),
        (
            "a bad offset",
            plan(&slot(banks, r#"["0xG","0xFFFF"]"#, "Slow", "")),
        ),
        ("an unknown speed", plan(&slot(banks, offsets, "Warp", ""))),
        (
            "a bad mask",
            plan(&slot(banks, offsets, "Slow", r#","mask":"zz""#)),
        ),
        (
            "a bad stride",
            plan(&slot(
                banks,
                offsets,
                "Slow",
                r#","bank_stride":"123456789""#,
            )),
        ),
        ("an unknown chip kind", build(&chip("zzz", "[]"))),
        (
            "a malformed observed range",
            build(&chip("sdd1", r#"["0x4300-0x437F","0x2-0x1"]"#)),
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
        "written: every key, compact" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"ab12\",\"size\":1048576,\"title\":\"EVERY KEY\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"custom\",\"fastrom\":true,\"sram_size\":8192,\"slots\":[{\"bank_range\":[\"0x80\",\"0xFF\"],\"offset_range\":[\"0x8000\",\"0xFFFF\"],\"target\":{\"kind\":\"Rom\",\"base_offset\":16},\"speed\":\"Fast\",\"mask\":\"0xFFFFF\",\"bank_stride\":\"0x10000\"},{\"bank_range\":[\"0x70\",\"0x7D\"],\"offset_range\":[\"0x0000\",\"0x7FFF\"],\"target\":{\"kind\":\"Sram\",\"base_offset\":0},\"speed\":\"Slow\",\"mask\":null,\"bank_stride\":null},{\"bank_range\":[\"0x00\",\"0x3F\"],\"offset_range\":[\"0x6000\",\"0x6FFF\"],\"target\":{\"kind\":\"OpenBus\"},\"speed\":\"XSlow\",\"mask\":null,\"bank_stride\":null}]},\"chips\":[{\"id\":5,\"kind\":\"sdd1\",\"mapping\":{\"bank_range\":[\"0xC0\",\"0xFF\"],\"offset_range\":[\"0x0000\",\"0xFFFF\"],\"target\":{\"kind\":\"ChipMmio\",\"chip_id\":9,\"base\":256},\"speed\":\"Slow\",\"mask\":null,\"bank_stride\":null},\"observes\":[\"0x4300-0x437F\"]},{\"id\":1,\"kind\":\"dsp1\",\"mapping\":{\"bank_range\":[\"0x30\",\"0x3F\"],\"offset_range\":[\"0x8000\",\"0xFFFF\"],\"target\":{\"kind\":\"Rom\",\"base_offset\":64},\"speed\":\"Slow\",\"mask\":\"0x3FFF\",\"bank_stride\":null},\"observes\":[]}],\"controllers\":{\"port1\":\"pad\",\"port2\":null},\"metadata\":{\"list\":[1,2,3],\"note\":\"kept as it is\"}}",
        "written: every key, pretty" => "{\n  \"$schema\": \"cartridge-manifest-v1\",\n  \"core\": \"snes1\",\n  \"version\": \"1.0\",\n  \"rom\": {\n    \"sha256\": \"ab12\",\n    \"size\": 1048576,\n    \"title\": \"EVERY KEY\",\n    \"region\": \"USA\"\n  },\n  \"mapping\": {\n    \"preset\": \"custom\",\n    \"fastrom\": true,\n    \"sram_size\": 8192,\n    \"slots\": [\n      {\n        \"bank_range\": [\n          \"0x80\",\n          \"0xFF\"\n        ],\n        \"offset_range\": [\n          \"0x8000\",\n          \"0xFFFF\"\n        ],\n        \"target\": {\n          \"kind\": \"Rom\",\n          \"base_offset\": 16\n        },\n        \"speed\": \"Fast\",\n        \"mask\": \"0xFFFFF\",\n        \"bank_stride\": \"0x10000\"\n      },\n      {\n        \"bank_range\": [\n          \"0x70\",\n          \"0x7D\"\n        ],\n        \"offset_range\": [\n          \"0x0000\",\n          \"0x7FFF\"\n        ],\n        \"target\": {\n          \"kind\": \"Sram\",\n          \"base_offset\": 0\n        },\n        \"speed\": \"Slow\",\n        \"mask\": null,\n        \"bank_stride\": null\n      },\n      {\n        \"bank_range\": [\n          \"0x00\",\n          \"0x3F\"\n        ],\n        \"offset_range\": [\n          \"0x6000\",\n          \"0x6FFF\"\n        ],\n        \"target\": {\n          \"kind\": \"OpenBus\"\n        },\n        \"speed\": \"XSlow\",\n        \"mask\": null,\n        \"bank_stride\": null\n      }\n    ]\n  },\n  \"chips\": [\n    {\n      \"id\": 5,\n      \"kind\": \"sdd1\",\n      \"mapping\": {\n        \"bank_range\": [\n          \"0xC0\",\n          \"0xFF\"\n        ],\n        \"offset_range\": [\n          \"0x0000\",\n          \"0xFFFF\"\n        ],\n        \"target\": {\n          \"kind\": \"ChipMmio\",\n          \"chip_id\": 9,\n          \"base\": 256\n        },\n        \"speed\": \"Slow\",\n        \"mask\": null,\n        \"bank_stride\": null\n      },\n      \"observes\": [\n        \"0x4300-0x437F\"\n      ]\n    },\n    {\n      \"id\": 1,\n      \"kind\": \"dsp1\",\n      \"mapping\": {\n        \"bank_range\": [\n          \"0x30\",\n          \"0x3F\"\n        ],\n        \"offset_range\": [\n          \"0x8000\",\n          \"0xFFFF\"\n        ],\n        \"target\": {\n          \"kind\": \"Rom\",\n          \"base_offset\": 64\n        },\n        \"speed\": \"Slow\",\n        \"mask\": \"0x3FFF\",\n        \"bank_stride\": null\n      },\n      \"observes\": []\n    }\n  ],\n  \"controllers\": {\n    \"port1\": \"pad\",\n    \"port2\": null\n  },\n  \"metadata\": {\n    \"list\": [\n      1,\n      2,\n      3\n    ],\n    \"note\": \"kept as it is\"\n  }\n}",
        "written: minimal, unknown keys dropped" => "{\"$schema\":null,\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"\",\"size\":4,\"title\":null,\"region\":null},\"mapping\":{\"preset\":\"hirom\",\"fastrom\":false,\"sram_size\":0,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "error: broken JSON" => "invalid JSON: key must be a string at line 1 column 3",
        "error: no mapping" => "invalid JSON: missing field `mapping` at line 1 column 61",
        "error: a size that is not a number" => "invalid JSON: invalid type: string \"big\", expected u64 at line 1 column 63",
        "error: an SRAM size that is not a number" => "invalid JSON: invalid type: string \"x\", expected usize at line 1 column 104",
        "error: a FastROM flag that is not a boolean" => "invalid JSON: invalid type: string \"yes\", expected a boolean at line 1 column 105",
        "error: a controller that is not a name" => "invalid JSON: invalid type: integer `5`, expected a string at line 1 column 116",
        "error: a chip with no kind" => "invalid JSON: missing field `kind` at line 1 column 107",
        "error: another version" => "unsupported manifest version: 2.0",
        "error: an unknown preset" => "unsupported preset: weird",
        "error: a bad bank" => "invalid hex in field 'slots[0].bank_range[1]': 0x100",
        "error: a bad offset" => "invalid hex in field 'slots[0].offset_range[0]': 0xG",
        "error: an unknown speed" => "unsupported speed: Warp",
        "error: a bad mask" => "invalid hex in field 'slots[0].mask': zz",
        "error: a bad stride" => "invalid hex in field 'slots[0].bank_stride': 123456789",
        "error: an unknown chip kind" => "unknown coprocessor chip kind: zzz",
        "error: a malformed observed range" => "manifest error: invalid hex in field 'chips[0].observes[1]': 0x2-0x1",
        "plan: lorom" => "lorom: 00-7D:8000-FFFF Rom { base_offset: 0 } mask Some(1048575) stride None Slow; 80-FF:8000-FFFF Rom { base_offset: 0 } mask Some(1048575) stride None Slow",
        "plan: lorom fast with SRAM" => "lorom-fastrom: 00-7D:8000-FFFF Rom { base_offset: 0 } mask Some(1048575) stride None Slow; 80-FF:8000-FFFF Rom { base_offset: 0 } mask Some(1048575) stride None Fast; 70-7D:0000-7FFF Sram { base_offset: 0 } mask Some(8191) stride None Slow",
        "plan: hirom" => "hirom: C0-FF:0000-FFFF Rom { base_offset: 0 } mask Some(1048575) stride None Slow; 00-3F:8000-FFFF Rom { base_offset: 32768 } mask Some(1048575) stride Some(65536) Slow; 80-BF:8000-FFFF Rom { base_offset: 32768 } mask Some(1048575) stride Some(65536) Slow; 20-3F:6000-7FFF Sram { base_offset: 0 } mask Some(2047) stride None Slow; A0-BF:6000-7FFF Sram { base_offset: 0 } mask Some(2047) stride None Slow",
        "plan: exhirom" => "exhirom-fastrom: C0-FF:0000-FFFF Rom { base_offset: 0 } mask None stride None Fast; 40-7D:0000-FFFF Rom { base_offset: 4194304 } mask None stride None Slow; 00-3F:8000-FFFF Rom { base_offset: 4227072 } mask None stride Some(65536) Slow; 80-BF:8000-FFFF Rom { base_offset: 32768 } mask None stride Some(65536) Fast",
        "plan: sdd1" => "sdd1-fastrom: 00-3F:4800-480F ChipMmio { chip_id: 5, base: 67108864 } mask Some(15) stride None Slow; 80-BF:4800-480F ChipMmio { chip_id: 5, base: 67108864 } mask Some(15) stride None Slow; 00-3F:8000-FFFF Rom { base_offset: 0 } mask Some(1048575) stride None Slow; 80-BF:8000-FFFF Rom { base_offset: 0 } mask Some(1048575) stride None Fast; 70-7F:0000-7FFF Sram { base_offset: 0 } mask Some(8191) stride None Slow; A0-BF:6000-7FFF Sram { base_offset: 0 } mask Some(8191) stride None Slow; 60-7F:0000-FFFF Rom { base_offset: 0 } mask None stride None Slow; C0-FF:0000-FFFF ChipMmio { chip_id: 5, base: 0 } mask None stride None Slow",
        "plan: spc7110" => "spc7110-fastrom: 00-3F:4800-4842 ChipMmio { chip_id: 6, base: 805306368 } mask None stride Some(0) Slow; 80-BF:4800-4842 ChipMmio { chip_id: 6, base: 805306368 } mask None stride Some(0) Slow; 00-3F:6000-7FFF ChipMmio { chip_id: 6, base: 536870912 } mask Some(8191) stride None Slow; 80-BF:6000-7FFF ChipMmio { chip_id: 6, base: 536870912 } mask Some(8191) stride None Slow; 00-0F:8000-FFFF Rom { base_offset: 32768 } mask None stride Some(65536) Slow; 80-BF:8000-FFFF Rom { base_offset: 32768 } mask None stride Some(65536) Fast; C0-CF:0000-FFFF Rom { base_offset: 0 } mask None stride None Slow; 50-50:0000-FFFF ChipMmio { chip_id: 6, base: 268435456 } mask Some(0) stride None Slow; 58-58:0000-FFFF ChipMmio { chip_id: 6, base: 268435456 } mask Some(0) stride None Slow; D0-FF:0000-FFFF ChipMmio { chip_id: 6, base: 0 } mask None stride None Slow",
        "plan: sa1" => "sa1: 00-3F:2200-23FF ChipMmio { chip_id: 7, base: 268435456 } mask None stride Some(0) Slow; 80-BF:2200-23FF ChipMmio { chip_id: 7, base: 268435456 } mask None stride Some(0) Slow; 00-3F:3000-37FF ChipMmio { chip_id: 7, base: 536870912 } mask None stride Some(0) Slow; 80-BF:3000-37FF ChipMmio { chip_id: 7, base: 536870912 } mask None stride Some(0) Slow; 00-3F:6000-7FFF ChipMmio { chip_id: 7, base: 805306368 } mask None stride Some(0) Slow; 80-BF:6000-7FFF ChipMmio { chip_id: 7, base: 805306368 } mask None stride Some(0) Slow; 40-4F:0000-FFFF ChipMmio { chip_id: 7, base: 1073741824 } mask None stride None Slow; 00-3F:8000-FFFF ChipMmio { chip_id: 7, base: 1342210048 } mask None stride Some(65536) Slow; 80-BF:8000-FFFF ChipMmio { chip_id: 7, base: 1350598656 } mask None stride Some(65536) Slow; C0-FF:0000-FFFF ChipMmio { chip_id: 7, base: 1354760192 } mask None stride None Slow",
        "plan: superfx" => "superfx: 00-3F:8000-FFFF ChipMmio { chip_id: 8, base: 805306368 } mask Some(1048575) stride None Slow; 40-5F:0000-FFFF ChipMmio { chip_id: 8, base: 805306368 } mask Some(1048575) stride Some(65536) Slow; 80-BF:8000-FFFF ChipMmio { chip_id: 8, base: 805306368 } mask Some(1048575) stride None Slow; C0-DF:0000-FFFF ChipMmio { chip_id: 8, base: 805306368 } mask Some(1048575) stride Some(65536) Slow; 00-3F:3000-34FF ChipMmio { chip_id: 8, base: 268435456 } mask None stride Some(0) Slow; 80-BF:3000-34FF ChipMmio { chip_id: 8, base: 268435456 } mask None stride Some(0) Slow; 00-3F:6000-7FFF ChipMmio { chip_id: 8, base: 536870912 } mask None stride Some(0) Slow; 80-BF:6000-7FFF ChipMmio { chip_id: 8, base: 536870912 } mask None stride Some(0) Slow; 60-7D:0000-FFFF ChipMmio { chip_id: 8, base: 536870912 } mask None stride None Slow; E0-FF:0000-FFFF ChipMmio { chip_id: 8, base: 536870912 } mask None stride None Slow",
        "plan: cx4" => "cx4: 00-3F:8000-FFFF ChipMmio { chip_id: 9, base: 805306368 } mask Some(1048575) stride None Slow; 80-BF:8000-FFFF ChipMmio { chip_id: 9, base: 805306368 } mask Some(1048575) stride None Slow; 00-3F:6000-7FFF ChipMmio { chip_id: 9, base: 268435456 } mask None stride Some(0) Slow; 80-BF:6000-7FFF ChipMmio { chip_id: 9, base: 268435456 } mask None stride Some(0) Slow",
        "plan: custom" => "custom: ",
        "plan: every key" => "custom: 80-FF:8000-FFFF Rom { base_offset: 16 } mask Some(1048575) stride Some(65536) Fast; 70-7D:0000-7FFF Sram { base_offset: 0 } mask None stride None Slow; 00-3F:6000-6FFF OpenBus mask None stride None XSlow; C0-FF:0000-FFFF ChipMmio { chip_id: 5, base: 256 } mask None stride None Slow; 30-3F:8000-FFFF ChipMmio { chip_id: 1, base: 64 } mask Some(16383) stride None Slow",
        "detected: lorom" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"9f258bbeea34478d75c149d3f8d8a148fb92fc113642b35af3cf44aadfa641e5\",\"size\":1048576,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"lorom\",\"fastrom\":true,\"sram_size\":8192,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: hirom" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"26dc92df956dd03061a5b5d986c3a79b1659288fae69315803fcd33edd7b92ad\",\"size\":4194304,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"hirom\",\"fastrom\":false,\"sram_size\":0,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: exhirom" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"59080ef56964fcdd962a9763af4677a3c1782680781a593edd184f2789f4347a\",\"size\":6291456,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"exhirom\",\"fastrom\":true,\"sram_size\":2048,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: sdd1" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"0c32911a494a355689de821b1dba2d2972078fbfcbe22f608f8a9dda2f759ea7\",\"size\":4194304,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"sdd1\",\"fastrom\":true,\"sram_size\":8192,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: spc7110" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"e545321ea21963770fe1b4e906467a3b4ecf91c8c807b4df6c12dcec9120eda5\",\"size\":4194304,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"spc7110\",\"fastrom\":true,\"sram_size\":8192,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: spc7110 with its clock" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"65c5b342b2539e4b0ffd01f9cf16ca971a66bd62468c37ab179455380bfc3cb9\",\"size\":4194304,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"spc7110\",\"fastrom\":true,\"sram_size\":8192,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: sa1" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"da362c80c45f8646a845131f6144a6bc026c0b9f830df72c82fea4767b7c344e\",\"size\":4194304,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"sa1\",\"fastrom\":false,\"sram_size\":32768,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: superfx" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"26b69f66df0123ee6bf7aae01414473695913236c9bf7c1d2a5a44e232724354\",\"size\":1048576,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"superfx\",\"fastrom\":false,\"sram_size\":32768,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: cx4" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"dd9b5779444b4286d17d0e161b124fe95e8fd813787efedbf28d2da6d98963ca\",\"size\":2097152,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"cx4\",\"fastrom\":false,\"sram_size\":0,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: dsp1 on lorom" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"e50698dde5a34210e64537ea9e9c9b17f1a794cff874476fc546485057ef0f2a\",\"size\":1048576,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"lorom\",\"fastrom\":false,\"sram_size\":0,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":{\"coprocessor\":{\"kind\":\"dsp1\",\"layout\":\"LoRom1Mb\"}}}",
        "detected: dsp1 on hirom" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"901f98df0697a79d8ce76bc041918aeee451134a183fc903d4c38eafeee5c8a4\",\"size\":1048576,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"hirom\",\"fastrom\":true,\"sram_size\":0,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":{\"coprocessor\":{\"kind\":\"dsp1\",\"layout\":\"HiRom\"}}}",
        "detected: a blank title" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"655143577b63faba467ab15a6c1a01380e1d5c26b30c7c82edf1a6bd9470f011\",\"size\":1048576,\"title\":null,\"region\":\"USA\"},\"mapping\":{\"preset\":\"lorom\",\"fastrom\":false,\"sram_size\":0,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: a copier header" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"9f258bbeea34478d75c149d3f8d8a148fb92fc113642b35af3cf44aadfa641e5\",\"size\":1048576,\"title\":\"MANIFEST TEST ROM\",\"region\":\"USA\"},\"mapping\":{\"preset\":\"lorom\",\"fastrom\":true,\"sram_size\":8192,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: no header" => "{\"$schema\":\"cartridge-manifest-v1\",\"core\":\"snes1\",\"version\":\"1.0\",\"rom\":{\"sha256\":\"ad7facb2586fc6e966c004d7d1d16b024f5805ff7cb47c7a85dabd8b48892ca7\",\"size\":4096,\"title\":null,\"region\":null},\"mapping\":{\"preset\":\"lorom\",\"fastrom\":false,\"sram_size\":0,\"slots\":[]},\"chips\":[],\"controllers\":null,\"metadata\":null}",
        "detected: pretty" => "{\n  \"$schema\": \"cartridge-manifest-v1\",\n  \"core\": \"snes1\",\n  \"version\": \"1.0\",\n  \"rom\": {\n    \"sha256\": \"9f258bbeea34478d75c149d3f8d8a148fb92fc113642b35af3cf44aadfa641e5\",\n    \"size\": 1048576,\n    \"title\": \"MANIFEST TEST ROM\",\n    \"region\": \"USA\"\n  },\n  \"mapping\": {\n    \"preset\": \"lorom\",\n    \"fastrom\": true,\n    \"sram_size\": 8192,\n    \"slots\": []\n  },\n  \"chips\": [],\n  \"controllers\": null,\n  \"metadata\": null\n}",
        _ => panic!("no literal for {name}"),
    }
}
