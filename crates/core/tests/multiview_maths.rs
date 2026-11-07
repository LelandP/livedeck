//! Multiview's layout and quality maths (plan.md, sections 1 and 3).

use htpc_core::layout::{self, pixel_height, rects, AREA, FULL_SCREEN, MAX_STREAMS};
use htpc_core::quality::{rendition, UPSCALE_ALLOWED};
use htpc_core::{init, reduce, Event, FrameRate, Layout, Quality, Rect, Rendition};

/// Every layout with every stream count it can hold, and every place for the sound.
fn every_arrangement() -> Vec<(Layout, usize, usize)> {
    let mut out = Vec::new();
    for layout in Layout::ALL {
        for streams in 1..=MAX_STREAMS {
            if !layout.fits(streams) {
                continue;
            }
            for sound in 0..streams {
                out.push((layout, streams, sound));
            }
        }
    }
    out
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

#[test]
fn one_rect_per_place() {
    for (layout, streams, sound) in every_arrangement() {
        assert_eq!(
            rects(layout, streams, sound).len(),
            layout.places(streams),
            "{layout:?} with {streams}"
        );
    }
    assert_eq!(Layout::Grid(6).places(2), 6, "empty places stay");
    assert_eq!(Layout::Spotlight.places(2), 4);
    assert_eq!(Layout::Spotlight.places(4), 4);
    assert_eq!(Layout::Spotlight.places(5), 9);
}

#[test]
fn tiles_stay_inside_the_area_and_never_overlap() {
    for (layout, streams, sound) in every_arrangement() {
        let rs = rects(layout, streams, sound);
        for (i, a) in rs.iter().enumerate() {
            assert!(
                AREA.contains(a),
                "{layout:?}/{streams}: {a:?} leaves the area"
            );
            for b in &rs[i + 1..] {
                assert!(!a.overlaps(b), "{layout:?}/{streams}: {a:?} overlaps {b:?}");
            }
        }
    }
}

#[test]
fn every_tile_is_16_by_9() {
    for (layout, streams, sound) in every_arrangement() {
        for r in rects(layout, streams, sound) {
            assert!(close(r.w / r.h, 16.0 / 9.0), "{layout:?}/{streams}: {r:?}");
        }
    }
}

#[test]
fn spotlight_makes_the_stream_with_sound_the_biggest() {
    for streams in 2..=MAX_STREAMS {
        for sound in 0..streams {
            let rs = rects(Layout::Spotlight, streams, sound);
            let big = rs[sound];
            for (i, r) in rs.iter().enumerate() {
                if i != sound {
                    assert!(r.h < big.h, "{streams} streams, sound {sound}: {r:?}");
                }
            }
        }
    }
}

#[test]
fn grid_tiles_are_all_the_same_size() {
    for size in Layout::GRID_SIZES {
        let rs = rects(Layout::Grid(size), 1, 0);
        assert!(rs
            .iter()
            .all(|r| close(r.w, rs[0].w) && close(r.h, rs[0].h)));
    }
}

#[test]
fn the_third_tile_of_a_grid_of_3_is_centred() {
    let rs = rects(Layout::Grid(3), 3, 0);
    assert!(close(rs[2].center().0, AREA.center().0));
    assert!(rs[2].y > rs[0].bottom());
}

#[test]
fn moving_the_sound_in_a_grid_moves_nothing() {
    assert_eq!(rects(Layout::Grid(4), 4, 0), rects(Layout::Grid(4), 4, 3));
}

/// Tile heights in the layout (plan.md, section 3: "Tile sizes in the layout").
#[test]
fn tile_heights_in_the_layout() {
    let height = |layout, streams, place: usize| rects(layout, streams, 0)[place].h;
    let rows: [(&str, f32, f32); 9] = [
        ("full screen", FULL_SCREEN.h, 800.0),
        ("grid of 2", height(Layout::Grid(2), 2, 0), 319.5),
        ("grid of 3", height(Layout::Grid(3), 3, 0), 308.0),
        ("grid of 4", height(Layout::Grid(4), 4, 0), 308.0),
        ("grid of 6", height(Layout::Grid(6), 6, 0), 210.0),
        ("grid of 9", height(Layout::Grid(9), 9, 0), 200.0),
        ("spotlight 2-4 big", height(Layout::Spotlight, 3, 0), 459.0),
        (
            "spotlight 2-4 small",
            height(Layout::Spotlight, 3, 1),
            180.0,
        ),
        ("spotlight 5-9 big", height(Layout::Spotlight, 7, 0), 319.5),
    ];
    for (name, got, want) in rows {
        assert!(close(got, want), "{name}: {got}, want {want}");
    }
    assert!(close(height(Layout::Spotlight, 7, 1), 155.25));
}

#[test]
fn layouts_too_small_for_the_streams_are_skipped() {
    assert!(!Layout::Grid(4).fits(5));
    assert!(Layout::Grid(6).fits(5));
    assert!(Layout::Spotlight.fits(9));
    assert_eq!(Layout::smallest_grid(1), Layout::Grid(2));
    assert_eq!(Layout::smallest_grid(5), Layout::Grid(6));
    assert_eq!(Layout::smallest_grid(9), Layout::Grid(9));

    // Five streams: 6, 9, Spotlight, and around again.
    assert_eq!(Layout::Grid(6).cycled(5, true), Layout::Grid(9));
    assert_eq!(Layout::Grid(9).cycled(5, true), Layout::Spotlight);
    assert_eq!(Layout::Spotlight.cycled(5, true), Layout::Grid(6));
    assert_eq!(Layout::Grid(6).cycled(5, false), Layout::Spotlight);
    // Two streams: every layout.
    assert_eq!(Layout::Grid(2).cycled(2, true), Layout::Grid(3));
    // A layout that no longer fits starts from the first one that does.
    assert_eq!(Layout::Grid(2).cycled(3, true), Layout::Grid(3));
}

#[test]
fn the_scale_follows_the_screen() {
    assert_eq!(layout::fit_scale(1280, 800), Some(1.0));
    assert_eq!(layout::fit_scale(3840, 2160), Some(2.7));
    assert_eq!(layout::fit_scale(0, 800), None);
    assert!(close(pixel_height(800.0, (1920, 1080)), 1080.0));
    assert!(
        close(pixel_height(200.0, (0, 0)), 200.0),
        "unknown: the Deck"
    );
}

// ---------------------------------------------------------------------------------------------
// Quality from tile size: "What each screen gets" in plan.md, section 3, one test per row.

const DECK: (u32, u32) = (1280, 800);
const TV_1080: (u32, u32) = (1920, 1080);
const QHD: (u32, u32) = (2560, 1440);
const TV_4K: (u32, u32) = (3840, 2160);

/// The rendition for a tile on each of the four screens, with Settings at Best and no network
/// limit.
fn on_screens(tile: Rect) -> [Rendition; 4] {
    [DECK, TV_1080, QHD, TV_4K].map(|d| rendition(pixel_height(tile.h, d), Quality::Best, 0))
}

use Rendition::{P1080, P160, P360, P480, P720};

#[test]
fn full_screen() {
    assert_eq!(on_screens(FULL_SCREEN), [P720, P1080, P1080, P1080]);
}

#[test]
fn grid_of_2() {
    for r in rects(Layout::Grid(2), 2, 0) {
        assert_eq!(on_screens(r), [P360, P480, P720, P1080]);
    }
}

#[test]
fn grid_of_3_or_4() {
    for size in [3, 4] {
        for r in rects(Layout::Grid(size), usize::from(size), 0) {
            assert_eq!(on_screens(r), [P360, P480, P720, P1080], "grid of {size}");
        }
    }
}

#[test]
fn grid_of_6() {
    for r in rects(Layout::Grid(6), 6, 0) {
        assert_eq!(on_screens(r), [P360, P360, P360, P720]);
    }
}

#[test]
fn grid_of_9() {
    for r in rects(Layout::Grid(9), 9, 0) {
        assert_eq!(on_screens(r), [P360, P360, P360, P720]);
    }
}

#[test]
fn spotlight_2_to_4() {
    for streams in 2..=4 {
        let rs = rects(Layout::Spotlight, streams, 0);
        assert_eq!(
            on_screens(rs[0]),
            [P480, P720, P1080, P1080],
            "big, {streams}"
        );
        for r in &rs[1..] {
            assert_eq!(on_screens(*r), [P360, P360, P360, P480], "small, {streams}");
        }
    }
}

#[test]
fn spotlight_5_to_9() {
    for streams in 5..=9 {
        let rs = rects(Layout::Spotlight, streams, 0);
        assert_eq!(
            on_screens(rs[0]),
            [P360, P480, P720, P1080],
            "big, {streams}"
        );
        for r in &rs[1..] {
            assert_eq!(on_screens(*r), [P160, P360, P360, P480], "small, {streams}");
        }
    }
}

#[test]
fn a_little_upscaling_is_allowed() {
    assert_eq!(UPSCALE_ALLOWED, 0.9);
    // 480 covers up to 533 px.
    assert_eq!(rendition(533.0, Quality::Best, 0), P480);
    assert_eq!(rendition(534.0, Quality::Best, 0), P720);
    assert_eq!(rendition(0.0, Quality::Best, 0), P160);
    assert_eq!(
        rendition(5000.0, Quality::Best, 0),
        P1080,
        "never above 1080p"
    );
}

#[test]
fn never_above_the_settings_quality() {
    let full_4k = pixel_height(FULL_SCREEN.h, TV_4K);
    assert_eq!(rendition(full_4k, Quality::Best, 0), P1080);
    assert_eq!(rendition(full_4k, Quality::P1080, 0), P1080);
    assert_eq!(rendition(full_4k, Quality::P720, 0), P720);
    assert_eq!(rendition(full_4k, Quality::P480, 0), P480);
    assert_eq!(rendition(full_4k, Quality::Worst, 0), P160);
    // A tile already below the cap keeps its size's quality.
    assert_eq!(rendition(200.0, Quality::P720, 0), P360);
}

#[test]
fn the_network_limit_lowers_by_steps() {
    assert_eq!(rendition(1080.0, Quality::Best, 1), P720);
    assert_eq!(rendition(1080.0, Quality::Best, 2), P480);
    assert_eq!(
        rendition(1080.0, Quality::Best, 9),
        P160,
        "stops at the lowest"
    );
    // Steps count from the capped rendition.
    assert_eq!(rendition(1080.0, Quality::P720, 1), P480);
}

// ---------------------------------------------------------------------------------------------
// Streamlink fallback lists. Rendition names seen on live channels on 2026-11-07:
//   Twitch: audio_only, 160p30, 360p30, 480p30, 720p30, 720p60, 1080p60
//   Kick:   160p, 360p, 480p, 720p60, 1080p60

#[test]
fn fallback_lists_name_every_spelling_then_go_lower() {
    assert_eq!(
        P480.streamlink_list(FrameRate::Smooth),
        "480p60,480p,480p30,360p60,360p,360p30,160p60,160p,160p30,worst"
    );
    assert_eq!(
        P720.streamlink_list(FrameRate::Light),
        "720p30,720p,720p60,480p30,480p,480p60,360p30,360p,360p60,160p30,160p,160p60,worst"
    );
    assert_eq!(
        P160.streamlink_list(FrameRate::Smooth),
        "160p60,160p,160p30,worst"
    );
    for r in Rendition::ALL {
        for rate in [FrameRate::Smooth, FrameRate::Light] {
            let list = r.streamlink_list(rate);
            assert!(list.ends_with(",worst"), "{list}");
            assert!(list.split(',').count() <= 16, "{list}");
        }
    }
}

#[test]
fn the_single_player_asks_for_its_settings_quality_as_a_list() {
    assert_eq!(Quality::Best.streamlink_list(), "best");
    assert_eq!(Quality::Worst.streamlink_list(), "worst");
    assert_eq!(
        Quality::P720.streamlink_list(),
        P720.streamlink_list(FrameRate::Smooth)
    );
    assert!(Quality::P1080
        .streamlink_list()
        .starts_with("1080p60,1080p,1080p30,720p60,"));
}

#[test]
fn renditions_label_and_order() {
    assert!(P160 < P360 && P360 < P480 && P480 < P720 && P720 < P1080);
    assert_eq!(P360.label(), "360p");
    assert_eq!(P720.lowered(0), P720);
    assert_eq!(P360.lowered(1), P160);
}

// ---------------------------------------------------------------------------------------------

#[test]
fn the_display_size_is_kept_and_an_empty_window_ignored() {
    let (s, _) = init();
    assert_eq!(s.display, (0, 0));
    let (s, cmds) = reduce(&s, Event::DisplaySize { w: 1920, h: 1080 });
    assert_eq!(s.display, (1920, 1080));
    assert!(cmds.is_empty());
    let (s, _) = reduce(&s, Event::DisplaySize { w: 0, h: 0 });
    assert_eq!(s.display, (1920, 1080), "minimised");
}
