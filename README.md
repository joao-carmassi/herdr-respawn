# herdr-respawn

A [herdr](https://herdr.dev) plugin that remembers the long-running processes you start in panes
(dev servers, watchers) and runs them again, in the same panes, when herdr restarts.

It does not rename or restructure anything: it only types the saved command into the pane
(`herdr pane run`) once the shell is ready, and creates a new tab if the pane is gone.

## Install

```sh
herdr plugin install joao-carmassi/herdr-respawn --yes
```

The build step downloads the prebuilt binary for your platform (checksum verified) and falls back
to `cargo build --release` if there is no matching release.

## Save a service

Start the process in a pane (`npm run start`), then do one of:

- Run the plugin action **`herdr-respawn.save`** ("Respawn: save this pane's process") with the pane
  focused. Bind it to a key in herdr's `config.toml`:

  ```toml
  [[keys.command]]
  key = "prefix+r"
  type = "plugin_action"
  command = "herdr-respawn.save"
  ```

  The service is named after the directory (`my-app`, `my-app-2`, ...). The command is rebuilt from
  the running process (`node .../npm-cli.js run start` becomes `npm run start`), so it works on a
  pane that is busy running it. Panes running an agent are refused.
- Or use the CLI. In the target pane the process is not running yet, so pass the command:

  ```sh
  herdr-respawn save cinemix -- npm run start
  herdr-respawn save cinemix --pane w5:p1K        # take the command from that pane's process
  ```

Other actions: **`herdr-respawn.forget`** (forget this pane's service) and **`herdr-respawn.restore`**
(restore everything now). The same from the CLI:

```sh
herdr-respawn list [--json]     # name, live status (running / idle / missing), pane, command
herdr-respawn remove <name>
herdr-respawn restore [--name <name>]
herdr-respawn version
```

The binary lives in the plugin root (`herdr plugin list --json` shows `plugin_root`). To use it
as a command, alias it, for example in PowerShell:

```powershell
$root = (herdr plugin list --json | ConvertFrom-Json).result.plugins |
  Where-Object plugin_id -eq 'herdr-respawn' | ForEach-Object plugin_root
Set-Alias herdr-respawn "$root\bin\herdr-respawn.exe"
```

## What happens on restart

When herdr starts (after it has restored the session) the plugin's `startup` command runs:

1. Waits for herdr's API to answer (up to 30 s).
2. For each enabled service finds a pane: the saved `pane_id`; otherwise a free pane in the same
   workspace (by id, else by workspace label) with the same directory; otherwise it creates a tab.
3. Skips a pane that runs an agent or already has a process (so restore is safe to repeat).
4. Waits for the shell prompt, types the command (with a `cd` first if the pane is elsewhere), and
   checks that a process appeared.
5. Writes the new pane id back to `services.json`.

## services.json

In the plugin config dir (`herdr plugin config-dir herdr-respawn`):

```json
{
  "version": 1,
  "services": [
    {
      "name": "cinemix",
      "command": "npm run start",
      "cwd": "D:\\cinemix",
      "pane_id": "w5:p1K",
      "workspace_id": "w5",
      "workspace_label": "Carmassi",
      "enabled": true
    }
  ]
}
```

`command` is shell text typed as-is into the pane; edit it freely. Set `enabled` to `false` to keep
a service without restoring it.

**Trust:** this file is run automatically every time herdr starts. Treat it like a shell profile:
anyone who can write to it can run commands as you.

## Notes

- On Windows herdr lists only the shell as the pane's foreground process, so the plugin asks WMI
  (through `powershell`) for the shell's children to see and rebuild the running command.
- Commands longer than 4096 bytes are refused.
- Output of actions and startup is in `herdr plugin log list --plugin herdr-respawn`.

## Development

```sh
cargo build --release && sh bin/fetch.sh      # Windows: bin\fetch.ps1; copies the local build into bin/
herdr plugin link /path/to/herdr-respawn
cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --locked
```

Releases: bump the version in `Cargo.toml` and `herdr-plugin.toml`, commit, tag `vX.Y.Z` and push the
tag; the release workflow builds and attaches the five binaries with their `.sha256` files.

MIT licensed.
