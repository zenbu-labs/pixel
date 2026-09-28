use super::harness::*;
use super::transmit_strategy::{MAX_PATCHES, POOL};
use super::tiles::{FIRST_TILE_ID, TILE_PX};
use super::{Identity, Presenter};
use super::super::FrameTransport;
use crate::canvas::Canvas;
use crate::surfaces::{OpaqueArea, Rect};
use crate::wrapper::Wrapper;

const KITTY: Identity = Identity::Kitty { version: (0, 48, 0) };

#[test]
fn caret_blink_reuses_one_patch() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::File, GHOSTTY);
    let mut canvas = base_canvas(W, H, 200);
    h.frame(&canvas, None);
    let caret = Rect { x: 123, y: 77, w: 2, h: 18 };
    for i in 0..40 {
        let color = if i % 2 == 0 { [0, 0, 0, 255] } else { [200, 100, 40, 255] };
        fill(&mut canvas, caret, color);
        h.frame(&canvas, Some(&[caret]));
        assert_eq!(h.sim.image_count(), 2, "the caret is one patch over the base, replaced in place");
        assert_eq!(h.sim.pixels_last_frame, 2 * 18, "exactly the caret's pixels went out");
    }
    assert_eq!(h.sim.max_commands_per_frame, 2, "one placement delete plus one transmit");
}

fn moving_boxes(present: Presenter, transport: FrameTransport, identity: Identity, seed: u32, feed_every: usize) -> Harness {
    let mut h = Harness::new(present, transport, identity);
    h.feed_every = feed_every;
    let mut rng = Lcg(seed);
    let shade = 90u8;
    let base = [shade, shade / 2, 40, 255];
    let mut canvas = base_canvas(W, H, shade);
    h.frame(&canvas, None);
    for _ in 0..160 {
        let count = 1 + rng.range(6);
        let mut damage = Vec::new();
        for _ in 0..count {
            let w = 1 + rng.range(W / 3);
            let hgt = 1 + rng.range(H / 3);
            let rect = Rect { x: rng.range(W - w), y: rng.range(H - hgt), w, h: hgt };
            let color = if rng.range(3) == 0 { base } else { [rng.next() as u8, rng.next() as u8, rng.next() as u8, 255] };
            fill(&mut canvas, rect, color);
            damage.push(rect);
        }
        h.frame(&canvas, Some(&damage));
    }
    assert!(
        h.sim.max_commands_per_frame <= 2 * MAX_PATCHES + POOL as usize + 2,
        "commands per frame stayed bounded"
    );
    h
}

#[test]
fn random_damage_stays_exact_over_every_transport_terminal_and_reader_lag() {
    let cases = [
        (Presenter::Patched, FrameTransport::File, GHOSTTY, 0x1234u32, 1usize),
        (Presenter::Patched, FrameTransport::Shared, KITTY, 0xbeef, 1),
        (Presenter::Patched, FrameTransport::Shared, GHOSTTY, 0x5151, 40),
        (Presenter::Patched, FrameTransport::File, KITTY, 0x5152, 40),
        (Presenter::Patched, FrameTransport::Shared, GHOSTTY, 0x22, 1),
        (Presenter::Patched, FrameTransport::Shared, GHOSTTY, 0x33, 1),
        (Presenter::Full, FrameTransport::File, GHOSTTY, 0x5555, 1),
    ];
    for (present, transport, identity, seed, lag) in cases {
        moving_boxes(present, transport, identity, seed, lag);
    }
}

#[test]
fn patches_stay_exact_inline_and_never_flush_a_whole_frame_for_pool_pressure() {
    let h = moving_boxes(Presenter::Patched, FrameTransport::Inline, Identity::Kitty { version: (0, 46, 0) }, 0x1a1e, 1);
    assert!(h.sim.transmits > 100, "changes went out as inline images");
}

#[test]
fn frame_edits_keep_one_image_exact_over_the_wire() {
    let h = moving_boxes(Presenter::Animation, FrameTransport::Inline, GHOSTTY, 0x7a11, 1);
    assert_eq!(h.sim.image_count(), 1, "the base image is the only image");
    assert_eq!(h.sim.placement_count(), 1, "and it has one placement");
    assert!(h.sim.frame_edits > 100, "changes went out as edits, saw {}", h.sim.frame_edits);
}

/// A browser-like layout: a see-through chrome strip on top, a webview surface below it with
/// rounded corners, declared as three opaque bands the way `paint::opaque_areas` does.
fn chrome_over_webview(h: &mut Harness, surface: u32) -> (Rect, Rect) {
    let strip = Rect { x: 0, y: 0, w: W, h: 40 };
    let webview = Rect { x: 5, y: 40, w: W - 10, h: H - 40 };
    let r = 10;
    h.opaque = vec![
        OpaqueArea { surface: Some(surface), rect: Rect { x: webview.x + r, y: webview.y, w: webview.w - 2 * r, h: r } },
        OpaqueArea { surface: Some(surface), rect: Rect { x: webview.x, y: webview.y + r, w: webview.w, h: webview.h - 2 * r } },
        OpaqueArea { surface: Some(surface), rect: Rect { x: webview.x + r, y: webview.y + webview.h - r, w: webview.w - 2 * r, h: r } },
    ];
    (strip, webview)
}

fn paint_tabs(canvas: &mut Canvas, strip: Rect, rng: &mut Lcg, translucent: bool) {
    fill(canvas, strip, [0, 0, 0, 0]);
    for tab in 0..(1 + rng.range(5)) {
        let x = (tab * 100 + rng.range(40)).min(W - 60);
        let alpha = if translucent { 128 + (rng.range(2) as u8) * 127 } else { 255 };
        fill(canvas, Rect { x, y: 8, w: 60, h: 20 }, [rng.next() as u8, 220, rng.next() as u8, alpha]);
    }
}

#[test]
fn translucent_chrome_repaints_never_resend_the_webview_and_never_stack() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
    let mut rng = Lcg(0x99);
    let mut canvas = base_canvas(W, H, 120);
    let (strip, webview) = chrome_over_webview(&mut h, 1);
    paint_tabs(&mut canvas, strip, &mut rng, true);
    h.frame(&canvas, None);
    let opaque: Vec<Rect> = h.opaque.iter().map(|a| a.rect).collect();
    assert!(h.term.patches.tiles.iter().all(|t| opaque.iter().all(|o| !o.intersects(t.rect))), "no tile overlaps the webview");
    let corner = Rect { x: webview.x, y: webview.y, w: 10, h: 10 };
    assert!(
        h.term.patches.tiles.iter().any(|t| t.rect.contains(corner) && t.rect.area() <= 2 * corner.area()),
        "the corner square is a small tile of its own"
    );
    let whole = Rect::sized(W, H);
    for round in 0..120 {
        // a chrome repaint: the engine reports the whole view plus any surface rects
        paint_tabs(&mut canvas, strip, &mut rng, true);
        if round % 3 == 0 {
            let rect = Rect { x: rng.range(W - 50), y: webview.y + rng.range(webview.h - 50), w: 50, h: 50 };
            fill(&mut canvas, rect, [rng.next() as u8, rng.next() as u8, rng.next() as u8, 255]);
            h.frame_repaint(&canvas, &[whole], &[rect]);
        } else {
            h.frame_repaint(&canvas, &[whole], &[]);
            assert!(
                h.sim.pixels_last_frame <= u64::from(W) * 40 * 2,
                "a chrome-only repaint must not resend the surface: {} pixels",
                h.sim.pixels_last_frame
            );
        }
        // a page change: only the surface's own damage
        let rect = Rect { x: rng.range(W - 80), y: webview.y + rng.range(webview.h - 80), w: 80, h: 80 };
        fill(&mut canvas, rect, [rng.next() as u8, rng.next() as u8, rng.next() as u8, 255]);
        h.frame(&canvas, Some(&[rect]));
    }
}

#[test]
fn a_translucent_screen_resends_only_the_grid_cell_around_a_change() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
    h.opaque = Vec::new();
    let mut canvas = base_canvas(W, H, 120);
    fill(&mut canvas, Rect::sized(W, H), [40, 40, 40, 128]);
    h.frame(&canvas, None);
    let cells = (W.div_ceil(TILE_PX) * H.div_ceil(TILE_PX)) as usize;
    assert_eq!(h.term.patches.tiles.len(), cells, "nothing is opaque, so the grid covers the screen");
    assert_eq!(h.image_ids_from(FIRST_TILE_ID), cells);
    let whole = u64::from(W) * u64::from(H);
    let cell = u64::from(TILE_PX) * u64::from(TILE_PX);
    let spot = Rect { x: 200, y: 300, w: 20, h: 20 };
    let mut sent = Vec::new();
    for i in 0..6u8 {
        fill(&mut canvas, spot, [i * 40, 200, 100, 128]);
        h.frame(&canvas, Some(&[spot]));
        sent.push(h.sim.pixels_last_frame);
    }
    assert!(sent.iter().all(|&px| px <= cell), "each change re-sends one cell, not the screen: {sent:?}");
    fill(&mut canvas, Rect::sized(W, H), [90, 30, 30, 128]);
    h.frame(&canvas, Some(&[Rect::sized(W, H)]));
    assert_eq!(h.sim.pixels_last_frame, whole, "a change over everything re-sends every cell");
    assert_eq!(h.term.patches.tiles.len(), cells, "the grid never changes shape");
    assert_eq!(h.image_ids_from(FIRST_TILE_ID), cells, "no image beyond the grid's is left behind");
}

#[test]
fn a_premultiplied_canvas_is_straightened_before_the_terminal_sees_it() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
    h.premultiplied = true;
    let mut rng = Lcg(0x5a);
    let mut canvas = base_canvas(W, H, 120);
    let (strip, webview) = chrome_over_webview(&mut h, 1);
    fill(&mut canvas, strip, [0, 0, 0, 0]);
    h.frame(&canvas, None);
    for _ in 0..30 {
        fill(&mut canvas, strip, [0, 0, 0, 0]);
        for tab in 0..3 {
            let color = premultiply([rng.next() as u8, 220, rng.next() as u8, 128]);
            fill(&mut canvas, Rect { x: tab * 120 + 10, y: 8, w: 60, h: 20 }, color);
        }
        let rect = Rect { x: rng.range(W - 60), y: webview.y + rng.range(webview.h - 60), w: 60, h: 60 };
        fill(&mut canvas, rect, [rng.next() as u8, rng.next() as u8, rng.next() as u8, 255]);
        h.frame_repaint(&canvas, &[Rect::sized(W, H)], &[rect]);
    }
}

#[test]
fn switching_the_surface_in_an_opaque_area_resends_it() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
    let mut canvas = base_canvas(W, H, 120);
    let webview = Rect { x: 0, y: 40, w: W, h: H - 40 };
    h.opaque = vec![OpaqueArea { surface: Some(1), rect: webview }];
    h.frame(&canvas, None);
    // page content changes through the surface's own damage
    fill(&mut canvas, webview, [10, 200, 30, 255]);
    h.frame(&canvas, Some(&[webview]));
    // a tab switch: another surface now fills the same area; the tree reports its rect as
    // changed, since no browser damage will
    h.opaque = vec![OpaqueArea { surface: Some(2), rect: webview }];
    fill(&mut canvas, webview, [200, 20, 90, 255]);
    h.frame_repaint(&canvas, &[Rect::sized(W, H)], &[webview]);
    // and the surface stays put afterwards, so a chrome-only repaint sends little
    fill(&mut canvas, Rect { x: 0, y: 0, w: W, h: 40 }, [0, 0, 0, 0]);
    h.frame_repaint(&canvas, &[Rect::sized(W, H)], &[]);
    assert!(h.sim.pixels_last_frame <= u64::from(W) * 40 * 2, "{} pixels", h.sim.pixels_last_frame);
}

#[test]
fn opaque_areas_moving_at_runtime_lay_the_tiles_out_again_and_drop_the_old_ones() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
    let mut rng = Lcg(0x42);
    let mut canvas = base_canvas(W, H, 120);
    let (strip, webview) = chrome_over_webview(&mut h, 1);
    paint_tabs(&mut canvas, strip, &mut rng, true);
    h.frame(&canvas, None);
    let rect = Rect { x: 30, y: webview.y + 30, w: 50, h: 50 };
    fill(&mut canvas, rect, [1, 2, 3, 255]);
    h.frame(&canvas, Some(&[rect]));
    let tiles_before = h.term.patches.tiles.iter().filter(|t| t.sent).count();
    assert_eq!(h.image_ids_from(FIRST_TILE_ID), tiles_before);
    // the toolbar grows: the webview moves down and shrinks, the tiles must follow
    let taller = Rect { x: 0, y: 0, w: W, h: 80 };
    let smaller = Rect { x: 40, y: 80, w: W - 80, h: H - 80 };
    let r = 10;
    h.opaque = vec![
        OpaqueArea { surface: Some(1), rect: Rect { x: smaller.x + r, y: smaller.y, w: smaller.w - 2 * r, h: r } },
        OpaqueArea { surface: Some(1), rect: Rect { x: smaller.x, y: smaller.y + r, w: smaller.w, h: smaller.h - 2 * r } },
        OpaqueArea { surface: Some(1), rect: Rect { x: smaller.x + r, y: smaller.y + smaller.h - r, w: smaller.w - 2 * r, h: r } },
    ];
    fill(&mut canvas, Rect::sized(W, H), [0, 0, 0, 0]);
    paint_tabs(&mut canvas, taller, &mut rng, true);
    fill(&mut canvas, smaller, [60, 60, 200, 255]);
    h.frame(&canvas, None);
    let tiles_after = h.term.patches.tiles.iter().filter(|t| t.sent).count();
    assert_eq!(h.image_ids_from(FIRST_TILE_ID), tiles_after, "old tile images are gone, only the new layout's remain");
    for _ in 0..20 {
        paint_tabs(&mut canvas, taller, &mut rng, true);
        let rect = Rect { x: smaller.x + rng.range(smaller.w - 40), y: smaller.y + rng.range(smaller.h - 40), w: 40, h: 40 };
        fill(&mut canvas, rect, [rng.next() as u8, rng.next() as u8, rng.next() as u8, 255]);
        h.frame_repaint(&canvas, &[Rect::sized(W, H)], &[rect]);
    }
}

#[test]
fn chrome_repaints_without_damage_stay_exact_between_patches() {
    for seed in [0x31u32, 0x32, 0x33] {
        let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
        let mut rng = Lcg(seed);
        let shade = 120u8;
        let base = [shade, shade / 2, 40, 255];
        let mut canvas = base_canvas(W, H, shade);
        h.frame(&canvas, None);
        for _ in 0..200 {
            if rng.range(3) == 0 {
                // a tab strip re-render: the whole top band changes and no damage is reported
                let strip = Rect { x: 0, y: 0, w: W, h: 40 };
                fill(&mut canvas, strip, [30, 30, 30, 255]);
                for tab in 0..(1 + rng.range(6)) {
                    let x = tab * 90 + rng.range(20);
                    fill(&mut canvas, Rect { x: x.min(W - 60), y: 8, w: 60, h: 20 }, [rng.next() as u8, 200, rng.next() as u8, 255]);
                }
                h.frame(&canvas, None);
                continue;
            }
            let count = 1 + rng.range(4);
            let mut damage = Vec::new();
            for _ in 0..count {
                let w = 1 + rng.range(W / 4);
                let hgt = 1 + rng.range(H / 4);
                let rect = Rect { x: rng.range(W - w), y: rng.range(H - hgt), w, h: hgt };
                let color = if rng.range(3) == 0 { base } else { [rng.next() as u8, rng.next() as u8, rng.next() as u8, 255] };
                fill(&mut canvas, rect, color);
                damage.push(rect);
            }
            h.frame(&canvas, Some(&damage));
        }
    }
}

#[test]
fn growing_and_shrinking_with_patches_and_tiles_leaves_no_stale_images() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
    let mut rng = Lcg(0x7e51);
    let sizes = [(W, H), (W + 300, H + 120), (W, H), (W - 200, H - 80), (W + 300, H + 120)];
    for (round, &(w, hgt)) in sizes.iter().enumerate() {
        let mut canvas = base_canvas(w, hgt, 90);
        let strip = Rect { x: 0, y: 0, w, h: 40 };
        let webview = Rect { x: 5, y: 40, w: w - 10, h: hgt - 40 };
        let r = 10;
        h.opaque = vec![
            OpaqueArea { surface: Some(1), rect: Rect { x: webview.x + r, y: webview.y, w: webview.w - 2 * r, h: r } },
            OpaqueArea { surface: Some(1), rect: Rect { x: webview.x, y: webview.y + r, w: webview.w, h: webview.h - 2 * r } },
            OpaqueArea { surface: Some(1), rect: Rect { x: webview.x + r, y: webview.y + webview.h - r, w: webview.w - 2 * r, h: r } },
        ];
        fill(&mut canvas, strip, [0, 0, 0, 0]);
        h.frame(&canvas, None);
        for _ in 0..40 {
            // page changes pile up patches; a small chrome change re-sends a top-row tile
            let rect = Rect { x: rng.range(webview.w - 60), y: webview.y + rng.range(webview.h - 60), w: 60, h: 60 };
            fill(&mut canvas, rect, [rng.next() as u8, rng.next() as u8, rng.next() as u8, 255]);
            h.frame(&canvas, Some(&[rect]));
            let spot = Rect { x: 10 + rng.range(w - 40), y: 10, w: 20, h: 20 };
            fill(&mut canvas, spot, [200, 200, 200, 128]);
            h.frame_repaint(&canvas, &[strip], &[]);
        }
        let expected = 1 + h.term.patches.live.len() + h.term.patches.tiles.iter().filter(|t| t.sent).count();
        assert_eq!(h.sim.image_count(), expected, "round {round} at {w}x{hgt}: every image the terminal holds is one we track");
    }
}

#[test]
fn large_damage_is_one_patch_and_a_resize_starts_a_fresh_base() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::File, GHOSTTY);
    let mut canvas = base_canvas(W, H, 30);
    h.frame(&canvas, None);
    let small = Rect { x: 40, y: 40, w: 30, h: 30 };
    fill(&mut canvas, small, [1, 2, 3, 255]);
    h.frame(&canvas, Some(&[small]));
    assert_eq!(h.sim.image_count(), 2);

    let most = Rect { x: 0, y: 0, w: W, h: H - CELL.1 };
    fill(&mut canvas, most, [9, 9, 9, 255]);
    h.frame(&canvas, Some(&[most]));
    assert_eq!(h.sim.image_count(), 2, "one patch covers the change and swallows the small one");
    assert_eq!(h.sim.pixels_last_frame, most.area(), "exactly the changed pixels went out");

    fill(&mut canvas, small, [7, 7, 7, 255]);
    h.frame(&canvas, Some(&[small]));
    let mut smaller = base_canvas(W - 100, H - 40, 120);
    h.frame(&smaller, None);
    assert_eq!(h.sim.image_count(), 1, "a resize starts a fresh base");
    fill(&mut smaller, small, [3, 3, 3, 255]);
    h.frame(&smaller, Some(&[small]));
    assert_eq!(h.sim.image_count(), 2);
}

#[test]
fn a_wandering_change_fills_the_pool_and_is_folded_instead_of_flattened() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
    let mut canvas = base_canvas(W, H, 60);
    h.frame(&canvas, None);
    let spots = 12 * 12;
    assert!(spots > POOL as usize, "the change visits more places than there are patch ids");
    let frame_px = u64::from(W) * u64::from(H);
    let mut compactions = 0;
    for i in 0..(3 * POOL) {
        let rect = Rect { x: (i % 12) * 50, y: ((i / 12) % 12) * 45, w: 10, h: 20 };
        fill(&mut canvas, rect, [i as u8, 50, 50, 255]);
        h.frame(&canvas, Some(&[rect]));
        assert!(h.sim.pixels_last_frame <= frame_px / 2 + rect.area(), "frame {i}: a compaction stays within half a frame");
        if h.sim.pixels_last_frame > rect.area() {
            compactions += 1;
        }
    }
    assert!((1..=4).contains(&compactions), "the pool filled a few times and was folded each time: {compactions}");
    assert!(h.sim.image_count() <= POOL as usize + 1, "images stay within the pool");
}

#[test]
fn a_quiet_pane_folds_nearby_patches_into_a_few_images() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
    let mut canvas = base_canvas(W, H, 60);
    h.frame(&canvas, None);
    // forty small changes inside a 200x200 corner of the page, each its own patch
    for i in 0..40u32 {
        let rect = Rect { x: 20 + (i % 8) * 24, y: 60 + (i / 8) * 36, w: 12, h: 12 };
        fill(&mut canvas, rect, [i as u8 * 6, 90, 200, 255]);
        h.frame(&canvas, Some(&[rect]));
    }
    assert!(h.sim.image_count() > 30, "each change is its own image while the page is busy");
    h.idle(&canvas);
    assert!(h.sim.image_count() <= 1 + 12, "idle folded them into a dozen or fewer: {}", h.sim.image_count());
}

#[test]
fn ghostty_gets_a_placement_delete_before_every_transmit() {
    let mut h = Harness::new(Presenter::Full, FrameTransport::File, GHOSTTY);
    let canvas = base_canvas(W, H, 10);
    for _ in 0..5 {
        h.frame(&canvas, None);
    }
    assert_eq!(h.sim.placement_deletes, 5);
    assert_eq!(h.sim.transient_transmits, 0);

    let mut k = Harness::new(Presenter::Full, FrameTransport::File, Identity::Kitty { version: (0, 48, 1) });
    for _ in 0..3 {
        k.frame(&canvas, None);
    }
    assert_eq!(k.sim.placement_deletes, 0);
    assert_eq!(k.sim.transient_transmits, 3, "kitty 0.48+ keeps frames out of its disk cache");

    let mut old = Harness::new(Presenter::Full, FrameTransport::File, Identity::Kitty { version: (0, 47, 0) });
    old.frame(&canvas, None);
    assert_eq!(old.sim.transient_transmits, 0, "older kitties reject the key");
}

fn random_boxes_through_tmux(present: Presenter, seed: u32) -> Harness {
    let mut h = Harness::behind(Wrapper::Tmux, present, FrameTransport::Shared, Identity::Unknown);
    let mut rng = Lcg(seed);
    let mut canvas = base_canvas(W, H, 70);
    h.frame(&canvas, None);
    for _ in 0..60 {
        let mut damage = Vec::new();
        for _ in 0..1 + rng.range(4) {
            let w = 1 + rng.range(W / 3);
            let hgt = 1 + rng.range(H / 3);
            let rect = Rect { x: rng.range(W - w), y: rng.range(H - hgt), w, h: hgt };
            fill(&mut canvas, rect, [rng.next() as u8, rng.next() as u8, rng.next() as u8, 255]);
            damage.push(rect);
        }
        h.frame(&canvas, Some(&damage));
    }
    assert_eq!(h.sim.placement_count(), 0, "through tmux nothing is placed by pixel position");
    assert_eq!(h.sim.image_count(), 1, "one image shown through placeholder cells");
    assert!(!h.sim.cells.is_empty());
    h
}

#[test]
fn frame_edits_through_tmux_keep_one_placeholder_image_exact() {
    let h = random_boxes_through_tmux(Presenter::Animation, 0x7e57);
    assert!(h.sim.frame_edits >= 60, "changes went out as edits of the one image: {}", h.sim.frame_edits);
    assert!(h.sim.transmits <= 3, "the whole frame went out only to start: {}", h.sim.transmits);
}

#[test]
fn whole_frames_through_tmux_stay_exact() {
    let h = random_boxes_through_tmux(Presenter::Full, 0x7e58);
    assert_eq!(h.sim.frame_edits, 0);
    assert!(h.sim.transmits >= 60);
}

/// A HUD painted over the page (the zoom indicator, a modal): the bands stay opaque, the
/// HUD's pixels are compared like chrome, and nothing costs a full frame.
#[test]
fn ui_painted_over_the_page_is_patched_and_never_costs_a_full_frame() {
    let mut h = Harness::new(Presenter::Patched, FrameTransport::Shared, GHOSTTY);
    let mut rng = Lcg(0x4d);
    let mut canvas = base_canvas(W, H, 120);
    let (strip, webview) = chrome_over_webview(&mut h, 1);
    paint_tabs(&mut canvas, strip, &mut rng, true);
    h.frame(&canvas, None);
    let whole = Rect::sized(W, H);
    let hud = Rect { x: webview.x + webview.w - 130, y: webview.y + 20, w: 110, h: 40 };
    let digits = Rect { x: hud.x + 10, y: hud.y + 10, w: 40, h: 20 };
    let quarter_frame = u64::from(W) * u64::from(H) / 4;
    let paint_hud = |canvas: &mut Canvas, shade: u8| {
        fill(canvas, hud, [30, 30, 36, 255]);
        fill(canvas, digits, [200, shade, 40, 255]);
    };

    paint_hud(&mut canvas, 200);
    h.ui_over_surfaces = vec![hud];
    h.frame_repaint(&canvas, &[whole], &[]);
    assert!(h.sim.pixels_last_frame < quarter_frame, "the HUD is a patch, not a full frame: {} px", h.sim.pixels_last_frame);
    assert_eq!(h.term.patches.opaque.len(), 3, "the bands stay opaque under the HUD");

    for i in 0..30u8 {
        // the page animates, including under the HUD, while the HUD's digits change
        let page = Rect { x: webview.x + rng.range(webview.w - 60), y: hud.y + 60 + rng.range(webview.h - 140), w: 60, h: 60 };
        fill(&mut canvas, page, [rng.next() as u8, rng.next() as u8, rng.next() as u8, 255]);
        let under = Rect { x: hud.x - 20, y: hud.y - 10, w: 60, h: 60 };
        fill(&mut canvas, under, [rng.next() as u8, 90, rng.next() as u8, 255]);
        paint_hud(&mut canvas, 200 - i);
        h.frame_repaint(&canvas, &[whole], &[page, under]);
        assert!(h.sim.pixels_last_frame < quarter_frame, "frame {i}: patches only, {} px", h.sim.pixels_last_frame);
    }

    // the HUD goes away and the page shows again where it was
    fill(&mut canvas, hud, [120, 60, 40, 255]);
    fill(&mut canvas, Rect { x: hud.x - 20, y: hud.y - 10, w: 60, h: 60 }.clamped(W, H), [120, 60, 40, 255]);
    h.ui_over_surfaces = Vec::new();
    h.frame_repaint(&canvas, &[whole], &[Rect { x: hud.x - 20, y: hud.y - 10, w: 60, h: 60 }]);
    assert!(h.sim.pixels_last_frame < quarter_frame, "the page under the old HUD is restored by a patch: {} px", h.sim.pixels_last_frame);
    assert_eq!(h.term.patches.opaque.len(), 3);
}
