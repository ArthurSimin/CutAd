# Connect CutAd to an AI client

CutAd runs an MCP server over local stdio. Use an absolute path to the CutAd executable and an existing saved `.cutad` project.

```sh
cutad --mcp-config codex "/absolute/path/My edit.cutad"
```

Replace `codex` with the client name below. Merge the printed entry into the client’s configuration and reconnect or restart that client.

| Client | Generator name | Where the entry goes |
| --- | --- | --- |
| Codex | `codex` | Add the TOML section to `~/.codex/config.toml` or the project's `.codex/config.toml`. You can also register it with `codex mcp add cutad -- /absolute/path/cutad --mcp "/absolute/path/My edit.cutad"`. [Codex MCP](https://developers.openai.com/codex/mcp) |
| Claude Code | `claude-code` | Merge into the project’s `.mcp.json`. You can also use `claude mcp add --transport stdio cutad -- /absolute/path/cutad --mcp "/absolute/path/My edit.cutad"`. [Claude Code MCP](https://code.claude.com/docs/en/mcp) |
| OpenCode | `opencode` | Merge under `mcp` in `opencode.json` or `opencode.jsonc`. [OpenCode MCP](https://opencode.ai/docs/mcp-servers/) |
| Kilo Code | `kilo-code` | Merge under `mcp` in `kilo.jsonc` or add it in Settings → Agent Behaviour → MCP Servers. [Kilo MCP](https://kilo.ai/docs/automate/mcp/using-in-kilo-code) |
| Antigravity | `antigravity` | Open the MCP configuration editor and merge into `mcp_config.json` under `mcpServers`. [Antigravity MCP](https://antigravity.google/docs/mcp) |

On Windows, run the installed `cutad.exe`. The generator escapes Windows paths for the selected format.

## Inspect, edit, and export

Start with “Inspect my CutAd project and list the clips with start and end times.”

When you want the client to make changes, regenerate its entry with `--write` at the end. Save and close the desktop project first. Use one write-enabled session per project and reopen the file in the editor to see the changes.

Read-only mode exposes `inspect_project`. Write mode also exposes `add_title`, `split_clip`, `set_clip_gain`, and `export`. Frame numbers use the project timeline’s 30 fps clock. Export accepts a new filename for MP4, WebM, MKV, WAV, or SRT next to the project. FFmpeg must be on PATH or beside CutAd for media export. Existing output files are protected.

CutAd MCP runs locally. Your AI client controls its model requests and access to tool results.
