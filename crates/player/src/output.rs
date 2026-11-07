//! The render half of the players: one Slint rendering notifier draws every slot's mpv frames,
//! each into its own texture at that slot's pixel size.
//!
//! Slint allows one rendering notifier per window, so [`VideoOutputs`] owns it and keeps a render
//! context (`Gpu`) per slot. Render contexts are made and freed only inside the notifier, on the
//! UI thread with the GL context current, as mpv's `render.h` requires.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use libmpv2::Mpv;
use slint::{GraphicsAPI, RenderingState};

use crate::gpu::Gpu;
use crate::pace::{Pacer, RefreshRate};
use crate::{Slot, SLOTS};

/// What a hidden slot (size (0, 0), for example a tile behind full screen) renders into. mpv's
/// video output only advances while frames are rendered, so a slot that is not drawn at all could
/// stall. Rendered this small it keeps playing and costs next to nothing (plan.md, phase 0).
const HIDDEN: (u32, u32) = (16, 9);

/// One player's video, made with it by `MpvPlayer::new`. Hand it to [`VideoOutputs::insert`].
pub struct VideoOutput {
    mpv: Arc<Mpv>,
    /// Gone once the player is dropped.
    alive: Weak<()>,
    /// Set once the player's render context exists. mpv opens its video output when a stream
    /// loads, and without a render context then it plays the stream with no picture at all, so
    /// the player holds a load until this is set.
    ready: Arc<AtomicBool>,
}

impl VideoOutput {
    pub(crate) fn new(mpv: Arc<Mpv>, alive: Weak<()>, ready: Arc<AtomicBool>) -> Self {
        Self { mpv, alive, ready }
    }

    fn alive(&self) -> bool {
        self.alive.strong_count() > 0
    }
}

/// A slot as the notifier sees it.
struct Drawn {
    output: VideoOutput,
    gpu: Option<Gpu>,
    /// Making the render context failed. Not tried again for this player.
    failed: bool,
}

impl Drawn {
    fn new(output: VideoOutput) -> Self {
        Self {
            output,
            gpu: None,
            failed: false,
        }
    }

    /// Frees the render context (GL must be current), then the mpv core off the UI thread, as
    /// `mpv_terminate_destroy` can take a moment.
    fn release(self, slot: usize) {
        drop(self.gpu);
        let output = self.output;
        let spawned = std::thread::Builder::new()
            .name("mpv-release".into())
            .spawn(move || drop(output));
        if let Err(e) = spawned {
            tracing::warn!(slot, %e, "could not free the mpv core in the background");
        }
        tracing::debug!(slot, "video output released");
    }
}

/// What the notifier does with its slots at the next frame.
enum Change {
    /// Draw this output in the slot, replacing what it had.
    Main(Slot, VideoOutput),
    /// Draw this output hidden, as the slot's spare (`PlayerPool::spare`).
    Spare(Slot, VideoOutput),
    /// The slot's spare took over: show it in the slot from now on.
    Promote(Slot),
}

/// Changes handed over by [`VideoOutputs`] that the notifier has not picked up yet, in order.
type Pending = Arc<Mutex<Vec<Change>>>;

/// Each slot's shown output and its hidden spare.
#[derive(Default)]
struct Slots {
    main: [Option<Drawn>; SLOTS],
    spare: [Option<Drawn>; SLOTS],
}

/// Draws every slot's video. Cheap to clone, and usable from any thread.
#[derive(Clone)]
pub struct VideoOutputs {
    pending: Pending,
    pacer: Arc<Pacer>,
}

impl VideoOutputs {
    /// Takes over `window`'s rendering notifier. Call on the UI thread, before the event loop
    /// runs. Needs an OpenGL renderer.
    ///
    /// - `size` gives a slot's video area in physical pixels. mpv renders at that size. (0, 0)
    ///   means hidden: the slot keeps playing but is not shown.
    /// - `show` receives a slot's new frame as an image. Runs on the UI thread.
    /// - `redraw` asks the window to redraw after the given delay (for example with
    ///   `Weak::upgrade_in_event_loop` and a `slint::Timer`). Called from mpv threads, so it must
    ///   not block or call mpv. Video asks at most once per refresh of `rate`.
    pub fn attach(
        window: &slint::Window,
        size: impl Fn(Slot) -> (u32, u32) + 'static,
        show: impl Fn(Slot, slint::Image) + 'static,
        redraw: impl Fn(Duration) + Send + Sync + 'static,
        rate: RefreshRate,
    ) -> Result<Self, slint::SetRenderingNotifierError> {
        let outputs = Self {
            pending: Arc::default(),
            pacer: Arc::new(Pacer::new(rate, redraw)),
        };
        let pending = Arc::clone(&outputs.pending);
        let pacer = Arc::clone(&outputs.pacer);
        let mut slots = Slots::default();
        window.set_rendering_notifier(move |state, api| match state {
            RenderingState::BeforeRendering => {
                pacer.drawing();
                take_pending(&pending, &mut slots);
                let Slots { main, spare } = &mut slots;
                for (shown, list) in [(true, main), (false, spare)] {
                    for (i, slot) in list.iter_mut().enumerate() {
                        if slot.as_ref().is_some_and(|d| !d.output.alive()) {
                            if let Some(d) = slot.take() {
                                d.release(i);
                            }
                            continue;
                        }
                        let Some(drawn) = slot.as_mut() else {
                            continue;
                        };
                        // A spare renders hidden until it takes over.
                        let wanted = |s| if shown { size(s) } else { (0, 0) };
                        draw(i as Slot, drawn, api, &pacer, &wanted, &show);
                    }
                }
            }
            RenderingState::RenderingTeardown => {
                // Frees the render contexts while the GL context is still current. The players
                // stay, and get new contexts if rendering starts again.
                for drawn in slots.main.iter_mut().chain(&mut slots.spare).flatten() {
                    drawn.gpu = None;
                    drawn.failed = false;
                    drawn.output.ready.store(false, Ordering::SeqCst);
                }
            }
            _ => {}
        })?;
        Ok(outputs)
    }

    fn push(&self, change: Change) {
        self.pending
            .lock()
            .expect("video outputs lock")
            .push(change);
        // So it happens now, not with the next unrelated redraw.
        self.pacer.frame_ready();
    }

    /// Draws `output` in `slot` from the next frame on, replacing what the slot had. Any thread.
    pub fn insert(&self, slot: Slot, output: VideoOutput) {
        assert!(usize::from(slot) < SLOTS, "slot {slot} is out of range");
        self.push(Change::Main(slot, output));
    }

    /// Draws `output` hidden as the spare of `slot`, replacing its last spare. Any thread.
    pub fn insert_spare(&self, slot: Slot, output: VideoOutput) {
        assert!(usize::from(slot) < SLOTS, "slot {slot} is out of range");
        self.push(Change::Spare(slot, output));
    }

    /// The spare of `slot` took over: from the next frame on it is drawn in the slot, with the
    /// render context it already has, and the slot's old output is freed. Any thread.
    pub fn promote(&self, slot: Slot) {
        assert!(usize::from(slot) < SLOTS, "slot {slot} is out of range");
        self.push(Change::Promote(slot));
    }
}

fn take_pending(pending: &Pending, slots: &mut Slots) {
    let changes = std::mem::take(&mut *pending.lock().expect("video outputs lock"));
    for change in changes {
        let (i, new, list) = match change {
            Change::Main(slot, output) => {
                (usize::from(slot), Some(Drawn::new(output)), &mut slots.main)
            }
            Change::Spare(slot, output) => (
                usize::from(slot),
                Some(Drawn::new(output)),
                &mut slots.spare,
            ),
            Change::Promote(slot) => {
                let i = usize::from(slot);
                (i, slots.spare[i].take(), &mut slots.main)
            }
        };
        let Some(new) = new else {
            continue;
        };
        if let Some(old) = list[i].replace(new) {
            old.release(i);
        }
    }
}

/// Makes the slot's render context if needed, then renders its newest frame.
fn draw(
    slot: Slot,
    drawn: &mut Drawn,
    api: &GraphicsAPI<'_>,
    pacer: &Arc<Pacer>,
    size: &dyn Fn(Slot) -> (u32, u32),
    show: &impl Fn(Slot, slint::Image),
) {
    if drawn.gpu.is_none() && !drawn.failed {
        let GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
            tracing::error!("the renderer is not OpenGL, video is off");
            drawn.failed = true;
            return;
        };
        let pacer = Arc::clone(pacer);
        match Gpu::new(
            Arc::clone(&drawn.output.mpv),
            *get_proc_address,
            move || pacer.frame_ready(),
        ) {
            Ok(gpu) => {
                drawn.gpu = Some(gpu);
                drawn.output.ready.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                tracing::error!(slot, %e, "could not create the mpv render context");
                drawn.failed = true;
            }
        }
    }
    let Some(gpu) = drawn.gpu.as_mut() else {
        return;
    };
    let wanted = size(slot);
    let hidden = wanted.0 == 0 || wanted.1 == 0;
    let (w, h) = if hidden { HIDDEN } else { wanted };
    match gpu.draw(w, h) {
        Ok(Some(image)) if !hidden => show(slot, image),
        Ok(_) => {}
        Err(e) => tracing::warn!(slot, %e, "video render error"),
    }
}
