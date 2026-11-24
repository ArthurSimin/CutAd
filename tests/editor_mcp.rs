#![cfg(feature = "desktop")]

use anyhow::Result;
use cutad::project::{AnimatedProperty, Asset, Clip, Project, load, save};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn stdio_modes_scope_tools_to_a_saved_project() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("Example project.cutad");
    save(&Project::default(), &path)?;
    let call = |write: bool| -> Result<Vec<Value>> {
        let mut process = Command::new(env!("CARGO_BIN_EXE_cutad"));
        process.arg("--mcp").arg(&path);
        if write {
            process.arg("--write");
        }
        let mut child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stdin = child.stdin.take().unwrap();
        for request in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"add_title","arguments":{"text":"MCP title","frame":0}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"export","arguments":{"filename":"../outside.mkv"}}}),
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"inspect_project"}}),
        ] {
            writeln!(stdin, "{request}")?;
        }
        drop(stdin);
        let result = child.wait_with_output()?;
        assert!(result.status.success());
        String::from_utf8(result.stdout)?
            .lines()
            .map(|line| Ok(serde_json::from_str(line)?))
            .collect()
    };
    let read = call(false)?;
    assert_eq!(read.len(), 5);
    assert_eq!(read[0]["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(read[1]["result"]["tools"].as_array().unwrap().len(), 1);
    assert_eq!(read[2]["result"]["isError"], true);
    assert_eq!(load(&path)?.duration(), 0);
    let write = call(true)?;
    assert_eq!(write[2]["result"]["isError"], false);
    assert_eq!(write[3]["result"]["isError"], true);
    assert!(
        write[4]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("MCP title")
    );
    assert!(load(&path)?.duration() > 0);
    Ok(())
}

#[test]
fn stdio_writes_gain_keys_at_timeline_coordinates() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("voiceover.cutad");
    let asset = Asset::video(temp.path().join("footage.mkv"), 96, true);
    let clip = Clip::new(asset.id.clone(), 24, 11, 36);
    let clip_id = clip.id.clone();
    let mut project = Project {
        assets: vec![asset],
        ..Project::default()
    };
    project.tracks[0].clips.push(clip);
    save(&project, &path)?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_cutad"))
        .arg("--mcp")
        .arg(&path)
        .arg("--write")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18"}})
    )?;
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )?;
    for (id, frame) in [(1, 23), (2, 33)] {
        let request = json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
            "name":"set_gain_keyframe","arguments":{"clip_id":clip_id,"frame":frame,"gain":0.5}
        }});
        writeln!(stdin, "{request}")?;
    }
    drop(stdin);
    let result = child.wait_with_output()?;
    assert!(result.status.success());
    let output = String::from_utf8(result.stdout)?;
    let answers = output
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(answers[1]["result"]["isError"], true);
    assert_eq!(answers[2]["result"]["isError"], false);
    let project = load(&path)?;
    let keys = &project.tracks[0].clips[0]
        .animation(AnimatedProperty::AudioGain)
        .keys;
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].frame, 9);
    assert_eq!(keys[0].value, 0.5);
    Ok(())
}
