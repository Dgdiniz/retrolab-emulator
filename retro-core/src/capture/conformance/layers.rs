//! The checks of the layers split by priority and of the order they are put
//! in: [`CaptureCore::render_layer_priority`] and
//! [`CaptureCore::draw_order`].

use std::collections::{BTreeMap, BTreeSet};

use super::{CoreUnderTest, EVERY_RECORDER, MOST_LAYERS};
use crate::capture::{layer, planes, CaptureCore, DrawOrder, DrawSlot};
use crate::video::PixelImage;

/// The draw order of the held frame covers every line the frame drew, once
/// and in order, from its first: runs one after another, none empty. Each
/// place names a layer of the core's list (one [`CaptureCore::render_layer`]
/// answers for), and no run names one layer's priority twice.
pub fn the_draw_order_covers_the_frame_s_lines_once<E: CaptureCore>(core: &CoreUnderTest<E>) {
    let mut emu = core.warmed_up(EVERY_RECORDER);
    core.hold_a_frame_with_sprites(&mut emu);
    let frame = emu.capture_frame_snapshot().frame;
    let orders = emu.draw_order();
    assert!(
        !orders.is_empty(),
        "frame {frame}: the core gives no draw order"
    );
    let table = emu.line_states().expect("the line table is kept");
    let drawn = table.values.len() / table.columns.len().max(1);
    let mut next = 0;
    for run in &orders {
        assert!(run.lines > 0, "frame {frame}: a run of no line");
        assert_eq!(
            run.first_line, next,
            "frame {frame}: a run starts on line {} where line {next} is next",
            run.first_line
        );
        next = run.first_line + run.lines;
        let mut named = BTreeSet::new();
        for slot in &run.slots {
            assert!(
                named.insert(*slot),
                "frame {frame}: the run from line {} names {slot:?} twice",
                run.first_line
            );
        }
    }
    let layers: BTreeSet<u32> = orders
        .iter()
        .flat_map(|run| run.slots.iter().map(|slot| slot.layer))
        .collect();
    for layer in layers {
        assert!(
            emu.render_layer(layer).is_some(),
            "frame {frame}: the draw order names layer {layer}, which the core does not draw"
        );
    }
    assert_eq!(
        next as usize, drawn,
        "frame {frame}: the draw order covers {next} lines of the {drawn} the frame drew"
    );
}

/// Each layer's images of the priorities the draw order names for it add
/// up to the layer: every pixel [`CaptureCore::render_layer`] draws is in
/// exactly one of them, in the same colour, and none of them draws a pixel
/// the layer does not. A priority the order does not name for the layer
/// draws nothing, and a layer the list lacks has no priority.
pub fn a_layer_s_priorities_add_up_to_the_layer<E: CaptureCore>(core: &CoreUnderTest<E>) {
    let mut emu = core.warmed_up(EVERY_RECORDER);
    core.hold_a_frame_with_sprites(&mut emu);
    let frame = emu.capture_frame_snapshot().frame;
    let named = priorities_named(&emu.draw_order());
    let mut layers_split = 0;
    for layer in 0..MOST_LAYERS {
        let Some(whole) = emu.render_layer(layer) else {
            assert!(
                emu.render_layer_priority(layer, 0).is_none(),
                "frame {frame}: layer {layer} is no layer, but has a priority"
            );
            continue;
        };
        let priorities = named.get(&layer).cloned().unwrap_or_default();
        let parts: Vec<(u8, PixelImage)> = priorities
            .iter()
            .map(|&priority| {
                let part = emu
                    .render_layer_priority(layer, priority)
                    .unwrap_or_else(|| {
                        panic!("frame {frame}: layer {layer} has no image of priority {priority}")
                    });
                (priority, part.image)
            })
            .collect();
        let whole = whole.image;
        for (priority, part) in &parts {
            assert_eq!(
                (part.width, part.height),
                (whole.width, whole.height),
                "frame {frame}: layer {layer}'s image of priority {priority} is not the layer's \
                 size"
            );
        }
        for (pixel, colour) in whole.rgba.as_chunks::<4>().0.iter().enumerate() {
            let holding: Vec<u8> = parts
                .iter()
                .filter(|(_, part)| part.rgba[pixel * 4 + 3] != 0)
                .map(|(priority, _)| *priority)
                .collect();
            if colour[3] == 0 {
                assert!(
                    holding.is_empty(),
                    "frame {frame}: layer {layer}'s images of priorities {holding:?} draw pixel \
                     {pixel}, which the layer does not"
                );
                continue;
            }
            assert_eq!(
                holding.len(),
                1,
                "frame {frame}: layer {layer}'s pixel {pixel} is in the images of priorities \
                 {holding:?}, not in one"
            );
            let (_, part) = parts
                .iter()
                .find(|(priority, _)| *priority == holding[0])
                .expect("found above");
            assert_eq!(
                &part.rgba[pixel * 4..pixel * 4 + 4],
                &colour[..],
                "frame {frame}: layer {layer}'s pixel {pixel} is another colour in its image of \
                 priority {}",
                holding[0]
            );
        }
        let unnamed = priorities.iter().max().map_or(0, |most| most + 1);
        let none = emu
            .render_layer_priority(layer, unnamed)
            .expect("the layer answers for any priority");
        assert!(
            none.image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 0),
            "frame {frame}: layer {layer} draws pixels of priority {unnamed}, which the draw \
             order does not name"
        );
        layers_split += usize::from(!parts.is_empty());
    }
    assert!(
        layers_split >= 2,
        "frame {frame}: the draw order names the priorities of {layers_split} layers; the program \
         draws a background and sprites"
    );
}

/// The layers the picture shows, split by priority and stacked as the draw
/// order puts them, are the picture: wherever a place of a shown layer has
/// a pixel, the picture shows the first such place's colour, front to back,
/// and its pixel sources name that place's layer; elsewhere they name no
/// layer. A layer is shown when the pixel sources name it for a pixel of
/// the picture. The suite's programs draw with nothing but the layers'
/// priorities, and whether a layer is shown at all, choosing what shows:
/// no window and no colour effect over a layer shown.
pub fn the_layers_stacked_in_the_draw_order_are_the_picture<E: CaptureCore>(
    core: &CoreUnderTest<E>,
) {
    let mut emu = core.warmed_up(EVERY_RECORDER);
    core.hold_a_frame_with_sprites(&mut emu);
    let snapshot = emu.capture_frame_snapshot();
    let frame = snapshot.frame;
    let grid = snapshot.grid;
    let width = snapshot.width as usize;
    let rows_per_line = u32::from(grid.rows_per_line);
    let sources = emu
        .pixel_sources(planes::LAYER)
        .expect("a program is loaded");
    assert!(
        sources.matches_picture,
        "frame {frame}: the pixel sources do not describe the picture"
    );
    let drew = sources.layer.expect("the layer plane was asked for");
    let layer_of = |code: u16| -> Option<u32> {
        let what = code & layer::WHAT_DREW;
        (what != layer::BACKDROP && what != layer::UNLAYERED_COLOUR && what != layer::NOTHING)
            .then(|| u32::from(what) - 1)
    };
    let shown: BTreeSet<u32> = drew.iter().filter_map(|&code| layer_of(code)).collect();
    let orders = emu.draw_order();
    let mut images: BTreeMap<DrawSlot, PixelImage> = BTreeMap::new();
    for slot in orders.iter().flat_map(|run| &run.slots) {
        if shown.contains(&slot.layer) && !images.contains_key(slot) {
            let image = emu
                .render_layer_priority(slot.layer, slot.priority)
                .unwrap_or_else(|| panic!("frame {frame}: {slot:?} has no image"))
                .image;
            images.insert(*slot, image);
        }
    }
    let mut compared = 0;
    for run in &orders {
        for line in run.first_line..run.first_line + run.lines {
            let Some(shown_line) = line.checked_sub(grid.first_line) else {
                continue;
            };
            for row in shown_line * rows_per_line..(shown_line + 1) * rows_per_line {
                if row >= snapshot.height {
                    continue;
                }
                for column in 0..width {
                    let pixel = row as usize * width + column;
                    let at = pixel * 4;
                    let front = run.slots.iter().find(|slot| {
                        images
                            .get(*slot)
                            .is_some_and(|image| image.rgba[at + 3] != 0)
                    });
                    let source = layer_of(drew[pixel]);
                    let Some(slot) = front else {
                        assert_eq!(
                            source, None,
                            "frame {frame}, line {line}, row {row}, column {column}: no place of \
                             the draw order has a pixel, but the pixel sources name a layer"
                        );
                        continue;
                    };
                    assert_eq!(
                        (source, &images[slot].rgba[at..at + 3]),
                        (Some(slot.layer), &snapshot.framebuffer[at..at + 3]),
                        "frame {frame}, line {line}, row {row}, column {column}: the picture is \
                         not the pixel of {slot:?}, the first place of the draw order with one"
                    );
                    compared += 1;
                }
            }
        }
    }
    assert!(
        compared > 0,
        "frame {frame}: no place of the draw order has a pixel on the picture"
    );
}

/// The priorities the draw order names for each layer, in any of its runs.
fn priorities_named(orders: &[DrawOrder]) -> BTreeMap<u32, BTreeSet<u8>> {
    let mut named: BTreeMap<u32, BTreeSet<u8>> = BTreeMap::new();
    for slot in orders.iter().flat_map(|run| &run.slots) {
        named.entry(slot.layer).or_default().insert(slot.priority);
    }
    named
}
