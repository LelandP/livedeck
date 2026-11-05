//! `VideoOutput`: the render half of the player, attached to a Slint window on the UI thread.

use std::sync::Arc;

use libmpv2::Mpv;
use slint::{GraphicsAPI, RenderingState};

use crate::gpu::Gpu;

/// Renders mpv's frames into a texture for Slint. Made by `MpvPlayer::new`.
pub struct VideoOutput {
    mpv: Arc<Mpv>,
}

impl VideoOutput {
    pub(crate) fn new(mpv: Arc<Mpv>) -> Self {
        Self { mpv }
    }

    /// Wires mpv into `window`'s rendering notifier. Call on the UI thread, before the event
    /// loop runs. Needs an OpenGL renderer.
    ///
    /// - `size` gives the pixel size of the video area. mpv renders at that size.
    /// - `show` receives each new frame as an image. Runs on the UI thread.
    /// - `redraw` is called from an mpv thread when a frame is ready and must ask the window
    ///   to redraw (for example with `Weak::upgrade_in_event_loop`). It must not call mpv.
    pub fn attach(
        self,
        window: &slint::Window,
        size: impl Fn() -> (u32, u32) + 'static,
        show: impl Fn(slint::Image) + 'static,
        redraw: impl Fn() + Send + Clone + 'static,
    ) -> Result<(), slint::SetRenderingNotifierError> {
        let mpv = self.mpv;
        let mut gpu: Option<Gpu> = None;
        window.set_rendering_notifier(move |state, api| match state {
            RenderingState::RenderingSetup => {
                let GraphicsAPI::NativeOpenGL { get_proc_address } = api else {
                    tracing::error!("the renderer is not OpenGL, video is off");
                    return;
                };
                match Gpu::new(Arc::clone(&mpv), *get_proc_address, redraw.clone()) {
                    Ok(g) => gpu = Some(g),
                    Err(e) => tracing::error!(%e, "could not create the mpv render context"),
                }
            }
            RenderingState::BeforeRendering => {
                let Some(gpu) = gpu.as_mut() else {
                    return;
                };
                let (w, h) = size();
                match gpu.draw(w, h) {
                    Ok(Some(image)) => show(image),
                    Ok(None) => {}
                    Err(e) => tracing::warn!(%e, "video render error"),
                }
            }
            RenderingState::RenderingTeardown => {
                // Frees the render context and GL objects while the context is still current.
                gpu = None;
            }
            _ => {}
        })
    }
}
