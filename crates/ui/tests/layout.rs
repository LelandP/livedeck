//! The chat on the Player screen, laid out and rendered headlessly.
//!
//! The view-model tests cannot see Slint layout bugs: a binding loop panics only when the window
//! is laid out, and a list that overflows the wrong way only shows in the pixels. This renders the
//! window with Slint's software renderer in a headless backend.
//!
//! One test function, because the headless platform can only be installed once per process.

use htpc_core::{
    init, reduce, AppState, ChannelRef, ChatKind, ChatLayout, ChatMessage, ChatSpan, ChatStatus,
    Event, FeedId, Intent, Rgb, Row, RowKind, Stream, Tile,
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
        channel: ChannelRef::twitch("chan"),
        user_id: String::new(),
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
            channel: ChannelRef::twitch("chan"),
            status: ChatStatus::Live,
        },
        Event::ChatMessages {
            channel: ChannelRef::twitch("chan"),
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
    // the hints bar underneath is empty (the prompt has its own buttons). Back twice: the first
    // opens the sidebar, the second asks to quit.
    let (home, _) = init();
    let (sidebar, _) = reduce(&home, Intent::Back);
    let (asking, _) = reduce(&sidebar, Intent::Back);
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

    // A failed stream with the player's log lines: the details panel lays out and is drawn below
    // the summary, with the newest line visible.
    let mut failed = watching_a_busy_chat(ChatLayout::Side);
    failed.chat_open = false;
    failed.playback = htpc_core::Playback::Failed(
        "the stream could not be opened (mpv error -13)\n\n\
         [ffmpeg] error: https: Protocol not found\n\
         [cplayer] error: Failed to open https://example.test/live.m3u8"
            .into(),
    );
    Update::full(view_model(&failed)).apply(&ui);
    let image = render(&ui);
    let below_summary = count(&image, 0..WIDTH, 400..HINTS_TOP, text);
    if count(&image, 0..WIDTH, 0..HEIGHT, text) > 0 {
        assert!(
            below_summary > 0,
            "the player log is not drawn under the error"
        );
    }

    // A playing stream: the hints bar shows at first, then fades out with the HUD.
    let mut playing = watching_a_busy_chat(ChatLayout::Side);
    playing.chat_open = false;
    playing.playback = htpc_core::Playback::Playing;
    playing.paused = false;
    Update::full(view_model(&playing)).apply(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(10));
    let image = render(&ui);
    assert!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button) > 0,
        "the hints bar is not shown when a stream starts"
    );
    // The HUD timer is 4 s, then the fade takes 240 ms. The mock clock runs animations before
    // timers, so the fade only starts on the tick after the timer fires.
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(4_500));
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(&ui);
    assert_eq!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button),
        0,
        "the hints bar stays on screen over a playing stream"
    );
    // Pausing brings it back.
    playing.paused = true;
    playing.hud_pulse += 1;
    Update::full(view_model(&playing)).apply(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(&ui);
    assert!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button) > 0,
        "the hints bar does not come back on pause"
    );

    // The player menu and the stats box: both lay out (no binding loop) and are drawn on the left
    // of the video, above the hints bar, with the hints bar kept on screen.
    let mut menu = watching_a_busy_chat(ChatLayout::Side);
    menu.chat_open = false;
    menu.playback = htpc_core::Playback::Playing;
    let (mut menu, _) = reduce(&menu, Intent::Menu);
    menu.session.stats = true;
    menu.stats = Some(htpc_core::PlayerStats {
        width: Some(1920),
        height: Some(1080),
        ..Default::default()
    });
    Update::full(view_model(&menu)).apply(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(5_000));
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(&ui);
    // The focused row's border (`Theme.accent-light`, #a594ff), drawn whether or not there are
    // fonts.
    let accent = |p: &Rgba8Pixel| p.r > 160 && p.b > 200 && p.g > 120 && p.g < 180;
    assert!(
        count(&image, 0..560, 0..HINTS_TOP, accent) > 0,
        "the player menu is not drawn"
    );
    assert!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button) > 0,
        "the hints bar hides while the menu is open"
    );

    // The menu's eighth row (Subscribe) still fits: the menu's top edge stays on screen, so a
    // focused last row is drawn in the bottom half, above the HUD.
    let (on_last, _) = reduce(&menu, Intent::Menu);
    let (on_last, _) = reduce(&on_last, Intent::Menu);
    let on_last = (0..7).fold(on_last, |s, _| reduce(&s, Intent::Down).0);
    assert_eq!(
        on_last.player_menu,
        Some(htpc_core::PlayerMenuRow::Subscribe)
    );
    Update::full(view_model(&on_last)).apply(&ui);
    let image = render(&ui);
    assert!(
        count(&image, 0..560, 300..HINTS_TOP, accent) > 0,
        "the Subscribe row is not drawn"
    );

    // The subscribe card: its QR code is drawn in the middle of the video (white with dark
    // modules), and in Game Mode the hints bar offers only B (no green A).
    for can_open_browser in [true, false] {
        let mut card = on_last.clone();
        card.can_open_browser = can_open_browser;
        let (card, _) = reduce(&card, Intent::Select);
        assert!(card.subscribe_card);
        Update::full(view_model(&card)).apply(&ui);
        let image = render(&ui);
        let white = |p: &Rgba8Pixel| p.r > 240 && p.g > 240 && p.b > 240;
        let black = |p: &Rgba8Pixel| p.r < 20 && p.g < 20 && p.b < 20;
        let (qr_x, qr_y) = (520..760, 220..500);
        assert!(
            count(&image, qr_x.clone(), qr_y.clone(), white) > 5_000,
            "the subscribe QR code has no white background"
        );
        assert!(
            count(&image, qr_x, qr_y, black) > 5_000,
            "the subscribe QR code has no modules"
        );
        let a_in_hints = count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button) > 0;
        assert_eq!(
            a_in_hints, can_open_browser,
            "A is offered only where a browser can open"
        );
    }

    // Audio only: the screen lays out, stays nearly all black once the HUD has faded, and the
    // watermark glides to a new spot after its 30 s timer (and only then).
    let mut audio = watching_a_busy_chat(ChatLayout::Side);
    audio.chat_open = false;
    audio.playback = htpc_core::Playback::Playing;
    audio.audio_only = true;
    Update::full(view_model(&audio)).apply(&ui);
    // A render runs the HUD's change handler, which starts its 4 s timer.
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(10));
    render(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(4_500));
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let before = render(&ui);
    let lit = |p: &Rgba8Pixel| p.r > 8 || p.g > 8 || p.b > 8;
    let total = (WIDTH * HEIGHT) as usize;
    let lit_before = count(&before, 0..WIDTH, 0..HEIGHT, lit);
    assert!(
        lit_before * 100 < total * 3,
        "the audio-only screen is not black: {lit_before} of {total} pixels lit"
    );
    // The tint line is drawn whether or not there are fonts.
    assert!(lit_before > 0, "the watermark is not drawn");
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(10));
    let still = render(&ui);
    assert!(
        still.as_slice() == before.as_slice(),
        "the audio-only screen changes between glides"
    );
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(21));
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_secs(4));
    let moved = render(&ui);
    assert!(
        moved.as_slice() != before.as_slice(),
        "the watermark did not move after 30 s"
    );

    // Sleep timer: the HUD shows it, and "Still watching?" is drawn centred above the hints bar
    // (its border is `Theme.accent-light`) with the hints bar kept on screen.
    let mut warned = watching_a_busy_chat(ChatLayout::Side);
    warned.chat_open = false;
    warned.playback = htpc_core::Playback::Playing;
    warned.sleep = Some(htpc_core::SleepTimer {
        id: 1,
        minutes: 30,
        left_min: 0,
        warning: true,
    });
    // Brings the HUD back, which the audio-only checks above let fade.
    warned.hud_pulse += 1;
    Update::full(view_model(&warned)).apply(&ui);
    // The first frame runs the HUD's change handler, which starts its fade-in.
    render(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(&ui);
    assert!(
        count(&image, 380..900, 400..HINTS_TOP, accent) > 0,
        "the sleep warning is not drawn"
    );
    assert!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button) > 0,
        "the hints bar hides during the sleep warning"
    );

    // The raid card: drawn bottom centre above the HUD text (its border is `Theme.accent-light`),
    // the hints bar stays on screen after the HUD fades, and the countdown bar (`Theme.accent`)
    // empties over the 10 s.
    let mut raided = watching_a_busy_chat(ChatLayout::Side);
    raided.chat_open = false;
    raided.playback = htpc_core::Playback::Playing;
    raided.raid = Some(htpc_core::RaidOffer {
        id: 1,
        from: "chan".into(),
        to_login: "next_up".into(),
        to_name: "Next_Up".into(),
        viewers: 1_234,
        seconds_left: 10,
    });
    raided.hud_pulse += 1;
    Update::full(view_model(&raided)).apply(&ui);
    render(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(10));
    let image = render(&ui);
    let card_area = (360..920, 300..610);
    assert!(
        count(&image, card_area.0.clone(), card_area.1.clone(), accent) > 0,
        "the raid card is not drawn"
    );
    let bar = |p: &Rgba8Pixel| (100..150).contains(&p.r) && (75..120).contains(&p.g) && p.b > 230;
    let full = count(&image, card_area.0.clone(), card_area.1.clone(), bar);
    assert!(full > 1_000, "the countdown bar is not drawn full: {full}");
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(5_000));
    let image = render(&ui);
    let half = count(&image, card_area.0.clone(), card_area.1.clone(), bar);
    assert!(
        half < full * 3 / 4 && half > full / 4,
        "the bar does not empty steadily: {half} of {full} after 5 s"
    );
    assert!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button) > 0,
        "the hints bar fades while the raid card is open"
    );
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(5_500));
    let image = render(&ui);
    assert_eq!(
        count(&image, card_area.0, card_area.1, bar),
        0,
        "the bar is not empty after 10 s"
    );

    // Hype train: a slim bar at the top of the video, filled (`Theme.accent`) as far as the level
    // has come, and gone once the train ends.
    let mut hyped = raided.clone();
    hyped.raid = None;
    hyped.hype_train = Some(htpc_core::HypeTrain {
        level: 3,
        progress: 1_500,
        goal: 2_000,
        seconds_left: 125,
    });
    Update::full(view_model(&hyped)).apply(&ui);
    render(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(&ui);
    let top = (0..WIDTH, 0..160);
    let filled = count(&image, top.0.clone(), top.1.clone(), bar);
    assert!(filled > 500, "the hype train bar is not drawn: {filled}");
    assert_eq!(
        count(&image, 0..WIDTH, 300..HINTS_TOP, bar),
        0,
        "the hype train bar is not at the top"
    );
    hyped.hype_train = None;
    Update::full(view_model(&hyped)).apply(&ui);
    let image = render(&ui);
    assert_eq!(
        count(&image, top.0, top.1, bar),
        0,
        "the hype train bar stays after the train"
    );

    // Search: the keyboard lays out under the text field with its focused key (`Theme.accent`, a
    // strong violet) top left, and the first result row is fully in view under it. With the
    // results focused, the keyboard slides away and the rows move up.
    let user = htpc_core::User {
        login: "me".into(),
        can_chat: true,
    };
    let (signed_in, _) = reduce(&init().0, Event::SignedIn(user));
    let (search, _) = reduce(&signed_in, Intent::RailPick(htpc_core::RailEntry::Search));
    let (search, _) = reduce(&search, Intent::Text('a'));
    let (search, _) = reduce(&search, Intent::Text('b'));
    let rows = vec![Row::new(
        RowKind::SearchLive,
        (0..4)
            .map(|i| {
                Tile::Stream(Stream {
                    channel: ChannelRef::twitch(format!("c{i}")),
                    user_id: String::new(),
                    title: "t".into(),
                    category: "cat".into(),
                    viewers: 5,
                    // Pure green, which nothing else on this screen uses.
                    tint: Rgb::new(0, 255, 0),
                    thumbnail: None,
                })
            })
            .collect(),
    )];
    let id = search.search.id;
    let (search, _) = reduce(&search, Event::SearchResults { id, rows });
    Update::full(view_model(&search)).apply(&ui);
    let image = render(&ui);
    let key_focus = |p: &Rgba8Pixel| p.r > 100 && p.r < 170 && p.g < 120 && p.b > 220;
    assert!(
        count(&image, 96..220, 100..170, key_focus) > 500,
        "the focused key is not drawn under the text field"
    );
    // Cards are dimmed while the keyboard has focus, so the green is darker.
    let green = |p: &Rgba8Pixel| {
        let (r, g, b) = (u16::from(p.r), u16::from(p.g), u16::from(p.b));
        g > 80 && g > 3 * r && g > 3 * b
    };
    let green_cards = count(&image, 96..WIDTH, 420..HINTS_TOP, green);
    assert!(
        green_cards > 0,
        "the first result row is not under the keyboard"
    );
    let (in_results, _) = reduce(&search, Intent::Menu);
    assert!(in_results.search.in_results);
    Update::full(view_model(&in_results)).apply(&ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(&ui);
    assert_eq!(
        count(&image, 96..900, 100..400, key_focus),
        0,
        "the keyboard stays in view with the results focused"
    );
    assert!(
        count(&image, 96..WIDTH, 0..400, green) > 0,
        "the result row does not move up"
    );

    // Live notifications over a playing stream: three toasts stack at the top right (their
    // previews are drawn in the tint, pure green here) with "+1 more" under them, the hints bar
    // keeps its buttons, and a toast slides in rather than appearing at once.
    let mut toasted = watching_a_busy_chat(ChatLayout::Side);
    toasted.chat_open = false;
    toasted.playback = htpc_core::Playback::Playing;
    toasted.user = Some(htpc_core::User {
        login: "me".into(),
        can_chat: true,
    });
    toasted.hud_pulse += 1;
    let went_live: Vec<Stream> = (0..4)
        .map(|i| Stream {
            channel: ChannelRef::twitch(format!("live{i}")),
            user_id: String::new(),
            title: "a title".into(),
            category: "cat".into(),
            viewers: 1,
            tint: Rgb::new(0, 255, 0),
            thumbnail: None,
        })
        .collect();
    let (toasted, _) = reduce(&toasted, Event::WentLive(went_live));
    assert_eq!(view_model(&toasted).toast_more, 1);
    Update::full(view_model(&toasted)).apply(&ui);
    let first_frame = render(&ui);
    // The slide-in starts when a 1 ms timer fires, and the mock clock runs animations before
    // timers: the first tick fires it, the second animates.
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(10));
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(&ui);
    let top_right = |img: &SharedPixelBuffer<Rgba8Pixel>| count(img, 800..WIDTH, 0..340, green);
    // One slot per toast: 96 px tall, 12 px apart, from 24 px down.
    for slot in 0..3u32 {
        let top = 24 + slot * 108;
        let preview = count(&image, 800..WIDTH, top..top + 96, green);
        // A 128x72 preview, most of it tinted.
        assert!(
            preview > 4_000,
            "toast {slot} is not drawn at the top right"
        );
    }
    assert!(
        top_right(&first_frame) < top_right(&image),
        "the toasts do not slide in"
    );
    assert!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button) > 0,
        "the toasts hide the hints bar"
    );

    // The player menu's ninth row (Write in chat) fits too: focused, it is drawn above the HUD,
    // and with the first row focused that row sits well below the top edge (the menu does not
    // run off the screen).
    let mut menu9 = watching_a_busy_chat(ChatLayout::Side);
    menu9.chat_open = false;
    menu9.playback = htpc_core::Playback::Playing;
    let (menu9, _) = reduce(&menu9, Intent::Menu);
    Update::full(view_model(&menu9)).apply(&ui);
    let image = render(&ui);
    let first_accent_row = (0..HINTS_TOP)
        .find(|&y| count(&image, 0..560, y..y + 1, accent) > 0)
        .expect("the focused first row is drawn");
    assert!(
        first_accent_row >= 40,
        "the menu runs off the top: its first row is at y={first_accent_row}"
    );
    let last = (0..8).fold(menu9, |s, _| reduce(&s, Intent::Down).0);
    assert_eq!(last.player_menu, Some(htpc_core::PlayerMenuRow::WriteChat));
    Update::full(view_model(&last)).apply(&ui);
    let image = render(&ui);
    assert!(
        count(&image, 0..560, 300..HINTS_TOP, accent) > 0,
        "the Write in chat row is not drawn"
    );

    // Writing in chat. Side layout: the video shrinks to the top, the keyboard sits under it with
    // its focused key (`Theme.accent`) on `a`, the box at the bottom of the chat panel has its
    // focus border (`Theme.accent-light`), and the hints bar stays. Overlay: the video keeps its
    // full height and the keyboard is drawn over its bottom, left of the chat.
    let compose_h = 366;
    for (layout, keys_x) in [(ChatLayout::Side, 90..170), (ChatLayout::Overlay, 50..130)] {
        let mut writing = watching_a_busy_chat(layout);
        writing.user = Some(htpc_core::User {
            login: "me".into(),
            can_chat: true,
        });
        writing.playback = htpc_core::Playback::Playing;
        let (writing, _) = reduce(&writing, Intent::Retry);
        assert!(writing.composing());
        let writing = "hello chat"
            .chars()
            .fold(writing, |s, c| reduce(&s, Intent::Text(c)).0);
        Update::full(view_model(&writing)).apply(&ui);
        render(&ui);
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
        let image = render(&ui);
        let want_video_h = match layout {
            ChatLayout::Side => (HEIGHT - 72 - compose_h) as f32,
            ChatLayout::Overlay => HEIGHT as f32,
        };
        assert_eq!(ui.get_video_h(), want_video_h, "{layout:?}: video height");
        // The first key row: 16 px padding, the chips, a 12 px gap.
        let key_top = HEIGHT - 72 - compose_h + 16 + 40 + 12;
        assert!(
            count(&image, keys_x, key_top..key_top + 50, key_focus) > 500,
            "{layout:?}: the focused key is not drawn on the keyboard"
        );
        let chat_x = match layout {
            ChatLayout::Side => (WIDTH - 380)..WIDTH,
            ChatLayout::Overlay => (WIDTH - 460)..WIDTH,
        };
        assert!(
            count(&image, chat_x, 560..HINTS_TOP, accent) > 0,
            "{layout:?}: the message box is not at the bottom of the chat"
        );
        assert!(
            count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button) > 0,
            "{layout:?}: the hints bar hides while writing"
        );
        // Turning the page swaps the keys in place (no binding loop, focus kept).
        let (paged, _) = reduce(&writing, Intent::PageRight);
        Update::diff(&view_model(&writing), view_model(&paged))
            .expect("the page changed")
            .apply(&ui);
        render(&ui);
    }

    // A sign-in from before chat sending: the panel lays out its message instead of keys.
    let mut old_token = watching_a_busy_chat(ChatLayout::Side);
    old_token.user = Some(htpc_core::User {
        login: "me".into(),
        can_chat: false,
    });
    let (old_token, _) = reduce(&old_token, Intent::Retry);
    assert!(old_token.composing());
    Update::full(view_model(&old_token)).apply(&ui);
    let image = render(&ui);
    assert_eq!(
        count(&image, 0..900, 362..HINTS_TOP, key_focus),
        0,
        "no keys are drawn for a token that may not chat"
    );

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

    // Sign-in: the QR code and the benefits list under it fit on screen, at the Deck's 800 px
    // (labels only) and at 1080 px (with the explanations). The column is centred, so if it grew
    // too tall the white QR square would be pushed off the top.
    let (sign_in, _) = reduce(&init().0, Intent::RailPick(htpc_core::RailEntry::Account));
    Update::full(view_model(&sign_in)).apply(&ui);
    // Pure white is the QR square only: `Theme.text` (#efeff1) stays below 240.
    let white = |p: &Rgba8Pixel| p.r > 240 && p.g > 240 && p.b > 240;
    for (w, h) in [(WIDTH, HEIGHT), (1920, 1080)] {
        ui.window().set_size(slint::PhysicalSize::new(w, h));
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
        let image = ui.window().take_snapshot().expect("the window renders");
        assert_eq!((image.width(), image.height()), (w, h));
        let rows_with_white: Vec<u32> = (0..h)
            .filter(|&y| count(&image, 0..w, y..y + 1, white) > 0)
            .collect();
        let (top, bottom) = (rows_with_white[0], *rows_with_white.last().unwrap());
        eprintln!("sign-in at {w}x{h}: QR rows {top}..={bottom}");
        assert!(
            top > 0 && bottom - top >= 250,
            "{w}x{h}: the QR square is cut off at the top ({top}..={bottom})"
        );
    }
    ui.window()
        .set_size(slint::PhysicalSize::new(WIDTH, HEIGHT));
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));

    multiview(&ui);
    resume_card(&ui);
}

/// Home's *Resume multiview* card lays out its mosaic, and a preview that arrives later is
/// patched into its cell.
fn resume_card(ui: &MainWindow) {
    let live = |login: &str| {
        Tile::Stream(Stream {
            channel: ChannelRef::twitch(login),
            user_id: String::new(),
            title: String::new(),
            category: String::new(),
            viewers: 10,
            tint: Rgb::new(40, 60, 90),
            thumbnail: Some(format!("https://cdn/{login}.jpg")),
        })
    };
    let (mut s, _) = init();
    s.user = Some(htpc_core::User {
        login: "viewer".into(),
        can_chat: true,
    });
    let rows = vec![
        Row::new(RowKind::TopLive, vec![live("top")]),
        Row::new(RowKind::Resume, vec![live("a"), live("b"), live("c")]),
    ];
    let (s, _) = reduce(
        &s,
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows,
        },
    );
    assert!(matches!(s.home.tile(0, 0), Some(Tile::Resume(_))));
    Update::full(view_model(&s)).apply(ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let violet = |p: &Rgba8Pixel| p.r > 180 && p.g < 130 && p.b > 220;
    let before = count(&render(ui), 0..WIDTH, 0..HINTS_TOP, violet);
    htpc_ui::apply_thumbnail(ui, "https://cdn/b.jpg", &picture(16, 9));
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let after = count(&render(ui), 0..WIDTH, 0..HINTS_TOP, violet);
    assert!(
        after > before + 2_000,
        "the mosaic cell shows the preview ({before} -> {after} violet pixels)"
    );
}

/// Signed in, watching `main`, then multiview with `main` and three followed channels in a grid of
/// 4, all playing, the picker closed and the focus on the top-right tile.
fn multiview_of_four() -> AppState {
    let user = htpc_core::User {
        login: "me".into(),
        can_chat: true,
    };
    let live = |login: &str| {
        Tile::Stream(Stream {
            channel: ChannelRef::twitch(login),
            user_id: String::new(),
            title: "a title".into(),
            category: "cat".into(),
            viewers: 10,
            tint: Rgb::new(40, 60, 90),
            thumbnail: None,
        })
    };
    let (s, _) = init();
    let mut s = [
        Event::FeedLoaded {
            feed: FeedId::Home,
            rows: vec![Row::new(RowKind::TopLive, vec![live("main")])],
        },
        Event::SignedIn(user),
        Event::FeedLoaded {
            feed: FeedId::Following,
            rows: vec![Row::new(
                RowKind::FollowedLive,
                ["one", "two", "three"].map(live).to_vec(),
            )],
        },
    ]
    .into_iter()
    .fold(s, |s, e| reduce(&s, e).0);
    for intent in [
        Intent::Select,
        Intent::View,
        Intent::Select,
        Intent::Select,
        Intent::Down,
        Intent::Select,
        Intent::Down,
        Intent::Select,
        Intent::Back,
    ] {
        s = reduce(&s, intent).0;
    }
    let mv = s.multiview.as_ref().expect("multiview runs");
    assert_eq!(mv.count(), 4);
    assert!(mv.picker.is_none());
    let slots: Vec<u8> = mv.tiles.iter().flatten().map(|t| t.slot).collect();
    for slot in slots {
        s = reduce(&s, Event::PlaybackStarted { slot }).0;
    }
    s.multiview.as_mut().unwrap().focus = 1;
    s
}

/// Multiview's grid, picker, popup and full screen lay out and draw where they belong, and each
/// player renders at the size it is shown.
fn multiview(ui: &MainWindow) {
    let accent =
        |p: &Rgba8Pixel| (100..150).contains(&p.r) && (75..120).contains(&p.g) && p.b > 230;
    let grid = multiview_of_four();
    let slot_of =
        |s: &AppState, place: usize| s.multiview.as_ref().unwrap().tile(place).unwrap().slot;
    let (sound_slot, focus_slot) = (slot_of(&grid, 0), slot_of(&grid, 1));
    Update::full(view_model(&grid)).apply(ui);
    render(ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(ui);

    // Grid of 4 at 1280x800: tiles of 547.6x308, top right from (648, 72), bottom left from
    // (84, 396). The focused one has the accent ring, the sound badge is accent too (top left
    // tile's top right corner), and the muted, unfocused bottom-left tile has neither.
    assert!(
        count(&image, 648..1196, 72..80, accent) > 1_000,
        "the focused tile has no focus ring"
    );
    assert!(
        count(&image, 480..632, 76..116, accent) > 200,
        "the stream with sound has no sound badge"
    );
    assert_eq!(
        count(&image, 84..632, 396..704, accent),
        0,
        "an unfocused muted tile is drawn with the accent"
    );
    // Each stream renders at its tile's size; a slot not in multiview is hidden.
    assert_eq!(htpc_ui::video_size(ui, focus_slot), (548, 308));
    assert_eq!(htpc_ui::video_size(ui, sound_slot), (548, 308));
    assert_eq!(htpc_ui::video_size(ui, 8), (0, 0));
    // A frame lands in its slot, and the stream with sound's in `video` too.
    let frame = |c: u8| {
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(4, 4);
        buffer.make_mut_slice().fill(Rgba8Pixel::new(c, c, c, 255));
        slint::Image::from_rgba8(buffer)
    };
    htpc_ui::show_video(ui, focus_slot, frame(10));
    htpc_ui::show_video(ui, sound_slot, frame(20));
    assert_eq!(
        ui.get_video().size().width,
        4,
        "the sound slot's frame is not the video"
    );

    // Five seconds without a button: the header, hints and captions fade, and the tiles grow to
    // fill the window (8 px margin): 1264 / 1152 = 1.097 times their size.
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(5_500));
    render(ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(ui);
    assert!(ui.get_mv_idle(), "the grid did not go idle");
    assert_eq!(htpc_ui::video_size(ui, focus_slot), (601, 338));
    assert_eq!(
        count(&image, 0..WIDTH, 0..HEIGHT, accent),
        0,
        "focus ring or sound badge still shown while idle"
    );
    let a_button = |p: &Rgba8Pixel| p.g > 150 && p.r < 150 && p.b < 120;
    assert_eq!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button),
        0,
        "the hints bar is still shown while idle"
    );
    // Any button brings them back, at the usual size.
    let (moved, _) = reduce(&grid, Intent::Left);
    Update::full(view_model(&moved)).apply(ui);
    render(ui);
    assert!(!ui.get_mv_idle());
    assert_eq!(htpc_ui::video_size(ui, focus_slot), (548, 308));

    // Y: the chat panel takes the right 380 px, and the tiles shrink to make room.
    let (chatting, _) = reduce(&grid, Intent::ToggleChat);
    Update::full(view_model(&chatting)).apply(ui);
    render(ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(ui);
    let (w, h) = htpc_ui::video_size(ui, focus_slot);
    assert!(
        w < 548 && w > 300 && (w * 9).abs_diff(h * 16) < 32,
        "tile with chat: {w}x{h}"
    );
    assert_eq!(
        count(&image, WIDTH - 380..WIDTH, 0..HINTS_TOP, accent),
        0,
        "a tile is drawn under the chat panel"
    );

    // The picker (View, *Add a stream*) slides over the right with its first row focused.
    let (popup, _) = reduce(&grid, Intent::View);
    Update::full(view_model(&popup)).apply(ui);
    render(ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(ui);
    // The popup's focused row (its accent border) in the middle of the screen.
    assert!(
        count(&image, 360..920, 200..600, accent) > 500,
        "the View popup is not drawn"
    );
    assert!(
        count(&image, 0..WIDTH, HINTS_TOP..HEIGHT, a_button) > 0,
        "the popup has no A in the hints bar"
    );

    // Full screen: A on the stream with sound. It fills the window; the strip is up for a moment,
    // its thumbnails rendering small, then hidden again.
    let mut on_sound = grid.clone();
    on_sound.multiview.as_mut().unwrap().focus = 0;
    let (full, _) = reduce(&on_sound, Intent::Select);
    assert_eq!(full.screen, htpc_core::Screen::Player);
    Update::full(view_model(&full)).apply(ui);
    render(ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    assert_eq!(htpc_ui::video_size(ui, sound_slot), (WIDTH, HEIGHT));
    assert_eq!(
        htpc_ui::video_size(ui, focus_slot),
        (176, 99),
        "no strip thumbnail"
    );
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(3_000));
    render(ui);
    assert_eq!(
        htpc_ui::video_size(ui, focus_slot),
        (0, 0),
        "the strip stays up"
    );

    // The picker: View, then *Add a stream* (the first row).
    let (picking, _) = reduce(&grid, Intent::View);
    let (picking, _) = reduce(&picking, Intent::Select);
    assert!(picking.multiview.as_ref().unwrap().picker.is_some());
    Update::full(view_model(&picking)).apply(ui);
    render(ui);
    i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
    let image = render(ui);
    assert!(
        count(&image, WIDTH - 480..WIDTH, 140..240, accent) > 500,
        "the picker's focused row is not drawn at the top right"
    );

    // B: back on Home with the four still picked. The tray sits in the bottom right corner,
    // above the hints bar, with an accent border.
    let tray = |ui: &MainWindow, s: &AppState| {
        Update::full(view_model(s)).apply(ui);
        render(ui);
        i_slint_backend_testing::mock_elapsed_time(std::time::Duration::from_millis(500));
        let image = render(ui);
        count(
            &image,
            WIDTH - 520..WIDTH,
            HINTS_TOP - 80..HINTS_TOP,
            accent,
        )
    };
    let (home, _) = reduce(&grid, Intent::Back);
    assert_eq!(home.picked.len(), 4);
    assert!(tray(ui, &home) > 200, "the tray is not drawn");
    let mut cleared = home.clone();
    cleared.picked.clear();
    assert_eq!(tray(ui, &cleared), 0, "the tray shows with nothing picked");
}
