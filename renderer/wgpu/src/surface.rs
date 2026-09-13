//! The window half of the renderer: a `winit` window, a `wgpu` surface, and the resize and
//! HiDPI bookkeeping between them.
//!
//! Deliberately thin. Everything above this file works in logical pixels and never learns
//! whether it is drawing to a window, an offscreen texture, or (at M22) a
//! `CAMetalLayer` handed over by UIKit. That is DECISIONS D-09: no code above the renderer
//! may assume a resizable desktop window exists.

use std::sync::Arc;

use crisol_display_list::Size;
use winit::window::Window;

use crate::gpu::{Gpu, GpuError, SharedGpu};

/// A window, its surface, and the configuration they were last agreed on.
#[derive(Debug)]
pub struct WindowSurface {
    window: Arc<dyn Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    /// Shared, because a second window must not mean a second device.
    ///
    /// An adapter and a logical device are the bulk of what an idle GPU application costs —
    /// about 17 MB of physical footprint on macOS, measured against the 60 MB the product
    /// claim allows. Giving every window its own would spend that budget on window count.
    gpu: SharedGpu,
}

/// What happened when a frame's surface texture was requested.
///
/// A surface can fail to produce a texture for reasons that are not errors — the window is
/// occluded, the compositor is busy, the swapchain went stale behind a resize. Making the
/// caller match on this rather than unwrap is what keeps a frame loop from panicking the
/// first time someone minimises the window.
#[derive(Debug)]
pub enum AcquiredFrame {
    /// Draw into this and then present it.
    Frame(wgpu::SurfaceTexture),
    /// Nothing to draw into this time. Not an error; try again next frame.
    Skip,
}

impl WindowSurface {
    /// Creates a surface for `window` and acquires a device that can present to it.
    ///
    /// Takes `Arc<dyn Window>` because the surface borrows the window handle for as long as it
    /// lives, and sharing ownership is the only way to promise that without a lifetime
    /// escaping into every type that touches the renderer.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError`] when the surface cannot be created, no adapter can present to
    /// it, or the surface exposes no format we can render to.
    pub fn new(window: Arc<dyn Window>) -> Result<Self, GpuError> {
        let instance = wgpu::Instance::new(crate::gpu::instance_descriptor());
        let surface = instance.create_surface(Arc::clone(&window))?;
        let gpu = SharedGpu::new(Gpu::for_surface(instance, &surface)?);
        Self::configure(window, surface, gpu)
    }

    /// Opens a surface for `window` on a device that already exists.
    ///
    /// What a second window uses. The adapter, the logical device and the queue are the bulk
    /// of an idle GPU application's memory, and they are per *application*, not per window —
    /// creating a second set would double the floor for no reason a user could name.
    ///
    /// The shared device comes from an existing surface via [`Self::shared_gpu`], which is
    /// also what guarantees it can present here: an adapter chosen for one window on a
    /// multi-GPU laptop is the one attached to that display, and a second window on the same
    /// display wants the same answer.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError`] when the surface cannot be created or exposes no format we can
    /// render to.
    pub fn with_gpu(gpu: SharedGpu, window: Arc<dyn Window>) -> Result<Self, GpuError> {
        let surface = gpu.instance.create_surface(Arc::clone(&window))?;
        Self::configure(window, surface, gpu)
    }

    fn configure(
        window: Arc<dyn Window>,
        surface: wgpu::Surface<'static>,
        gpu: SharedGpu,
    ) -> Result<Self, GpuError> {
        let capabilities = surface.get_capabilities(&gpu.adapter);
        let format = choose_format(&capabilities).ok_or(GpuError::NoSurfaceFormat)?;

        let size = window.surface_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            // A zero-sized window is legal on Windows while minimised, and configuring a
            // zero-sized surface is not. Clamp rather than refusing to start.
            width: size.width.max(1),
            height: size.height.max(1),
            // sRGB, standard dynamic range. HDR and wide-gamut output would change how
            // every colour in the engine is encoded and is a decision for a later
            // milestone, not a default we drift into because a driver offered it.
            color_space: wgpu::SurfaceColorSpace::Srgb,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: capabilities.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);

        Ok(Self {
            window,
            surface,
            config,
            gpu,
        })
    }

    /// The window.
    #[must_use]
    pub fn window(&self) -> &Arc<dyn Window> {
        &self.window
    }

    /// The device this surface presents with.
    #[must_use]
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// A handle to that device, for opening a second window on it.
    ///
    /// See [`Self::with_gpu`]: a second device is the single most expensive thing a second
    /// window could do.
    #[must_use]
    pub fn shared_gpu(&self) -> SharedGpu {
        SharedGpu::clone(&self.gpu)
    }

    /// The colour format to build a [`crate::Renderer`] for.
    #[must_use]
    pub fn format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Surface width in physical pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.config.width
    }

    /// Surface height in physical pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.config.height
    }

    /// Physical pixels per logical pixel, as the platform currently reports it.
    #[must_use]
    pub fn scale_factor(&self) -> f32 {
        self.window.scale_factor() as f32
    }

    /// Surface size in logical pixels, which is the coordinate space display lists are
    /// authored in.
    #[must_use]
    pub fn logical_size(&self) -> Size {
        let scale = self.scale_factor().max(f32::MIN_POSITIVE);
        Size::new(
            self.config.width as f32 / scale,
            self.config.height as f32 / scale,
        )
    }

    /// Reconfigures the swapchain for a new physical size.
    ///
    /// Ignores zero-sized requests: minimising a window on Windows reports `0x0`, and
    /// configuring a zero-sized surface is a validation error. The size before minimising
    /// is kept so that restoring the window does not need a second resize event.
    pub fn resize(&mut self, width: u32, height: u32) {
        let current = (self.config.width, self.config.height);
        if let Some((width, height)) = reconfigure_to(current, (width, height)) {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.gpu.device, &self.config);
        }
    }

    /// Reconfigures with the window's current size. Call after a scale factor change, which
    /// arrives without a resize event on some platforms.
    pub fn refresh(&mut self) {
        let size = self.window.surface_size();
        self.config.width = size.width.max(1);
        self.config.height = size.height.max(1);
        self.surface.configure(&self.gpu.device, &self.config);
    }

    /// Hands a drawn frame to the compositor.
    ///
    /// Wrapped rather than left to the caller because `Queue::present` is the one place the
    /// swapchain and the queue have to be the same pair, and a caller holding both has no
    /// way to be reminded of that.
    pub fn present(&self, frame: wgpu::SurfaceTexture) {
        self.gpu.queue.present(frame);
    }

    /// Gets the texture for this frame, recovering from a stale swapchain.
    ///
    /// A swapchain goes out of date whenever the window changes behind our back — a resize
    /// we have not processed yet, a monitor change, a compositor restart. One reconfigure
    /// and retry handles all of them; a second failure means skip the frame rather than
    /// spin.
    pub fn acquire(&mut self) -> AcquiredFrame {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => AcquiredFrame::Frame(frame),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.refresh();
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(frame)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => AcquiredFrame::Frame(frame),
                    _ => AcquiredFrame::Skip,
                }
            }
            // Occluded, Timeout and Validation: nothing useful to draw into right now.
            _ => AcquiredFrame::Skip,
        }
    }
}

/// Prefers an sRGB format so the hardware does the final encode (DECISIONS D-15).
///
/// Falls back to the sRGB view of the surface's preferred format, and finally to the
/// preferred format itself — on which colours will be slightly off, which is better than
/// refusing to open a window.
fn choose_format(capabilities: &wgpu::SurfaceCapabilities) -> Option<wgpu::TextureFormat> {
    capabilities
        .formats
        .iter()
        .copied()
        .find(|format| format.is_srgb())
        .or_else(|| capabilities.formats.first().map(|f| f.add_srgb_suffix()))
        .or_else(|| capabilities.formats.first().copied())
}

/// The size to reconfigure a swapchain to, or `None` to leave it alone.
///
/// Split out of [`WindowSurface::resize`] because both of its `None` cases are reachable only
/// through a real window doing something — minimising on Windows, or delivering a resize event
/// for the size it already has — and a CI runner has no window at all. The logic is three
/// lines and correct; what it did not have was anything that would notice if it stopped being.
/// Issue #15, whose remaining half needs a human at a machine.
fn reconfigure_to(current: (u32, u32), requested: (u32, u32)) -> Option<(u32, u32)> {
    let (width, height) = requested;
    if width == 0 || height == 0 {
        return None;
    }
    if current == requested {
        return None;
    }
    Some(requested)
}

#[cfg(test)]
mod tests {
    use super::reconfigure_to;

    #[test]
    fn a_new_size_reconfigures() {
        assert_eq!(reconfigure_to((800, 600), (1024, 768)), Some((1024, 768)));
    }

    #[test]
    fn the_size_it_already_has_does_not() {
        assert_eq!(
            reconfigure_to((800, 600), (800, 600)),
            None,
            "a resize event for the current size should not rebuild the swapchain"
        );
    }

    #[test]
    fn a_minimised_window_is_ignored_rather_than_clamped() {
        // Windows reports 0x0 while minimised. Configuring that is a validation error, and
        // clamping it to 1x1 would be worse than ignoring it: the window would come back
        // from the taskbar at 1x1 until a second resize event arrived to undo it.
        assert_eq!(reconfigure_to((800, 600), (0, 0)), None);
        assert_eq!(reconfigure_to((800, 600), (0, 600)), None, "zero width");
        assert_eq!(reconfigure_to((800, 600), (800, 0)), None, "zero height");
    }

    #[test]
    fn a_window_restored_to_its_previous_size_needs_no_reconfigure() {
        // The whole reason the pre-minimise size is kept: minimise then restore is
        // `(0,0)` then `(800,600)`, and the second is the size already configured.
        assert_eq!(reconfigure_to((800, 600), (0, 0)), None);
        assert_eq!(reconfigure_to((800, 600), (800, 600)), None);
    }
}
