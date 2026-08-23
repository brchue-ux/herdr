use super::*;

fn remote_manifest(version: &str, state: &str, contains: &str) -> String {
    format!(
        r#"
id = "codex"
version = "{version}"
min_engine_version = 1
updated_at = "2026-06-10T12:00:00Z"

[[rules]]
id = "test"
state = "{state}"
contains = ["{contains}"]
"#
    )
}

fn local_manifest(state: &str, contains: &str) -> String {
    format!(
        r#"
id = "codex"

[[rules]]
id = "test"
state = "{state}"
contains = ["{contains}"]
"#
    )
}

fn rules_manifest(rules: &str) -> String {
    format!(
        r#"
id = "codex"

{rules}
"#
    )
}

fn with_manifest_dirs<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let _guard = crate::config::test_config_env_lock().lock().unwrap();
    let old_config = std::env::var_os("XDG_CONFIG_HOME");
    let old_state = std::env::var_os("XDG_STATE_HOME");
    let base = std::env::temp_dir().join(format!(
        "herdr-manifest-loader-{name}-{}",
        std::process::id()
    ));
    let config_dir = base.join("config");
    let state_dir = base.join("state");
    let _ = std::fs::remove_dir_all(&base);
    std::env::set_var("XDG_CONFIG_HOME", &config_dir);
    std::env::set_var("XDG_STATE_HOME", &state_dir);
    reload_manifests();
    let result = f();
    match old_config {
        Some(value) => std::env::set_var("XDG_CONFIG_HOME", value),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    match old_state {
        Some(value) => std::env::set_var("XDG_STATE_HOME", value),
        None => std::env::remove_var("XDG_STATE_HOME"),
    }
    reload_manifests();
    let _ = std::fs::remove_dir_all(&base);
    result
}

fn write_remote_codex(content: &str) {
    let path = crate::detect::manifest_update::remote_manifest_path(Agent::Codex);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    reload_manifests();
}

fn write_remote_codex_without_reload(content: &str) {
    let path = crate::detect::manifest_update::remote_manifest_path(Agent::Codex);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn write_local_codex(content: &str) {
    let path = override_path(Agent::Codex).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
    reload_manifests();
}

#[test]
fn known_agent_no_match_defaults_to_idle_fallback() {
    let explain = explain(Agent::Codex, "ordinary prompt text");

    assert_eq!(explain.state, AgentState::Idle);
    assert!(!explain.visible_idle);
    assert_eq!(
        explain.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
}

#[test]
fn rule_semantics_apply_gates_priority_and_line_regex() {
    with_manifest_dirs("rule-semantics", || {
        write_local_codex(&rules_manifest(
            r#"
[[rules]]
id = "low_contains"
state = "idle"
priority = 1
contains = ["match"]

[[rules]]
id = "high_nested_gates"
state = "working"
priority = 10
contains = ["match"]
all = [
  { any = [{ regex = ["w[io]n"] }, { contains = ["fallback"] }] },
]
not = [
  { contains = ["blocked"] },
]

[[rules]]
id = "line_regex"
state = "blocked"
priority = 20
line_regex = ["^exact line$"]
"#,
        ));

        let high = explain(Agent::Codex, "match win");
        assert_eq!(high.state, AgentState::Working);
        assert_eq!(
            high.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("high_nested_gates")
        );

        let not_gate = explain(Agent::Codex, "match win blocked");
        assert_eq!(not_gate.state, AgentState::Idle);
        assert_eq!(
            not_gate.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("low_contains")
        );

        let line = explain(Agent::Codex, "before\nexact line\nafter");
        assert_eq!(line.state, AgentState::Blocked);
        assert_eq!(
            line.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("line_regex")
        );
    });
}

#[test]
fn remote_manifest_loads_between_local_override_and_bundled() {
    with_manifest_dirs("remote-source", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Blocked);
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
        assert_eq!(explain.manifest_version.as_deref(), Some("9999.01.01.1"));
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );
    });
}

#[test]
fn fallback_explain_preserves_active_manifest_version() {
    with_manifest_dirs("fallback-version", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "ordinary prompt text");

        assert_eq!(explain.state, AgentState::Idle);
        assert_eq!(
            explain.fallback_reason.as_deref(),
            Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
        );
        assert_eq!(explain.manifest_version.as_deref(), Some("9999.01.01.1"));
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
    });
}

#[test]
fn older_cached_remote_manifest_does_not_shadow_newer_bundled_manifest() {
    with_manifest_dirs("older-remote-bundled-fallback", || {
        write_remote_codex(&remote_manifest("2026.06.10.0", "blocked", "remote-ready"));

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Idle);
        assert!(matches!(explain.source, Some(ManifestSource::Bundled)));
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("2026.06.10.0")
        );
        assert!(explain
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("older than bundled")));
    });
}

#[test]
fn local_override_shadows_cached_remote_manifest() {
    with_manifest_dirs("local-shadows-remote", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));
        write_local_codex(&local_manifest("idle", "local-ready"));

        let explain = explain(Agent::Codex, "local-ready");

        assert_eq!(explain.state, AgentState::Idle);
        assert!(matches!(explain.source, Some(ManifestSource::Override(_))));
        assert!(explain.local_override_shadowing_remote);
        assert_eq!(
            explain.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );
    });
}

#[test]
fn invalid_local_override_falls_back_to_cached_remote_manifest() {
    with_manifest_dirs("invalid-local-remote-fallback", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));
        write_local_codex("id = ");

        let explain = explain(Agent::Codex, "remote-ready");

        assert_eq!(explain.state, AgentState::Blocked);
        assert!(matches!(
            explain.source,
            Some(ManifestSource::Remote { .. })
        ));
        assert!(explain.warning.is_some());
    });
}

#[test]
fn detection_uses_cached_manifest_until_explicit_reload() {
    with_manifest_dirs("cache-boundary", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "cached-ready"));

        let cached = explain(Agent::Codex, "cached-ready");
        assert_eq!(cached.state, AgentState::Blocked);
        assert!(matches!(cached.source, Some(ManifestSource::Remote { .. })));
        assert_eq!(
            cached.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("test")
        );

        write_remote_codex_without_reload(&remote_manifest("9999.01.01.2", "working", "new-ready"));

        let unchanged = explain(Agent::Codex, "new-ready");
        assert_eq!(unchanged.state, AgentState::Idle);
        assert_eq!(
            unchanged.fallback_reason.as_deref(),
            Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
        );
        assert_eq!(
            unchanged.cached_remote_version.as_deref(),
            Some("9999.01.01.1")
        );

        reload_manifests();

        let reloaded = explain(Agent::Codex, "new-ready");
        assert_eq!(reloaded.state, AgentState::Working);
        assert_eq!(
            reloaded.cached_remote_version.as_deref(),
            Some("9999.01.01.2")
        );
        assert_eq!(
            reloaded.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("test")
        );
    });
}

#[test]
fn all_bundled_manifests_parse_and_validate() {
    for agent in Agent::SCREEN_MANIFEST_AGENTS {
        assert!(
            bundled_manifest(agent).is_some(),
            "missing bundled manifest for {}",
            agent_label(agent)
        );
    }
}

#[test]
fn devin_manifest_detects_idle_working_and_blocked_states() {
    let idle = explain(
        Agent::Devin,
        "─────────────────────────────────────────────────────\n❭ Ask Devin to build features, fix bugs, or work on\n  your code\n─────────────────────────────────────────────────────\nSWE-1.6               Context: 16k / 200k tokens (7%)",
    );
    assert_eq!(idle.state, AgentState::Idle);
    assert!(idle.visible_idle);

    let live_footer_idle = explain(
        Agent::Devin,
        "Done.\n\n────────────────────────────────────────────────── (bypass permissions on) ─\n❭\n────────────────────────────────────────────────────────────────────────────\nClaude Opus 4.6 Thinking                                    Context: 38k / 200k tokens (18%)",
    );
    assert_eq!(live_footer_idle.state, AgentState::Idle);
    assert_eq!(
        live_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("live_prompt_footer")
    );
    assert!(live_footer_idle.visible_idle);

    let welcome_footer_idle = explain(
        Agent::Devin,
        "⠀⠀⠀⠀⠀⣴⣾⣶⡄⠀⠀⠀⠀\n⠀⣴⣾⣶⡾⠛⠿⠟⠃⣴⣾⣶⡄  Devin CLI\n⠀⠛⠿⠟⠃⣴⣾⣶⡾⠛⠿⠟⠃  v2026.5.26-8\n⠀⣤⣶⣦⡄⠻⢿⠿⢷⣤⣶⣦⡄\n⠀⠻⢿⠿⢷⣤⣶⣦⡄⠻⢿⠿⠃  Hybrid\n⠀⠀⠀⠀⠀⠻⢿⠿⠃⠀⠀⠀⠀\n\n───────────────────────────\n❭ Ask Devin to build\n  features, fix bugs, or\n  work on your code\n───────────────────────────\nClaude Opus Looking for\n4.6 Thinkingplan mode? /\n            plan",
    );
    assert_eq!(welcome_footer_idle.state, AgentState::Idle);
    assert_eq!(
        welcome_footer_idle
            .matched_rule
            .as_ref()
            .map(|rule| rule.id.as_str()),
        Some("welcome_prompt_footer")
    );
    assert!(welcome_footer_idle.visible_idle);

    let working = explain(
        Agent::Devin,
        "◔ Reading shell 91b655\n  │ Timeout: 35s\n\n⠀⡆ Running tools · 27s (esc to interrupt)\n─────────────────────────────────────────────────────\n❭ Guide Devin while it works",
    );
    assert_eq!(working.state, AgentState::Working);
    assert!(working.visible_working);

    let trust_prompt = explain(
        Agent::Devin,
        "Do you trust the authors of this directory?\nFor security, devin should not be run in directories\nwith untrusted content.\n❭ 1 Yes, trust /private/tmp/devin-hook-probe\n· 2 No, exit",
    );
    assert_eq!(trust_prompt.state, AgentState::Blocked);
    assert!(trust_prompt.visible_blocker);

    let permission_prompt = explain(
        Agent::Devin,
        "⏺ Running command\n  └ $ sleep 30\n\n❭ 1 Yes  (Approve once)\n· 2 Yes, allow `sleep` commands\n· 3 Yes, always allow `sleep` commands\n· 4 No\n↑↓ select · ↵ confirm · esc cancel",
    );
    assert_eq!(permission_prompt.state, AgentState::Blocked);
    assert!(permission_prompt.visible_blocker);
}

#[test]
fn manifest_validation_rejects_unknown_fields_empty_rules_invalid_regions_and_regexes() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "typo"
state = "working"
contain = ["Working"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "empty"
state = "working"
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_region"
state = "working"
region = "after_last_promt_marker"
contains = ["Working"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_regex"
state = "working"
regex = ["["]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_nested_regex"
state = "working"
any = [{ line_regex = ["["] }]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_keeps_skip_rules_neutral() {
    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_state"
state = "idle"
skip_state_update = true
contains = ["menu"]
"#
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"

[[rules]]
id = "bad_skip_visible"
state = "unknown"
skip_state_update = true
visible_blocker = true
contains = ["menu"]
"#
    )
    .is_err());
}

#[test]
fn manifest_validation_rejects_excessive_rule_count() {
    let mut manifest = String::from(
        r#"
id = "codex"
"#,
    );
    for index in 0..129 {
        manifest.push_str(&format!(
            r#"
[[rules]]
id = "rule_{index}"
state = "idle"
contains = ["ready"]
"#
        ));
    }

    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_gate_depth() {
    let manifest = r#"
id = "codex"

[[rules]]
id = "deep"
state = "idle"
contains = ["ready"]
all = [
  { contains = ["1"], all = [
    { contains = ["2"], all = [
      { contains = ["3"], all = [
        { contains = ["4"], all = [
          { contains = ["5"], all = [
            { contains = ["6"], all = [
              { contains = ["7"], all = [
                { contains = ["8"], all = [
                  { contains = ["9"] },
                ] },
              ] },
            ] },
          ] },
        ] },
      ] },
    ] },
  ] },
]
"#;

    assert!(parse_manifest(manifest).is_err());
}

#[test]
fn manifest_validation_rejects_excessive_matchers() {
    let matchers = (0..33)
        .map(|index| format!(r#""m{index}""#))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        r#"
id = "codex"

[[rules]]
id = "many"
state = "idle"
contains = [{matchers}]
"#
    );

    assert!(parse_manifest(&manifest).is_err());
}

#[test]
fn bottom_non_empty_lines_uses_bottom_occurrence_for_repeated_text() {
    let content = "marker\nold\n\nmiddle\nmarker\nnew\n";

    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: "",
            },
            "bottom_non_empty_lines(2)"
        ),
        "marker\nnew\n"
    );
}

#[test]
fn top_non_empty_lines_uses_top_occurrence_for_repeated_text() {
    let content = "\nmarker\nold\n\nmiddle\nmarker\nnew\n";

    assert_eq!(
        region(
            DetectionInput {
                screen: content,
                osc_title: "",
                osc_progress: "",
            },
            "top_non_empty_lines(2)"
        ),
        "\nmarker\nold\n"
    );
}

#[test]
fn top_non_empty_lines_requires_a_canonical_positive_bounded_count() {
    let name = "top_non_empty_lines";
    assert!(validate_region_name(&format!("{name}(1)")).is_ok());
    assert!(validate_region_name(&format!("{name}({})", u16::MAX)).is_ok());
    for count in ["0", "01", "+1", "65536", "999999999999999999999999"] {
        assert!(
            validate_region_name(&format!("{name}({count})")).is_err(),
            "{name} accepted invalid count {count}"
        );
    }
}

#[test]
fn top_non_empty_lines_requires_engine_three_when_declared() {
    let manifest = r#"
id = "grok"
version = "1"
min_engine_version = 2

[[rules]]
id = "background"
state = "working"
region = " top_non_empty_lines(1) "
contains = ["active"]
"#;

    assert!(parse_manifest(manifest).is_err());
}

// ---------------------------------------------------------------------------
// OSC rule tests — exercise the new osc_title / osc_progress regions against
// the bundled Claude and Codex manifests.
// ---------------------------------------------------------------------------

fn osc_explain(
    agent: Agent,
    screen: &str,
    osc_title: &str,
    osc_progress: &str,
) -> DetectionExplain {
    explain_with_input(
        agent,
        DetectionInput {
            screen,
            osc_title,
            osc_progress,
        },
    )
}

// --- Claude OSC rules ---

#[test]
fn claude_osc_title_braille_prefix_is_working() {
    // "⠂" is U+2802, in the braille block U+2800-U+28FF
    let result = osc_explain(Agent::Claude, "", "⠂ project", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn claude_osc_title_half_circle_frames_are_working() {
    for frame in ['◐', '◓', '◑', '◒'] {
        let title = format!("{frame} Initial conversation with Claude");
        let result = osc_explain(Agent::Claude, "", &title, "");
        assert_eq!(result.state, AgentState::Working, "frame {frame}");
        assert_eq!(
            result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
            Some("osc_title_working"),
            "frame {frame}"
        );
        assert!(result.visible_working, "frame {frame}");
    }
}

#[test]
fn claude_osc_title_static_prefix_is_idle() {
    // "✳" is U+2733, static prefix when Claude is not working
    let result = osc_explain(Agent::Claude, "", "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn claude_osc_progress_4_3_alone_does_not_force_working() {
    // Claude leaves progress stuck at 4;3 while waiting for permission, so
    // 4;3 must not be a working signal on its own. With no other evidence it
    // falls back to idle; blocked screen rules can win when present.
    let result = osc_explain(Agent::Claude, "", "", "4;3;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_working);
}

#[test]
fn claude_blocker_screen_outranks_stale_osc_progress() {
    // Regression: progress 4;3 persists during permission prompts. The
    // blocked form on screen must win because no rule treats 4;3 as working.
    let blocker_screen =
        "──────────\n  1. Yes\n  2. No\n\nEnter to select · ↑/↓ to navigate · Esc to cancel\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Task title", "4;3;");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_osc_progress_4_0_is_idle() {
    let result = osc_explain(Agent::Claude, "", "", "4;0;");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_progress_idle")
    );
}

#[test]
fn claude_blocker_screen_outranks_osc_idle_title() {
    // When the OSC title shows ✳ (idle) but the screen has a bash permission
    // prompt, the blocked rule at priority 850 beats osc_title_idle at 250.
    let blocker_screen = "do you want to proceed?\n\
        bash command: rm -rf /tmp/test\n\
        ❯ 1. Yes\n   2. No\n\n\
        Esc to cancel · Tab to amend · ctrl+e to explain\n";
    let result = osc_explain(Agent::Claude, blocker_screen, "✳ Claude Code", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert!(result.visible_blocker);
}

#[test]
fn claude_empty_osc_empty_screen_is_idle_fallback() {
    // No OSC data, no matching screen rule → fallback idle (unchanged V3 behavior)
    let result = osc_explain(Agent::Claude, "", "", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.fallback_reason.as_deref(),
        Some(DEFAULT_KNOWN_AGENT_IDLE_FALLBACK)
    );
    assert!(!result.visible_idle);
}

// --- Codex OSC rules ---

#[test]
fn codex_osc_title_braille_spinner_is_working() {
    // "⠋" is U+280B, in the braille block
    let result = osc_explain(Agent::Codex, "", "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_osc_title_action_required_is_blocked() {
    let result = osc_explain(Agent::Codex, "", "[ . ] Action Required | llm-proxy", "");
    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_blocked")
    );
    assert!(result.visible_blocker);
}

#[test]
fn codex_osc_title_plain_is_idle() {
    let result = osc_explain(Agent::Codex, "", "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn codex_trust_directory_requires_live_top_region() {
    let screen = "> You are in C:\\Users\\user\\project\n\n\
        Do you trust the contents of this\n\
        directory? Working with untrusted\n\
        contents comes with higher risk of\n\
        prompt injection. Trusting the\n\
        directory allows project-local config,\n\
        hooks, and exec policies to load.\n\n\
        › 1. Yes, continue\n\
          2. No, quit\n\n\
        Press enter to continue\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("trust_directory")
    );
    assert!(result.visible_blocker);

    let transcript = "› > You are in C:\\Users\\user\\project\n\n\
        Do you trust the contents of this\n\
        directory? Working with untrusted contents comes with higher risk.\n";
    let result = osc_explain(Agent::Codex, transcript, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_ne!(
        result.matched_rule.as_ref().map(|rule| rule.id.as_str()),
        Some("trust_directory")
    );
    assert!(!result.visible_blocker);
}

#[test]
fn codex_background_terminal_screen_does_not_override_osc_idle() {
    // Background terminal tasks can be long-lived helpers such as dev servers.
    // They should not make Codex look busy once the foreground turn is idle.
    let screen = "background terminal running · /ps to view · /stop to close\n";
    let result = osc_explain(Agent::Codex, screen, "llm-proxy", "");
    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
}

#[test]
fn codex_screen_working_fallback_handles_static_osc_title() {
    let screen = "• I’ll run it and wait for completion.\n\n\
        ◦ Working (1m 16s • esc to interrupt) · 1 background…\n\n\
        › Use /skills to list available skills\n\n\
        gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("screen_working_fallback")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_osc_working_remains_preferred_over_screen_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\n\
        › Use /skills to list available skills\n\n\
        gpt-5.6-sol default · /work\n";
    let result = osc_explain(Agent::Codex, screen, "⠸ project", "");

    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
    assert!(result.visible_working);
}

#[test]
fn codex_screen_blocker_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › 1. Yes, proceed\n\
        Press enter to confirm or esc to cancel\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("live_strong_blocker")
    );
    assert!(result.visible_blocker);
    assert!(!result.visible_working);
}

#[test]
fn codex_weak_blocker_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        do you want to continue? [y/n]\n\
        › Use /skills to list available skills\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Blocked);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("weak_blocker")
    );
    assert!(!result.visible_working);
}

#[test]
fn codex_transcript_viewer_outranks_working_fallback() {
    let screen = "• Working (4s • esc to interrupt)\n\
        › transcript\n\
        ↑/↓ to scroll · pgup/pgdn to move · home/end to jump · q to quit · esc to edit prev\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Unknown);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("transcript_viewer")
    );
    assert!(result.skip_state_update);
    assert!(!result.visible_working);
}

#[test]
fn codex_screen_working_fallback_ignores_stale_and_prompt_text() {
    let screens = [
        "◦ Working (1m 16s • esc to interrupt)\n\
         ■ Conversation interrupted\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
        "› Explain the text ◦ Working (1m 16s • esc to interrupt)\n\
         gpt-5.6-sol default · /work\n",
        "  ◦ Working (1m 16s • esc to interrupt)\n\
         › Use /skills to list available skills\n\
         gpt-5.6-sol default · /work\n",
    ];

    for screen in screens {
        let result = osc_explain(Agent::Codex, screen, "project", "");
        assert_eq!(result.state, AgentState::Idle);
        assert_eq!(
            result.matched_rule.as_ref().map(|r| r.id.as_str()),
            Some("osc_title_idle")
        );
        assert!(result.visible_idle);
        assert!(!result.visible_working);
    }
}

#[test]
fn codex_screen_working_fallback_ignores_interrupted_short_terminal() {
    let screen = "◦ Working (1m 16s • esc to interrupt)\n\
        ■ Conversation interrupted\n\
        ›\n";
    let result = osc_explain(Agent::Codex, screen, "project", "");

    assert_eq!(result.state, AgentState::Idle);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_idle")
    );
    assert!(result.visible_idle);
    assert!(!result.visible_working);
}

#[test]
fn codex_osc_working_beats_weak_blocker_screen() {
    // A stale [y/n] on screen triggers weak_blocker at priority 600, but an
    // active braille spinner in the OSC title is priority 1050 — OSC wins.
    let screen = "do you want to continue? [y/n]\n";
    let result = osc_explain(Agent::Codex, screen, "⠋ llm-proxy", "");
    assert_eq!(result.state, AgentState::Working);
    assert_eq!(
        result.matched_rule.as_ref().map(|r| r.id.as_str()),
        Some("osc_title_working")
    );
}

// ---------------------------------------------------------------------------
// transcript_region — the `transcript` read source. Never participates in state
// detection; it only tells a reader where the composer stops.
// ---------------------------------------------------------------------------

#[test]
fn bundled_claude_transcript_region_excludes_the_composer_box() {
    let screen = concat!(
        "> summarize the report\n",
        "\n",
        "  The report says three things.\n",
        "─────────────────────────────\n",
        " ❯ rm -rf /tmp/scratch\n",
        "─────────────────────────────\n",
        "  ? for shortcuts\n",
    );

    let range = transcript_line_range(Agent::Claude, screen).expect("claude transcript region");
    let lines: Vec<&str> = screen.lines().collect();
    let transcript = lines[range.clone()].join("\n");

    assert!(transcript.contains("The report says three things."));
    assert!(
        !transcript.contains("rm -rf /tmp/scratch"),
        "composer body leaked into the transcript: {transcript:?}"
    );
    assert_eq!(range.start, 0);
}

#[test]
fn bundled_codex_transcript_region_stops_at_the_current_prompt_marker() {
    let screen = concat!("• ran tests\n", "  all green\n", "› deploy to prod\n",);

    let range = transcript_line_range(Agent::Codex, screen).expect("codex transcript region");
    let transcript = screen.lines().collect::<Vec<_>>()[range].join("\n");

    assert!(transcript.contains("all green"));
    assert!(!transcript.contains("deploy to prod"));
}

#[test]
fn transcript_region_covers_everything_when_no_composer_is_on_screen() {
    let screen = "• ran tests\n  all green\n";
    let range = transcript_line_range(Agent::Codex, screen).expect("codex transcript region");
    assert_eq!(range, 0..2);
}

#[test]
fn manifest_without_transcript_region_reports_none() {
    // Only the manifests that have evidence for a composer region declare one.
    assert!(transcript_region_spec(Agent::Gemini).is_none());
    assert!(transcript_line_range(Agent::Gemini, "hello\n").is_none());
}

/// A remote-catalog manifest update is numerically newer than the bundled one
/// on an entirely independent schedule (see `older_cached_remote_manifest_does_not_shadow_newer_bundled_manifest`),
/// so it routinely predates a herdr-engine capability like `transcript_region`
/// that only this fork's bundled manifest declares — the upstream catalog has
/// no reason to know about it. Adopting the remote manifest for its (real,
/// wanted) rule improvements must not silently disable a bundled-only
/// capability it simply never mentions.
#[test]
fn remote_manifest_missing_transcript_region_is_backfilled_from_bundled() {
    with_manifest_dirs("remote-missing-transcript-region", || {
        write_remote_codex(&remote_manifest("9999.01.01.1", "blocked", "remote-ready"));

        let spec = transcript_region_spec(Agent::Codex);

        assert_eq!(spec.as_deref(), Some("before_current_prompt_marker"));
        let explain = explain(Agent::Codex, "remote-ready");
        assert!(
            matches!(explain.source, Some(ManifestSource::Remote { .. })),
            "the remote manifest's own rules must still be the active ones"
        );
    });
}

#[test]
fn local_override_missing_transcript_region_is_backfilled_from_bundled() {
    with_manifest_dirs("override-missing-transcript-region", || {
        write_local_codex(&local_manifest("idle", "local-ready"));

        let spec = transcript_region_spec(Agent::Codex);

        assert_eq!(spec.as_deref(), Some("before_current_prompt_marker"));
        let explain = explain(Agent::Codex, "local-ready");
        assert!(matches!(explain.source, Some(ManifestSource::Override(_))));
    });
}

#[test]
fn remote_manifest_declaring_its_own_transcript_region_is_not_overridden() {
    with_manifest_dirs("remote-own-transcript-region", || {
        let manifest = r#"
id = "codex"
version = "9999.01.01.1"
min_engine_version = 4
transcript_region = "before_current_prompt_marker"

[[rules]]
id = "test"
state = "blocked"
contains = ["remote-ready"]
"#;
        write_remote_codex(manifest);

        assert_eq!(
            transcript_region_spec(Agent::Codex).as_deref(),
            Some("before_current_prompt_marker")
        );
    });
}

// ---------------------------------------------------------------------------
// prompt_box_body_line_range — the terminal triview render path's live boundary
// source (Claude only).
// ---------------------------------------------------------------------------

#[test]
fn claude_prompt_box_body_line_range_excludes_both_borders() {
    let screen = concat!(
        "> summarize the report\n",
        "\n",
        "  The report says three things.\n",
        "─────────────────────────────\n",
        " ❯ rm -rf /tmp/scratch\n",
        "─────────────────────────────\n",
        "  ? for shortcuts\n",
    );

    let range =
        prompt_box_body_line_range(Agent::Claude, screen).expect("claude composer body range");
    let lines: Vec<&str> = screen.lines().collect();
    let body = lines[range].join("\n");

    assert_eq!(body, " ❯ rm -rf /tmp/scratch");
}

#[test]
fn claude_prompt_box_body_line_range_is_none_without_a_closing_border() {
    // Only one horizontal rule on screen: no second border to close the box,
    // so there is nothing safe to crop — the caller must fall back.
    let screen = concat!(
        "  some output\n",
        "─────────────────────────────\n",
        " ❯ still typing\n",
    );
    assert!(prompt_box_body_line_range(Agent::Claude, screen).is_none());
}

#[test]
fn claude_prompt_box_body_line_range_is_none_for_adjacent_borders() {
    // Two borders with nothing between them is not a real composer body.
    let screen = concat!(
        "  some output\n",
        "─────────────────────────────\n",
        "─────────────────────────────\n",
    );
    assert!(prompt_box_body_line_range(Agent::Claude, screen).is_none());
}

#[test]
fn prompt_box_body_line_range_is_none_for_a_non_claude_agent() {
    let screen = concat!(
        "  some output\n",
        "─────────────────────────────\n",
        " ❯ typing\n",
        "─────────────────────────────\n",
    );
    assert!(prompt_box_body_line_range(Agent::Codex, screen).is_none());
}

#[test]
fn transcript_region_must_name_a_known_region() {
    let manifest = r#"
id = "codex"
version = "1"
min_engine_version = 4
transcript_region = "not_a_region"

[[rules]]
id = "test"
state = "idle"
contains = ["ready"]
"#;

    let err = parse_manifest(manifest).expect_err("invalid transcript region must be rejected");
    assert!(err.contains("transcript_region"), "{err}");
}

#[test]
fn transcript_region_requires_engine_four_when_declared() {
    let manifest = r#"
id = "codex"
version = "1"
min_engine_version = 3
transcript_region = "before_current_prompt_marker"

[[rules]]
id = "test"
state = "idle"
contains = ["ready"]
"#;

    assert!(parse_manifest(manifest).is_err());
}

#[test]
fn subslice_line_range_rejects_a_foreign_slice() {
    let content = "one\ntwo\n";
    assert_eq!(subslice_line_range(content, &content[..4]), Some(0..1));
    assert_eq!(subslice_line_range(content, content), Some(0..2));
    assert_eq!(subslice_line_range(content, ""), None);
}

#[test]
fn manifest_validation_rejects_identity_rules_below_their_engine_version() {
    let err = parse_manifest(
        r#"
id = "codex"
min_engine_version = 4

[[rules]]
id = "idle"
state = "idle"
contains = ["›"]

[[identity]]
id = "too_old"
region = "osc_title"
contains = ["codex"]
"#,
    )
    .unwrap_err();

    assert!(
        err.contains("min_engine_version"),
        "unexpected error: {err}"
    );
}

#[test]
fn manifest_validation_rejects_identity_rules_without_a_positive_matcher() {
    assert!(parse_manifest(
        r#"
id = "codex"
min_engine_version = 5

[[rules]]
id = "idle"
state = "idle"
contains = ["›"]

[[identity]]
id = "empty"
region = "osc_title"
"#,
    )
    .is_err());

    assert!(parse_manifest(
        r#"
id = "codex"
min_engine_version = 5

[[rules]]
id = "idle"
state = "idle"
contains = ["›"]

[[identity]]
id = "bad_region"
region = "after_last_promt_marker"
contains = ["codex"]
"#,
    )
    .is_err());
}

#[test]
fn identity_rules_are_separate_from_state_rules() {
    // An identity block carries no state, priority, or visible-signal flags.
    assert!(parse_manifest(
        r#"
id = "codex"
min_engine_version = 5

[[rules]]
id = "idle"
state = "idle"
contains = ["›"]

[[identity]]
id = "state_is_not_an_identity_field"
region = "osc_title"
state = "idle"
contains = ["codex"]
"#,
    )
    .is_err());
}

#[test]
fn bundled_claude_manifest_identifies_its_osc_title_and_nothing_else_does() {
    let identified = identify_agent_from_screen(DetectionInput {
        screen: "",
        osc_title: "\u{2733} Claude Code",
        osc_progress: "",
    });

    assert_eq!(
        identified.map(|candidate| candidate.agent),
        Some(Agent::Claude)
    );
}

#[test]
fn bundled_claude_manifest_identifies_a_live_task_title() {
    // Claude Code replaces "Claude Code" with a task summary after the first
    // turn, so identification cannot depend on the startup title.
    let identified = identify_agent_from_screen(DetectionInput {
        screen: "",
        osc_title: "\u{2733} Write terminal haiku",
        osc_progress: "",
    });

    assert_eq!(
        identified.map(|candidate| candidate.agent),
        Some(Agent::Claude)
    );
}

#[test]
fn screen_identification_stays_silent_for_a_plain_shell_title() {
    // The captured shell title from a pane before any agent started.
    assert_eq!(
        identify_agent_from_screen(DetectionInput {
            screen: "",
            osc_title: "bchue@homeserver: ~/.treehouse/herdr",
            osc_progress: "",
        }),
        None
    );
}

#[test]
fn screen_identification_stays_silent_without_an_osc_title() {
    // A Claude transcript on screen is not identification evidence: a pager
    // showing one would look identical.
    let screen = " \u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259c}\u{258c}   Claude Code v2.1.220\n\
        \u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n\u{276f}\n\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\n";

    assert_eq!(
        identify_agent_from_screen(DetectionInput {
            screen,
            osc_title: "",
            osc_progress: "",
        }),
        None
    );
}
