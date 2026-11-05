//! Saves and restores the GL state that mpv may change.
//!
//! Slint's docs on `RenderingState` require the notifier to leave the context state exactly as
//! it found it. mpv's `render_gl.h` expects default-ish state except viewport, scissor box,
//! blend func and clear colour. So capture the state, reset it for mpv, render, then restore.

use std::num::NonZeroU32;

use glow::HasContext;

const CAPS: [u32; 5] = [
    glow::BLEND,
    glow::SCISSOR_TEST,
    glow::STENCIL_TEST,
    glow::DEPTH_TEST,
    glow::CULL_FACE,
];

pub struct SavedGlState {
    draw_fbo: i32,
    read_fbo: i32,
    program: i32,
    vao: i32,
    array_buffer: i32,
    active_texture: i32,
    texture_units: [i32; 2],
    viewport: [i32; 4],
    scissor_box: [i32; 4],
    clear_color: [f32; 4],
    blend_func: [i32; 4],
    unpack_alignment: i32,
    enabled: [(u32, bool); 5],
}

fn name(value: i32) -> Option<NonZeroU32> {
    NonZeroU32::new(value as u32)
}

impl SavedGlState {
    pub fn capture(gl: &glow::Context) -> Self {
        // SAFETY: state queries on the current context. Only the active texture unit changes,
        // and it is put back before returning.
        unsafe {
            let int = |p| gl.get_parameter_i32(p);
            let active_texture = int(glow::ACTIVE_TEXTURE);
            let mut texture_units = [0; 2];
            for (unit, slot) in texture_units.iter_mut().enumerate() {
                gl.active_texture(glow::TEXTURE0 + unit as u32);
                *slot = int(glow::TEXTURE_BINDING_2D);
            }
            gl.active_texture(active_texture as u32);

            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mut scissor_box = [0; 4];
            gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut scissor_box);
            let mut clear_color = [0.0; 4];
            gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut clear_color);

            Self {
                draw_fbo: int(glow::DRAW_FRAMEBUFFER_BINDING),
                read_fbo: int(glow::READ_FRAMEBUFFER_BINDING),
                program: int(glow::CURRENT_PROGRAM),
                vao: int(glow::VERTEX_ARRAY_BINDING),
                array_buffer: int(glow::ARRAY_BUFFER_BINDING),
                active_texture,
                texture_units,
                viewport,
                scissor_box,
                clear_color,
                blend_func: [
                    int(glow::BLEND_SRC_RGB),
                    int(glow::BLEND_DST_RGB),
                    int(glow::BLEND_SRC_ALPHA),
                    int(glow::BLEND_DST_ALPHA),
                ],
                unpack_alignment: int(glow::UNPACK_ALIGNMENT),
                enabled: CAPS.map(|cap| (cap, gl.is_enabled(cap))),
            }
        }
    }

    /// Puts the context into the default state mpv expects before `mpv_render_context_render`.
    pub fn reset_for_mpv(&self, gl: &glow::Context) {
        // SAFETY: plain state changes on the current context, undone by `restore`.
        unsafe {
            for cap in CAPS {
                gl.disable(cap);
            }
            gl.use_program(None);
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.active_texture(glow::TEXTURE0);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
        }
    }

    pub fn restore(&self, gl: &glow::Context) {
        // SAFETY: restores values previously read from this same context.
        unsafe {
            let fbo = |v| name(v).map(glow::NativeFramebuffer);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, fbo(self.draw_fbo));
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, fbo(self.read_fbo));
            gl.use_program(name(self.program).map(glow::NativeProgram));
            gl.bind_vertex_array(name(self.vao).map(glow::NativeVertexArray));
            gl.bind_buffer(
                glow::ARRAY_BUFFER,
                name(self.array_buffer).map(glow::NativeBuffer),
            );
            for (unit, binding) in self.texture_units.iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + unit as u32);
                gl.bind_texture(glow::TEXTURE_2D, name(*binding).map(glow::NativeTexture));
            }
            gl.active_texture(self.active_texture as u32);
            let [x, y, w, h] = self.viewport;
            gl.viewport(x, y, w, h);
            let [x, y, w, h] = self.scissor_box;
            gl.scissor(x, y, w, h);
            let [r, g, b, a] = self.clear_color;
            gl.clear_color(r, g, b, a);
            let [src_rgb, dst_rgb, src_a, dst_a] = self.blend_func.map(|v| v as u32);
            gl.blend_func_separate(src_rgb, dst_rgb, src_a, dst_a);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, self.unpack_alignment);
            for (cap, on) in self.enabled {
                if on {
                    gl.enable(cap);
                } else {
                    gl.disable(cap);
                }
            }
        }
    }
}
