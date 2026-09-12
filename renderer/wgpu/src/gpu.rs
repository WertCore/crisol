//! Adapter, device and queue acquisition.
//!
//! Split out from the renderer so a headless test, a window and (at M22) a mobile surface
//! all share one path to a device, with one place that decides limits and backends.

use std::sync::Arc;

/// Anything that can go wrong between "we want to draw" and "we have a device".
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// No adapter matched the request. On a headless CI machine this usually means no
    /// software rasteriser is installed.
    #[error("no suitable GPU adapter: {0}")]
    NoAdapter(#[from] wgpu::RequestAdapterError),

    /// An adapter was found but would not give us a device with the limits we asked for.
    #[error("could not create a device: {0}")]
    NoDevice(#[from] wgpu::RequestDeviceError),

    /// The window handle could not be turned into a drawable surface.
    #[error("could not create a surface: {0}")]
    Surface(#[from] wgpu::CreateSurfaceError),

    /// The surface offered no texture format we can render to.
    #[error("surface exposes no usable texture format")]
    NoSurfaceFormat,
}

/// A device and the handles needed to use it.
///
/// `wgpu`'s `Device` and `Queue` are already reference-counted handles, so cloning a `Gpu`
/// is cheap and does not duplicate any GPU-side resource.
#[derive(Clone, Debug)]
pub struct Gpu {
    /// Backend instance the adapter came from. Kept because surfaces are created from it.
    pub instance: wgpu::Instance,
    /// The physical device that was selected.
    pub adapter: wgpu::Adapter,
    /// The logical device.
    pub device: wgpu::Device,
    /// The queue belonging to `device`.
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// Acquires a device with no surface, for offscreen rendering and tests.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::NoAdapter`] when no backend is available at all.
    pub fn headless() -> Result<Self, GpuError> {
        let instance = wgpu::Instance::new(instance_descriptor());
        Self::from_instance(instance, None)
    }

    /// Acquires a device that is known to be able to present to `surface`.
    ///
    /// Passing the surface matters: on a multi-GPU laptop the fastest adapter is not
    /// necessarily the one the display is attached to, and picking the wrong one silently
    /// costs a full cross-adapter copy every frame.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError`] when no adapter can present to the surface or the device
    /// request fails.
    pub fn for_surface(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
    ) -> Result<Self, GpuError> {
        Self::from_instance(instance, Some(surface))
    }

    fn from_instance(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, GpuError> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            force_fallback_adapter: false,
            compatible_surface,
            ..Default::default()
        }))?;

        // Ask for the WebGPU downlevel baseline rather than whatever this adapter happens
        // to offer. A shader that accidentally depends on a desktop-only limit then fails
        // here, on the developer's machine, instead of at M22 on a phone (DECISIONS D-09).
        let required_limits = wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits());

        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("crisol"),
                required_features: wgpu::Features::empty(),
                required_limits,
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                trace: wgpu::Trace::Off,
            }))?;

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// Blocks until every submitted command has finished.
    ///
    /// Only needed around readback and shutdown; the frame loop must never call it.
    pub fn wait_idle(&self) {
        // A poll failure here means the device is lost, which the next submit reports with
        // a better message than we could produce.
        let _ = self.device.poll(wgpu::PollType::wait_indefinitely());
    }
}

/// Shared instance configuration.
///
/// `InstanceFlags::from_build_config` turns validation and debug labels on for debug builds
/// and off for release, which is what we want on both counts.
pub(crate) fn instance_descriptor() -> wgpu::InstanceDescriptor {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    // PRIMARY is Vulkan + Metal + DX12: every backend we intend to support, and
    // specifically not GL, which would let a GL-only fallback quietly paper over a real
    // problem on a machine that should have had a modern backend.
    desc.backends = wgpu::Backends::PRIMARY;
    desc.flags = wgpu::InstanceFlags::from_build_config();
    // `with_env` lets `WGPU_BACKEND=vulkan` and friends override the above when bisecting a
    // backend-specific rendering bug. It is the last word on purpose.
    desc.with_env()
}

/// A [`Gpu`] shared between a renderer and whatever owns the window.
pub type SharedGpu = Arc<Gpu>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_headless_device_can_be_acquired() {
        match Gpu::headless() {
            Ok(gpu) => {
                let info = gpu.adapter.get_info();
                assert!(!info.name.is_empty());
            }
            Err(e) => panic!("no GPU adapter available: {e}"),
        }
    }
}
