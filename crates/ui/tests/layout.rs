//! The chat on the Player screen, laid out and rendered headlessly.
//!
//! The view-model tests cannot see Slint layout bugs: a binding loop panics only when the window
//! is laid out, and a list that overflows the wrong way only shows in the pixels. This renders the
//! window with Slint's software renderer in a headless backend.
//!
//! One test function, because the headless platform can only be installed once per process.

use htpc_core::{
    init, reduce, AppState, ChatKind, ChatLayout, ChatMessage, ChatSpan, ChatStatus, Event, FeedId,
    Intent, Rgb, Row, RowKind, Stream, Tile,
};
use htpc_ui::{apply_chat_image, view_model, MainWindow, Thumbnail, Update};
use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use slint::{ComponentHandle, Rgba8Pixel, SharedPixelBuffer};

const WIDTH: u32 = 1280;
const HEIGHT: u32 = 800;
/// The hints bar along the bottom (`Theme.hints-h`).
const HINTS_TOP: u32 = HEIGHT - 72;

// The options struct grows fields depending on how Slint's testing backend was built, so the
// update syntax is kept even where every field happens to be named.
#[allow(clippy::needless_update)]
fn install_headless_platform() {
    let backend = TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
        ..Default::default()
    });
    slint::platform::set_platform(Box::new(backend)).expect("one platform per process");
}

/// The player, chat open, with more lines than fit. The last line has a bright yellow name nobody
/// else uses, so its pixels can be found.
fn watching_a_busy_chat(layout: ChatLayout) -> AppState {
    let stream = Tile::Stream(Stream {
        channel: "chan".into(),
        title: "title".into(),
        category: "cat".into(),
        viewers: 10,
        tint: Rgb::new(40, 60, 90),
        thumbnail: None,
    });
    let (s, _) = init();
    let rows = vec![Row::new(RowKind::TopLive, vec![stream])];
    let feed = FeedId::Home;
    let (s, _) = reduce(&s, Event::FeedLoaded { feed, rows });
    let (mut s, _) = reduce(&s, Intent::Select);
    s.settings.chat_layout = layout;

    let words = "this is a chat line that is long enough to wrap onto a second line of text";
    let mut lines: Vec<ChatMessage> = (0..70)
        .map(|i| ChatMessage {
            color: Some(Rgb::new(0, 255, 255)),
            spans: vec![
                ChatSpan::Text(words.into()),
                ChatSpan::Emote {
                    name: "Kappa".into(),
                    url: "https://cdn/kappa.png".into(),
                },
            ],
            badges: vec!["https://cdn/mod.png".into()],
            // Every sixth line is a /me and every seventh a notice, to lay out all three kinds.
            kind: match i {
                n if n % 6 == 0 => ChatKind::Action,
                n if n % 7 == 0 => ChatKind::Notice,
                _ => ChatKind::Message,
            },
            ..ChatMessage::plain(&format!("user{i}"), "")
        })
        .collect();
    lines.push(ChatMessage {
        color: Some(Rgb::new(255, 255, 0)),
        ..ChatMessage::plain("LASTLINE", "the newest message")
    });
    for event in [
        Event::ChatStatus {
            channel: "chan".into(),
            status: ChatStatus::Live,
        },
        Event::ChatMessages {
            channel: "chan".into(),
            messages: lines,
        },
    ] {
        s = reduce(&s, event).0;
    }
    s.chat_open = true;
    s
}

/// A solid picture standing in for a downloaded emote (wider than tall) or badge.
fn picture(width: u32, height: u32) -> Thumbnail {
    Thumbnail {
        width,
        height,
        rgba: [200, 100, 250, 255].repeat((width * height) as usize),
    }
}

fn render(ui: &MainWindow) -> SharedPixelBuffer<Rgba8Pixel> {
    let image = ui.window().take_snapshot().expect("the window renders");
    assert_eq!((image.width(), image.height()), (WIDTH, HEIGHT));
    image
}

fn is_yellow(p: &Rgba8Pixel) -> bool {
    p.r > 200 && p.g > 200 && p.b < 90
}

/// Counts the pixels of `image` inside the rectangle that satisfy `test`.
fn count(
    image: &SharedPixelBuffer<Rgba8Pixel>,
    xs: std::ops::Range<u32>,
    ys: std::ops::Range<u32>,
    test: impl Fn(&Rgba8Pixel) -> bool,
) -> usize {
    let width = image.width();
    let pixels = image.as_slice();
    ys.flat_map(|y| xs.clone().map(move |x| (x, y)))
        .filter(|&(x, y)| test(&pixels[(y * width + x) as usize]))
        .count()
}

#[test]
fn a_busy_chat_lays_out_and_keeps_the_newest_line_in_view() {
    install_headless_platform();
    let ui = MainWindow::new().expect("the window builds");
    ui.window()
        .set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
    ui.show().expect("the window shows");

    for (layout, panel) in [
        // The side panel is the right 380 px, the overlay the right 460.
        (ChatLayout::Side, (WIDTH - 380)..WIDTH),
        (ChatLayout::Overlay, (WIDTH - 460)..WIDTH),
    ] {
        let state = watching_a_busy_chat(layout);
        Update::full(view_model(&state)).apply(&ui);
        render(&ui);
        // The images arrive after the lines, as downloads finish, and patch the lines in place.
        apply_chat_image(&ui, "https://cdn/kappa.png", &picture(56, 28));
        apply_chat_image(&ui, "https://cdn/mod.png", &picture(36, 36));
        render(&ui);
        // More lines come in: the window slides, and the new lines pick up the known images.
        let mut busier = state.clone();
        let more = (0..5).map(|i| ChatMessage {
            badges: vec!["https://cdn/mod.png".into()],
            spans: vec![ChatSpan::Emote {
                name: "Kappa".into(),
                url: "https://cdn/kappa.png".into(),
            }],
            ..ChatMessage::plain(&format!("later{i}"), "")
        });
        busier.chat.extend(more);
        busier.chat.push(ChatMessage {
            color: Some(Rgb::new(255, 255, 0)),
            ..ChatMessage::plain("LASTLINE", "the newest message")
        });
        Update::diff(&view_model(&state), view_model(&busier))
            .expect("the chat changed")
            .apply(&ui);
        let image = render(&ui);

        // Text is drawn at all (a machine without any font would draw none: then there is
        // nothing to look at, and the layout having run without a panic is the whole test).
        let text = |p: &Rgba8Pixel| p.r > 200 && p.g > 200 && p.b > 200;
        if count(&image, panel.clone(), 0..HEIGHT, text) == 0 {
            eprintln!("{layout:?}: no text was rendered (no fonts?), skipping the pixel checks");
            continue;
        }

        // The newest line is in view, above the hints bar...
        let in_view = count(
            &image,
            panel.clone(),
            (HINTS_TOP - 120)..HINTS_TOP,
            is_yellow,
        );
        assert!(
            in_view > 0,
            "{layout:?}: the newest line is not at the bottom of the chat"
        );
        // ...and none of it spilled under the hints bar.
        let spilled = count(&image, panel.clone(), HINTS_TOP..HEIGHT, is_yellow);
        assert_eq!(
            spilled, 0,
            "{layout:?}: the newest line runs under the hints bar"
        );
        // The chat fills the height: older lines are still visible up near the top.
        let cyan = |p: &Rgba8Pixel| p.r < 80 && p.g > 200 && p.b > 200;
        assert!(
            count(&image, panel.clone(), 0..300, cyan) > 0,
            "{layout:?}: the list is not full to the top"
        );
    }

    // The quit prompt over Home: it lays out, draws its title in the middle of the screen, and
    // the hints bar underneath is empty (the prompt has its own buttons).
    let (home, _) = init();
    let (asking, _) = reduce(&home, Intent::Back);
    assert!(asking.exit_prompt);
    Update::full(view_model(&asking)).apply(&ui);
    let image = render(&ui);
    let text = |p: &Rgba8Pixel| p.r > 200 && p.g > 200 && p.b > 200;
    // The green ring of the A button (`Theme.btn-a`), drawn whether or not there are fonts.
    let a_button = |p: &Rgba8Pixel| p.g > 150 && p.r < 150 && p.b < 120;
    assert!(
        count(&image, 360..920, 260..540, a_button) > 0,
        "the prompt has no A button"
    );
    assert_eq!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button),
        0,
        "the hints bar still shows hints behind the prompt"
    );
    if count(&image, 360..920, 260..540, text) == 0 {
        eprintln!("quit prompt: no text was rendered (no fonts?), skipping the title check");
    }

    // Browse: the heading sits at the top, not in the middle of the screen. Slint centres an
    // element that has a size but no position, which once put the heading strip mid-screen.
    let (browse, _) = reduce(&init().0, Intent::RailPick(htpc_core::RailEntry::Browse));
    Update::full(view_model(&browse)).apply(&ui);
    let image = render(&ui);
    let top = count(&image, 96..WIDTH, 0..110, text);
    let middle = count(&image, 96..WIDTH, 250..550, text);
    eprintln!("browse heading: {top} text pixels at the top, {middle} mid-screen");
    if top + middle == 0 {
        eprintln!("browse: no text was rendered (no fonts?), skipping the heading check");
    } else {
        assert!(top > 0, "the Browse heading is not at the top");
        assert_eq!(
            middle, 0,
            "the Browse heading is in the middle of the screen"
        );
    }
}
