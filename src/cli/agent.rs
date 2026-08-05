use std::time::{Duration, Instant};

use crate::api::schema::{
    AgentPromptParams, AgentPromptWaitOptions, AgentReadParams, AgentRenameParams,
    AgentSendKeysParams, AgentStartParams, AgentTarget, AgentViewClearParams, AgentViewFilter,
    AgentViewSetParams, AgentViewSort, AgentViewSortField, AgentViewSortOrder, AgentWaitParams,
    EmptyParams, Method, ReadFormat, ReadSource, Request,
};

pub(super) fn run_agent_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_agent_help();
        return Ok(2);
    };

    match subcommand {
        "list" => agent_list(&args[1..]),
        "get" => agent_get(&args[1..]),
        "read" => agent_read(&args[1..]),
        "send-keys" => agent_send_keys(&args[1..]),
        "prompt" => agent_prompt(&args[1..]),
        "rename" => agent_rename(&args[1..]),
        "focus" => agent_focus(&args[1..]),
        "wait" => agent_wait(&args[1..]),
        "attach" => agent_attach(&args[1..]),
        "start" => agent_start(&args[1..]),
        "explain" => agent_explain(&args[1..]),
        "identify" => agent_identify(&args[1..]),
        "view" => agent_view(&args[1..]),
        "help" | "--help" | "-h" => {
            print_agent_help();
            Ok(0)
        }
        _ => {
            print_agent_help();
            Ok(2)
        }
    }
}

/// `herdr agent view` — the Agents panel projection, from the shell.
///
/// The view set here is the API tier, which is written to `session.json` and
/// reapplied at startup, so a script that wants the panel filtered says so once
/// instead of re-asserting it on a timer.
fn agent_view(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("set") => agent_view_set(&args[1..]),
        Some("clear") => agent_view_clear(&args[1..]),
        Some("get") => agent_view_get(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print_agent_view_help();
            Ok(0)
        }
        _ => {
            print_agent_view_help();
            Ok(2)
        }
    }
}

fn print_agent_view_help() {
    eprintln!("herdr agent view commands:");
    eprintln!("  herdr agent view get");
    eprintln!(
        "  herdr agent view set --source ID [--label TEXT] [--filter JSON] [--sort FIELD[:asc|desc]]..."
    );
    eprintln!("  herdr agent view clear [--source ID]");
    eprintln!("  --filter takes a filter object as JSON, @PATH to read a file, or - for stdin");
    eprintln!("  --sort fields are builtin names or $token; repeat for more than one key");
    eprintln!("  the view survives a restart; `config` and `ui` are reserved sources");
}

fn agent_view_set(args: &[String]) -> std::io::Result<i32> {
    let mut source = None;
    let mut label = None;
    let mut filter_text = None;
    let mut sort = Vec::new();

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--source" | "--label" | "--filter" | "--sort" => {
                let flag = args[index].as_str();
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for {flag}");
                    return Ok(2);
                };
                match flag {
                    "--source" => source = Some(value.clone()),
                    "--label" => label = Some(value.clone()),
                    "--filter" => filter_text = Some(value.clone()),
                    _ => match parse_agent_view_sort(value) {
                        Ok(field) => sort.push(field),
                        Err(message) => {
                            eprintln!("{message}");
                            return Ok(2);
                        }
                    },
                }
                index += 2;
            }
            "help" | "--help" | "-h" => {
                print_agent_view_help();
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(source) = source.filter(|source| !source.trim().is_empty()) else {
        eprintln!("missing required --source");
        return Ok(2);
    };
    let filter = match filter_text {
        Some(text) => {
            let text = match read_inline_json_argument(&text) {
                Ok(text) => text,
                Err(message) => {
                    eprintln!("{message}");
                    return Ok(2);
                }
            };
            match serde_json::from_str::<AgentViewFilter>(&text) {
                Ok(filter) => Some(filter),
                Err(err) => {
                    eprintln!("invalid --filter: {err}");
                    return Ok(2);
                }
            }
        }
        None => None,
    };
    if filter.is_none() && sort.is_empty() {
        eprintln!("agent view set needs at least one of --filter or --sort");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:view:set".into(),
        method: Method::AgentViewSet(AgentViewSetParams {
            source,
            label,
            filter,
            sort,
        }),
    })?)
}

fn agent_view_clear(args: &[String]) -> std::io::Result<i32> {
    let mut source = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = Some(value.clone());
                index += 2;
            }
            "help" | "--help" | "-h" => {
                print_agent_view_help();
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:view:clear".into(),
        method: Method::AgentViewClear(AgentViewClearParams { source }),
    })?)
}

/// Report the view in force. `session.snapshot` is the only place that names
/// it, because a view can be owned by a tier no client ever set.
fn agent_view_get(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr agent view get");
        return Ok(2);
    }

    let response = super::send_request(&Request {
        id: "cli:agent:view:get".into(),
        method: Method::SessionSnapshot(EmptyParams::default()),
    })?;
    // `session.snapshot` answers as `{"result": {"type": ..., "snapshot": {...}}}`,
    // so the view is one level below `result`. Reading `result.agent_view`
    // directly always misses and reports every view as inactive.
    let Some(snapshot) = response
        .get("result")
        .and_then(|result| result.get("snapshot"))
    else {
        return super::print_response(&response);
    };
    let view = snapshot.get("agent_view").cloned().unwrap_or(
        // An absent view is a fact, not an error: nothing is filtering the
        // panel. Say so in the same shape a set view is reported.
        serde_json::json!({ "active": false }),
    );
    println!("{}", serde_json::to_string_pretty(&view)?);
    Ok(0)
}

/// `field`, `field:asc`, or `field:desc`, where `field` is a builtin sort key
/// or a `$token` published through pane metadata.
fn parse_agent_view_sort(value: &str) -> Result<AgentViewSort, String> {
    let (field, order) = match value.rsplit_once(':') {
        Some((field, "asc")) => (field, AgentViewSortOrder::Asc),
        Some((field, "desc")) => (field, AgentViewSortOrder::Desc),
        Some((_, other)) => {
            return Err(format!(
                "invalid --sort order: {other} (expected asc or desc)"
            ))
        }
        None => (value, AgentViewSortOrder::Asc),
    };
    let field = match field.strip_prefix('$') {
        Some(token) => serde_json::json!({ "token": token }),
        None => serde_json::Value::String(field.to_string()),
    };
    serde_json::from_value::<AgentViewSortField>(field)
        .map(|field| AgentViewSort { field, order })
        .map_err(|err| format!("invalid --sort field: {err}"))
}

/// A JSON argument given inline, as `@PATH`, or as `-` for stdin. Filters get
/// long enough that quoting them into a shell command line stops being kind.
fn read_inline_json_argument(value: &str) -> Result<String, String> {
    match value {
        "-" => {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                .map_err(|err| format!("failed to read JSON from stdin: {err}"))?;
            Ok(text)
        }
        value => match value.strip_prefix('@') {
            Some(path) => {
                std::fs::read_to_string(path).map_err(|err| format!("failed to read {path}: {err}"))
            }
            None => Ok(value.to_string()),
        },
    }
}

/// Offline harness for tuning `[[identity]]` manifest rules against a captured
/// screen. Identification runs before any agent is known, so unlike `explain`
/// this takes no `--agent`: it reports which agents the screen matches, and
/// whether that answer is unique enough to act on.
fn agent_identify(args: &[String]) -> std::io::Result<i32> {
    const USAGE: &str =
        "usage: herdr agent identify --file PATH [--osc-title TEXT] [--osc-progress TEXT] [--json]";
    let mut file = None;
    let mut osc_title = String::new();
    let mut osc_progress = String::new();
    let mut json = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--file" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --file");
                    return Ok(2);
                };
                file = Some(value.clone());
                index += 2;
            }
            "--osc-title" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --osc-title");
                    return Ok(2);
                };
                osc_title = value.clone();
                index += 2;
            }
            "--osc-progress" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --osc-progress");
                    return Ok(2);
                };
                osc_progress = value.clone();
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "help" | "--help" | "-h" => {
                eprintln!("{USAGE}");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(path) = file else {
        eprintln!("{USAGE}");
        return Ok(2);
    };
    let screen = std::fs::read_to_string(path)?;
    let input = crate::detect::manifest::DetectionInput {
        screen: &screen,
        osc_title: &osc_title,
        osc_progress: &osc_progress,
    };
    let candidates = crate::detect::manifest::screen_identity_candidates(input);
    let identified = crate::detect::manifest::identify_agent_from_screen(input);

    if json {
        let value = serde_json::json!({
            "identified_agent": identified
                .as_ref()
                .map(|candidate| crate::detect::agent_label(candidate.agent)),
            "matched_rule": identified.as_ref().map(|candidate| candidate.rule_id.clone()),
            "candidates": candidates
                .iter()
                .map(|candidate| serde_json::json!({
                    "agent": crate::detect::agent_label(candidate.agent),
                    "rule": candidate.rule_id,
                }))
                .collect::<Vec<_>>(),
        });
        println!("{value}");
        return Ok(0);
    }

    match identified {
        Some(candidate) => println!(
            "identified: {} (rule={})",
            crate::detect::agent_label(candidate.agent),
            candidate.rule_id
        ),
        None if candidates.is_empty() => println!("identified: none (no identity rule matched)"),
        None => println!("identified: none (ambiguous)"),
    }
    for candidate in &candidates {
        println!(
            "  candidate: {} (rule={})",
            crate::detect::agent_label(candidate.agent),
            candidate.rule_id
        );
    }
    Ok(0)
}

fn agent_explain(args: &[String]) -> std::io::Result<i32> {
    let mut file = None;
    let mut agent = None;
    let mut json = false;
    let mut verbose = false;
    let mut target = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--file" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --file");
                    return Ok(2);
                };
                file = Some(value.clone());
                index += 2;
            }
            "--agent" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --agent");
                    return Ok(2);
                };
                agent = Some(value.clone());
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                match value.as_str() {
                    "json" => json = true,
                    "text" => json = false,
                    other => {
                        eprintln!("invalid --format: {other} (expected text or json)");
                        return Ok(2);
                    }
                }
                index += 2;
            }
            "--verbose" | "-v" => {
                verbose = true;
                index += 1;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: herdr agent explain <target> [--json|--verbose]");
                eprintln!(
                    "usage: herdr agent explain --file PATH --agent LABEL [--json|--verbose]"
                );
                return Ok(0);
            }
            value if value.starts_with('-') => {
                eprintln!("unknown option: {value}");
                return Ok(2);
            }
            value => {
                if target.is_some() {
                    eprintln!("usage: herdr agent explain <target> [--json]");
                    return Ok(2);
                }
                target = Some(value.to_string());
                index += 1;
            }
        }
    }

    let explain = if let Some(path) = file {
        if target.is_some() {
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        }
        let Some(agent_label) = agent else {
            eprintln!("herdr agent explain --file requires --agent LABEL");
            return Ok(2);
        };
        let content = std::fs::read_to_string(path)?;
        crate::detect::manifest::explain_to_json_value(&crate::detect::manifest::explain_for_label(
            &agent_label,
            &content,
        ))
    } else {
        let Some(target) = target else {
            eprintln!("usage: herdr agent explain <target> [--json]");
            eprintln!("usage: herdr agent explain --file PATH --agent LABEL [--json]");
            return Ok(2);
        };
        if agent.is_some() {
            eprintln!("--agent is only valid with --file");
            return Ok(2);
        }

        let response = super::send_request(&Request {
            id: "cli:agent:explain".into(),
            method: Method::AgentExplain(AgentTarget {
                target: target.to_owned(),
            }),
        })?;
        if response.get("error").is_some() {
            eprintln!("{}", serde_json::to_string(&response).unwrap());
            return Ok(1);
        }
        response["result"]["explain"].clone()
    };

    if json {
        println!("{explain}");
    } else {
        print_agent_explain_text(&explain, verbose);
    }
    Ok(0)
}

fn print_agent_explain_text(explain: &serde_json::Value, verbose: bool) {
    println!("agent: {}", explain["agent"].as_str().unwrap_or("unknown"));
    println!("state: {}", explain["state"].as_str().unwrap_or("unknown"));
    println!(
        "manifest: {} {}",
        explain["manifest_source"].as_str().unwrap_or("none"),
        explain["manifest_version"].as_str().unwrap_or("unknown")
    );
    if let Some(rule) = explain["matched_rule"].as_object() {
        let rule_id = rule
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        println!(
            "rule: {} (region={} priority={})",
            rule_id,
            rule.get("region")
                .and_then(|value| value.as_str())
                .unwrap_or("-"),
            rule.get("priority")
                .and_then(|value| value.as_i64())
                .unwrap_or(0),
        );
        if let Some(preview) = matched_rule_region_preview(explain, rule_id) {
            println!("evidence: {preview:?}");
        }
    } else {
        println!("rule: none");
    }
    if let Some(reason) = explain["fallback_reason"].as_str() {
        println!("fallback_reason: {reason}");
    }
    if let Some(reason) = explain["screen_detection_skip_reason"].as_str() {
        println!("screen_detection_skip_reason: {reason}");
    }
    if let Some(reason) = explain["skipped_update_reason"].as_str() {
        println!("skipped_update_reason: {reason}");
    }
    if let Some(warning) = explain["warning"].as_str() {
        println!("warning: {warning}");
    }

    if !verbose {
        return;
    }

    println!(
        "visible: idle={} blocker={} working={}",
        explain["visible_idle"].as_bool().unwrap_or(false),
        explain["visible_blocker"].as_bool().unwrap_or(false),
        explain["visible_working"].as_bool().unwrap_or(false)
    );
    println!(
        "cached_remote_version: {}",
        explain["cached_remote_version"].as_str().unwrap_or("none")
    );
    println!(
        "local_override_shadowing_remote: {}",
        explain["local_override_shadowing_remote"]
            .as_bool()
            .unwrap_or(false)
    );
    if let Some(status) = explain["remote_update_status"].as_str() {
        println!("remote_update_status: {status}");
    }
    if let Some(error) = explain["remote_update_error"].as_str() {
        println!("remote_update_error: {error}");
    }
    if let Some(evaluated_rules) = explain["evaluated_rules"]
        .as_array()
        .filter(|rules| !rules.is_empty())
    {
        println!("evaluated_rules:");
        for rule in evaluated_rules {
            println!(
                "  {} {} priority={} region={} state={}",
                if rule["matched"].as_bool().unwrap_or(false) {
                    "✓"
                } else {
                    "✗"
                },
                rule["id"].as_str().unwrap_or("-"),
                rule["priority"].as_i64().unwrap_or(0),
                rule["region"].as_str().unwrap_or("-"),
                rule["state"].as_str().unwrap_or("unknown")
            );
            let evidence = &rule["evidence"];
            println!(
                "    matchers: contains={:?} regex={:?} line_regex={:?} all={} any={} not={}",
                evidence["contains"],
                evidence["regex"],
                evidence["line_regex"],
                evidence["all_count"].as_u64().unwrap_or(0),
                evidence["any_count"].as_u64().unwrap_or(0),
                evidence["not_count"].as_u64().unwrap_or(0)
            );
            println!(
                "    region: bytes={} preview={:?}",
                evidence["region_bytes"].as_u64().unwrap_or(0),
                evidence["region_preview"].as_str().unwrap_or("")
            );
        }
    }
}

fn matched_rule_region_preview<'a>(
    explain: &'a serde_json::Value,
    rule_id: &str,
) -> Option<&'a str> {
    explain["evaluated_rules"]
        .as_array()?
        .iter()
        .find(|rule| rule["id"].as_str() == Some(rule_id))?["evidence"]["region_preview"]
        .as_str()
        .filter(|preview| !preview.is_empty())
}

fn agent_start(args: &[String]) -> std::io::Result<i32> {
    let Some(name) = args.first() else {
        eprintln!("usage: herdr agent start <name> --kind KIND --pane ID [--timeout MS] [-- <agent-args...>]");
        return Ok(2);
    };
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let mut kind = None;
    let mut pane_id = None;
    let mut timeout_ms = None;
    let mut index = 1;
    while index < separator {
        match args[index].as_str() {
            "--kind" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --kind");
                    return Ok(2);
                };
                kind = Some(value.clone());
                index += 2;
            }
            "--pane" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --pane");
                    return Ok(2);
                };
                pane_id = Some(super::normalize_pane_id(value));
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1).filter(|_| index + 1 < separator) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = match parse_timeout(value) {
                    Ok(timeout_ms) => Some(timeout_ms),
                    Err(exit_code) => return Ok(exit_code),
                };
                index += 2;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    let Some(kind) = kind else {
        eprintln!("missing required --kind");
        return Ok(2);
    };
    let Some(pane_id) = pane_id else {
        eprintln!("missing required --pane");
        return Ok(2);
    };
    let Some(expected_kind) = crate::detect::parse_agent_label(&kind) else {
        eprintln!("unsupported interactive agent kind: {kind}");
        return Ok(2);
    };
    let expected_kind = crate::detect::agent_label(expected_kind).to_string();
    let mut response = super::send_request(&Request {
        id: "cli:agent:start".into(),
        method: Method::AgentStart(AgentStartParams {
            name: name.clone(),
            kind,
            pane_id: pane_id.clone(),
            args: if separator < args.len() {
                args[separator + 1..].to_vec()
            } else {
                Vec::new()
            },
            timeout_ms,
        }),
    })?;
    if response.get("error").is_some() {
        return super::print_response(&response);
    }
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(30_000));
    let Some(expected_terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        return super::print_response(&cli_agent_error(
            "cli:agent:start",
            "agent_start_failed",
            "agent start response did not include terminal_id",
        ));
    };
    let waited = wait_for_named_agent(
        name,
        &pane_id,
        timeout,
        &expected_kind,
        expected_terminal_id,
    );
    match waited {
        Ok(Ok(agent)) => {
            response["result"]["agent"] = agent;
            super::print_response(&response)
        }
        Ok(Err(error)) => super::print_response(&error),
        Err(err) => {
            print_agent_transport_error(err, "cli:agent:start", "agent_start_transport_failed")
        }
    }
}

fn agent_list(args: &[String]) -> std::io::Result<i32> {
    if !args.is_empty() {
        eprintln!("usage: herdr agent list");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:list".into(),
        method: Method::AgentList(EmptyParams::default()),
    })?)
}

fn agent_get(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent get <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr agent get <target>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:get".into(),
        method: Method::AgentGet(AgentTarget {
            target: target.clone(),
        }),
    })?)
}

fn agent_focus(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent focus <target>");
        return Ok(2);
    };
    if args.len() != 1 {
        eprintln!("usage: herdr agent focus <target>");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:focus".into(),
        method: Method::AgentFocus(AgentTarget {
            target: target.clone(),
        }),
    })?)
}

fn agent_attach(args: &[String]) -> std::io::Result<i32> {
    let (target, takeover) =
        match super::parse_attach_target(args, "usage: herdr agent attach <target> [--takeover]") {
            Ok(parsed) => parsed,
            Err(code) => return Ok(code),
        };

    let response = resolve_agent_target(&target, "cli:agent:attach:resolve")?;
    if response.get("error").is_some() {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    let Some(terminal_id) = response["result"]["agent"]["terminal_id"].as_str() else {
        eprintln!("agent attach failed: response did not include terminal_id");
        return Ok(1);
    };
    crate::client::run_terminal_attach(terminal_id.to_owned(), takeover)?;
    Ok(0)
}

fn agent_wait(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent wait <target> [--until STATUS]... [--timeout MS]");
        return Ok(2);
    };
    let mut until = Vec::new();
    let mut timeout_ms = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--until" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--until requires at least one status");
                    return Ok(2);
                };
                let status = match super::parse_agent_status(value) {
                    Ok(status) => status,
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                };
                until.push(status);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = match parse_timeout(value) {
                    Ok(timeout_ms) => Some(timeout_ms),
                    Err(exit_code) => return Ok(exit_code),
                };
                index += 2;
            }
            "help" | "--help" | "-h" => {
                eprintln!("usage: herdr agent wait <target> [--until STATUS]... [--timeout MS]");
                return Ok(0);
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    super::print_response(&super::send_request(&Request {
        id: "cli:agent:wait".into(),
        method: Method::AgentWait(AgentWaitParams {
            target: target.clone(),
            until,
            timeout_ms,
        }),
    })?)
}

fn wait_for_named_agent(
    name: &str,
    fallback_pane_id: &str,
    timeout: Duration,
    expected_kind: &str,
    expected_terminal_id: &str,
) -> std::io::Result<Result<serde_json::Value, serde_json::Value>> {
    let started_at = Instant::now();
    let deadline = started_at.checked_add(timeout);
    let mut first_poll = true;
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            // Let the server reconcile its matching startup deadline before
            // returning so the pending name is immediately reusable.
            let _ = resolve_agent_target_unchecked(name, "cli:agent:start:timeout");
            return Ok(Err(agent_wait_timeout()));
        }
        let poll_id = "cli:agent:start";
        let mut response = if first_poll {
            first_poll = false;
            resolve_agent_target(name, poll_id)?
        } else {
            resolve_agent_target_unchecked(name, poll_id)?
        };
        if response.get("error").is_some() {
            response = resolve_agent_target_unchecked(fallback_pane_id, poll_id)?;
            if response.get("error").is_some() {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        }
        let agent = &response["result"]["agent"];
        let outcome = if agent["terminal_id"].as_str() != Some(expected_terminal_id) {
            Some(Err(agent_name_lost_error("cli:agent:start", name)))
        } else if let Some(actual) = agent["agent"]
            .as_str()
            .filter(|actual| *actual != expected_kind)
        {
            Some(Err(cli_agent_error(
                "cli:agent:start",
                "agent_kind_mismatch",
                format!("expected {expected_kind}, detected {actual}"),
            )))
        } else if agent["name"].as_str() != Some(name) {
            Some(Err(agent_name_lost_error("cli:agent:start", name)))
        } else if agent["interactive_ready"].as_bool().unwrap_or(false) {
            Some(Ok(agent.clone()))
        } else if !agent["launch_pending"].as_bool().unwrap_or(false) {
            Some(Err(cli_agent_error(
                "cli:agent:start",
                "agent_start_failed",
                "agent process exited before becoming interactive",
            )))
        } else {
            None
        };
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn agent_name_lost_error(request_id: &str, expected_name: &str) -> serde_json::Value {
    cli_agent_error(
        request_id,
        "agent_name_not_found",
        format!("named agent {expected_name} no longer owns the target terminal"),
    )
}

fn print_agent_transport_error(
    err: std::io::Error,
    request_id: &str,
    code: &str,
) -> std::io::Result<i32> {
    if super::protocol_mismatch_was_reported(&err) {
        return Ok(1);
    }
    // A dead-server marker reaches here from `send_request` in the agent
    // startup path; surface its deferred response exactly once instead of
    // printing a second, generic transport-error line.
    if let Some(response) = super::server_not_running_reported_response(&err) {
        let value = serde_json::to_value(response).map_err(std::io::Error::other)?;
        return super::print_response(&value);
    }
    super::print_response(&cli_agent_error(request_id, code, err.to_string()))
}

fn agent_wait_timeout() -> serde_json::Value {
    cli_agent_error(
        "cli:agent:start",
        "timeout",
        "timed out waiting for agent startup",
    )
}

fn cli_agent_error(id: &str, code: &str, message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

fn resolve_agent_target(target: &str, request_id: &str) -> std::io::Result<serde_json::Value> {
    super::send_request(&agent_get_request(target, request_id))
}

fn resolve_agent_target_unchecked(
    target: &str,
    request_id: &str,
) -> std::io::Result<serde_json::Value> {
    super::send_request_unchecked(&agent_get_request(target, request_id))
}

fn agent_get_request(target: &str, request_id: &str) -> Request {
    Request {
        id: request_id.into(),
        method: Method::AgentGet(AgentTarget {
            target: target.to_owned(),
        }),
    }
}

fn agent_rename(args: &[String]) -> std::io::Result<i32> {
    let [target, value] = args else {
        eprintln!("usage: herdr agent rename <target> <name>|--clear");
        return Ok(2);
    };
    let name = if value == "--clear" {
        None
    } else {
        Some(value.clone())
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:rename".into(),
        method: Method::AgentRename(AgentRenameParams {
            target: target.clone(),
            name,
        }),
    })?)
}

fn agent_prompt(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!(
            "usage: herdr agent prompt <target> <text> [--wait] [--until STATUS]... [--timeout MS]"
        );
        return Ok(2);
    };
    let Some(text) = args.get(1) else {
        eprintln!("agent prompt requires text");
        return Ok(2);
    };
    let mut wait = false;
    let mut until = Vec::new();
    let mut timeout_ms = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--wait" => {
                wait = true;
                index += 1;
            }
            "--until" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--until requires at least one status");
                    return Ok(2);
                };
                let status = match super::parse_agent_status(value) {
                    Ok(status) => status,
                    Err(err) => {
                        eprintln!("{err}");
                        return Ok(2);
                    }
                };
                until.push(status);
                index += 2;
            }
            "--timeout" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --timeout");
                    return Ok(2);
                };
                timeout_ms = match parse_timeout(value) {
                    Ok(timeout_ms) => Some(timeout_ms),
                    Err(exit_code) => return Ok(exit_code),
                };
                index += 2;
            }
            option => {
                eprintln!("unknown option: {option}");
                return Ok(2);
            }
        }
    }
    if !until.is_empty() && !wait {
        eprintln!("--until requires --wait");
        return Ok(2);
    }
    if timeout_ms.is_some() && !wait {
        eprintln!("--timeout requires --wait");
        return Ok(2);
    }
    let response = super::send_request(&Request {
        id: "cli:agent:prompt".into(),
        method: Method::AgentPrompt(AgentPromptParams {
            target: target.clone(),
            text: text.clone(),
            wait: wait.then_some(AgentPromptWaitOptions { until, timeout_ms }),
        }),
    })?;
    super::print_response(&response)
}

fn agent_send_keys(args: &[String]) -> std::io::Result<i32> {
    if args.len() < 2 {
        eprintln!("usage: herdr agent send-keys <target> <key> [key ...]");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:agent:send-keys".into(),
        method: Method::AgentSendKeys(AgentSendKeysParams {
            target: args[0].clone(),
            keys: args[1..].to_vec(),
        }),
    })?)
}

fn agent_read(args: &[String]) -> std::io::Result<i32> {
    let Some(target) = args.first() else {
        eprintln!("usage: herdr agent read <target> [--source visible|recent|recent-unwrapped|detection|transcript] [--lines N] [--format text|ansi] [--ansi]");
        return Ok(2);
    };

    let mut source = ReadSource::Recent;
    let mut lines = None;
    let mut format = ReadFormat::Text;
    let mut strip_ansi = true;

    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--source" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --source");
                    return Ok(2);
                };
                source = super::parse_read_source(value)?;
                index += 2;
            }
            "--lines" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --lines");
                    return Ok(2);
                };
                lines = Some(super::parse_u32_flag("--lines", value)?);
                index += 2;
            }
            "--format" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --format");
                    return Ok(2);
                };
                format = super::parse_read_format(value)?;
                strip_ansi = !matches!(format, ReadFormat::Ansi);
                index += 2;
            }
            "--ansi" => {
                format = ReadFormat::Ansi;
                strip_ansi = false;
                index += 1;
            }
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let response = super::send_request(&Request {
        id: "cli:agent:read".into(),
        method: Method::AgentRead(AgentReadParams {
            target: target.clone(),
            source,
            lines,
            format,
            strip_ansi,
        }),
    })?;
    super::print_read_response(&response)
}

fn print_agent_help() {
    eprintln!("herdr agent commands:");
    eprintln!("  herdr agent list");
    eprintln!("  herdr agent get <target>");
    eprintln!("  herdr agent read <target> [--source visible|recent|recent-unwrapped|detection|transcript] [--lines N] [--format text|ansi] [--ansi]");
    eprintln!("  herdr agent send-keys <target> <key> [key ...]");
    eprintln!("  herdr agent prompt <target> <text> [--wait] [--until STATUS]... [--timeout MS]");
    eprintln!("  herdr agent rename <target> <name>|--clear");
    eprintln!("  herdr agent focus <target>");
    eprintln!("  herdr agent wait <target> [--until STATUS]... [--timeout MS]");
    eprintln!("  herdr agent attach <target> [--takeover]");
    eprintln!(
        "  herdr agent start <name> --kind KIND --pane ID [--timeout MS] [-- <agent-args...>]"
    );
    eprintln!("  herdr agent explain <target> [--json|--format text|json] [--verbose]");
    eprintln!(
        "  herdr agent explain --file PATH --agent LABEL [--json|--format text|json] [--verbose]"
    );
    eprintln!(
        "  herdr agent identify --file PATH [--osc-title TEXT] [--osc-progress TEXT] [--json]"
    );
    eprintln!("  herdr agent view get|set|clear   (durable Agents panel filter and sort)");
    eprintln!("  targets accept unique agent names and pane ids that currently host agents");
    eprintln!("  kinds: {}", super::spec::agent_kind_values().join("|"));
}

fn parse_timeout(value: &str) -> Result<u64, i32> {
    super::parse_u64_flag("--timeout", value).map_err(|err| {
        eprintln!("{err}");
        2
    })
}

/// The `session.snapshot` traversal `agent view get` depends on.
///
/// This is a wire-shape contract, not an internal detail: the view sits at
/// `result.snapshot.agent_view`, and reading `result.agent_view` silently
/// reports every active view as inactive rather than failing.
#[cfg(test)]
mod agent_view_get_tests {
    #[test]
    fn the_view_is_read_from_result_snapshot_not_result() {
        let response = serde_json::json!({
            "id": "cli:agent:view:get",
            "result": {
                "type": "session_snapshot",
                "snapshot": {
                    "agent_view": { "source": "workers-only", "label": "workers" }
                }
            }
        });

        let view = response
            .get("result")
            .and_then(|result| result.get("snapshot"))
            .and_then(|snapshot| snapshot.get("agent_view"));

        assert_eq!(
            view.and_then(|view| view.get("source"))
                .and_then(|s| s.as_str()),
            Some("workers-only")
        );
        // The shape this test exists to pin down: one level up finds nothing.
        assert!(response
            .get("result")
            .and_then(|result| result.get("agent_view"))
            .is_none());
    }
}
