//! `images` held to the Mega Drive core's golden images: run on each image
//! set's program (`../core/tests/golden/images/<set>/program.bin`) to each
//! of its frames, it writes the set's PNGs, file for file and byte for byte.

use std::path::Path;

use crate::console::Genesis;

#[test]
fn images_writes_the_core_s_golden_images() {
    let sets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/golden/images");
    let scratch = std::env::temp_dir().join(format!("sg1-cli-images-{}", std::process::id()));
    let mut checked = 0;
    for entry in std::fs::read_dir(&sets).expect("the core's image sets") {
        let set = entry.expect("an image set").path();
        let name = set
            .file_name()
            .expect("a name")
            .to_string_lossy()
            .into_owned();
        retro_cli::cmd::images::check_against_image_set::<Genesis>(&set, &scratch.join(&name))
            .unwrap_or_else(|e| panic!("{e:#}"));
        checked += 1;
    }
    std::fs::remove_dir_all(&scratch).expect("the scratch folder");
    assert!(checked > 0, "no image set in {}", sets.display());
}
