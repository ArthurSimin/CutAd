# CutAd

## Product goals

- Build an independent, open-source desktop video editor for Linux and Windows from one shared codebase. Give CutAd its own name, visual design, and assets.
- Ship the same editing features on both platforms. Make every built-in feature available for free, with no ads, promotions, subscription prompts, mandatory accounts, or export watermarks.
- Use CapCut's documented editing workflows as a feature reference. Implement comparable capabilities in CutAd with original code and redistributable dependencies. Use original or openly licensed templates, fonts, effects, footage, music, and models. Keep proprietary code, assets, service APIs, and subscription mechanisms out of the project.
- Make local editing, project persistence, and export work offline. Keep user media and project data local by default.

## Workspace and REA

The workspace starts with `CapCut_7710642509730349063_installer.exe`. `file` identifies it as a 32-bit Windows NSIS installer. Its SHA-256 is `69dbc6f939bf4ac63a90dc56e1e9600b4d4284848a99c4190423c3f856c5961e`. CutAd is implemented in Rust. See README.md for the current feature coverage and build commands.

- Keep the installer local and outside version control and release packages. Use it for read-only artifact identification and interoperability research. Base implementation on public documentation, independently specified behavior, and original test fixtures.
- Use Reverse Engineer Anything (REA) for questions that require shipped-artifact evidence. Record the input digest, tool version, Evidence ID, findings, and limitations. The installer inspection establishes facts about the installer itself. Installed editor architecture and editing behavior require separate evidence.
- The installed REA CLI is `6.5.0`. This read-only command worked without an analysis engine and returned Evidence ID `ev_cf2195c2db2306abf10013d6706b6428ea39c5ced696625772c6a4e28c24f78d` for the PE resource inventory.

  ```sh
  rea inspect-pe-resources "$PWD/CapCut_7710642509730349063_installer.exe" --json
  ```

- REA is registered with Codex. The app needs to reload its MCP configuration to expose new tools in a session. The CLI remains available for artifact inspection. [REA installation guide](https://github.com/morluto/rea/blob/main/docs/getting-started/installation.md)

## Development milestones

1. **Editing and export.** Import local video, audio, and images. Build a multitrack timeline with trims, splits, snapping, undo and redo, and synchronized preview. Save, reopen, and export a project on both platforms.
2. **Creative controls.** Add keyframes, transforms, speed curves, transitions, masks, chroma key, color correction, text, subtitle tracks, and audio mixing. Build editable templates from licensed assets.
3. **Advanced features.** Add automatic captions, background removal, motion tracking, auto reframe, audio cleanup and isolation, optical flow, and enhancement using redistributable components. Track generative tools, asset libraries, and collaboration as separate workflows with their own dependencies and acceptance tests. [CapCut desktop feature reference](https://www.capcut.com/resource/pc-professional-video-editor) · [CapCut Standard and Pro feature reference](https://www.capcut.com/resource/capcut-standard-vs-pro)

## Implementation and verification

- Choose the desktop toolkit, media engine, and project license before building the first release. Verify that dependencies, codecs, models, and assets can ship under compatible licenses, and record their provenance and notices.
- Keep the timeline and versioned project format independent of the UI and platform packaging. Use the same timing, transforms, and effects in preview and export. Keep source media intact when users edit or save projects.
- Track feature coverage against a dated desktop reference. For each feature, record the expected workflow, known limitations, and Linux and Windows verification. Claim parity only for workflows exercised on both platforms.
- Use small original or openly licensed media fixtures. Verify that the same saved project can reopen on both platforms and exports with the expected duration, frame timing, audio sync, and resolution. Exercise missing media, mixed frame rates, and canceled exports as those workflows become available.
- Verify the complete import, edit, save, reopen, and export flow on both operating systems before publishing packages. Confirm that the packaged editor opens and exports offline without ads, upgrade prompts, or watermarks. Document the actual build and test commands when they exist.
