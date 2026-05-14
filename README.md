<p align="center"><code>npm i -g @openai/codex</code><br />or <code>brew install --cask codex</code></p>
<p align="center"><strong>Codex CLI</strong> is a coding agent from OpenAI that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

## Codex Teams fork notes

This local fork adds an experimental `codex team` workflow for running a lead
session plus multiple department sessions across local, SSH, Docker, and
SSH-hosted Docker nodes. The intended use is to give a natural-language request
to a lead, let the lead create departments/nodes/tasks, and keep the team
coordinating through the shared mailbox, jobs, waits, journals, and dashboard.

### Local command setup

After building the Rust CLI, point a short `codex` command at this fork:

```shell
cd /home/yukimaru/codex/codex-rs
cargo build -p codex-cli
ln -sfn /home/yukimaru/codex/codex-rs/target/debug/codex ~/.local/bin/codex
codex --version
```

`~/.local/bin` should be earlier in `PATH` than any npm/brew Codex install.

### Start or attach to a team lead

Open an interactive lead session. This behaves like the normal Codex TUI, but
the session is the team lead and can create departments, tasks, SSH/Docker
nodes, jobs, waits, and handoffs.

```shell
codex team --yolo --language ja
```

Attach to an existing team lead:

```shell
codex team --team team-20260515033952 --yolo --language ja
```

Use a normal Codex session as a secretary by asking naturally for teams, for
example "teamsを使ってこのタスクを進めて". The `codex-team-secretary` skill can
start or attach to a team and relay user instructions to the lead.

### Run a one-shot team from the CLI

```shell
codex team swarm "小さなWebアプリを作って。設計、実装、レビュー部署で協調して。" \
  --app-server \
  --language ja \
  --dangerously-bypass-approvals-and-sandbox
```

By default team runs are keep-alive. To let an idle runtime pause itself:

```shell
codex team swarm "..." --app-server --idle-exit-after-sec 1800 --dangerously-bypass-approvals-and-sandbox
```

### Dashboard and realtime views

```shell
codex team ui --open
```

The dashboard shows team list/status, lead chat, team messages, tasks, jobs,
waits, nodes, token usage, member assignment/tasks, member journals, debug
events, and realtime/flow views.

Useful monitor command:

```shell
codex team monitor --team team-xxxx
```

### Status, pause, and resume

```shell
codex team list
codex team list --json
codex team status --team team-xxxx

codex team stop --team team-xxxx
codex team stop --all

codex team resume --team team-xxxx --dangerously-bypass-approvals-and-sandbox
```

Runtime labels:

- `running`: runtime process is active.
- `stop(idle)`: runtime is alive/idle or paused-like, with state preserved.
- `exiting`: runtime is stopped; team state remains on disk and can be resumed.
- `unknown`: state exists but runtime status cannot be confidently inferred.

### Cleanup

Local-only cleanup deletes `~/.codex/teams/<team-id>` on this PC:

```shell
codex team cleanup --team team-xxxx --force
```

Preview exiting teams before deleting:

```shell
codex team cleanup --exiting --dry-run
```

Delete all local exiting team states:

```shell
codex team cleanup --exiting --force
```

Distributed cleanup can also touch registered SSH/Docker nodes:

```shell
codex team cleanup --team team-xxxx --force --remote-state
codex team cleanup --team team-xxxx --force --remote-state --containers
```

`--remote-state` removes matching `~/.codex/teams/<team-id>` state on registered
SSH/Docker/SSH-Docker nodes. `--containers` also removes registered Docker or
SSH-Docker containers. Add `--ignore-remote-errors` if stale/unreachable remote
nodes should not block local cleanup.

### SSH and Docker nodes

The lead can add nodes dynamically when the task needs them. Manual commands are
also available:

```shell
codex team node --team team-xxxx list
codex team node --team team-xxxx add saitou --kind ssh --host saitou --cwd /data2/nonaka
codex team node --team team-xxxx create-docker runtime \
  --host saitou \
  --image nvidia/cuda:12.4.1-devel-ubuntu22.04 \
  --mount /data2/nonaka/work:/workspace \
  --gpus \
  --replace
codex team node --team team-xxxx sync-assets runtime
codex team node --team team-xxxx remove runtime --force
```

Docker/SSH-Docker nodes should be registered after a real long-lived container
exists. Runtime execution, installs, tests, rendering, and verification should
move to a container-internal department instead of staying hidden behind
host-side `docker exec`.

### Remote Codex install and auth

For a new SSH host, the bootstrap flow can install/use Codex remotely and then
authenticate via device auth. The preferred local prerequisite is:

- local PC has a browser session capable of signing in to ChatGPT/OpenAI;
- SSH host has `git` and basic shell tools;
- passwordless `sudo` is used when package install is needed and available.

Auth browser helper:

```shell
codex team auth-browser login
codex team auth-browser status
codex team auth-browser authorize CODE-12345
```

If automated device auth cannot complete after retries, the fallback may copy
local auth state when explicitly allowed by the team bootstrap path.

### Jobs and waits

Use jobs for trackable long-running commands and waits for external/non-PID
conditions:

```shell
codex team job --team team-xxxx start --owner runtime --task 1 --node runtime --cwd /workspace -- \
  bash -lc "pytest -q"
codex team job --team team-xxxx status job-1
codex team job --team team-xxxx logs job-1 --tail 80

codex team wait --team team-xxxx add "deep research request" \
  --owner research \
  --task 1 \
  --condition "MCP result artifact exists and contains all required sections" \
  --progress "request submitted"
codex team wait --team team-xxxx list
codex team wait --team team-xxxx set wait-1 --status completed --progress "artifact written" --evidence /path/to/result.md
```

### Member journals and token usage

Each member gets a periodic activity journal while the runtime is active:

```text
~/.codex/teams/<team-id>/member_journals/<member>.jsonl
~/.codex/teams/<team-id>/member_journals/<member>.md
~/.codex/teams/<team-id>/member_journals/<member>.digest.md
~/.codex/teams/<team-id>/member_journals/<member>.digest.jsonl
```

The `.md` and `.jsonl` files are machine journals generated from tasks, jobs,
waits, mailbox messages, events, and last output. The `.digest.md` file is an
AI-written interpretation generated only at milestone changes such as task/job/
wait/member completion, blocking, or failure. The digest explains what the
department was thinking, what progressed, what is stuck, and what should happen
next. Remote/SSH/Docker nodes receive read-only journal copies under
`$HOME/.codex/teams/<team-id>/member_journals/`.

The dashboard also shows these under the member panels. Token usage is available
in the dashboard with input/cached/uncached/output breakdowns by feature/cell so
that expensive triggers such as `team_message`, `lead_tick`, `idle_wakeup`, and
`department_heartbeat` can be inspected.

## Quickstart

### Installing and running Codex CLI

Install globally with your preferred package manager:

```shell
# Install using npm
npm install -g @openai/codex
```

```shell
# Install using Homebrew
brew install --cask codex
```

Then simply run `codex` to get started.

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
