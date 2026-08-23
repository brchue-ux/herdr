//! GPU compute for the parts of sidebar rasterisation that are a pure
//! per-pixel function of a handful of parameters.
//!
//! # Why this exists
//!
//! The R1a decision (2026-08-06, "Windows-native client, Linux-owned server")
//! moved card rasterisation off the server and onto the client precisely so the
//! machine sitting in front of the terminal does the drawing. Three pieces were
//! scoped; the local pixel transport, the Windows `--remote` port and the
//! client-side `CardScene` all shipped, and all three left the *generation*
//! itself on the same software rasteriser the server had always used. This is
//! the missing fourth piece: the splat-and-blend inner loop, on the GPU.
//!
//! # What is and is not here
//!
//! Only the bloom pass ([`bloom`]). Layout, text shaping, font rasterisation
//! and card geometry stay on the CPU — they are branchy, allocate, and read
//! font tables, and the prior cost model found no gain in moving them. What is
//! here is the one stage that is a closed-form function of `(pixel, splat)`
//! with no data dependence between pixels.
//!
//! # No zero-copy path out
//!
//! The Kitty graphics protocol takes bytes, so whatever the GPU computes has to
//! come back to host memory regardless. That readback is the fixed cost this
//! module is designed around: work is batched into **one** dispatch and **one**
//! readback for a whole frame's cards rather than one per card (see
//! [`bloom::compose`]), and [`bloom::WORTH_A_DISPATCH`] declines the GPU
//! outright when a frame is too small to earn that round trip. Shared-memory
//! transport (`t=s`) already removes the *second* copy, from Herdr to the
//! terminal.
//!
//! # The gate
//!
//! [`enabled`] is Windows-client-only by default, and the Linux server keeps
//! its existing threaded CPU path untouched. Everything here is additionally
//! behind the `gpu-raster` cargo feature, on by default; with it off,
//! `bloom_disabled.rs` stands in and reports the compute path unavailable, so
//! no caller needs a `cfg`.

#[cfg(feature = "gpu-raster")]
pub(crate) mod bloom;
#[cfg(feature = "gpu-raster")]
mod device;
mod scene;

/// Drive a `wgpu` future to completion on the calling thread.
///
/// Re-exported for `herdr bench upload-churn`, which stands up a *second*,
/// deliberately differently-configured device (see
/// `crate::cli::bench::upload_churn`) and so cannot go through [`device`]'s
/// process-wide one — but has the same three `async` calls to resolve and no
/// more reason than [`device`] has to pull in an executor for them.
#[cfg(feature = "gpu-raster")]
pub(crate) use device::block_on;

#[cfg(not(feature = "gpu-raster"))]
#[path = "bloom_disabled.rs"]
pub(crate) mod bloom;

/// Whether this process should try the GPU for card rasterisation at all.
///
/// The real gate is exactly the one that decides whether this process
/// rasterises cards in the first place: a Windows client drawing its own
/// `ServerMessage::CardScene`. A Linux server rasterising cards *for* clients
/// keeps the threaded CPU path — it is hosting a fleet of agent panes and has
/// no business holding a GPU queue open for a sidebar.
///
/// `HERDR_GPU_CARD_BLOOM` overrides the platform gate, the same bargain
/// `HERDR_CLIENT_RASTERIZED_CARDS` already makes for the client-rasterisation
/// gate itself: there is no Windows hardware to exercise the real gate from a
/// Unix dev box or CI, so this is how a Unix process is driven through the same
/// code path for testing and measurement.
///
/// Read once per frame, under `Rasteriser::shapes` — not per card and not per
/// pixel. The environment and the platform gate are cached because they cannot
/// change inside a process; [`PINNED`] is a relaxed load in front of that cache
/// for the two callers that need the answer to change inside one, and costs a
/// load from a static.
pub(crate) fn enabled() -> bool {
    if let Some(pinned) = pinned() {
        return pinned;
    }
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| {
        match std::env::var("HERDR_GPU_CARD_BLOOM").ok().as_deref() {
            Some("1" | "true" | "force") => return true,
            Some("0" | "false") => return false,
            _ => {}
        }
        cfg!(windows) && crate::client::rasterises_cards_locally()
    })
}

/// Whether to send every batch to the GPU regardless of what the cost model
/// makes of it.
///
/// `HERDR_GPU_CARD_BLOOM=force`. The cost model exists because a device's round
/// trip is measured, not assumed — but measuring the *speedup* on a new card
/// means running the GPU path on batches the model would have declined, and
/// there is no way to do that from the outside otherwise. This is how the
/// captain's RX 6900 XT gets a number rather than a prediction.
pub(crate) fn ignore_cost_model() -> bool {
    if COST_MODEL_PINNED_OFF.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| std::env::var("HERDR_GPU_CARD_BLOOM").ok().as_deref() == Some("force"))
}

/// [`enabled`], pinned: `0` is "ask the real gate", `1` on, `2` off.
///
/// The real gate is answered once per process and cached, because it cannot
/// change inside one — which is right in production and wrong for the two
/// callers whose entire job is drawing the same frame both ways: the parity
/// tests ([`ForceEnabled`]) and `herdr bench cards`. Nothing in the ordinary
/// client or server path ever writes it, so production reads a zero for the
/// life of the process and takes the cached answer below.
static PINNED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// [`ignore_cost_model`], pinned on. Set alongside [`PINNED`] whenever the GPU
/// is pinned *on*: a caller that has pinned the backend is measuring or
/// comparing it, and a cost model that declines the batch would leave it
/// measuring the CPU under a GPU label.
static COST_MODEL_PINNED_OFF: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn pinned() -> Option<bool> {
    match PINNED.load(std::sync::atomic::Ordering::Relaxed) {
        1 => Some(true),
        2 => Some(false),
        _ => None,
    }
}

/// Pin the bloom backend for the rest of this process.
///
/// For `herdr bench cards`, which runs both backends in one process so the two
/// numbers come off one machine in one state. Pinning the GPU on also stands
/// the cost model down — see [`ignore_cost_model`]; the benchmark's whole job is
/// to produce the measurement the model is otherwise predicting from.
///
/// This does not *create* a device. A machine with no adapter pinned to the GPU
/// still declines every batch to the CPU, which the benchmark reports as such
/// rather than passing off as a GPU number.
pub(crate) fn pin_backend(gpu: bool) {
    COST_MODEL_PINNED_OFF.store(gpu, std::sync::atomic::Ordering::Relaxed);
    PINNED.store(
        if gpu { 1 } else { 2 },
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// Hold [`enabled`] at `on` until the returned guard drops.
///
/// Serialised across the whole test binary: the gate is process-wide, so two
/// tests pinning it at once would each see the other's answer.
#[cfg(test)]
pub(crate) struct ForceEnabled(
    /// Held, not read: the lock is what serialises the tests, and dropping it
    /// is what releases them.
    #[allow(dead_code)]
    std::sync::MutexGuard<'static, ()>,
);

#[cfg(test)]
impl ForceEnabled {
    /// Take the gate lock without pinning it, for a test asking what the *real*
    /// policy answers. Without the lock such a test reads whichever answer a
    /// concurrently-running parity test happened to have pinned.
    pub(crate) fn released() -> Self {
        let guard = Self::lock();
        PINNED.store(0, std::sync::atomic::Ordering::Relaxed);
        bloom::PRETEND_INSTANT_FOR_TEST.store(false, std::sync::atomic::Ordering::Relaxed);
        Self(guard)
    }

    fn lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn new(on: bool) -> Self {
        let guard = Self::lock();
        PINNED.store(if on { 1 } else { 2 }, std::sync::atomic::Ordering::Relaxed);
        // A known state on the way in as well as out: holding the lock is only
        // half of it if the previous holder left the cost model pinned.
        bloom::PRETEND_INSTANT_FOR_TEST.store(false, std::sync::atomic::Ordering::Relaxed);
        Self(guard)
    }

    /// Also make the GPU look free, so the batch under test is sent whatever the
    /// cost model thinks of it.
    ///
    /// A test fixture's fleet is smaller than the frame this path exists for,
    /// and "did the two backends agree" is a different question from "was this
    /// batch worth a dispatch" — which
    /// `bloom::tests::the_device_measures_its_own_round_trip` asks on its own.
    pub(crate) fn ignoring_the_cost(self) -> Self {
        bloom::PRETEND_INSTANT_FOR_TEST.store(true, std::sync::atomic::Ordering::Relaxed);
        self
    }
}

#[cfg(test)]
impl Drop for ForceEnabled {
    fn drop(&mut self) {
        PINNED.store(0, std::sync::atomic::Ordering::Relaxed);
        bloom::PRETEND_INSTANT_FOR_TEST.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    /// The gate is off unless something turns it on, and a Unix test process is
    /// never a Windows client — so the CPU path is what every other test in this
    /// repository is measuring, which is the point.
    #[test]
    fn the_gate_is_shut_on_a_plain_unix_process() {
        let _gate = super::ForceEnabled::released();
        if std::env::var("HERDR_GPU_CARD_BLOOM").is_ok() {
            // Deliberately driven by the operator for a measurement run.
            return;
        }
        assert!(
            !cfg!(unix) || !super::enabled(),
            "a plain Unix process opened the GPU gate; every other test in this \
             repository is then measuring a path production would not take"
        );
    }
}
