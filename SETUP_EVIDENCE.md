# CapCut Windows setup evidence

Inspected November 21, 2026. The supplied `CapCut_7710642509730349063_installer.exe` is 2,879,000 bytes and has SHA-256 `69dbc6f939bf4ac63a90dc56e1e9600b4d4284848a99c4190423c3f856c5961e`. The setup and extracted code were analyzed statically.

## REA and extraction evidence

| Scope | Tool and evidence | Finding |
| --- | --- | --- |
| Outer setup executable | REA CLI 6.5.0 `inspect-artifact`, `ev_77370f6e32792a158d6c89504ab519d72909ab81c698f8661ec29a51b8e20136` | The root is a PE executable. The PE reader inventories the root, and `extract-artifact` reports that the PE format has no extraction reader. |
| PE resources | REA CLI 6.5.0 `inspect-pe-resources`, `ev_cf2195c2db2306abf10013d6706b6428ea39c5ced696625772c6a4e28c24f78d` | The static PE resource inventory has five resources. This is a separate inventory from the appended NSIS payload. |
| Appended NSIS payload | 7-Zip 26.06, `7z l -slt` and `7z x` | The NSIS-3 Unicode payload starts at byte 307,200 and uses Deflate. Its five regular files were recovered without running the installer. |
| Embedded resource ZIP | REA CLI 6.5.0 `inspect-artifact`, `ev_2c01182368ac9c75115e87a1addac5ffdd732272af3eb033287db60fb46870c5` | Central-directory inventory is complete. It records 65 regular files plus the root. |
| Materialized ZIP contents | REA CLI 6.5.0 `extract-artifact`, `ev_c83d6970bd71b669f7e505cab4d35d37603380bd9801ba95ed30e1da6e4a3000` | REA extracted 32 PNGs, two JPEGs, 26 JSON files, and five XML files with verified containment. XML filenames identify install and uninstall views and dialogs. |

## Exact files in the NSIS payload

All five files are under `$PLUGINSDIR`. Hashes describe the extracted bytes.

| File | Bytes | SHA-256 |
| --- | ---: | --- |
| `BgWorker.dll` | 2,560 | `50f735ab8f3473423e6873d628150bbc0777be7b4f6405247cddf22bb00fb6be` |
| `System.dll` | 12,288 | `3eb38ae99653a7dbc724132ee240f6e5c4af4bfe7c01d31d23faf373f9f2eaca` |
| `downloader_nsis_plugin.dll` | 1,464,832 | `e5f5627134d9792e63cbc3867a0ab47fd02bdb86fe39d91c826bc669da8dfd34` |
| `res.zip` | 787,761 | `b0e00d3e7e22dbbbc5fbd5bfcb1d71034355beb950115b7197e0a0a11409c50e` |
| `shell_downloader.dll` | 2,371,072 | `685e9f64a1915f3663f7ebd26cc2e23621e82e5bea94c04cc9037b039e30bbf9` |

The supplied file is a bootstrap installer. Its embedded DLLs and resources establish what ships in that setup. The installed editor, downloadable effects, Pro controls, and account-dependent services require separate evidence. The [desktop workflow inventory](CAPCUT_WORKFLOWS.md) covers workflows documented by CapCut. CutAd implements them independently with original code and redistributable dependencies.

## Reproduce locally

```sh
setup="$PWD/CapCut_7710642509730349063_installer.exe"
work=$(mktemp -d)
sha256sum "$setup"
rea inspect-artifact "$setup" --json
rea inspect-pe-resources "$setup" --json
7z x "-o$work" "$setup" -sns-
rea inspect-artifact "$work/"'$PLUGINSDIR/res.zip' --json
rea extract-artifact "$work/"'$PLUGINSDIR/res.zip' --json
```

The extraction stays in temporary directories outside the CutAd source tree and releases.
