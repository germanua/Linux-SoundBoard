use std::process::Command;

pub fn run() -> i32 {
    println!("Linux Soundboard — Audio Routing Diagnosis");
    println!("===========================================\n");

    check_pipewire();
    check_virtual_mic();
    check_wireplumber_settings();
    check_default_source();
    check_metadata();
    check_input_streams();

    0
}

fn check_pipewire() {
    println!("[ PipeWire ]");
    match Command::new("pw-cli").args(["info", "0"]).output() {
        Ok(out) if out.status.success() => println!("  status : running"),
        _ => println!("  status : NOT RUNNING — soundboard requires PipeWire"),
    }

    if let Ok(out) = Command::new("wpctl").args(["--version"]).output() {
        let v = String::from_utf8_lossy(&out.stdout);
        println!("  wpctl  : {}", v.trim());
    }
    println!();
}

fn check_virtual_mic() {
    println!("[ Virtual Mic — linuxsoundboard.virtual_mic ]");
    let found = Command::new("pactl")
        .args(["list", "short", "sources"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("linuxsoundboard.virtual_mic"))
        .unwrap_or(false);

    if found {
        println!("  visible in pactl : YES");
    } else {
        println!("  visible in pactl : NO — install the package or run the app once to create it");
    }

    // wpctl view
    let wp_found = Command::new("wpctl")
        .args(["status", "-n"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout).contains("linuxsoundboard.virtual_mic")
        })
        .unwrap_or(false);
    println!(
        "  visible in wpctl : {}",
        if wp_found { "YES (in Sources)" } else { "NO" }
    );
    println!();
}

fn check_wireplumber_settings() {
    println!("[ WirePlumber Settings ]");
    match Command::new("wpctl")
        .args(["settings", "linking.allow-moving-streams"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let value = String::from_utf8_lossy(&out.stdout);
            let value = value.trim();
            let state = match value {
                "" => "default (enabled)",
                "true" | "1" => "enabled",
                "false" | "0" => "DISABLED ← auto-routing will NOT work",
                other => other,
            };
            println!("  linking.allow-moving-streams : {state}");
            if matches!(value, "false" | "0") {
                println!(
                    "  FIX: wpctl settings --save linking.allow-moving-streams true"
                );
            }
        }
        Ok(_) => println!("  linking.allow-moving-streams : unavailable (older WirePlumber — assumed OK)"),
        Err(_) => println!("  wpctl not found"),
    }
    println!();
}

fn check_default_source() {
    println!("[ System Default Source ]");
    let default = Command::new("pactl")
        .args(["get-default-source"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "<unknown>".to_string());

    let is_ours = default.contains("linuxsoundboard");
    println!("  current : {default}");
    if is_ours {
        println!(
            "  WARNING : soundboard virtual mic is set as system default — \
             apps that respect the default will work, but EasyEffects-style \
             per-stream routing is more reliable. Consider switching to \
             'Auto-Route While Running' mode in the soundboard settings."
        );
    }
    println!();
}

fn check_metadata() {
    println!("[ PipeWire Metadata (target.object assignments) ]");
    match Command::new("pw-metadata")
        .args(["-n", "default", "-d"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let routing_lines: Vec<&str> = text
                .lines()
                .filter(|l| l.contains("target.object") || l.contains("target.node"))
                .collect();
            if routing_lines.is_empty() {
                println!("  No target.object assignments — soundboard may not be running");
            } else {
                for line in routing_lines {
                    println!("  {}", line.trim());
                }
            }
        }
        _ => println!("  pw-metadata not available"),
    }
    println!();
}

fn check_input_streams() {
    println!("[ Stream/Input/Audio nodes (recording streams) ]");

    let output = match Command::new("pactl").args(["list", "source-outputs"]).output() {
        Ok(o) if o.status.success() => o,
        _ => {
            println!("  pactl not available");
            return;
        }
    };

    let text = String::from_utf8_lossy(&output.stdout);
    let streams = parse_source_outputs(&text);

    if streams.is_empty() {
        println!("  None found — start an app that uses a microphone (Discord, OBS, etc.)");
        println!();
        return;
    }

    for stream in &streams {
        let decision = if stream.dont_move {
            "SKIP — node.dont-move=true"
        } else if stream.capture_sink {
            "SKIP — stream.capture.sink=true (desktop audio capture, not a mic)"
        } else if stream.name.starts_with("linuxsoundboard.") {
            "SKIP — own stream"
        } else if is_processor_internal(&stream.name, &stream.app_name) {
            "SKIP — processor internal"
        } else if stream
            .target
            .as_deref()
            .map(|t| t == "linuxsoundboard.virtual_mic")
            .unwrap_or(false)
        {
            "already routed to virtual mic"
        } else {
            "ROUTE → linuxsoundboard.virtual_mic"
        };

        println!("  id={}", stream.id);
        println!("    app             : {}", stream.app_name.as_deref().unwrap_or("<unknown>"));
        println!("    node.name       : {}", stream.name);
        println!("    capture.sink    : {}", stream.capture_sink);
        if let Some(t) = &stream.target {
            println!("    target.object   : {t}");
        }
        println!("    decision        : {decision}");
        println!();
    }
}

struct SourceOutput {
    id: u32,
    name: String,
    app_name: Option<String>,
    target: Option<String>,
    dont_move: bool,
    capture_sink: bool,
}

fn parse_source_outputs(text: &str) -> Vec<SourceOutput> {
    let mut streams = Vec::new();
    let mut current_id: Option<u32> = None;
    let mut current_name = String::new();
    let mut current_app: Option<String> = None;
    let mut current_target: Option<String> = None;
    let mut current_dont_move = false;
    let mut current_capture_sink = false;

    let flush = |streams: &mut Vec<SourceOutput>,
                 id: u32,
                 name: &str,
                 app: &Option<String>,
                 target: &Option<String>,
                 dont_move: bool,
                 capture_sink: bool| {
        streams.push(SourceOutput {
            id,
            name: name.to_string(),
            app_name: app.clone(),
            target: target.clone(),
            dont_move,
            capture_sink,
        });
    };

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Source Output #") {
            if let Some(id) = current_id {
                flush(
                    &mut streams,
                    id,
                    &current_name,
                    &current_app,
                    &current_target,
                    current_dont_move,
                    current_capture_sink,
                );
            }
            current_id = rest.trim().parse().ok();
            current_name = String::new();
            current_app = None;
            current_target = None;
            current_dont_move = false;
            current_capture_sink = false;
        } else if let Some(v) = extract_prop(trimmed, "node.name") {
            current_name = v;
        } else if let Some(v) = extract_prop(trimmed, "application.name") {
            current_app = Some(v);
        } else if let Some(v) = extract_prop(trimmed, "target.object") {
            current_target = Some(v);
        } else if trimmed.contains("node.dont-move = \"true\"") {
            current_dont_move = true;
        } else if trimmed.contains("stream.capture.sink = \"true\"") {
            current_capture_sink = true;
        }
    }

    if let Some(id) = current_id {
        flush(
            &mut streams,
            id,
            &current_name,
            &current_app,
            &current_target,
            current_dont_move,
            current_capture_sink,
        );
    }

    streams
}

fn extract_prop(line: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} = \"");
    let rest = line.strip_prefix(&prefix)?;
    let value = rest.strip_suffix('"').unwrap_or(rest);
    Some(value.to_string())
}

fn is_processor_internal(node_name: &str, app_name: &Option<String>) -> bool {
    if node_name.starts_with("easyeffects.") || node_name.starts_with("easyeffects_") {
        return true;
    }
    if node_name.starts_with("ee_") || node_name.starts_with("linuxsoundboard.") {
        return true;
    }
    let check = |s: &str| {
        let l = s.to_ascii_lowercase();
        l.contains("easyeffects") || l.contains("noisetorch") || l.contains("noise_torch")
    };
    if check(node_name) {
        return true;
    }
    app_name.as_deref().map(check).unwrap_or(false)
}
