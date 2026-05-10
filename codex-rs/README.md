# Codex CLI (Rust Implementation)

We provide Codex CLI as a standalone executable to ensure a zero-dependency install.

## Installing Codex

Today, the easiest way to install Codex is via `npm`:

```shell
npm i -g @openai/codex
codex
```

You can also install via Homebrew (`brew install --cask codex`) or download a platform-specific release directly from our [GitHub Releases](https://github.com/openai/codex/releases).

## Documentation quickstart

- First run with Codex? Start with [`docs/getting-started.md`](../docs/getting-started.md) (links to the walkthrough for prompts, keyboard shortcuts, and session management).
- Want deeper control? See [`docs/config.md`](../docs/config.md) and [`docs/install.md`](../docs/install.md).

## What's new in the Rust CLI

The Rust implementation is now the maintained Codex CLI and serves as the default experience. It includes a number of features that the legacy TypeScript CLI never supported.

### Config

Codex supports a rich set of configuration options. Note that the Rust CLI uses `config.toml` instead of `config.json`. See [`docs/config.md`](../docs/config.md) for details.

### Model Context Protocol Support

#### MCP client

Codex CLI functions as an MCP client that allows the Codex CLI and IDE extension to connect to MCP servers on startup. See the [`configuration documentation`](../docs/config.md#connecting-to-mcp-servers) for details.

#### MCP server (experimental)

Codex can be launched as an MCP _server_ by running `codex mcp-server`. This allows _other_ MCP clients to use Codex as a tool for another agent.

Use the [`@modelcontextprotocol/inspector`](https://github.com/modelcontextprotocol/inspector) to try it out:

```shell
npx @modelcontextprotocol/inspector codex mcp-server
```

Use `codex mcp` to add/list/get/remove MCP server launchers defined in `config.toml`, and `codex mcp-server` to run the MCP server directly.

### Notifications

You can enable notifications by configuring a script that is run whenever the agent finishes a turn. The [notify documentation](../docs/config.md#notify) includes a detailed example that explains how to get desktop notifications via [terminal-notifier](https://github.com/julienXX/terminal-notifier) on macOS. When Codex detects that it is running under WSL 2 inside Windows Terminal (`WT_SESSION` is set), the TUI automatically falls back to native Windows toast notifications so approval prompts and completed turns surface even though Windows Terminal does not implement OSC 9.

### `codex exec` to run Codex programmatically/non-interactively

To run Codex non-interactively, run `codex exec PROMPT` (you can also pass the prompt via `stdin`) and Codex will work on your task until it decides that it is done and exits. If you provide both a prompt argument and piped stdin, Codex appends stdin as a `<stdin>` block after the prompt so patterns like `echo "my output" | codex exec "Summarize this concisely"` work naturally. Output is printed to the terminal directly. You can set the `RUST_LOG` environment variable to see more about what's going on.
Use `codex exec --ephemeral ...` to run without persisting session rollout files to disk.

### Codex Teams orchestration (experimental)

This fork includes an experimental `codex team` workflow for coordinating multiple Codex sessions through an app-server runtime. A team has a live lead session plus peer departments such as `research`, `ops`, `reviewer`, or `remote_ops`. The lead reads the natural-language goal, creates the needed departments, places them on local/SSH/Docker execution nodes, and coordinates work through a shared mailbox, task state, side-channel replies, and periodic wakeups.

Start a team from a natural-language request:

```shell
codex team swarm "Use teams to inspect ssh saitou-h200, create a remote_ops department there, write a hello file, and have a local reviewer verify it." \
  --app-server \
  --language ja \
  --dangerously-bypass-approvals-and-sandbox
```

Inspect and operate a running team:

```shell
codex team status --team <team-id>
codex team monitor --team <team-id>
codex team ui --open
codex team message --team <team-id> --from user lead "Please continue from the previous result."
```

Track long-running or externally completed work:

```shell
codex team job --team <team-id> start --owner <department> --task <task-id> --node <node-id> -- <command...>
codex team wait --team <team-id> add "external request" --owner <department> --task <task-id> \
  --condition "the request result is saved and cited" \
  --progress "request id or status URL"
codex team wait --team <team-id> set <wait-id> --status completed --evidence <path-or-url>
```

`team job` is for commands the team runtime can launch and inspect by PID/log/exit status. `team wait` is the generic ledger for anything that has a completion condition but no reliable team-managed PID, such as external tool polling, service-side processing, human/account gates, or any other asynchronous dependency. A task with an open wait is not accepted as cleanly complete; when the wait completes or fails, the owner is resumed to inspect the result and publish the real handoff, next action, or blocker.

#### Remote node bootstrap

When the lead assigns a department to an SSH or Docker node, the team runtime bootstraps that execution site before starting the remote department session. The bootstrap is deterministic shell code, not an AI-written install step:

- Connect to the requested SSH host or container.
- Install basic dependencies when possible.
- Reuse an existing `codex` binary if one is already on `PATH`, under `$HOME/.codex/bin`, `$HOME/.local/bin`, or `$HOME/bin`.
- Otherwise download the matching release artifact from GitHub Releases, for example `codex-x86_64-unknown-linux-musl` on x86_64 Linux, and install it to `$HOME/bin/codex`.
- Install the `codex-team` helper under `$HOME/bin`.
- Start `codex app-server` on the remote node with SSH port forwarding back to the local team runtime.

After bootstrap, the AI department takes over normal work on that node: deciding commands, creating artifacts, reporting blockers, and handing results to other departments.

#### Device-auth automation

If the remote node has no `$HOME/.codex/auth.json`, the bootstrap runs `codex login --device-auth` on the remote side and the local team runtime captures the device URL/code from the log. The code is then completed automatically through a dedicated local Chromium profile:

```shell
codex team auth-browser login
codex team auth-browser status
codex team auth-browser authorize <DEVICE-CODE>
```

`auth-browser login` opens a dedicated browser profile. Sign in with Google there once. Later device-auth prompts can be completed automatically: a temporary Chrome extension observes the OpenAI device-auth page, clicks the Google/consent steps, fills the one-time code, and waits for a success state. This is rule-based browser automation, not an LLM decision. It can still fail if the provider changes the page, requires extra 2FA, shows an extended security challenge, or the dedicated browser profile is not signed in.

Local auth-browser automation requires:

- A local graphical session with an X11 display. The command uses `$DISPLAY` when set, and falls back to `DISPLAY=:1` when available.
- Chromium or Chrome on the local machine: `chromium-browser`, `chromium`, `google-chrome`, or `google-chrome-stable`.
- `xdotool` on the local machine so the runtime can find and activate the browser window.
- A writable dedicated browser profile. With Snap Chromium, the default profile is placed under `$HOME/snap/chromium/common/codex-team-auth-browser/chromium-profile` to avoid Snap profile-directory restrictions. Other installs use an XDG/local-data profile path.
- A Google-signed-in state in that dedicated profile. Run `codex team auth-browser login` once and complete Google sign-in there.
- Local network access to `https://auth.openai.com/codex/device` and the Google sign-in/consent pages.
- A remote `codex login --device-auth` log that contains the device URL and one-time code; the team runtime parses this output and forwards the code to the local auth-browser.

The auth-browser path is intentionally not headless and does not use Chrome DevTools Protocol for the OpenAI login page. It drives a normal browser window plus a temporary extension because remote-debugging based automation can trigger provider security checks more aggressively.

Known fragile cases:

- Google asks for additional 2FA or a manual security confirmation.
- The dedicated profile has multiple ambiguous Google accounts and the first account is not the intended one.
- The security verification page remains active for too long.
- OpenAI or Google changes the page structure or visible labels used by the rule-based extension.
- The Chromium profile is locked by another process or has become corrupt.
- No usable X11 display is available, for example on a fully headless local machine.

On a second run against the same remote node, bootstrap usually reuses:

- `$HOME/bin/codex`
- `$HOME/bin/codex-team`
- `$HOME/.codex/auth.json`
- synced team assets such as selected skills/config/MCP settings

That means subsequent runs normally skip the Codex download and device-auth flow, then only reconnect, refresh the helper, start the remote app-server, and launch the remote department sessions.

### Experimenting with the Codex Sandbox

To test to see what happens when a command is run under the sandbox provided by Codex, we provide the following subcommands in Codex CLI:

```
# macOS
codex sandbox macos [--log-denials] [COMMAND]...

# Linux
codex sandbox linux [COMMAND]...

# Windows
codex sandbox windows [COMMAND]...

# Legacy aliases
codex debug seatbelt [--log-denials] [COMMAND]...
codex debug landlock [COMMAND]...
```

To try a writable legacy sandbox mode with these commands, pass an explicit config override such
as `-c 'sandbox_mode="workspace-write"'`.

### Selecting a sandbox policy via `--sandbox`

The Rust CLI exposes a dedicated `--sandbox` (`-s`) flag that lets you pick the sandbox policy **without** having to reach for the generic `-c/--config` option:

```shell
# Run Codex with the default, read-only sandbox
codex --sandbox read-only

# Allow the agent to write within the current workspace while still blocking network access
codex --sandbox workspace-write

# Danger! Disable sandboxing entirely (only do this if you are already running in a container or other isolated env)
codex --sandbox danger-full-access
```

The same setting can be persisted in `~/.codex/config.toml` via the top-level `sandbox_mode = "MODE"` key, e.g. `sandbox_mode = "workspace-write"`.
In `workspace-write`, Codex also includes `~/.codex/memories` in its writable roots so memory maintenance does not require an extra approval.

## Code Organization

This folder is the root of a Cargo workspace. It contains quite a bit of experimental code, but here are the key crates:

- [`core/`](./core) contains the business logic for Codex. Ultimately, we hope this becomes a library crate that is generally useful for building other Rust/native applications that use Codex.
- [`exec/`](./exec) "headless" CLI for use in automation.
- [`tui/`](./tui) CLI that launches a fullscreen TUI built with [Ratatui](https://ratatui.rs/).
- [`cli/`](./cli) CLI multitool that provides the aforementioned CLIs via subcommands.

If you want to contribute or inspect behavior in detail, start by reading the module-level `README.md` files under each crate and run the project workspace from the top-level `codex-rs` directory so shared config, features, and build scripts stay aligned.
