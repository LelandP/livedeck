//! FBO with an RGBA8 colour texture that mpv renders into and Slint samples from.

use glow::HasContext;

use crate::gl_state::SavedGlState;

pub struct Target {
    pub fbo: glow::NativeFramebuffer,
    pub texture: glow::NativeTexture,
    pub width: u32,
    pub height: u32,
}

impl Target {
    /// Creates the texture and FBO. GL bindings are restored before returning.
    pub fn new(gl: &glow::Context, width: u32, height: u32) -> Result<Self, String> {
        let saved = SavedGlState::capture(gl);
        // SAFETY: GL object creation on the current context.
        let created = unsafe { Self::create(gl, width, height) };
        saved.restore(gl);

        let (target, status) = created?;
        if status != glow::FRAMEBUFFER_COMPLETE {
            target.delete(gl);
            return Err(format!("framebuffer incomplete: 0x{status:x}"));
        }
        Ok(target)
    }

    unsafe fn create(gl: &glow::Context, width: u32, height: u32) -> Result<(Self, u32), String> {
        let texture = gl.create_texture()?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        for (param, value) in [
            (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
            (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
            (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
            (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
        ] {
            gl.tex_parameter_i32(glow::TEXTURE_2D, param, value as i32);
        }
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            width as i32,
            height as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(None),
        );

        let fbo = gl.create_framebuffer()?;
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(texture),
            0,
        );
        let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
        let target = Self {
            fbo,
            texture,
            width,
            height,
        };
        Ok((target, status))
    }

    pub fn delete(self, gl: &glow::Context) {
        // SAFETY: both objects were created on this context and are no longer referenced.
        unsafe {
            gl.delete_framebuffer(self.fbo);
            gl.delete_texture(self.texture);
        }
    }
}
