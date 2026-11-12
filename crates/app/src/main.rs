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
mod chat_limit;
mod hype_train;
mod kick;
mod live_watch;
mod mock;
mod playback;
mod power;
mod provider;
mod raids;
mod search;
mod session;
mod sleep;
mod store;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use htpc_chat::{ChatClient, IrcChat, KickChat};
use htpc_core::{Event, Intent, Msg, RailEntry};
#[cfg(not(feature = "mpv"))]
use htpc_player::UnavailablePlayer;
use htpc_player::{Player, PlayerEvent, PlayerPool, RefreshRate, Slot, MAIN_SLOT};
use htpc_resolver::{StreamResolver, StreamlinkResolver, TokioRunner};
use htpc_storage::TokenStore;
use htpc_twitch_api::{Credentials, FakeAuth, GqlClient, HelixClient, HttpAuth, HybridApi};
use htpc_twitch_api::{RefreshingApi, RetryPolicy};
use htpc_ui::{InputDevice, MainWindow, UiAction};
use slint::ComponentHandle;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::chat::Chat;
use crate::kick::KickSource;
use crate::playback::Playback;
use crate::power::Suspend;
use crate::provider::{ApiProvider, Provider};
use crate::raids::Raids;
use crate::session::{AppAuth, Apply, Notify, Session, Viewer, NO_CLIENT_ID};
use crate::sleep::Sleep;
use crate::store::{SaveFavorites, SaveMultiview, SaveSettings, Store};

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
  Chat is read anonymously, over IRC on a WebSocket (irc-ws.chat.twitch.tv), so it works
  signed out. Y shows or hides it. chat_layout in config.toml is `side` (a panel the video
  shrinks for) or `overlay` (lines over the video). Signed in, X (or Write in chat in the
  player menu) writes in chat with the on-screen keyboard, through Helix (user:write:chat).
  quick_messages in config.toml lists the ready-made messages offered there.

RAIDS:
  Signed in, the channel on screen is watched for raids over Twitch EventSub
  (eventsub.wss.twitch.tv). A raid opens a 10-second countdown to follow it.

HYPE TRAINS:
  The channel on screen is checked for a hype train over the website's GQL API
  (gql.twitch.tv), signed in or not: once a minute, every 10 s while one runs.

CONFIG:
  config.toml (chat_layout, quality, live_notifications, background_play, image_cache_max_mb,
  quick_messages, client_id) is
  created with defaults on the first run in the platform config folder
  (Linux: ~/.config/livedeck, Windows: %APPDATA%\\livedeck\\config).
  favorites.toml (the starred channels, written by the app) is in the data folder
  (Linux: ~/.local/share/livedeck, Windows: %APPDATA%\\livedeck\\data).

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
                favorites: Vec::new(),
                favorites_store: None,
                last_multiview: None,
                multiview_store: None,
            };
        }
    };
    let favorites_store = htpc_storage::FavoritesStore::new(paths.favorites_file());
    let favorites = favorites_store.load_or_default();
    tracing::info!(
        path = %favorites_store.path().display(),
        count = favorites.len(),
        "favorites loaded"
    );
    let multiview_store = htpc_storage::MultiviewStore::new(paths.multiview_file());
    let last_multiview = multiview_store.load_or_default();
    tracing::info!(
        path = %multiview_store.path().display(),
        streams = last_multiview.as_ref().map_or(0, |s| s.channels.len()),
        "last multiview session loaded"
    );
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
        favorites,
        favorites_store: Some(favorites_store),
        last_multiview,
        multiview_store: Some(multiview_store),
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
    /// The starred channels, from `favorites.toml`.
    favorites: Vec<htpc_core::ChannelRef>,
    /// Where they are written when they change.
    favorites_store: Option<htpc_storage::FavoritesStore>,
    /// The last multiview session, from `multiview.toml`.
    last_multiview: Option<htpc_core::SavedSet>,
    /// Where it is written when a running session changes.
    multiview_store: Option<htpc_storage::MultiviewStore>,
}

/// Saves the multiview set into `multiview.toml`.
fn multiview_saver(store: Option<htpc_storage::MultiviewStore>) -> Option<SaveMultiview> {
    let store = store?;
    Some(Arc::new(move |set: &htpc_core::SavedSet| {
        match store.save(set) {
            Ok(()) => tracing::debug!(streams = set.channels.len(), "multiview set saved"),
            Err(err) => tracing::warn!(%err, "could not save the multiview set"),
        }
    }))
}

/// Saves the favorites into `favorites.toml`.
fn favorites_saver(store: Option<htpc_storage::FavoritesStore>) -> Option<SaveFavorites> {
    let store = store?;
    Some(Arc::new(
        move |list: &[htpc_core::ChannelRef]| match store.save(list) {
            Ok(()) => tracing::debug!(count = list.len(), "favorites saved"),
            Err(err) => tracing::warn!(%err, "could not save the favorites"),
        },
    ))
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

/// How often players stopped for a minute are dropped (`PlayerPool::drop_idle`).
const IDLE_SWEEP: Duration = Duration::from_secs(15);

/// The players, one per slot: libmpv with the `mpv` feature, otherwise players that report "no
/// video support". Their events go to the store as `Event`s. The single player (slot 0) is made
/// at once, so a broken libmpv shows in the log at startup.
fn make_pool(ui: &MainWindow, tx: &UnboundedSender<Msg>, rate: RefreshRate) -> Arc<PlayerPool> {
    let on_event = {
        let tx = tx.clone();
        let clock = playback::Clock::start();
        move |slot: Slot, event: PlayerEvent| {
            if let Some(event) = playback::player_event(slot, event, clock.ms()) {
                let _ = tx.send(event.into());
            }
        }
    };
    #[cfg(feature = "mpv")]
    let pool = mpv_pool(ui, rate, on_event);
    #[cfg(not(feature = "mpv"))]
    let pool = {
        let _ = (ui, rate);
        PlayerPool::new(
            |_, _, report| {
                Ok(Arc::new(UnavailablePlayer::new(move |e| report(e))) as Arc<dyn Player>)
            },
            on_event,
        )
    };
    let pool = Arc::new(pool);
    pool.player(MAIN_SLOT);
    pool
}

/// A pool of mpv players, every slot's frames drawn by one `VideoOutputs`, each at the size the
/// window shows it (`htpc_ui::video_size`): the Player screen's video, a multiview tile, a strip
/// thumbnail, or hidden.
#[cfg(feature = "mpv")]
fn mpv_pool(
    ui: &MainWindow,
    rate: RefreshRate,
    on_event: impl Fn(Slot, PlayerEvent) + Send + Sync + 'static,
) -> PlayerPool {
    let size = ui.as_weak();
    let show = ui.as_weak();
    let redraw = ui.as_weak();
    let outputs = htpc_player::VideoOutputs::attach(
        ui.window(),
        move |slot| match size.upgrade() {
            Some(ui) => htpc_ui::video_size(&ui, slot),
            None => (0, 0),
        },
        move |slot, image| {
            if let Some(ui) = show.upgrade() {
                htpc_ui::show_video(&ui, slot, image);
            }
        },
        move |wait| {
            let _ = redraw.upgrade_in_event_loop(move |ui| request_redraw(&ui, wait));
        },
        rate,
    );
    let outputs = match outputs {
        Ok(outputs) => outputs,
        Err(err) => {
            let reason = format!("could not hook into the renderer: {err}");
            return PlayerPool::new(move |_, _, _| Err(reason.clone()), on_event);
        }
    };
    let promote = outputs.clone();
    PlayerPool::new(
        move |slot, seat, report| {
            let (player, output) = htpc_player::MpvPlayer::new(move |e| report(e))?;
            match seat {
                htpc_player::Seat::Main => outputs.insert(slot, output),
                htpc_player::Seat::Spare => outputs.insert_spare(slot, output),
            }
            Ok(Arc::new(player) as Arc<dyn Player>)
        },
        on_event,
    )
    // A new quality's first frame: its player is drawn in the slot from then on.
    .with_swap(move |slot| promote.promote(slot))
}

/// Redraws the window now, or after `wait` (video frames are paced to the display's rate).
#[cfg(feature = "mpv")]
fn request_redraw(ui: &MainWindow, wait: Duration) {
    if wait.is_zero() {
        ui.window().request_redraw();
        return;
    }
    let weak = ui.as_weak();
    slint::Timer::single_shot(wait, move || {
        if let Some(ui) = weak.upgrade() {
            ui.window().request_redraw();
        }
    });
}

/// Drops players that have been stopped for a minute, then redraws so their render contexts are
/// freed (that only happens while drawing).
fn sweep_idle_players(runtime: &tokio::runtime::Runtime, pool: &Arc<PlayerPool>, ui: &MainWindow) {
    let pool = Arc::clone(pool);
    let weak = ui.as_weak();
    runtime.spawn(async move {
        let mut tick = tokio::time::interval(IDLE_SWEEP);
        loop {
            tick.tick().await;
            if pool.drop_idle() > 0 {
                let _ = weak.upgrade_in_event_loop(|ui| ui.window().request_redraw());
            }
        }
    });
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
///
/// The store hears the window's physical size (`Event::DisplaySize`) once the window is up and on
/// every change, also with `SLINT_SCALE_FACTOR` set: multiview's tile quality comes from it.
fn wire_scaling(ui: &MainWindow, tx: &UnboundedSender<Msg>) {
    let fit = env_var("SLINT_SCALE_FACTOR").is_none();
    let reported = std::rc::Rc::new(std::cell::Cell::new((0, 0)));
    let on_change = {
        let weak = ui.as_weak();
        let tx = tx.clone();
        move || {
            let (weak, tx, reported) = (weak.clone(), tx.clone(), reported.clone());
            // Not from inside the property-change handler: the events below change the size again.
            slint::Timer::single_shot(Duration::ZERO, move || {
                if let Some(ui) = weak.upgrade() {
                    if fit {
                        fit_to_window(&ui);
                    }
                    report_display_size(&ui, &tx, &reported);
                }
            });
        }
    };
    // The first report, once the event loop runs and the window has its real size.
    on_change();
    ui.on_geometry_changed(on_change);
}

/// Sends `Event::DisplaySize` when the window's physical size differs from the last one sent.
fn report_display_size(
    ui: &MainWindow,
    tx: &UnboundedSender<Msg>,
    reported: &std::cell::Cell<(u32, u32)>,
) {
    let size = ui.window().size();
    let now = (size.width, size.height);
    if now == reported.get() || now.0 == 0 || now.1 == 0 {
        return;
    }
    reported.set(now);
    tracing::debug!(width = now.0, height = now.1, "display size");
    let _ = tx.send(Event::DisplaySize { w: now.0, h: now.1 }.into());
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
    /// Opens the EventSub connection that watches for raids. A fake that never raids in `--mock`.
    raid_connect: raids::Connect,
    /// The ready-made chat messages from `config.toml`.
    quick_messages: Vec<String>,
    /// The starred channels from `favorites.toml`. None in `--mock`.
    favorites: Vec<htpc_core::ChannelRef>,
    /// Writes them when they change. `None` in `--mock`.
    save_favorites: Option<SaveFavorites>,
    /// The last multiview session from `multiview.toml`. None in `--mock`.
    last_multiview: Option<htpc_core::SavedSet>,
    /// Writes it when a running session changes. `None` in `--mock`.
    save_multiview: Option<SaveMultiview>,
}

/// The real raid watch: EventSub with the session's token.
fn eventsub_connect(session: &Arc<Session>) -> raids::Connect {
    let session = Arc::clone(session);
    Arc::new(
        move |events| match htpc_twitch_api::EventSub::start(Arc::clone(&session), events) {
            Ok(sub) => Some(Box::new(sub) as Box<dyn htpc_twitch_api::RaidWatch>),
            Err(err) => {
                tracing::warn!(%err, "raids cannot be followed");
                None
            }
        },
    )
}

/// How often the session checks whether the access token is about to run out. It also notices a
/// wake from sleep within this time, which is when an old token is the likely state.
const RESUME_CHECK: Duration = Duration::from_secs(60);

/// How often the feed on screen reloads by itself, so a stream that went live shows up (the
/// reducer's `Event::FeedRefreshDue`).
const FEED_REFRESH: Duration = Duration::from_secs(60);

/// Spawns the store with `provider` and runs the window until it closes.
fn run_app<P: Provider, R: StreamResolver>(
    provider: P,
    resolver: R,
    chat: Arc<dyn ChatClient>,
    kick_chat: Arc<dyn ChatClient>,
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
        raid_connect,
        quick_messages,
        favorites,
        save_favorites,
        last_multiview,
        save_multiview,
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
    wire_scaling(&ui, &tx);
    if fullscreen {
        ui.window().set_fullscreen(true);
    }
    // Which device was used last, so the hints bar shows its buttons. Starts with the controller.
    let device = Arc::new(AtomicBool::new(true));
    wire_ui(&ui, &tx, &device);
    let rate = RefreshRate::new();
    wire_window_events(&ui, &tx, rate.clone());
    let pool = make_pool(&ui, &tx, rate);
    sweep_idle_players(&runtime, &pool, &ui);

    let gamepad_tx = tx.clone();
    let pad_ui = ui.as_weak();
    // Detached: the thread exits with the process or once the store channel closes.
    let _gamepad = htpc_input::spawn_gamepad_thread(move |intent| {
        note_device(&device, &pad_ui, true);
        send(&gamepad_tx, intent)
    });

    let (mut state, mut startup) = htpc_core::init();
    state.settings = settings;
    // Before the first Home load, so it already has (or has not) its Kick row.
    startup.insert(0, htpc_core::Command::SetKick(settings.show_kick));
    state.quick_messages = quick_messages;
    state.favorites = favorites;
    state.last_multiview = last_multiview;
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
    // Feeds reload on their own. The first tick is skipped: Home loads at start-up.
    let refresh = tx.clone();
    runtime.spawn(async move {
        let mut tick = tokio::time::interval(FEED_REFRESH);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            if refresh.send(Event::FeedRefreshDue.into()).is_err() {
                return;
            }
        }
    });
    let sleep = Sleep::new(pool.handle(MAIN_SLOT), power, tx.clone());
    let playback = Playback::new(resolver, pool);
    let chat = Chat::new(chat, kick_chat, tx.clone());
    let raids = Raids::new(raid_connect, tx.clone());
    let mut store = Store::new(state, provider, playback, chat, session, ui.as_weak(), tx)
        .with_sleep(sleep)
        .with_raids(raids);
    if let Some(save) = save_settings {
        store = store.with_settings_saver(save);
    }
    if let Some(save) = save_favorites {
        store = store.with_favorites_saver(save);
    }
    if let Some(save) = save_multiview {
        store = store.with_multiview_saver(save);
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

/// Tells the window which device was used last, for the hints bar's buttons or keys. Only when it
/// changes, so a controller does not queue an update for every press. Works from any thread.
fn note_device(last: &AtomicBool, ui: &slint::Weak<MainWindow>, controller: bool) {
    if last.swap(controller, Ordering::Relaxed) != controller {
        let _ = ui.upgrade_in_event_loop(move |ui| {
            ui.global::<InputDevice>().set_controller(controller);
        });
    }
}

/// Forwards window input (keys and mouse fallbacks) to the store.
fn wire_ui(ui: &MainWindow, tx: &UnboundedSender<Msg>, device: &Arc<AtomicBool>) {
    let t = tx.clone();
    let keys_ui = ui.as_weak();
    let keys_device = Arc::clone(device);
    ui.on_key(move |text, repeat| {
        // A held arrow is one press and one release, as on a controller: its auto-repeat is
        // taken and dropped here.
        if htpc_input::is_dropped_repeat(&text, repeat) {
            return true;
        }
        match htpc_input::map_key(&text) {
            Some(intent) => {
                note_device(&keys_device, &keys_ui, false);
                send(&t, intent)
            }
            None => false,
        }
    });

    let t = tx.clone();
    ui.on_key_released(move |text| match htpc_input::map_key_release(&text) {
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
            UiAction::PageRight => Intent::PageRight,
            UiAction::View => Intent::View,
        };
        send(&t, intent);
    });
}

/// Listens to the window's own events. Slint still handles every event as usual. Without the winit
/// backend nothing is reported: the stream keeps playing in the background, and video is paced
/// for 60 Hz.
///
/// - Focus: the store pauses the stream while another app is in front.
/// - The monitor's refresh rate, read whenever the window is shown, moved or resized (which is
///   also when it may have moved to another monitor). Video frames are paced to it.
fn wire_window_events(ui: &MainWindow, tx: &UnboundedSender<Msg>, rate: RefreshRate) {
    use slint::winit_030::{winit::event::WindowEvent, EventResult, WinitWindowAccessor};

    let tx = tx.clone();
    ui.window().on_winit_window_event(move |window, event| {
        match event {
            WindowEvent::Focused(focused) => {
                tracing::debug!(focused, "window focus changed");
                let _ = tx.send(Event::WindowFocus(*focused).into());
            }
            WindowEvent::Moved(_)
            | WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. } => {
                let monitor = window
                    .with_winit_window(|w| w.current_monitor()?.refresh_rate_millihertz())
                    .flatten();
                if let Some(mhz) = monitor.filter(|&mhz| mhz != rate.millihertz()) {
                    tracing::debug!(hz = f64::from(mhz) / 1000.0, "display refresh rate");
                    rate.set_millihertz(mhz);
                }
            }
            _ => {}
        }
        EventResult::Propagate
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
        favorites,
        favorites_store,
        last_multiview,
        multiview_store,
    } = load_storage();
    let settings = config.settings;
    let save_settings = settings_saver(&config, config_store);
    let save_favorites = favorites_saver(favorites_store);
    let save_multiview = multiview_saver(multiview_store);
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
        // Every live channel plays a generated test picture, so the player and multiview can be
        // tried without a network. The offline ones show the end screen.
        let resolver = mock::MockResolver;
        // Kick is offline fixtures too, while "Show Kick" is on.
        let provider = ApiProvider::new(api)
            .with_account(viewer)
            .with_kick(Arc::new(KickSource::new(mock::fake_kick())));
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
            // Nothing raids offline.
            raid_connect: Arc::new(|_| {
                Some(Box::new(htpc_twitch_api::FakeEventSub::new())
                    as Box<dyn htpc_twitch_api::RaidWatch>)
            }),
            quick_messages: config.quick_messages,
            // A fresh list each run, never written: the user's real favorites stay as they are.
            favorites: Vec::new(),
            save_favorites: None,
            // The same for the multiview set.
            last_multiview: None,
            save_multiview: None,
        };
        // Chat is scripted too: a few lines at once, then a new one every couple of seconds.
        let chat = Arc::new(mock::fake_chat(true));
        let kick_chat = Arc::new(mock::fake_chat(true));
        return run_app(provider, resolver, chat, kick_chat, settings, wiring);
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
    // Kick, signed out, through the website's API. Nothing is asked while "Show Kick" is off.
    let kick = htpc_kick_api::KickWebClient::new().context("failed to create the Kick client")?;
    let provider = ApiProvider::new(api)
        .with_images(images)
        .with_account(viewer)
        .with_kick(Arc::new(KickSource::new(kick.clone())));
    let raid_connect = eventsub_connect(&session);
    let wiring = Wiring {
        session,
        tx,
        rx,
        env_token,
        save_settings,
        fullscreen,
        power: Arc::new(power::SystemSuspend),
        raid_connect,
        quick_messages: config.quick_messages,
        favorites,
        save_favorites,
        last_multiview,
        save_multiview,
    };
    // Chat is anonymous and read-only, so it works signed out.
    run_app(
        provider,
        streamlink(),
        Arc::new(IrcChat::new()),
        Arc::new(KickChat::new(Arc::new(kick))),
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
