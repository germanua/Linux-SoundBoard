// Diagnostic output intentionally goes to stdout/stderr — this module is the
// approved location for user-facing terminal output.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::HashMap;
use std::process::Command;

use serde_json::Value;

/// Capture a focused PipeWire-graph snapshot to `path`, writing one JSON line
/// per object of interest (Stream/Input/Audio nodes, Audio/Source/Audio/Sink
/// nodes, and Links). Filters out unrelated objects so the user can bundle the
/// snapshot with the route-audit log without leaking unrelated PipeWire state.
///
/// Companion to `LSB_ROUTE_AUDIT=1` route audit logging — see
/// `docs/TROUBLESHOOTING.md` "Capturing Auto-route audit data".
pub fn run_graph_snapshot(path: &std::path::Path) -> i32 {
    use std::io::Write;
    let pw_dump = match Command::new("pw-dump").output() {
        Ok(out) if out.status.success() => out.stdout,
        Ok(out) => {
            eprintln!(
                "pw-dump exited with status {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return 1;
        }
        Err(err) => {
            eprintln!("Failed to run pw-dump: {err}");
            return 1;
        }
    };
    let value: Value = match serde_json::from_slice(&pw_dump) {
        Ok(v) => v,
        Err(err) => {
            eprintln!("Failed to parse pw-dump JSON: {err}");
            return 1;
        }
    };
    let objects = match value.as_array() {
        Some(arr) => arr,
        None => {
            eprintln!("pw-dump did not return a JSON array");
            return 1;
        }
    };

    let mut file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("Failed to open {} for writing: {err}", path.display());
            return 1;
        }
    };

    let mut written = 0usize;
    for obj in objects {
        if let Some(record) = relevant_graph_record(obj) {
            if writeln!(file, "{record}").is_err() {
                eprintln!("Failed to write to {}", path.display());
                return 1;
            }
            written += 1;
        }
    }

    if let Err(err) = file.flush() {
        eprintln!("Failed to flush {}: {err}", path.display());
        return 1;
    }

    println!("Wrote {} graph record(s) to {}", written, path.display());
    0
}

fn relevant_graph_record(obj: &Value) -> Option<String> {
    let info = obj.get("info")?;
    let props = info.get("props")?;
    let media_class = props.get("media.class").and_then(|v| v.as_str());
    let object_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

    let is_input_stream = media_class == Some("Stream/Input/Audio");
    let is_audio_source = matches!(
        media_class,
        Some("Audio/Source") | Some("Audio/Source/Virtual")
    );
    let is_audio_sink = matches!(media_class, Some("Audio/Sink") | Some("Audio/Sink/Virtual"));
    let is_link = object_type == "PipeWire:Interface:Link";
    if !(is_input_stream || is_audio_source || is_audio_sink || is_link) {
        return None;
    }

    // Pull a tight set of fields. `pw-dump` exposes Link endpoints under
    // info.props, the same place media.class lives.
    let prop = |key: &str| props.get(key).cloned().unwrap_or(Value::Null);
    let mut record = serde_json::Map::new();
    record.insert("id".into(), obj.get("id").cloned().unwrap_or(Value::Null));
    record.insert("type".into(), Value::String(object_type.to_string()));
    record.insert(
        "media_class".into(),
        media_class
            .map(|s| Value::String(s.into()))
            .unwrap_or(Value::Null),
    );
    record.insert("node.name".into(), prop("node.name"));
    record.insert("node.description".into(), prop("node.description"));
    record.insert("application.name".into(), prop("application.name"));
    record.insert(
        "application.process.binary".into(),
        prop("application.process.binary"),
    );
    record.insert("media.name".into(), prop("media.name"));
    record.insert("media.role".into(), prop("media.role"));
    record.insert("target.object".into(), prop("target.object"));
    record.insert("node.dont-move".into(), prop("node.dont-move"));
    record.insert("stream.capture.sink".into(), prop("stream.capture.sink"));
    if is_link {
        record.insert("link.input.node".into(), prop("link.input.node"));
        record.insert("link.input.port".into(), prop("link.input.port"));
        record.insert("link.output.node".into(), prop("link.output.node"));
        record.insert("link.output.port".into(), prop("link.output.port"));
    }
    serde_json::to_string(&Value::Object(record)).ok()
}

pub fn run() -> i32 {
    println!("Linux Soundboard — Audio Routing Diagnosis");
    println!("===========================================\n");

    check_pipewire();
    check_virtual_mic();
    let default_source = load_default_source();
    check_default_source(&default_source);
    check_metadata();
    check_input_streams(default_source.as_deref());

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

    let wp_found = Command::new("wpctl")
        .args(["status", "-n"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("linuxsoundboard.virtual_mic"))
        .unwrap_or(false);
    println!(
        "  visible in wpctl : {}",
        if wp_found { "YES (in Sources)" } else { "NO" }
    );
    println!();
}

fn load_default_source() -> Option<String> {
    Command::new("pactl")
        .args(["get-default-source"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|source| !source.is_empty())
}

fn check_default_source(default_source: &Option<String>) {
    println!("[ System Default Source ]");
    let default = default_source.as_deref().unwrap_or("<unknown>");

    let is_ours = default.contains("linuxsoundboard");
    println!("  current : {default}");
    if is_ours {
        println!(
            "  status  : OK — Linux Soundboard is the system default mic. Apps \
             (Discord, Arma, browsers, …) that don't have an explicit device \
             pinned will use Soundboard automatically."
        );
    } else {
        println!(
            "  status  : NOT Soundboard. If routing mode is set to 'Default' in \
             Settings → Microphone Routing, the engine will re-assert ownership \
             automatically. If mode is 'Manual', this reflects your own choice."
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

fn check_input_streams(default_source: Option<&str>) {
    println!("[ Recording streams (Stream/Input/Audio) ]");
    println!(
        "  Note: the engine no longer moves these streams. Apps inherit the \
         system default ({}). Streams that have an explicit target.object \
         picked something other than the default — that's the user's or the \
         app's choice, not Soundboard's interference.",
        default_source.unwrap_or("<unknown>")
    );
    println!();

    if let Some(graph) = load_pipewire_graph() {
        if !graph.streams.is_empty() {
            for stream in &graph.streams {
                print_stream(stream, Some(&graph));
            }
            return;
        }
    }

    let output = match Command::new("pactl")
        .args(["list", "source-outputs"])
        .output()
    {
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
        print_stream(stream, None);
    }
}

fn print_stream(stream: &SourceOutput, graph: Option<&PipeWireGraph>) {
    println!("  id={}", stream.id);
    println!(
        "    app             : {}",
        stream.app_name.as_deref().unwrap_or("<unknown>")
    );
    println!("    node.name       : {}", stream.name);
    if let Some(role) = &stream.media_role {
        println!("    media.role      : {role}");
    }
    println!("    capture.sink    : {}", stream.capture_sink);
    println!(
        "    target.object   : {}",
        stream.target.as_deref().unwrap_or("<inherits default>")
    );
    println!(
        "    linked.source   : {}",
        graph
            .map(|g| linked_source_label(stream, g))
            .unwrap_or_else(|| "<unavailable>".to_string())
    );
    println!();
}

#[derive(Clone, Debug)]
struct DiagnosticSource {
    name: String,
}

#[derive(Clone, Debug)]
struct DiagnosticLink {
    output_node_id: u32,
    input_node_id: u32,
}

#[derive(Clone, Debug, Default)]
struct PipeWireGraph {
    sources: HashMap<u32, DiagnosticSource>,
    streams: Vec<SourceOutput>,
    links: Vec<DiagnosticLink>,
}

fn load_pipewire_graph() -> Option<PipeWireGraph> {
    let output = Command::new("pw-dump").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let objects = value.as_array()?;
    let mut graph = PipeWireGraph::default();

    for object in objects {
        let Some(id) = object.get("id").and_then(value_as_u32) else {
            continue;
        };
        let Some(props) = object.pointer("/info/props") else {
            continue;
        };

        if let (Some(output_node_id), Some(input_node_id)) = (
            prop_u32(props, "link.output.node"),
            prop_u32(props, "link.input.node"),
        ) {
            graph.links.push(DiagnosticLink {
                output_node_id,
                input_node_id,
            });
            continue;
        }

        let Some(media_class) = prop_string(props, "media.class") else {
            continue;
        };
        match media_class.as_str() {
            "Stream/Input/Audio" => {
                let Some(name) = prop_string(props, "node.name") else {
                    continue;
                };
                graph.streams.push(SourceOutput {
                    id,
                    name,
                    app_name: prop_string(props, "application.name"),
                    media_role: prop_string(props, "media.role"),
                    target: prop_string(props, "target.object"),
                    capture_sink: matches!(
                        prop_string(props, "stream.capture.sink").as_deref(),
                        Some("true" | "1")
                    ),
                });
            }
            "Audio/Source" | "Audio/Source/Virtual" => {
                let Some(name) = prop_string(props, "node.name") else {
                    continue;
                };
                graph.sources.insert(id, DiagnosticSource { name });
            }
            _ => {}
        }
    }

    Some(graph)
}

fn prop_string(props: &Value, key: &str) -> Option<String> {
    value_to_string(props.get(key)?)
}

fn prop_u32(props: &Value, key: &str) -> Option<u32> {
    props.get(key).and_then(value_as_u32)
}

fn value_as_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(number) => number.as_u64().and_then(|n| u32::try_from(n).ok()),
        Value::String(s) => s.parse::<u32>().ok(),
        _ => None,
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn linked_source_label(stream: &SourceOutput, graph: &PipeWireGraph) -> String {
    let mut saw_unknown = false;
    for link in graph
        .links
        .iter()
        .filter(|link| link.input_node_id == stream.id)
    {
        if let Some(source) = graph.sources.get(&link.output_node_id) {
            return source.name.clone();
        }
        saw_unknown = true;
    }
    if saw_unknown {
        "<unknown node>".to_string()
    } else {
        "<none>".to_string()
    }
}

#[derive(Clone, Debug)]
struct SourceOutput {
    id: u32,
    name: String,
    app_name: Option<String>,
    media_role: Option<String>,
    target: Option<String>,
    capture_sink: bool,
}

fn parse_source_outputs(text: &str) -> Vec<SourceOutput> {
    let mut streams = Vec::new();
    let mut current_id: Option<u32> = None;
    let mut current_name = String::new();
    let mut current_app: Option<String> = None;
    let mut current_media_role: Option<String> = None;
    let mut current_target: Option<String> = None;
    let mut current_capture_sink = false;

    fn flush(
        streams: &mut Vec<SourceOutput>,
        id: u32,
        name: &str,
        app: &Option<String>,
        media_role: &Option<String>,
        target: &Option<String>,
        capture_sink: bool,
    ) {
        streams.push(SourceOutput {
            id,
            name: name.to_string(),
            app_name: app.clone(),
            media_role: media_role.clone(),
            target: target.clone(),
            capture_sink,
        });
    }

    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Source Output #") {
            if let Some(id) = current_id {
                flush(
                    &mut streams,
                    id,
                    &current_name,
                    &current_app,
                    &current_media_role,
                    &current_target,
                    current_capture_sink,
                );
            }
            current_id = rest.trim().parse().ok();
            current_name = String::new();
            current_app = None;
            current_media_role = None;
            current_target = None;
            current_capture_sink = false;
        } else if let Some(v) = extract_prop(trimmed, "node.name") {
            current_name = v;
        } else if let Some(v) = extract_prop(trimmed, "application.name") {
            current_app = Some(v);
        } else if let Some(v) = extract_prop(trimmed, "media.role") {
            current_media_role = Some(v);
        } else if let Some(v) = extract_prop(trimmed, "target.object") {
            current_target = Some(v);
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
            &current_media_role,
            &current_target,
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
