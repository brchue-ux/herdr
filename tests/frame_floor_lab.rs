//! Live lab for the headless animation frame floor.
//!
//! These are measurements, not assertions about a fixed number, so they are
//! `#[ignore]`d and never run in CI: they spawn a real server, attach a real
//! client, and time what actually comes down the socket. Run them by hand with
//!
//! ```text
//! cargo test --test frame_floor_lab -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Everything is confined to this test's own `XDG_CONFIG_HOME`, `XDG_RUNTIME_DIR`
//! and API socket under `/tmp`, so no lab run can reach a real Herdr fleet or
//! read a real `config.toml`.

mod support;

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;
use support::{
    cleanup_test_base, client_handshake, read_server_message, register_runtime_dir,
    register_spawned_herdr_pid, unregister_spawned_herdr_pid, CURRENT_PROTOCOL,
};

/// `ServerMessage::Frame` — a rendered frame for a semantic-frame client. The
/// server skips a frame whose content is identical to the last one it sent, so
/// counting these counts visible change rather than loop passes.
const FRAME_VARIANT: u32 = 1;

/// How long each arm is observed for. Long enough that a 5 fps arm still
/// produces enough samples for a tail to mean anything.
const OBSERVE: Duration = Duration::from_secs(12);

struct SpawnedHerdr {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Drop for SpawnedHerdr {
    fn drop(&mut self) {
        let pid = self.child.process_id();
        let _ = self.child.kill();
        if let Some(pid) = pid {
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                let mut status = 0;
                let result =
                    unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
                if result == pid as libc::pid_t || result == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            unregister_spawned_herdr_pid(Some(pid));
        }
    }
}

fn unique_test_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    PathBuf::from(format!(
        "/tmp/herdr-frame-floor-lab-{tag}-{}-{nanos}",
        std::process::id()
    ))
}

fn wait_for_socket(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() && UnixStream::connect(path).is_ok() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("socket did not appear at {}", path.display());
}

fn spawn_server(
    config_home: &Path,
    runtime_dir: &Path,
    api_socket: &Path,
    config: &str,
) -> SpawnedHerdr {
    // A debug build reads `herdr-dev`, a release build reads `herdr`, so seed
    // both and then point `HERDR_CONFIG_PATH` at the file explicitly. That env
    // var moves only the config file, never the socket, so the isolation the
    // `XDG_*` overrides give us is untouched.
    fs::create_dir_all(config_home.join("herdr")).unwrap();
    fs::create_dir_all(config_home.join("herdr-dev")).unwrap();
    fs::create_dir_all(runtime_dir).unwrap();
    register_runtime_dir(runtime_dir);
    let config_file = config_home.join("herdr/config.toml");
    fs::write(&config_file, config).unwrap();
    fs::write(config_home.join("herdr-dev/config.toml"), config).unwrap();

    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 48,
            cols: 160,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_herdr"));
    cmd.arg("server");
    cmd.env("XDG_CONFIG_HOME", config_home);
    cmd.env("XDG_RUNTIME_DIR", runtime_dir);
    cmd.env("HERDR_SOCKET_PATH", api_socket);
    cmd.env("HERDR_CONFIG_PATH", &config_file);
    cmd.env_remove("HERDR_CLIENT_SOCKET_PATH");
    cmd.env("SHELL", "/bin/sh");
    cmd.env_remove("HERDR_ENV");

    let child = pair.slave.spawn_command(cmd).unwrap();
    register_spawned_herdr_pid(child.process_id());
    drop(pair.slave);

    SpawnedHerdr {
        _master: pair.master,
        child,
    }
}

fn send_json_request(socket_path: &Path, request: &str) -> Value {
    let mut stream = UnixStream::connect(socket_path).expect("connect to API socket");
    writeln!(stream, "{request}").unwrap();
    let mut reader = BufReader::new(stream);
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).expect("valid JSON response")
}

/// Build a fleet of `spaces` workspaces, reporting an agent on each pane when
/// `report_agents` is set.
///
/// Whether agents are reported is the single most important variable in this
/// lab, and not for the reason the sidebar suggests. A fleet with reported
/// agents puts the server into a continuous whole-app render loop, so the
/// frames counted here are that loop's rather than the animation's — the
/// floor's own effect is only legible against a fleet that is otherwise quiet.
fn seed_fleet(api_socket: &Path, spaces: usize, report_agents: bool) {
    for index in 0..spaces {
        let response = send_json_request(
            api_socket,
            &format!(
                r#"{{"id":"ws{index}","method":"workspace.create","params":{{"label":"lab-{index}"}}}}"#
            ),
        );
        assert!(
            response.get("error").is_none(),
            "workspace.create: {response}"
        );
        let pane_id = response
            .pointer("/result/root_pane/pane_id")
            .and_then(Value::as_str)
            .expect("root pane id")
            .to_string();

        if report_agents {
            let response = send_json_request(
                api_socket,
                &format!(
                    r#"{{"id":"ag{index}","method":"pane.report_agent","params":{{"pane_id":"{pane_id}","agent":"pi","state":"working","source":"frame-floor-lab"}}}}"#
                ),
            );
            assert!(
                response.get("error").is_none(),
                "pane.report_agent: {response}"
            );
        }
    }
}

/// Inter-arrival times, in milliseconds, of every frame the server pushed.
struct Arm {
    name: String,
    gaps_ms: Vec<f64>,
    frames: usize,
    observed: Duration,
}

impl Arm {
    fn fps(&self) -> f64 {
        self.frames as f64 / self.observed.as_secs_f64()
    }

    fn percentile(&self, p: f64) -> f64 {
        if self.gaps_ms.is_empty() {
            return f64::NAN;
        }
        let mut sorted = self.gaps_ms.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let index = ((sorted.len() - 1) as f64 * p).round() as usize;
        sorted[index]
    }

    fn report(&self) {
        println!(
            "\n=== {} ===\n  frames               : {}\n  observed             : {:.2} s\n  frames per second    : {:.2}\n  gap p50              : {:.2} ms\n  gap p90              : {:.2} ms\n  gap p99              : {:.2} ms\n  gap p99.9 (0.1% low) : {:.2} ms\n  gap max (worst case) : {:.2} ms",
            self.name,
            self.frames,
            self.observed.as_secs_f64(),
            self.fps(),
            self.percentile(0.50),
            self.percentile(0.90),
            self.percentile(0.99),
            self.percentile(0.999),
            self.gaps_ms.iter().cloned().fold(f64::NAN, f64::max),
        );
    }
}

/// Spawn a server on `config`, attach one full-app client, and time the frames.
fn run_arm(name: &str, tag: &str, config: &str, spaces: usize, report_agents: bool) -> Arm {
    let base = unique_test_dir(tag);
    let config_home = base.join("config");
    let runtime_dir = base.join("runtime");
    let api_socket = runtime_dir.join("herdr.sock");
    let client_socket = runtime_dir.join("herdr-client.sock");

    let spawned = spawn_server(&config_home, &runtime_dir, &api_socket, config);
    wait_for_socket(&api_socket, Duration::from_secs(20));
    seed_fleet(&api_socket, spaces, report_agents);
    wait_for_socket(&client_socket, Duration::from_secs(20));

    let mut stream = UnixStream::connect(&client_socket).expect("connect to client socket");
    let (_version, error) =
        client_handshake(&mut stream, CURRENT_PROTOCOL, 160, 48).expect("handshake");
    assert!(error.is_none(), "handshake rejected: {error:?}");
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();

    // Let the first burst of frames (the initial paint, agent arriving, the
    // animation mounting) settle so the measurement is of steady state.
    let settle = Instant::now() + Duration::from_secs(3);
    while Instant::now() < settle {
        let _ = read_server_message(&mut stream);
    }

    let mut gaps_ms = Vec::new();
    let mut frames = 0usize;
    let started = Instant::now();
    let mut last = started;
    while started.elapsed() < OBSERVE {
        match read_server_message(&mut stream) {
            Ok((variant, _)) if variant == FRAME_VARIANT => {
                let now = Instant::now();
                gaps_ms.push((now - last).as_secs_f64() * 1000.0);
                last = now;
                frames += 1;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let observed = started.elapsed();

    drop(stream);
    drop(spawned);
    cleanup_test_base(&base);

    Arm {
        name: name.to_string(),
        gaps_ms,
        frames,
        observed,
    }
}

/// A Space row that pulses. `pulse` sits on the catalogue's cheap tier — a
/// 100 ms frame interval — which the old hard-coded 200 ms floor halved.
const PULSE_ROWS: &str = r#"
onboarding = false

[ui.sidebar.spaces]
rows = [[{ token = "workspace", emphasis = "pulse" }]]
"#;

/// The same row on the catalogue's smooth tier — a 50 ms frame interval, which
/// the old floor quartered.
const SHIMMER_ROWS: &str = r#"
onboarding = false

[ui.sidebar.spaces]
rows = [[{ token = "workspace", emphasis = "shimmer" }]]
"#;

fn with_floor(rows: &str, ms: u64) -> String {
    format!("{rows}\n[advanced]\nheadless_animation_interval_ms = {ms}\n")
}

/// What the configured floor governs on a live server.
///
/// This lab previously recorded the opposite finding, and that finding was a
/// bug rather than a property: `Engine::advance()` stepped every element on
/// every loop pass, so a resting element always reported movement, which held
/// `needs_render` true and made `last_render + MIN_RENDER_INTERVAL` the
/// smallest deadline in the list forever. The floor fed only
/// `Engine::next_deadline`, which never won, so floors of 16, 200 and 1000 ms
/// were genuinely indistinguishable — at ~58 fps whatever tier was asked for.
///
/// `advance` now steps an element only on its own
/// [`Behaviour::frame_interval`], raised by the floor, so the floor reaches
/// the change signal itself and is the one real control over what a headless
/// server spends on animation. The arms below are the evidence: a 1000 ms
/// floor is now clearly slower than the default, and the control still shows
/// the frames are the animation rather than background churn.
#[test]
#[ignore = "live lab: spawns a real server and measures wall-clock frame timing"]
fn frame_floor_lab_idle_animation_clock() {
    let control = run_arm(
        "Z  CONTROL: no animation configured",
        "z",
        "onboarding = false\n",
        3,
        false,
    );
    let default_pulse = run_arm(
        "A  pulse (100 ms tier), key absent — the new default",
        "a",
        PULSE_ROWS,
        3,
        false,
    );
    let old_pulse = run_arm(
        "B  pulse, floor = 200 ms — the constant this change removed",
        "b",
        &with_floor(PULSE_ROWS, 200),
        3,
        false,
    );
    let explicit_pulse = run_arm(
        "C  pulse, floor = 16 ms stated explicitly",
        "c",
        &with_floor(PULSE_ROWS, 16),
        3,
        false,
    );
    let ceiling_pulse = run_arm(
        "Y  pulse, floor = 1000 ms (the clamp ceiling)",
        "y",
        &with_floor(PULSE_ROWS, 1000),
        3,
        false,
    );
    let default_shimmer = run_arm(
        "D  shimmer (50 ms tier), key absent",
        "d",
        SHIMMER_ROWS,
        3,
        false,
    );

    for arm in [
        &control,
        &default_pulse,
        &old_pulse,
        &explicit_pulse,
        &ceiling_pulse,
        &default_shimmer,
    ] {
        arm.report();
    }

    assert_eq!(
        control.frames, 0,
        "a server with nothing animating must push no frames at all, or every \
         other number here is background churn rather than the animation"
    );
    assert!(
        default_pulse.fps() > 5.0,
        "the animation must clear 5 fps on the default config, got {:.2}",
        default_pulse.fps()
    );
    assert!(
        default_shimmer.fps() > 5.0,
        "and so must the 50 ms tier, got {:.2}",
        default_shimmer.fps()
    );

    // The finding. Every floor lands in the same place because the loop is
    // already free-running at MIN_RENDER_INTERVAL whenever anything animates.
    let spread = [&default_pulse, &old_pulse, &explicit_pulse, &ceiling_pulse]
        .iter()
        .map(|arm| arm.fps())
        .fold((f64::MAX, f64::MIN), |(lo, hi), fps| {
            (lo.min(fps), hi.max(fps))
        });
    println!(
        "\n  >>> floors of 16 / 200 / 1000 ms span {:.2}-{:.2} fps.\n  >>> The floor now raises each element's own frame interval, so it reaches\n  >>> the change signal rather than only a deadline that never wins. Raising\n  >>> it is what a host too small for a behaviour's natural cadence has.\n",
        spread.0, spread.1
    );
    assert!(
        ceiling_pulse.fps() * 2.0 < default_pulse.fps(),
        "a 1000 ms floor must be clearly slower than the default, saw {:.2} against {:.2}",
        ceiling_pulse.fps(),
        default_pulse.fps()
    );
    assert!(
        old_pulse.fps() < default_pulse.fps(),
        "a 200 ms floor must be slower than the pulse's own 100 ms tier, saw {:.2} against {:.2}",
        old_pulse.fps(),
        default_pulse.fps()
    );
}

/// Frame-time distribution under a fleet with agents reported.
///
/// Not about the floor. This is the shape of the frame time actually delivered
/// to an attached client — worst case and tail, not a mean.
#[test]
#[ignore = "live lab: spawns a real server and measures wall-clock frame timing"]
fn frame_floor_lab_frame_time_distribution() {
    let arm = run_arm(
        "F  12 spaces, agents reported, default config",
        "f",
        PULSE_ROWS,
        12,
        true,
    );
    arm.report();
    assert!(
        arm.frames > 100,
        "not enough samples for a tail: {}",
        arm.frames
    );
}

/// The same realistic tree, with the notification tray's eight badges drawn as
/// animated artwork.
///
/// The tray needs the graphics path to draw badges at all, so `kitty_graphics`
/// is on in both arms — otherwise the "with" arm would be measuring the
/// difference between artwork and character marks rather than the difference
/// between still artwork and moving artwork, which is the question.
const TRAY_STILL: &str = r#"
onboarding = false

[experimental]
kitty_graphics = true

[ui.sidebar.spaces]
rows = [[{ token = "workspace", emphasis = "pulse" }]]

[ui.sidebar.signal_tray]
enabled = true
animate = false
"#;

const TRAY_ANIMATED: &str = r#"
onboarding = false

[experimental]
kitty_graphics = true

[ui.sidebar.spaces]
rows = [[{ token = "workspace", emphasis = "pulse" }]]

[ui.sidebar.signal_tray]
enabled = true
animate = true
"#;

/// What eight animating badges cost, measured rather than estimated.
///
/// The standard this answers to is worst case and tail, not the mean: 60 fps is
/// the floor for stability and the 0.1% lows are what a reader actually
/// notices. Three arms, so the badges' cost is isolated from the tray's:
///
/// - **G** — the tray off entirely. The baseline the fork already ships.
/// - **H** — the tray on, badges still. Adds the artwork raster, once per state
///   change, and the layer's own upload.
/// - **I** — the tray on, badges animating. Adds a re-raster and re-upload of
///   the eight-badge layer on the badge frame tier.
///
/// Deliberately not asserting a fixed millisecond figure. The numbers move with
/// the machine; what the assertions hold is the shape — enough samples for a
/// tail to mean anything, and the animated arm not collapsing against the still
/// one.
#[test]
#[ignore = "live lab: spawns a real server and measures wall-clock frame timing"]
fn frame_floor_lab_animated_tray_badges() {
    let without = run_arm("G  12 spaces, no tray", "g", PULSE_ROWS, 12, true);
    let still = run_arm(
        "H  12 spaces, tray on, badges still",
        "h",
        TRAY_STILL,
        12,
        true,
    );
    let animated = run_arm(
        "I  12 spaces, tray on, badges animating",
        "i",
        TRAY_ANIMATED,
        12,
        true,
    );

    for arm in [&without, &still, &animated] {
        arm.report();
    }
    println!(
        "\n  >>> fps  no tray {:.2} | still badges {:.2} | animating badges {:.2}\n  >>> p99  no tray {:.2} ms | still {:.2} ms | animating {:.2} ms\n  >>> worst no tray {:.2} ms | still {:.2} ms | animating {:.2} ms\n",
        without.fps(),
        still.fps(),
        animated.fps(),
        without.percentile(0.99),
        still.percentile(0.99),
        animated.percentile(0.99),
        without.gaps_ms.iter().cloned().fold(f64::NAN, f64::max),
        still.gaps_ms.iter().cloned().fold(f64::NAN, f64::max),
        animated.gaps_ms.iter().cloned().fold(f64::NAN, f64::max),
    );

    assert!(
        animated.frames > 100,
        "not enough samples for a tail: {}",
        animated.frames
    );
    assert!(
        animated.fps() > without.fps() * 0.85,
        "eight animating badges cost more than a seventh of the frame rate: {:.2} against {:.2}",
        animated.fps(),
        without.fps()
    );
}

/// The tree's cards, still and moving.
///
/// The cards need the graphics path *and* `sidebar_card_shapes` to be drawn as
/// pixels at all, so both are on in every arm — otherwise the "with" arm would
/// be measuring the difference between pixel cards and character rows rather
/// than the difference between still cards and breathing ones, which is the
/// question.
const CARDS_STILL: &str = r#"
onboarding = false

[experimental]
kitty_graphics = true
sidebar_card_shapes = true

[ui.sidebar.spaces]
rows = [[{ token = "workspace", emphasis = "pulse" }]]

[ui.sidebar.cards]
pulse = false
wash = false
"#;

const CARDS_BREATHING: &str = r#"
onboarding = false

[experimental]
kitty_graphics = true
sidebar_card_shapes = true

[ui.sidebar.spaces]
rows = [[{ token = "workspace", emphasis = "pulse" }]]

[ui.sidebar.cards]
pulse = true
wash = true
"#;

/// What a full tree of breathing cards costs, measured rather than estimated.
///
/// The standard this answers to is worst case and tail, not the mean: 60 fps is
/// the floor for stability and the 0.1% lows are what a reader actually
/// notices. A breath is a *per-card, per-frame* cost — the one shape of change
/// that shows up in the tail rather than in the average — so the arms isolate
/// it from the pixel-card path it rides on:
///
/// - **J** — pixel cards drawn, nothing about them moving. The baseline.
/// - **K** — the same tree with every card breathing.
///
/// Deliberately not asserting a fixed millisecond figure; the numbers move with
/// the machine. What the assertions hold is the shape — enough samples for a
/// tail to mean anything, and the breathing arm not collapsing against the
/// still one.
#[test]
#[ignore = "live lab: spawns a real server and measures wall-clock frame timing"]
fn frame_floor_lab_breathing_cards() {
    let still = run_arm(
        "J  12 spaces, pixel cards, still",
        "j",
        CARDS_STILL,
        12,
        true,
    );
    let breathing = run_arm(
        "K  12 spaces, pixel cards, breathing",
        "k",
        CARDS_BREATHING,
        12,
        true,
    );

    for arm in [&still, &breathing] {
        arm.report();
    }
    println!(
        "\n  >>> fps   still {:.2} | breathing {:.2}\n  >>> p99   still {:.2} ms | breathing {:.2} ms\n  >>> 0.1%  still {:.2} ms | breathing {:.2} ms\n  >>> worst still {:.2} ms | breathing {:.2} ms\n",
        still.fps(),
        breathing.fps(),
        still.percentile(0.99),
        breathing.percentile(0.99),
        still.percentile(0.999),
        breathing.percentile(0.999),
        still.gaps_ms.iter().cloned().fold(f64::NAN, f64::max),
        breathing.gaps_ms.iter().cloned().fold(f64::NAN, f64::max),
    );

    assert!(
        breathing.frames > 100,
        "not enough samples for a tail: {}",
        breathing.frames
    );
    assert!(
        breathing.fps() > still.fps() * 0.85,
        "a breathing tree cost more than a seventh of the frame rate: {:.2} against {:.2}",
        breathing.fps(),
        still.fps()
    );
}
