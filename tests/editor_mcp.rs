#![cfg(feature = "desktop")]

use anyhow::Result;
use cutad::project::{Project, load, save};
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
