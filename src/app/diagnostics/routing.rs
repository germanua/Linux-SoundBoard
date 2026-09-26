#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::collections::HashMap;

use serde_json::Value;

use crate::app_meta::{ENGINE_SERVICE_NAME, MIC_CAPTURE_NODE_NAME, VIRTUAL_SOURCE_NAME};
use crate::audio::command_runner::run_command;

pub fn run_graph_snapshot(path: &std::path::Path) -> i32 {
    use std::io::Write;
    let pw_dump = match run_command("pw-dump", &[]) {
        Ok(out) if out.success => out.stdout.into_bytes(),
        Ok(out) => {
            eprintln!("pw-dump failed: {}", out.stderr.trim());
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

    if writeln!(file, "{}", default_devices_record()).is_err() {
        eprintln!("Failed to write to {}", path.display());
        return 1;
    }
    written += 1;
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

fn default_devices_record() -> String {
    default_record_text(
        pactl_default_text("get-default-sink"),
        pactl_default_text("get-default-source"),
    )
}

fn default_record_text(default_sink: String, default_source: String) -> String {
    let mut record = serde_json::Map::new();
    record.insert(
        "type".into(),
        Value::String("Diagnostics:Defaults".to_string()),
    );
    record.insert("default.sink".into(), Value::String(default_sink));
    record.insert("default.source".into(), Value::String(default_source));
    serde_json::to_string(&Value::Object(record)).unwrap_or_default()
}

fn pactl_default_text(getter: &str) -> String {
    match run_command("pactl", &[getter]) {
        Ok(out) if out.success => {
            let name = out.stdout.trim().to_string();
            if name.is_empty() {
                "<empty>".to_string()
            } else {
                name
            }
        }
        Ok(out) => format!("<failed: {}>", out.stderr.trim()),
        Err(err) => format!("<failed: {err}>"),
    }
}

const APP_CAPTURE_NODE_NAME: &str = MIC_CAPTURE_NODE_NAME;

const UNRESOLVED_PROFILE: &str = "<unresolved>";

#[derive(Debug, Default, PartialEq, Eq)]
struct ActiveProfile {
    index: Option<u64>,
    name: Option<String>,
    description: Option<String>,
}

fn active_profile(info: &Value) -> ActiveProfile {
    let Some(index) = info
        .pointer("/params/Profile/0/index")
        .and_then(Value::as_u64)
    else {
        return ActiveProfile::default();
    };
    let entry = info
        .pointer("/params/EnumProfile")
        .and_then(Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("index").and_then(Value::as_u64) == Some(index))
        });
    let Some(entry) = entry else {
        return ActiveProfile {
            index: Some(index),
            ..ActiveProfile::default()
        };
    };
    ActiveProfile {
        index: Some(index),
        name: entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        description: entry
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn relevant_graph_record(obj: &Value) -> Option<String> {
    let info = obj.get("info")?;
    let props = info.get("props")?;
    let media_class = props.get("media.class").and_then(|v| v.as_str());
    let object_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

    let is_input_stream = media_class == Some("Stream/Input/Audio");

    let is_output_stream = media_class == Some("Stream/Output/Audio");
    let is_audio_source = matches!(
        media_class,
        Some("Audio/Source") | Some("Audio/Source/Virtual")
    );
    let is_audio_sink = matches!(media_class, Some("Audio/Sink") | Some("Audio/Sink/Virtual"));

    let is_audio_device =
        object_type == "PipeWire:Interface:Device" && media_class == Some("Audio/Device");
    let is_link = object_type == "PipeWire:Interface:Link";
    if !(is_input_stream
        || is_output_stream
        || is_audio_source
        || is_audio_sink
        || is_audio_device
        || is_link)
    {
        return None;
    }
    let is_node = is_input_stream || is_output_stream || is_audio_source || is_audio_sink;

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
    if is_node {
        record.insert(
            "info.state".into(),
            info.get("state").cloned().unwrap_or(Value::Null),
        );
    }
    if is_audio_device {
        record.insert("device.name".into(), prop("device.name"));
        record.insert("device.api".into(), prop("device.api"));
        record.insert("device.description".into(), prop("device.description"));
        let profile = active_profile(info);
        record.insert(
            "device.profile.index".into(),
            profile.index.map_or(Value::Null, Value::from),
        );
        record.insert(
            "device.profile.name".into(),
            Value::String(
                profile
                    .name
                    .unwrap_or_else(|| UNRESOLVED_PROFILE.to_string()),
            ),
        );
        record.insert(
            "device.profile.description".into(),
            Value::String(
                profile
                    .description
                    .unwrap_or_else(|| UNRESOLVED_PROFILE.to_string()),
            ),
        );
    }
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

    check_engine();
    check_pipewire();
    check_virtual_mic();
    let default_source = load_default_source();
    check_default_source(&default_source);
    check_metadata();
    check_audio_devices();
    check_input_streams(default_source.as_deref());

    0
}

fn check_engine() {
    println!("[ Audio Engine ]");
    let ui_binary = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    println!("  UI binary       : {ui_binary}");
    println!(
        "  expected        : version={} protocol={} schema={}",
        crate::app_meta::APP_VERSION,
        crate::audio::engine_ipc::ENGINE_PROTOCOL_VERSION,
        crate::config::CURRENT_SCHEMA_VERSION
    );

    match crate::audio::engine_ipc::engine_info() {
        Ok(info) => {
            println!("  engine binary   : {}", info.binary_path);
            println!(
                "  engine          : version={} protocol={} schema={}",
                info.app_version, info.engine_protocol_version, info.config_schema_version
            );
            let compatible = crate::audio::engine_ipc::engine_info_compatible(&info);
            println!(
                "  compatibility   : {}",
                if compatible { "OK" } else { "INCOMPATIBLE" }
            );
            if !compatible {
                println!(
                    "  repair           : curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh | bash -s -- fix"
                );
            }
        }
        Err(err) => println!("  engine          : unavailable ({err})"),
    }

    match run_command(
        "systemctl",
        &[
            "--user",
            "show",
            ENGINE_SERVICE_NAME,
            "--property=FragmentPath",
            "--property=ExecStart",
            "--property=ActiveState",
            "--property=SubState",
            "--property=MainPID",
        ],
    ) {
        Ok(out) if out.success => {
            for line in out.stdout.lines().filter(|line| !line.is_empty()) {
                println!("  service {line}");
            }
        }
        _ => println!("  service         : unavailable"),
    }
    println!();
}

fn check_pipewire() {
    println!("[ PipeWire ]");
    match run_command("pw-cli", &["info", "0"]) {
        Ok(out) if out.success => println!("  status : running"),
        _ => println!("  status : NOT RUNNING — soundboard requires PipeWire"),
    }

    println!();
}

fn check_virtual_mic() {
    println!("[ Virtual Mic — {VIRTUAL_SOURCE_NAME} ]");
    let found = run_command("pactl", &["list", "short", "sources"])
        .ok()
        .filter(|output| output.success)
        .map(|output| output.stdout.contains(VIRTUAL_SOURCE_NAME))
        .unwrap_or(false);

    if found {
        println!("  visible in pactl : YES");
    } else {
        println!("  visible in pactl : NO — install the package or run the app once to create it");
    }

    let wp_found = run_command("wpctl", &["status", "-n"])
        .ok()
        .filter(|output| output.success)
        .map(|output| output.stdout.contains(VIRTUAL_SOURCE_NAME))
        .unwrap_or(false);
    println!(
        "  visible in wpctl : {}",
        if wp_found { "YES (in Sources)" } else { "NO" }
    );
    println!();
}

fn load_default_source() -> Option<String> {
    run_command("pactl", &["get-default-source"])
        .ok()
        .filter(|output| output.success)
        .map(|output| output.stdout.trim().to_string())
        .filter(|source| !source.is_empty())
}

fn check_default_source(default_source: &Option<String>) {
    println!("[ System Default Source ]");
    let default = default_source.as_deref().unwrap_or("<unknown>");

    let is_ours = default == VIRTUAL_SOURCE_NAME;
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
    match run_command("pw-metadata", &["-n", "default", "-d"]) {
        Ok(out) if out.success => {
            let text = out.stdout;
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

fn check_audio_devices() {
    println!("[ Audio Devices (cards) and System Defaults ]");
    println!(
        "  default sink   : {}",
        pactl_default_text("get-default-sink")
    );
    println!(
        "  default source : {}",
        pactl_default_text("get-default-source")
    );

    match load_pw_dump_objects() {
        Some(objects) => {
            let mut found = 0usize;
            for object in &objects {
                if object.get("type").and_then(Value::as_str) != Some("PipeWire:Interface:Device") {
                    continue;
                }
                let Some(props) = object.pointer("/info/props") else {
                    continue;
                };
                if prop_string(props, "media.class").as_deref() != Some("Audio/Device") {
                    continue;
                }
                let Some(info) = object.get("info") else {
                    continue;
                };
                found += 1;
                let profile = active_profile(info);
                println!(
                    "  device : {}",
                    prop_string(props, "device.name").unwrap_or_else(|| "<unnamed>".to_string())
                );
                println!(
                    "    description    : {}",
                    prop_string(props, "device.description")
                        .unwrap_or_else(|| "<unknown>".to_string())
                );
                println!(
                    "    api            : {}",
                    prop_string(props, "device.api").unwrap_or_else(|| "<unknown>".to_string())
                );
                println!(
                    "    active profile : {} ({}) [index {}]",
                    profile
                        .name
                        .unwrap_or_else(|| UNRESOLVED_PROFILE.to_string()),
                    profile
                        .description
                        .unwrap_or_else(|| UNRESOLVED_PROFILE.to_string()),
                    profile
                        .index
                        .map_or(UNRESOLVED_PROFILE.to_string(), |index| index.to_string()),
                );
            }
            if found == 0 {
                println!("  No Audio/Device cards reported by pw-dump");
            }
        }
        None => println!("  pw-dump unavailable or did not return a JSON array"),
    }
    println!();
}

fn load_pw_dump_objects() -> Option<Vec<Value>> {
    let output = run_command("pw-dump", &[]).ok()?;
    if !output.success {
        return None;
    }
    let value: Value = serde_json::from_str(&output.stdout).ok()?;
    value.as_array().cloned()
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
            print_app_capture_summary(&graph);
            return;
        }
    }

    let output = match run_command("pactl", &["list", "source-outputs"]) {
        Ok(output) if output.success => output,
        _ => {
            println!("  pactl not available");
            return;
        }
    };

    let streams = parse_source_outputs(&output.stdout);

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
    if stream.name == APP_CAPTURE_NODE_NAME {
        println!("    soundboard      : YES — this is the app's own capture stream");
    }
    println!();
}

fn print_app_capture_summary(graph: &PipeWireGraph) {
    match graph
        .streams
        .iter()
        .find(|stream| stream.name == APP_CAPTURE_NODE_NAME)
    {
        Some(stream) => println!(
            "  Soundboard capture — {APP_CAPTURE_NODE_NAME} resolved target: {}",
            linked_source_label(stream, graph)
        ),
        None => println!("  Soundboard capture — {APP_CAPTURE_NODE_NAME}: not in the graph"),
    }
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
    let output = run_command("pw-dump", &[]).ok()?;
    if !output.success {
        return None;
    }
    let value: Value = serde_json::from_str(&output.stdout).ok()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(media_class: &str, state: &str) -> Value {
        json!({
            "id": 7,
            "type": "PipeWire:Interface:Node",
            "info": {
                "state": state,
                "props": { "media.class": media_class, "node.name": "node.example" }
            }
        })
    }

    fn enum_profile(index: u64, name: &str, description: &str) -> Value {
        json!({ "index": index, "name": name, "description": description, "available": "yes" })
    }

    fn card(profile_index: u64, enum_profiles: Value) -> Value {
        json!({
            "id": 3,
            "type": "PipeWire:Interface:Device",
            "info": {
                "props": {
                    "media.class": "Audio/Device",
                    "device.name": "alsa_card.pci-0000_12_00.6",
                    "device.api": "alsa",
                    "device.description": "Ryzen HD Audio Controller"
                },
                "params": {
                    "Profile": [
                        { "index": profile_index, "name": "unused", "description": "unused" }
                    ],
                    "EnumProfile": enum_profiles
                }
            }
        })
    }

    fn snapshot_record(value: &Value) -> Value {
        let text = relevant_graph_record(value).expect("object must be recorded");
        serde_json::from_str(&text).expect("record must be JSON")
    }

    #[test]
    fn the_snapshot_keeps_every_class_needed_to_explain_routing() {
        for media_class in [
            "Stream/Input/Audio",
            "Stream/Output/Audio",
            "Audio/Source",
            "Audio/Source/Virtual",
            "Audio/Sink",
            "Audio/Sink/Virtual",
        ] {
            let record = snapshot_record(&node(media_class, "running"));
            assert_eq!(record["media_class"], json!(media_class), "{media_class}");
            assert_eq!(record["node.name"], json!("node.example"), "{media_class}");
            assert_eq!(record["info.state"], json!("running"), "{media_class}");
        }

        let record = snapshot_record(&card(
            1,
            json!([enum_profile(1, "output:analog-stereo", "Analog Stereo")]),
        ));
        assert_eq!(record["media_class"], json!("Audio/Device"));
        assert_eq!(record["device.name"], json!("alsa_card.pci-0000_12_00.6"));
        assert_eq!(record["device.api"], json!("alsa"));
        assert_eq!(
            record["device.description"],
            json!("Ryzen HD Audio Controller")
        );
        assert_eq!(record["device.profile.index"], json!(1));
        assert_eq!(record["device.profile.name"], json!("output:analog-stereo"));
        assert_eq!(record["device.profile.description"], json!("Analog Stereo"));

        let link = json!({
            "id": 11,
            "type": "PipeWire:Interface:Link",
            "info": {
                "state": "running",
                "props": {
                    "link.output.node": 5,
                    "link.output.port": 6,
                    "link.input.node": 7,
                    "link.input.port": 8
                }
            }
        });
        let record = snapshot_record(&link);
        assert_eq!(record["link.output.node"], json!(5));
        assert_eq!(record["link.input.node"], json!(7));
        assert!(
            record.get("info.state").is_none(),
            "info.state belongs to node records"
        );
    }

    #[test]
    fn the_node_state_is_carried_through_for_every_node_class() {
        assert_eq!(
            snapshot_record(&node("Audio/Sink", "suspended"))["info.state"],
            json!("suspended")
        );
        assert_eq!(
            snapshot_record(&node("Stream/Output/Audio", "idle"))["info.state"],
            json!("idle")
        );
        assert_eq!(
            snapshot_record(&node("Audio/Source", "error"))["info.state"],
            json!("error")
        );

        let record = snapshot_record(&json!({
            "id": 9,
            "type": "PipeWire:Interface:Node",
            "info": { "props": { "media.class": "Audio/Sink" } }
        }));
        assert!(
            record.get("info.state").is_some(),
            "info.state must be present even when the graph reports no state: {record}"
        );
        assert_eq!(record["info.state"], Value::Null);
    }

    #[test]
    fn the_active_profile_is_resolved_through_the_enum_profile() {
        let info = json!({
            "params": {
                "Profile": [ { "index": 4, "name": "ignored", "description": "ignored" } ],
                "EnumProfile": [
                    enum_profile(3, "off", "Off"),
                    enum_profile(4, "output:analog-stereo+input:analog-stereo", "Duplex")
                ]
            }
        });
        let profile = active_profile(&info);
        assert_eq!(profile.index, Some(4));
        assert_eq!(
            profile.name.as_deref(),
            Some("output:analog-stereo+input:analog-stereo")
        );
        assert_eq!(profile.description.as_deref(), Some("Duplex"));
    }

    #[test]
    fn an_unresolvable_active_profile_is_recorded_as_unresolved() {
        for params in [
            json!({}),
            json!({ "params": {} }),
            json!({ "params": { "Profile": [] } }),
            json!({ "params": { "Profile": [ { "description": "no index" } ] } }),
            json!({
                "params": {
                    "Profile": [ { "index": 3 } ],
                    "EnumProfile": [ enum_profile(4, "other", "Other") ]
                }
            }),
            json!({
                "params": {
                    "Profile": [ { "index": 5 } ],
                    "EnumProfile": [ { "index": 5, "available": "yes" } ]
                }
            }),
        ] {
            let object = json!({
                "id": 3,
                "type": "PipeWire:Interface:Device",
                "info": {
                    "props": { "media.class": "Audio/Device" },
                    "params": params["params"].clone()
                }
            });
            let record = snapshot_record(&object);
            assert_eq!(
                record["device.profile.name"],
                json!("<unresolved>"),
                "an unresolvable profile must not be guessed: {params}"
            );
            assert_eq!(record["device.profile.description"], json!("<unresolved>"));
        }
    }

    #[test]
    fn unrelated_objects_are_still_left_out_of_the_snapshot() {
        for object in [
            node("Midi/Bridge", "running"),
            node("Stream/Output/Video", "running"),
            node("Audio/Video", "running"),
            json!({
                "id": 1,
                "type": "PipeWire:Interface:Client",
                "info": { "props": { "application.name": "some-app" } }
            }),
            json!({
                "id": 2,
                "type": "PipeWire:Interface:Port",
                "info": { "props": { "port.name": "some-port" } }
            }),
            json!({
                "id": 4,
                "type": "PipeWire:Interface:Node",
                "info": { "props": { "media.class": "Audio/Device" } }
            }),
            json!({
                "id": 5,
                "type": "PipeWire:Interface:Device",
                "info": { "props": { "media.class": "Video/Device" } }
            }),
            json!({ "id": 6, "type": "PipeWire:Interface:Node" }),
            json!({ "id": 7 }),
        ] {
            assert!(
                relevant_graph_record(&object).is_none(),
                "object must stay out of the snapshot: {object}"
            );
        }
    }

    #[test]
    fn the_defaults_record_always_carries_both_defaults() {
        let record: Value = serde_json::from_str(&default_record_text(
            "sink.name".to_string(),
            "source.name".to_string(),
        ))
        .expect("the defaults record must be JSON");

        assert_eq!(record["type"], json!("Diagnostics:Defaults"));
        assert_eq!(record["default.sink"], json!("sink.name"));
        assert_eq!(record["default.source"], json!("source.name"));
        assert_eq!(
            record
                .as_object()
                .expect("the record must be a JSON object")
                .keys()
                .count(),
            3,
            "the marker and both defaults, with nothing dropped or invented: {record}"
        );

        let record: Value = serde_json::from_str(&default_record_text(
            "<failed: status exit status: 1: Permission denied>".to_string(),
            "<empty>".to_string(),
        ))
        .expect("the defaults record must be JSON");
        assert!(
            record.get("default.sink").is_some(),
            "the sink field must exist even when the lookup failed: {record}"
        );
        assert!(
            record.get("default.source").is_some(),
            "the source field must exist even when the lookup failed: {record}"
        );
        assert_eq!(
            record["default.sink"],
            json!("<failed: status exit status: 1: Permission denied>")
        );
        assert_eq!(record["default.source"], json!("<empty>"));
    }
}
