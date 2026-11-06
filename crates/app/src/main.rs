//! livedeck: wiring only. CLI flags, logging, tokio runtime, the store loop and the window.
//!
//! Threads: the main thread runs the Slint event loop. The store and all commands run on tokio
//! workers. Gamepad polling has its own thread. Input from every source becomes an `Intent` on
//! the store channel, and view-model updates come back via `slint::invoke_from_event_loop`.

// Release builds on Windows are GUI programs: launching them opens no console window. Debug builds
// keep the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod awake;
mod browser;
mod chat;
mod live_watch;
mod mock;
mod playback;
mod power;
mod provider;
mod search;
mod session;
mod sleep;
mod store;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use htpc_chat::{ChatClient, IrcChat};
use htpc_core::{Event, Intent, Msg, RailEntry};
use htpc_player::{Player, PlayerEvent, UnavailablePlayer};
use htpc_resolver::{ResolveError, StreamResolver, StreamlinkResolver, TokioRunner};
use htpc_storage::TokenStore;
use htpc_twitch_api::{Credentials, FakeAuth, GqlClient, HelixClient, HttpAuth, HybridApi};
use htpc_twitch_api::{RefreshingApi, RetryPolicy};
use htpc_ui::{MainWindow, UiAction};
use slint::ComponentHandle;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::chat::Chat;
use crate::playback::Playback;
use crate::power::Suspend;
use crate::provider::{ApiProvider, Provider};
use crate::session::{AppAuth, Apply, Notify, Session, Viewer, NO_CLIENT_ID};
use crate::sleep::Sleep;
use crate::store::{SaveSettings, Store};

const HELP: &str = "\
livedeck: couch-first, unofficial Twitch client for SteamOS

USAGE:
  livedeck [--mock] [--fullscreen | --windowed]

FLAGS:
  --mock        Use offline fixture data instead of the Twitch API
  --fullscreen  Start full screen (the default in Steam's Game Mode)
  --windowed    Start in a window, also in Game Mode
  -h, --help    Print this help

DISPLAY:
  The layout is drawn for 1280x800 and scaled to fit the window. SLINT_SCALE_FACTOR
  sets a fixed scale instead.

DATA SOURCE (without --mock):
  Signed out, the app browses anonymously through Twitch's unofficial web GQL API
  (no account needed, first page of each row only). Signing in (rail, Sign in) uses
  the official Helix API, with the Following tab. The app has its own public client ID
  built in, so there is nothing to set up. A fork can use its own Twitch application
  (a public client, with the Device Code flow):
  TWITCH_CLIENT_ID      Override the client ID (or `client_id` in config.toml)
  TWITCH_ACCESS_TOKEN   Optional, for development: use this access token instead of
                        signing in. It is never stored.
  TWITCH_API_BASE_URL   Helix base URL override (default https://api.twitch.tv/helix)
  TWITCH_AUTH_BASE_URL  OAuth base URL override (default https://id.twitch.tv/oauth2)

PLAYBACK:
  Streams are resolved with Streamlink. The app uses a copy shipped next to it
  (streamlink/bin/, streamlink/ or bin/), otherwise `streamlink` from PATH.
  HTPC_STREAMLINK       Path of the streamlink program (overrides both)
  HTPC_MPV_LOG          Write an mpv log to this file (builds with the `mpv` feature)
  Video needs a build with `--features mpv` and libmpv installed.

CHAT:
  Chat is anonymous and read-only, over IRC on a WebSocket (irc-ws.chat.twitch.tv), so it
  works signed out. Y shows or hides it. chat_layout in config.toml is `side` (a panel the
  video shrinks for) or `overlay` (lines over the video).

CONFIG:
  config.toml (chat_layout, quality, live_notifications, image_cache_max_mb, client_id) is
  created with defaults on the first run in the platform config folder
  (Linux: ~/.config/livedeck, Windows: %APPDATA%\\livedeck\\config).

Logging: set RUST_LOG, e.g. RUST_LOG=livedeck=debug
";

const ENV_CLIENT_ID: &str = "TWITCH_CLIENT_ID";
const ENV_ACCESS_TOKEN: &str = "TWITCH_ACCESS_TOKEN";
const ENV_BASE_URL: &str = "TWITCH_API_BASE_URL";
const ENV_AUTH_URL: &str = "TWITCH_AUTH_BASE_URL";
const ENV_STREAMLINK: &str = "HTPC_STREAMLINK";

#[derive(Debug)]
struct Args {
    mock: bool,
    /// `Some` when a flag chose; `None` leaves it to `want_fullscreen`.
    fullscreen: Option<bool>,
}

fn parse_args() -> Result<Option<Args>> {
    let mut args = pico_args::Arguments::from_env();
    if args.contains(["-h", "--help"]) {
        print!("{HELP}");
        return Ok(None);
    }
    let mock = args.contains("--mock");
    let fullscreen = match (args.contains("--fullscreen"), args.contains("--windowed")) {
        (true, true) => anyhow::bail!("--fullscreen and --windowed cannot be used together"),
        (true, false) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    };
    let parsed = Args { mock, fullscreen };
    let rest = args.finish();
    if !rest.is_empty() {
        anyhow::bail!("unexpected arguments: {rest:?}\n\n{HELP}");
    }
    Ok(Some(parsed))
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

/// A non-empty env var, or `None`.
fn env_var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// The public client ID of the Twitch application: `TWITCH_CLIENT_ID`, or failing that
/// `client_id` in `config.toml`, or failing that the one built into the app.
fn client_id(config_client_id: Option<String>) -> Option<String> {
    pick_client_id(
        env_var(ENV_CLIENT_ID),
        config_client_id,
        Some(DEFAULT_CLIENT_ID),
    )
}

/// The client ID of this project's Twitch application (a public client, so there is no secret to
/// keep). A client ID is a public identifier: it is in every login URL, and every Twitch app ships
/// one. Having it here is what lets a user sign in without setting anything up.
///
/// A fork or a repackaged build should register its own application and replace this, or set
/// `TWITCH_CLIENT_ID` / `client_id` in `config.toml`: apps sharing one client ID share its limits
/// and risk its suspension.
const DEFAULT_CLIENT_ID: &str = "b4d5txrh32mzg91jwrc0z79zk3myyd";

/// Environment first, then `config.toml` (both are for developers, or for someone who wants to
/// use their own Twitch application), then the one built into the app. Blank values count as
/// unset.
fn pick_client_id(
    env: Option<String>,
    config: Option<String>,
    built_in: Option<&str>,
) -> Option<String> {
    let usable = |id: &str| (!id.trim().is_empty()).then(|| id.trim().to_owned());
    env.as_deref()
        .and_then(usable)
        .or_else(|| config.as_deref().and_then(usable))
        .or_else(|| built_in.and_then(usable))
}

/// A user or app token from the environment (a developer affordance, never stored). It needs a
/// client ID, because Helix wants both.
fn env_token(client_id: Option<&str>) -> Result<Option<String>> {
    match (env_var(ENV_ACCESS_TOKEN), client_id) {
        // Accept tokens copied with their `oauth:` / `Bearer ` prefix.
        (Some(token), Some(_)) => Ok(Some(
            token
                .trim_start_matches("oauth:")
                .trim_start_matches("Bearer ")
                .to_owned(),
        )),
        // Half a configuration is almost certainly a mistake, so do not silently go anonymous.
        (Some(_), None) => anyhow::bail!("{ENV_ACCESS_TOKEN} is set but {ENV_CLIENT_ID} is not"),
        (None, _) => Ok(None),
    }
}

/// The Helix client. It starts without credentials: the session installs them once there is a
/// login (or a token from the environment), and until then `HybridApi` browses anonymously.
fn helix_client() -> Result<HelixClient> {
    let client = match env_var(ENV_BASE_URL) {
        Some(url) => HelixClient::with_base_url(url, None, RetryPolicy::default()),
        None => HelixClient::new(None),
    };
    client.context("failed to create the Twitch API client")
}

/// The OAuth client for the device-code login.
fn auth_client(client_id: &str) -> Result<HttpAuth> {
    let auth = match env_var(ENV_AUTH_URL) {
        Some(url) => HttpAuth::with_base_url(url, client_id),
        None => HttpAuth::new(client_id),
    };
    auth.context("failed to create the Twitch sign-in client")
}

/// Loads `config.toml` (writing defaults on the first run so it can be edited) and opens the
/// image cache, which evicts down to the configured cap. Storage problems never stop the app:
/// they are logged and defaults are used, and without a cache cards keep their tint.
fn load_storage() -> Storage {
    let paths = match htpc_storage::Paths::from_os() {
        Ok(paths) => paths,
        Err(err) => {
            tracing::warn!(%err, "no storage folders, settings and sign-in will not persist");
            return Storage {
                config: htpc_storage::Config::default(),
                images: None,
                tokens: None,
                config_store: None,
            };
        }
    };
    let store = htpc_storage::ConfigStore::new(paths.config_file());
    let config = store.load_or_default();
    if !store.path().exists() {
        if let Err(err) = store.save(&config) {
            tracing::warn!(%err, "could not write the default config file");
        }
    }
    tracing::info!(path = %store.path().display(), settings = ?config.settings, "config loaded");

    // Opening enforces the size cap on old entries.
    let cache = match htpc_storage::ImageCache::open(
        &paths.image_cache_dir(),
        config.image_cache_max_bytes(),
    ) {
        Ok(cache) => {
            tracing::debug!(stats = ?cache.stats(), "image cache ready");
            Some(cache)
        }
        Err(err) => {
            tracing::warn!(%err, "image cache unavailable, thumbnails are off");
            None
        }
    };
    // The OS keyring first, a private file in the data folder if there is none (Game Mode).
    let tokens = Arc::new(TokenStore::new(paths.token_file()));
    Storage {
        config,
        images: cache,
        tokens: Some(tokens),
        config_store: Some(store),
    }
}

/// What `load_storage` opens. Everything but the config is `None` when its folder cannot be used.
struct Storage {
    config: htpc_storage::Config,
    /// The thumbnail cache.
    images: Option<htpc_storage::ImageCache>,
    /// The sign-in token store.
    tokens: Option<Arc<TokenStore>>,
    /// Where settings changed on the Settings screen are written.
    config_store: Option<htpc_storage::ConfigStore>,
}

/// Saves settings into `config.toml`, keeping the file's other values (client ID, cache size).
fn settings_saver(
    config: &htpc_storage::Config,
    store: Option<htpc_storage::ConfigStore>,
) -> Option<SaveSettings> {
    let store = store?;
    let base = config.clone();
    Some(Arc::new(move |settings: htpc_core::Settings| {
        let config = htpc_storage::Config {
            settings,
            ..base.clone()
        };
        match store.save(&config) {
            Ok(()) => tracing::debug!(?settings, "settings saved"),
            Err(err) => tracing::warn!(%err, "could not save the settings"),
        }
    }))
}

/// The player: libmpv with the `mpv` feature, otherwise one that reports "no video support".
/// Its events go to the store as `Event`s.
fn make_player(ui: &MainWindow, tx: &UnboundedSender<Msg>) -> Arc<dyn Player> {
    let report = {
        let tx = tx.clone();
        move |event: PlayerEvent| {
            if let Some(event) = playback::player_event(event) {
                let _ = tx.send(event.into());
            }
        }
    };
    #[cfg(feature = "mpv")]
    let unavailable = match attach_mpv(ui, report.clone()) {
        Ok(player) => return player,
        Err(err) => {
            tracing::error!(%err, "video is off");
            // Shown on the player's error screen, so the real cause is visible without a log.
            UnavailablePlayer::with_reason(report, format!("video could not start: {err:#}"))
        }
    };
    #[cfg(not(feature = "mpv"))]
    let unavailable = {
        let _ = ui;
        UnavailablePlayer::new(report)
    };
    Arc::new(unavailable)
}

/// Starts mpv and connects its frames to the window's `video` image.
#[cfg(feature = "mpv")]
fn attach_mpv(
    ui: &MainWindow,
    report: impl Fn(PlayerEvent) + Send + Clone + 'static,
) -> Result<Arc<dyn Player>> {
    let (player, output) = htpc_player::MpvPlayer::new(report).map_err(anyhow::Error::msg)?;
    let size = ui.as_weak();
    let show = ui.as_weak();
    let redraw = ui.as_weak();
    output
        .attach(
            ui.window(),
            move || match size.upgrade() {
                // The video area in physical pixels. Nothing is drawn off the Player screen.
                Some(ui) if ui.get_screen() == htpc_ui::ScreenId::Player as i32 => {
                    let scale = ui.window().scale_factor();
                    let px = |len: f32| (len * scale).round().max(0.0) as u32;
                    (px(ui.get_video_w()), px(ui.get_video_h()))
                }
                _ => (0, 0),
            },
            move |image| {
                if let Some(ui) = show.upgrade() {
                    ui.set_video(image);
                }
            },
            move || {
                let _ = redraw.upgrade_in_event_loop(|ui| ui.window().request_redraw());
            },
        )
        .map_err(|e| anyhow::anyhow!("could not hook into the renderer: {e}"))?;
    Ok(Arc::new(player))
}

/// Full screen when a flag says so, otherwise in Steam's Game Mode (Steam sets `SteamGamepadUI`
/// for what it launches there).
fn want_fullscreen(flag: Option<bool>) -> bool {
    flag.unwrap_or_else(|| env_var("SteamGamepadUI").is_some_and(|v| v != "0"))
}

/// Keeps the 1280x800 layout fitted to the window: on every size change the scale factor becomes
/// `htpc_ui::fit_scale` of the window's physical size. The system's own factor can be far off (X11
/// guesses about 2x on the Deck's screen), which pushed the hints bar off the bottom.
/// `SLINT_SCALE_FACTOR` turns this off.
fn wire_scaling(ui: &MainWindow) {
    if env_var("SLINT_SCALE_FACTOR").is_some() {
        return;
    }
    let weak = ui.as_weak();
    ui.on_geometry_changed(move || {
        let weak = weak.clone();
        // Not from inside the property-change handler: the events below change the size again.
        slint::Timer::single_shot(Duration::ZERO, move || {
            if let Some(ui) = weak.upgrade() {
                fit_to_window(&ui);
            }
        });
    });
}

fn fit_to_window(ui: &MainWindow) {
    use slint::platform::WindowEvent;

    let window = ui.window();
    let size = window.size();
    let Some(scale) = htpc_ui::fit_scale(size.width, size.height) else {
        return;
    };
    let logical = slint::LogicalSize::new(size.width as f32 / scale, size.height as f32 / scale);
    let close = |a: f32, b: f32| (a - b).abs() < 0.5;
    if (window.scale_factor() - scale).abs() < 0.001
        && close(ui.get_window_w(), logical.width)
        && close(ui.get_window_h(), logical.height)
    {
        return;
    }
    tracing::debug!(
        width = size.width,
        height = size.height,
        system = window.scale_factor(),
        scale,
        "fitting the layout to the window"
    );
    // Only Slint's idea of the window changes. The OS window keeps its size.
    window.dispatch_event(WindowEvent::ScaleFactorChanged {
        scale_factor: scale,
    });
    window.dispatch_event(WindowEvent::Resized { size: logical });
}

/// The pieces `main` builds before the window exists: the session (login, tokens, refresh) and
/// the store's message channel, which the session reports into.
struct Wiring {
    session: Arc<Session>,
    tx: UnboundedSender<Msg>,
    rx: UnboundedReceiver<Msg>,
    /// A token from the environment, adopted at startup instead of a stored login.
    env_token: Option<String>,
    /// Writes the settings changed on the Settings screen. `None` in `--mock`.
    save_settings: Option<SaveSettings>,
    /// Start full screen (see `want_fullscreen`).
    fullscreen: bool,
    /// What the sleep timer suspends with. Only logs in `--mock`.
    power: Arc<dyn Suspend>,
}

/// How often the session checks whether the access token is about to run out. It also notices a
/// wake from sleep within this time, which is when an old token is the likely state.
const RESUME_CHECK: Duration = Duration::from_secs(60);

/// Spawns the store with `provider` and runs the window until it closes.
fn run_app<P: Provider, R: StreamResolver>(
    provider: P,
    resolver: R,
    chat: Arc<dyn ChatClient>,
    settings: htpc_core::Settings,
    wiring: Wiring,
) -> Result<()> {
    let Wiring {
        session,
        tx,
        rx,
        env_token,
        save_settings,
        fullscreen,
        power,
    } = wiring;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("htpc-worker")
        .build()
        .context("failed to start the tokio runtime")?;

    // The mpv texture path needs an OpenGL renderer (FemtoVG), never the software renderer.
    #[cfg(feature = "mpv")]
    slint::BackendSelector::new()
        .require_opengl()
        .select()
        .context("video needs an OpenGL renderer")?;

    let ui = MainWindow::new().context("failed to create the window")?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());
    wire_scaling(&ui);
    if fullscreen {
        ui.window().set_fullscreen(true);
    }
    wire_ui(&ui, &tx);
    let player = make_player(&ui, &tx);

    let gamepad_tx = tx.clone();
    // Detached: the thread exits with the process or once the store channel closes.
    let _gamepad = htpc_input::spawn_gamepad_thread(move |intent| send(&gamepad_tx, intent));

    let (mut state, mut startup) = htpc_core::init();
    state.settings = settings;
    state.can_open_browser = browser::can_open_browser();
    // The account is in place before the first load, so Home already has its Followed row. A
    // token from the environment wins over a stored login.
    state.user = match env_token {
        Some(token) => runtime.block_on(session.adopt(token)),
        None => session.restore(),
    };
    // A restored sign-in watches for followed channels going live from the start. From here on
    // the reducer starts and stops the watch.
    if state.wants_live_watch() {
        startup.push(htpc_core::Command::StartLiveWatch);
    }
    // Renews a token that is about to run out, at startup and again every minute.
    let resume = Arc::clone(&session);
    runtime.spawn(async move {
        let mut tick = tokio::time::interval(RESUME_CHECK);
        loop {
            tick.tick().await;
            resume.resume().await;
        }
    });
    let sleep = Sleep::new(Arc::clone(&player), power, tx.clone());
    let playback = Playback::new(resolver, player);
    let chat = Chat::new(chat, tx.clone());
    let mut store =
        Store::new(state, provider, playback, chat, session, ui.as_weak(), tx).with_sleep(sleep);
    if let Some(save) = save_settings {
        store = store.with_settings_saver(save);
    }
    // Stops the screen dimming, locking or sleeping mid-stream.
    if let Some(keep_awake) = awake::keep_awake() {
        store = store.with_keep_awake(keep_awake);
    }
    runtime.spawn(store.run(startup, rx));

    ui.run().context("UI event loop failed")?;

    // The store loop runs forever, so stop the runtime explicitly.
    runtime.shutdown_timeout(Duration::from_millis(500));
    Ok(())
}

/// Sends an intent to the store. Returns false once the store is gone.
fn send(tx: &UnboundedSender<Msg>, intent: Intent) -> bool {
    tx.send(intent.into()).is_ok()
}

/// Forwards window input (keys and mouse fallbacks) to the store.
fn wire_ui(ui: &MainWindow, tx: &UnboundedSender<Msg>) {
    let t = tx.clone();
    ui.on_key(move |text| match htpc_input::map_key(&text) {
        Some(intent) => send(&t, intent),
        None => false,
    });

    let t = tx.clone();
    ui.on_card_clicked(move |row, col| {
        if let (Ok(row), Ok(col)) = (usize::try_from(row), usize::try_from(col)) {
            send(&t, Intent::Pick { row, col });
        }
    });

    let t = tx.clone();
    ui.on_rail_clicked(move |index| {
        if let Some(entry) = usize::try_from(index).ok().and_then(RailEntry::from_index) {
            send(&t, Intent::RailPick(entry));
        }
    });

    let t = tx.clone();
    ui.on_action(move |action| {
        let intent = match action {
            UiAction::Select => Intent::Select,
            UiAction::Back => Intent::Back,
            UiAction::ToggleChat => Intent::ToggleChat,
            UiAction::Menu => Intent::Menu,
            UiAction::Retry => Intent::Retry,
        };
        send(&t, intent);
    });
}

/// A GUI-subsystem program has no console, so `--help` and the logs would go nowhere. When it was
/// started from a terminal, this connects to that terminal's console and points stdout/stderr at it.
/// Started by double-click there is no parent console, and nothing happens.
#[cfg(all(windows, not(debug_assertions)))]
fn attach_parent_console() {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn SetStdHandle(std_handle: u32, handle: *mut c_void) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;

    // SAFETY: plain Win32 calls with valid arguments. The console handle is leaked on purpose, as
    // stdout and stderr use it for the rest of the process.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        if let Ok(console) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            let handle = console.as_raw_handle().cast::<c_void>();
            SetStdHandle(STD_OUTPUT_HANDLE, handle);
            SetStdHandle(STD_ERROR_HANDLE, handle);
            std::mem::forget(console);
        }
    }
}

fn main() -> Result<()> {
    #[cfg(all(windows, not(debug_assertions)))]
    attach_parent_console();
    let Some(args) = parse_args()? else {
        return Ok(());
    };
    init_logging();
    let Storage {
        config,
        images,
        tokens,
        config_store,
    } = load_storage();
    let settings = config.settings;
    let save_settings = settings_saver(&config, config_store);
    let fullscreen = want_fullscreen(args.fullscreen);
    let (tx, rx) = unbounded_channel::<Msg>();
    let notify: Notify = {
        let tx = tx.clone();
        Arc::new(move |event: Event| {
            // The store is gone at shutdown, when nobody needs the answer.
            let _ = tx.send(event.into());
        })
    };
    let viewer = Viewer::default();

    if args.mock {
        // The fixtures carry no image URLs (offline), so mock cards keep their tints. Sign-in is
        // scripted too: the code shows for two seconds, then the user "approves" it.
        tracing::info!("--mock: using offline fixture data");
        // `FakeApi` knows the mock user from the start. Without an account the Followed row
        // would be empty, so the fake only reveals it after a (mock) sign-in.
        let api = mock::fake_api();
        let apply: Apply = Arc::new(|_| {});
        // The mock never stores tokens: a restart is signed out, like a fresh install.
        let session = Session::new(
            AppAuth::Fake(FakeAuth::approving()),
            "mock-client",
            None,
            apply,
            notify,
            Arc::clone(&viewer),
        );
        // There is no stream to play offline, so every channel shows the end screen.
        let resolver = htpc_resolver::FakeResolver::failing(ResolveError::Offline);
        let provider = ApiProvider::new(api).with_account(viewer);
        let wiring = Wiring {
            session,
            tx,
            rx,
            env_token: None,
            // The mock must not overwrite the user's real config.
            save_settings: None,
            fullscreen,
            // Nor put the developer's machine to sleep.
            power: Arc::new(power::LogOnly),
        };
        // Chat is scripted too: a few lines at once, then a new one every couple of seconds.
        let chat = Arc::new(mock::fake_chat(true));
        return run_app(provider, resolver, chat, settings, wiring);
    }

    let client_id = client_id(config.client_id);
    let env_token = env_token(client_id.as_deref())?;
    let gql = GqlClient::new().context("failed to create the GQL client")?;
    let helix = helix_client()?;
    // Signed out, `HybridApi` browses over GQL. Once the session installs credentials
    // it switches to Helix, and a 401 on an expired token is repaired by the session.
    let api = HybridApi::new(gql, helix);
    let auth = match &client_id {
        Some(id) => AppAuth::Real(auth_client(id)?),
        None => AppAuth::Unavailable(NO_CLIENT_ID.to_owned()),
    };
    let apply: Apply = {
        let api = api.clone();
        Arc::new(move |creds: Option<Credentials>| api.set_credentials(creds))
    };
    let session = Session::new(
        auth,
        client_id.unwrap_or_default(),
        tokens,
        apply,
        notify,
        Arc::clone(&viewer),
    );
    tracing::info!("browsing anonymously until there is a sign-in");
    let api = RefreshingApi::new(api, Arc::clone(&session));
    let provider = ApiProvider::new(api)
        .with_images(images)
        .with_account(viewer);
    let wiring = Wiring {
        session,
        tx,
        rx,
        env_token,
        save_settings,
        fullscreen,
        power: Arc::new(power::SystemSuspend),
    };
    // Chat is anonymous and read-only, so it works signed out.
    run_app(
        provider,
        streamlink(),
        Arc::new(IrcChat::new()),
        settings,
        wiring,
    )
}

/// Where a Streamlink shipped with the app may sit, relative to the folder of the executable.
/// A release package puts a portable Streamlink here, so users install nothing.
fn bundled_streamlink_candidates(app_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let exe = format!("streamlink{}", std::env::consts::EXE_SUFFIX);
    [
        ["streamlink", "bin"].as_slice(),
        ["streamlink"].as_slice(),
        ["bin"].as_slice(),
        [].as_slice(),
    ]
    .iter()
    .map(|dirs| {
        let mut path = app_dir.to_path_buf();
        path.extend(*dirs);
        path.join(&exe)
    })
    .collect()
}

/// The first bundled Streamlink that exists (`exists` is a parameter so tests need no files).
fn find_bundled_streamlink(
    app_dir: &std::path::Path,
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<std::path::PathBuf> {
    bundled_streamlink_candidates(app_dir)
        .into_iter()
        .find(|p| exists(p))
}

/// Streamlink: the program named by `HTPC_STREAMLINK`, else a copy shipped next to the
/// executable, else `streamlink` from `PATH`.
fn streamlink() -> StreamlinkResolver<TokioRunner> {
    let resolver = StreamlinkResolver::new(TokioRunner);
    if let Some(path) = env_var(ENV_STREAMLINK) {
        return resolver.with_program(path);
    }
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf))
        .and_then(|dir| find_bundled_streamlink(&dir, |p| p.is_file()));
    match bundled {
        Some(path) => {
            tracing::info!(path = %path.display(), "using the bundled Streamlink");
            resolver.with_program(path.to_string_lossy().into_owned())
        }
        None => resolver,
    }
}

#[cfg(test)]
mod tests {
    use super::{find_bundled_streamlink, pick_client_id, DEFAULT_CLIENT_ID};
    use std::path::{Path, PathBuf};

    fn exe() -> String {
        format!("streamlink{}", std::env::consts::EXE_SUFFIX)
    }

    #[test]
    fn a_streamlink_next_to_the_app_is_found() {
        let dir = Path::new("app");
        let want: PathBuf = dir.join("streamlink").join("bin").join(exe());
        let found = find_bundled_streamlink(dir, |p| p == want);
        assert_eq!(found, Some(want));
    }

    #[test]
    fn the_folder_layout_is_searched_before_a_loose_binary() {
        let dir = Path::new("app");
        let found = find_bundled_streamlink(dir, |_| true).unwrap();
        assert_eq!(found, dir.join("streamlink").join("bin").join(exe()));
        let loose = dir.join(exe());
        assert_eq!(find_bundled_streamlink(dir, |p| p == loose), Some(loose));
    }

    #[test]
    fn no_bundled_streamlink_means_fall_back_to_path() {
        assert_eq!(find_bundled_streamlink(Path::new("app"), |_| false), None);
    }

    #[test]
    fn the_shipped_client_id_is_usable_and_wins_over_nothing() {
        assert!(!DEFAULT_CLIENT_ID.trim().is_empty());
        assert_eq!(
            pick_client_id(None, None, Some(DEFAULT_CLIENT_ID)).as_deref(),
            Some(DEFAULT_CLIENT_ID)
        );
    }

    #[test]
    fn the_built_in_id_is_used_when_nothing_overrides_it() {
        assert_eq!(
            pick_client_id(None, None, Some("built-in")).as_deref(),
            Some("built-in")
        );
    }

    #[test]
    fn the_environment_beats_the_config_which_beats_the_built_in_id() {
        let pick = |env: Option<&str>, config: Option<&str>| {
            pick_client_id(
                env.map(str::to_owned),
                config.map(str::to_owned),
                Some("built-in"),
            )
        };
        assert_eq!(pick(Some("env"), Some("cfg")).as_deref(), Some("env"));
        assert_eq!(pick(None, Some("cfg")).as_deref(), Some("cfg"));
        assert_eq!(pick(None, None).as_deref(), Some("built-in"));
    }

    #[test]
    fn blank_values_do_not_hide_the_next_source() {
        let id = pick_client_id(Some("  ".into()), Some(String::new()), Some(" built-in "));
        assert_eq!(id.as_deref(), Some("built-in"));
    }

    #[test]
    fn no_source_means_no_id() {
        assert_eq!(pick_client_id(None, None, None), None);
        assert_eq!(pick_client_id(None, None, Some("")), None);
    }
}
