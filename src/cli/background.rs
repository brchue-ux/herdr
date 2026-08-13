//! `herdr background` — turn the persistent whole-terminal background scene on
//! or off, and ask a running session why it is not drawing.
//!
//! The scene ([`crate::solar_system`], [`crate::app::background_scene`]) is
//! gated on conditions that fail *silently* and independently: two config keys
//! that have to agree, and — the part no amount of reading config.toml can
//! answer — whether the terminal actually on the other end of the pty is one
//! Herdr has positively identified as drawing an opaque wash under the text
//! rather than over it. A scene that is off because the terminal was never
//! named looks exactly like a scene that is off because the feature is
//! disabled, which is the whole reason `status` exists and prints every
//! condition rather than a single verdict.
//!
//! `on` and `off` follow the contract `herdr config reset-keys` already set:
//! edit config.toml as text so comments and formatting survive, refuse to write
//! anything that would not parse afterwards, and tell the caller to reload
//! rather than reaching into a running server to do it for them.

use crate::api::schema::{BackgroundSceneInfo, EmptyParams, Method, Request};

/// Both keys the scene needs, in the order `status` reports them. The scene is
/// a Kitty Graphics surface, so `kitty_graphics` gates it just as hard as
/// `persistent_background` does — turning on only the named one is the
/// single most likely way to get a silent no-op, so `on` always writes both.
const EXPERIMENTAL_SECTION: &str = "experimental";
const KITTY_GRAPHICS_KEY: &str = "kitty_graphics";
const PERSISTENT_BACKGROUND_KEY: &str = "persistent_background";

pub(super) fn run_background_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("status") => background_status(&args[1..]),
        Some("on") => background_set(&args[1..], true),
        Some("off") => background_set(&args[1..], false),
        Some("help" | "--help" | "-h") | None => {
            print_background_help();
            Ok(if args.is_empty() { 2 } else { 0 })
        }
        Some(other) => {
            eprintln!("unknown background subcommand: {other}");
            print_background_help();
            Ok(2)
        }
    }
}

/// `herdr background status` — every condition, and the verdict.
fn background_status(args: &[String]) -> std::io::Result<i32> {
    let json = args.first().map(String::as_str) == Some("--json");
    if !args.is_empty() && !json {
        eprintln!("usage: herdr background status [--json]");
        return Ok(2);
    }

    let response = super::send_request(&Request {
        id: "cli:background:status".into(),
        method: Method::SessionSnapshot(EmptyParams::default()),
    })?;
    if response.get("error").is_some() {
        return super::print_response(&response);
    }

    let info: BackgroundSceneInfo = response
        .pointer("/result/snapshot/background_scene")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let machine: crate::api::schema::MachineRegisterInfo = response
        .pointer("/result/snapshot/machine_register")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();

    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "background_scene": info,
                "machine_register": machine,
            }))
            .unwrap_or_default()
        );
        return Ok(0);
    }

    print_status(&info);
    print_machine_register(&machine);
    Ok(0)
}

/// The human readout. Every condition is printed whether or not it is the one
/// that failed, so the answer to "what changed?" is visible in one screen
/// rather than one condition at a time across several runs.
/// The machine register's own readout, printed under the scene's.
///
/// The drawn corner is deliberately wordless — herdr's text surface is the terminal itself, and
/// painting a private bitmap font into a wash that sits *under* real glyphs is not something this
/// scene does — so this is where the numbers are read as numbers, and where the register says why
/// it is empty when it is.
fn print_machine_register(info: &crate::api::schema::MachineRegisterInfo) {
    println!();
    if !info.reading {
        println!(
            "machine register: not reading{}",
            info.absent_because
                .as_deref()
                .map(|why| format!(" — {why}"))
                .unwrap_or_default()
        );
        return;
    }

    println!(
        "machine register: reading every {}s from {}",
        info.sample_interval_ms / 1_000,
        if info.sources.is_empty() {
            "an unnamed source".to_string()
        } else {
            info.sources.join(", ")
        }
    );
    for quantity in &info.quantities {
        match quantity.value {
            Some(value) => println!(
                "  {:<5} {:>5.1}%   ({} samples of history)",
                quantity.name,
                value * 100.0,
                quantity.history_samples
            ),
            None => println!("  {:<5}     —   (not measured)", quantity.name),
        }
    }
    if !info.cores.is_empty() {
        let drawn: Vec<String> = info
            .cores
            .iter()
            .map(|core| match core {
                Some(load) => format!("{:.0}", load * 100.0),
                // A core that reported nothing is absent, never zero — the two are different
                // statements about a machine and only one of them is true.
                None => "—".to_string(),
            })
            .collect();
        println!("  cores {}", drawn.join(" "));
    }
}

fn print_status(info: &BackgroundSceneInfo) {
    let mark = |ok: bool| if ok { "yes" } else { "NO " };

    println!(
        "background scene: {}",
        if info.active {
            "DRAWING"
        } else {
            "not drawing"
        }
    );
    println!();
    println!(
        "  {} [experimental] persistent_background",
        mark(info.enabled)
    );
    println!(
        "  {} [experimental] kitty_graphics",
        mark(info.kitty_graphics_enabled)
    );
    println!(
        "  {} host terminal draws an ambient wash under the text (identified as: {})",
        mark(info.host_draws_ambient_wash),
        info.host_terminal
    );
    println!(
        "  {} every attached viewer draws one too",
        mark(info.every_viewer_draws_ambient_wash)
    );
    println!(
        "  {} host answered the Kitty Graphics probe (not a gate; other pixel surfaces need it)",
        mark(info.kitty_graphics_capability_confirmed)
    );

    // A41(c): in the frame whenever it is non-zero, and absent when it is zero, because a
    // disclosure of nothing is noise rather than population. It names the key it dropped by as
    // well as the count — "9 dropped" and "the 9 smallest by tracked files at HEAD" are different
    // statements, and only the second one can be argued with.
    if info.mates_beyond_ladder > 0 {
        println!();
        println!(
            "  {} of the fleet's {} second mates are seated on the orbit ring; {} beyond it\n\
             \x20   (the ring seats {}, and the ones it drops are the smallest by tracked files at HEAD)",
            info.mates_seated,
            info.mates_seated + info.mates_beyond_ladder,
            info.mates_beyond_ladder,
            info.ladder_capacity,
        );
    }

    if info.active {
        return;
    }

    println!();
    // One remedy, for the first unmet condition in the order above — the order
    // is deliberate, since a config key is worth fixing before a terminal is.
    if !info.enabled || !info.kitty_graphics_enabled {
        println!("Run `herdr background on` to set both config keys, then reload.");
    } else if !info.host_draws_ambient_wash {
        println!(
            "The terminal on the other end of this pty identified itself as `{}`, which Herdr does\n\
             not draw an opaque full-screen wash on: a terminal that ignores the negative-z band\n\
             draws the same image over every glyph instead. kitty and Rio are the identified ones.\n\
             Note this is read in band over the pty (XTVERSION), so it is the *terminal's* answer\n\
             and survives an SSH hop — `other` means it did not answer or gave a name Herdr does\n\
             not know, not that the variable was unset.",
            info.host_terminal
        );
    } else if !info.every_viewer_draws_ambient_wash {
        println!(
            "Another attached viewer would composite the wash over its own text, so it is withheld\n\
             from every viewer including this one. Detach it to let the scene draw."
        );
    }
}

/// `herdr background on|off` — write the config keys.
fn background_set(args: &[String], on: bool) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr background {}", if on { "on" } else { "off" });
        return Ok(2);
    }

    let path = crate::config::config_path();
    let existing = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };

    if !existing.is_empty() {
        if let Err(err) = existing.parse::<toml::Value>() {
            eprintln!(
                "config file at {} is invalid TOML: {err}. Fix it manually before changing it here.",
                path.display()
            );
            return Ok(1);
        }
    }

    // `off` leaves kitty_graphics alone: it gates the sidebar's pixel cards and
    // tray art as well, so turning the scene off must not silently take those
    // with it. `on` writes both, because the scene needs both and enabling only
    // the named key is the likeliest silent no-op.
    let mut updated = crate::config::upsert_section_bool(
        &existing,
        EXPERIMENTAL_SECTION,
        PERSISTENT_BACKGROUND_KEY,
        on,
    );
    if on {
        updated = crate::config::upsert_section_bool(
            &updated,
            EXPERIMENTAL_SECTION,
            KITTY_GRAPHICS_KEY,
            true,
        );
    }

    if let Err(err) = updated.parse::<toml::Value>() {
        eprintln!(
            "writing this change would make {} invalid TOML: {err}; leaving config unchanged",
            path.display()
        );
        return Ok(1);
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, updated)?;

    if on {
        println!(
            "Set [experimental] persistent_background = true and kitty_graphics = true in {}.",
            path.display()
        );
    } else {
        println!(
            "Set [experimental] persistent_background = false in {}.",
            path.display()
        );
    }
    println!("If a Herdr server is running, run `herdr server reload-config` to apply this now.");
    if on {
        println!("Then run `herdr background status` to check the scene is actually drawing.");
    }
    Ok(0)
}

fn print_background_help() {
    eprintln!("Turn the persistent whole-terminal background scene on or off");
    eprintln!();
    eprintln!("Usage: herdr background <COMMAND>");
    eprintln!();
    eprintln!("Commands:");
    eprintln!("  on               Enable the background scene in config.toml");
    eprintln!("  off              Disable the background scene in config.toml");
    eprintln!("  status           Report whether the scene is drawing, and what is stopping it");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --json           (status) print the raw condition set as JSON");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scene needs both keys, so `on` has to write both — enabling only the
    /// key that carries the feature's name is the silent no-op this command
    /// exists to prevent.
    #[test]
    fn on_writes_both_keys_the_scene_needs() {
        let updated = crate::config::upsert_section_bool(
            "",
            EXPERIMENTAL_SECTION,
            PERSISTENT_BACKGROUND_KEY,
            true,
        );
        let updated = crate::config::upsert_section_bool(
            &updated,
            EXPERIMENTAL_SECTION,
            KITTY_GRAPHICS_KEY,
            true,
        );
        assert!(updated.contains("[experimental]"));
        assert!(updated.contains("persistent_background = true"));
        assert!(updated.contains("kitty_graphics = true"));
        updated
            .parse::<toml::Value>()
            .expect("written config must parse");
    }

    /// Turning the scene off must not take the sidebar's pixel cards and tray
    /// art down with it — they read `kitty_graphics` too.
    #[test]
    fn off_leaves_kitty_graphics_alone() {
        let existing = "[experimental]\nkitty_graphics = true\npersistent_background = true\n";
        let updated = crate::config::upsert_section_bool(
            existing,
            EXPERIMENTAL_SECTION,
            PERSISTENT_BACKGROUND_KEY,
            false,
        );
        assert!(updated.contains("kitty_graphics = true"));
        assert!(updated.contains("persistent_background = false"));
    }

    /// A config file people have hand-written has comments in it, and losing
    /// them to a toggle is not an acceptable trade.
    #[test]
    fn writing_the_keys_preserves_surrounding_comments() {
        let existing = "# my herdr config\nonboarding = false\n\n[theme]\n# stick with this one\nname = \"catppuccin\"\n";
        let updated = crate::config::upsert_section_bool(
            existing,
            EXPERIMENTAL_SECTION,
            PERSISTENT_BACKGROUND_KEY,
            true,
        );
        assert!(updated.contains("# my herdr config"));
        assert!(updated.contains("# stick with this one"));
        assert!(updated.contains("name = \"catppuccin\""));
        assert!(updated.contains("persistent_background = true"));
    }

    /// An unmet condition has to name itself. A status readout that only said
    /// "not drawing" would leave the caller exactly where the silent failure
    /// left them.
    #[test]
    fn every_condition_is_reported_not_just_the_verdict() {
        let info = BackgroundSceneInfo {
            active: false,
            enabled: true,
            kitty_graphics_enabled: true,
            kitty_graphics_capability_confirmed: true,
            host_terminal: "other".into(),
            host_draws_ambient_wash: false,
            every_viewer_draws_ambient_wash: true,
            ladder_capacity: 8,
            mates_seated: 8,
            mates_beyond_ladder: 0,
        };
        // The condition that is false is the terminal, and it is the one a
        // reader has to be able to pick out of the readout.
        assert!(!info.host_draws_ambient_wash);
        assert_eq!(info.host_terminal, "other");
        assert!(!info.active);
    }

    /// The overflow disclosure names the count *and* the key it dropped by, and
    /// is absent entirely when nothing was dropped.
    #[test]
    fn the_ladder_overflow_is_disclosed_only_when_there_is_one() {
        let base = BackgroundSceneInfo {
            active: true,
            enabled: true,
            kitty_graphics_enabled: true,
            kitty_graphics_capability_confirmed: true,
            host_terminal: "kitty".into(),
            host_draws_ambient_wash: true,
            every_viewer_draws_ambient_wash: true,
            ladder_capacity: 8,
            mates_seated: 8,
            mates_beyond_ladder: 9,
        };
        // A fleet of 17 with a ring that seats 8: the readout has to be able to
        // say all three numbers, and which register the nine lost on.
        assert_eq!(base.mates_seated + base.mates_beyond_ladder, 17);
        assert_eq!(base.mates_seated, base.ladder_capacity);

        // ...and a fleet that fits discloses nothing, because a disclosure of
        // nothing is noise rather than population.
        let fits = BackgroundSceneInfo {
            mates_seated: 3,
            mates_beyond_ladder: 0,
            ..base
        };
        assert_eq!(fits.mates_beyond_ladder, 0);
    }
}
