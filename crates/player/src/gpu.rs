//! Render-thread side: the mpv render context. Every function here runs on the Slint UI
//! thread, inside the rendering notifier, with Slint's OpenGL context current.

use std::ffi::{c_void, CStr, CString};
use std::sync::Arc;

use libmpv2::render::{
    mpv_render_update, OpenGLInitParams, RenderContext, RenderParam, RenderParamApiType,
};
use libmpv2::Mpv;
use slint::{BorrowedOpenGLTextureBuilder, BorrowedOpenGLTextureOrigin};

use crate::gl_state::SavedGlState;
use crate::target::Target;

type SlintProcFn<'a> = &'a dyn Fn(&CStr) -> *const c_void;

/// Context for mpv's `get_proc_address` callback: a pointer to Slint's loader closure.
///
/// Slint's loader is only valid during the notifier call. libmpv only resolves GL functions
/// inside `mpv_render_context_create`, and libmpv2 frees these init params right after that
/// call, so the pointer is never used after `Gpu::new` returns.
struct SlintGlLoader(*const c_void);

fn mpv_get_proc_address(loader: &SlintGlLoader, name: &str) -> *mut c_void {
    let Ok(name) = CString::new(name) else {
        return std::ptr::null_mut();
    };
    // SAFETY: see `SlintGlLoader`. The pointee outlives `create_render_context`.
    let get_proc_address = unsafe { *(loader.0 as *const SlintProcFn) };
    get_proc_address(&name) as *mut c_void
}

pub struct Gpu {
    gl: glow::Context,
    // Field order matters: `render` must drop before `_mpv`, because the render context may not
    // outlive the core it was created from.
    render: RenderContext<'static>,
    _mpv: Arc<Mpv>,
    target: Option<Target>,
}

impl Gpu {
    /// `on_frame` runs on an mpv thread whenever a new frame is ready. It must not call mpv.
    pub fn new(
        mpv: Arc<Mpv>,
        get_proc_address: SlintProcFn<'_>,
        on_frame: impl Fn() + Send + 'static,
    ) -> Result<Self, String> {
        // SAFETY: Slint guarantees its GL context is current during the notifier callback.
        let gl = unsafe { glow::Context::from_loader_function_cstr(|name| get_proc_address(name)) };

        let loader = SlintGlLoader(&get_proc_address as *const SlintProcFn as *const c_void);
        let render = mpv
            .create_render_context(vec![
                RenderParam::ApiType(RenderParamApiType::OpenGl),
                RenderParam::InitParams(OpenGLInitParams {
                    get_proc_address: mpv_get_proc_address,
                    ctx: loader,
                }),
            ])
            .map_err(|e| e.to_string())?;
        // SAFETY: the lifetime on `RenderContext` only marks the borrow of the core. `_mpv` keeps
        // the core alive, and it is declared after `render`, so it drops after it.
        let mut render =
            unsafe { std::mem::transmute::<RenderContext<'_>, RenderContext<'static>>(render) };
        render.set_update_callback(on_frame);

        Ok(Self {
            gl,
            render,
            _mpv: mpv,
            target: None,
        })
    }

    /// Renders a new mpv frame if one is ready (or the size changed) and returns it as an image.
    pub fn draw(&mut self, width: u32, height: u32) -> Result<Option<slint::Image>, String> {
        if width == 0 || height == 0 {
            return Ok(None);
        }
        let resized = self
            .target
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height);
        if resized {
            if let Some(old) = self.target.take() {
                old.delete(&self.gl);
            }
            self.target = Some(Target::new(&self.gl, width, height)?);
        }

        let flags = self.render.update().map_err(|e| e.to_string())?;
        if !resized && flags & mpv_render_update::Frame == 0 {
            return Ok(None);
        }

        let Some(target) = self.target.as_ref() else {
            return Ok(None);
        };
        let saved = SavedGlState::capture(&self.gl);
        saved.reset_for_mpv(&self.gl);
        // flip = false: mpv writes the top video row to texture row 0, which matches
        // `BorrowedOpenGLTextureOrigin::TopLeft`.
        let result = self.render.render::<SlintGlLoader>(
            target.fbo.0.get() as i32,
            width as i32,
            height as i32,
            false,
        );
        saved.restore(&self.gl);
        result.map_err(|e| e.to_string())?;

        // SAFETY: the texture was created on this window's context and lives until teardown.
        let builder = unsafe {
            BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                target.texture.0,
                (width, height).into(),
            )
        };
        Ok(Some(
            builder.origin(BorrowedOpenGLTextureOrigin::TopLeft).build(),
        ))
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        if let Some(target) = self.target.take() {
            target.delete(&self.gl);
        }
        // `render` drops after this body, calling mpv_render_context_free on this thread
        // while the GL context is still current (we are inside RenderingTeardown).
    }
}
