//! One process-wide wgpu device, acquired lazily and never rebuilt.
//!
//! Adapter enumeration and device creation cost tens of milliseconds and touch
//! a driver, so they happen once, on the first frame that actually wants the
//! GPU, and never on a process that does not. A failure is cached as hard as a
//! success: a machine with no usable adapter must not pay the probe again on
//! every card.

use std::sync::OnceLock;

use tracing::{info, warn};

/// The device and queue every compute pass in this module shares.
///
/// `Device` and `Queue` are `Send + Sync` and wgpu serialises submissions
/// internally, so the card rasteriser's worker threads may all reach this at
/// once — which they do, since `Rasteriser::draw_shapes` hands cards out to a
/// bounded pool.
pub(super) struct Context {
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    /// What was actually picked, for the log line and for honest reporting: the
    /// difference between "the GPU did this" and "a software adapter did this"
    /// is not visible in the pixels, which are identical either way.
    pub(super) adapter: wgpu::AdapterInfo,
    /// `max_storage_buffer_binding_size` for the device that was granted, so a
    /// batch too large for it declines to the CPU rather than panicking inside
    /// wgpu's validation.
    pub(super) max_storage_binding: u64,
    /// `min_uniform_buffer_offset_alignment`, the stride between per-tile
    /// uniform records in [`super::bloom`]'s dynamic-offset buffer.
    pub(super) uniform_alignment: u32,
}

/// The shared context, or `None` on any machine that could not give us one.
pub(super) fn context() -> Option<&'static Context> {
    static CONTEXT: OnceLock<Option<Context>> = OnceLock::new();
    CONTEXT.get_or_init(acquire).as_ref()
}

fn acquire() -> Option<Context> {
    // `wgpu::Instance::new` *panics* on a target none of the compiled-in
    // backends implements — it does not return an error — so the backend set is
    // checked before an instance is asked for. Cargo picks those per target
    // (see the `wgpu` entries in `Cargo.toml`) and a target that acquires a new
    // one, or loses the backend it had, must degrade to the CPU rather than
    // take the client down on its first card.
    if wgpu::Instance::enabled_backend_features().is_empty() {
        warn!(
            target_os = std::env::consts::OS,
            "no wgpu backend is compiled in for this target; keeping the CPU path"
        );
        return None;
    }
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    // `HighPerformance` is the whole point of the exercise on the captain's box:
    // the discrete card should do this, not the integrated one sharing the
    // display. It is a preference, not a requirement — a machine with only an
    // integrated adapter still gets that one.
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }));
    let adapter = match adapter {
        Ok(adapter) => adapter,
        Err(error) => {
            warn!(%error, "no GPU adapter for card rasterisation; keeping the CPU path");
            return None;
        }
    };
    let info = adapter.get_info();
    let limits = adapter.limits();

    let device = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("herdr card bloom"),
        required_features: wgpu::Features::empty(),
        // What the adapter already offers, rather than a floor we impose: this
        // pass wants a large storage buffer and nothing else exotic, and asking
        // for less would cap the batch size for no reason.
        required_limits: limits.clone(),
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        trace: wgpu::Trace::Off,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
    }));
    let (device, queue) = match device {
        Ok(pair) => pair,
        Err(error) => {
            warn!(%error, adapter = %info.name, "GPU adapter refused a compute device; keeping the CPU path");
            return None;
        }
    };

    // A validation error inside a compute pass is not recoverable per-dispatch,
    // so it is logged rather than swallowed. The pass itself still falls back to
    // the CPU on any error it can see.
    device.on_uncaptured_error(std::sync::Arc::new(|error| {
        warn!(%error, "wgpu reported an uncaptured error during card rasterisation");
    }));

    info!(
        adapter = %info.name,
        backend = ?info.backend,
        device_type = ?info.device_type,
        driver = %info.driver,
        driver_info = %info.driver_info,
        "GPU compute acquired for card rasterisation"
    );

    Some(Context {
        device,
        queue,
        max_storage_binding: u64::from(limits.max_storage_buffer_binding_size),
        uniform_alignment: limits.min_uniform_buffer_offset_alignment,
        adapter: info,
    })
}

/// Drive a future to completion on this thread.
///
/// wgpu's adapter and device requests are `async` even though every native
/// backend resolves them without ever yielding. Rather than take an executor
/// dependency for three call sites — Herdr's own `tokio` runtime is the
/// server's, and the client that needs this has none — this parks until the
/// waker says otherwise, which for a native backend means it never parks at
/// all.
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::sync::Arc;
    use std::task::{Context, Poll, Wake, Waker};

    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}
