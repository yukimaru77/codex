# Codex Remote Teams

このリポジトリは OpenAI Codex CLI をベースに、複数の Codex セッションを
「lead」と「部署」として束ねる実験的な `codex team` 機能を追加したフォークです。

目的は、ユーザーが自然言語で大きめの作業を渡したときに、lead が必要な部署、
SSH ノード、Docker ノード、タスク、長時間ジョブ、待ち条件を作り、各セッションが
メールボックス、handoff、journal、dashboard を通じて協調しながら進めることです。

## できること

- `codex team --yolo --language ja` で lead 本体と対話する
- 通常の `codex --yolo` セッションから secretary skill 経由で team を起動する
- local / SSH / Docker / SSH 上 Docker の各場所に部署セッションを立てる
- remote 側にも app-server を立て、remote Codex のネイティブツールを使わせる
- team message / task / job / wait / node / journal で状態を共有する
- 長時間処理を `team job`、外部完了待ちを `team wait` で追跡する
- idle wakeup、heartbeat、lead tick、task watchdog で止まりっぱなしを検知する
- side-channel reply で作業中 turn を止めずに短い返信を返す
- member journal と AI digest journal で各部署の活動履歴を確認する
- Web UI で team 一覧、lead chat、messages、tasks、jobs、waits、nodes、token 使用量、journal、flow を見る
- exiting team の local / remote state や Docker container を cleanup する

## ビルドと `codex` コマンド

```shell
cd /home/yukimaru/codex/codex-rs
cargo build -p codex-cli
ln -sfn /home/yukimaru/codex/codex-rs/target/debug/codex ~/.local/bin/codex
codex --version
```

`~/.local/bin` が npm / brew 版 Codex より先に `PATH` に入っている必要があります。

## lead と直接対話する

```shell
codex team --yolo --language ja
```

これは通常の Codex TUI に近い操作感ですが、会話相手は team の lead です。
lead は必要に応じて部署、task、SSH/Docker node、job、wait、handoff を作ります。

既存 team の lead に再接続する場合:

```shell
codex team --team team-xxxxxxxxxxxxxx --yolo --language ja
```

`--language ja` を付けると、team runtime が送る自然文プロンプト、team message、
status、debug log も原則日本語になります。コマンド名、path、JSON key、status enum
などの機械可読語は英語のままです。

## 1回の CLI 実行で team を起動する

```shell
codex team swarm "小さなWebアプリを作って。設計、実装、レビュー部署で協調して。" \
  --app-server \
  --language ja \
  --dangerously-bypass-approvals-and-sandbox
```

デフォルトでは keep-alive です。完了後に一定時間 idle なら runtime を終了させたい場合:

```shell
codex team swarm "..." \
  --app-server \
  --idle-exit-after-sec 1800 \
  --dangerously-bypass-approvals-and-sandbox
```

完了したらそのまま終了させたい場合:

```shell
codex team swarm "..." \
  --app-server \
  --no-keep-alive \
  --dangerously-bypass-approvals-and-sandbox
```

## UI と監視

```shell
codex team ui --open
```

UI では以下を確認できます。

- team 一覧と runtime 状態
- lead chat
- Team Messages
- Member Tasks
- Jobs / Waits
- Nodes
- Token Usage
- Member Journals / AI Digest
- Debug Events
- Realtime / Flow view

CLI で監視する場合:

```shell
codex team monitor --team team-xxxxxxxxxxxxxx
codex team status --team team-xxxxxxxxxxxxxx
codex team list
codex team list --json
```

runtime 状態の目安:

- `running`: runtime process が動作中
- `stop(idle)`: runtime は動いているが、実質的にやることがなく待機中
- `exiting`: runtime は停止済みで、state は disk に残っている
- `unknown`: state はあるが runtime 状態を確定できない

## 停止、再開、cleanup

```shell
codex team stop --team team-xxxxxxxxxxxxxx
codex team stop --all
codex team resume --team team-xxxxxxxxxxxxxx --dangerously-bypass-approvals-and-sandbox
```

local state だけを削除:

```shell
codex team cleanup --team team-xxxxxxxxxxxxxx --force
```

exiting team を確認してから削除:

```shell
codex team cleanup --exiting --dry-run
codex team cleanup --exiting --force
```

remote state や container も含めて削除する場合:

```shell
codex team cleanup --team team-xxxxxxxxxxxxxx --force --remote-state
codex team cleanup --team team-xxxxxxxxxxxxxx --force --remote-state --containers
```

`--remote-state` は登録済み SSH / Docker / SSH-Docker node 側の
`~/.codex/teams/<team-id>` も削除します。`--containers` は登録済み container も削除します。
古い remote や停止済み container が原因で失敗しても local を消したい場合は
`--ignore-remote-errors` を付けます。

## SSH / Docker node

lead はタスク内容から必要と判断した場合に node を追加できます。手動でも操作できます。

```shell
codex team node --team team-xxxxxxxxxxxxxx list
codex team node --team team-xxxxxxxxxxxxxx add saitou --kind ssh --host saitou --cwd /data2/nonaka
codex team node --team team-xxxxxxxxxxxxxx create-docker runtime \
  --host saitou \
  --image nvidia/cuda:12.4.1-devel-ubuntu22.04 \
  --mount /data2/nonaka/work:/workspace \
  --gpus \
  --replace
codex team node --team team-xxxxxxxxxxxxxx sync-assets runtime
codex team node --team team-xxxxxxxxxxxxxx remove runtime --force
```

Docker image / container を作ったら、原則として Docker node を登録し、container 内部部署に
install、runtime 実行、test、smoke、rendering、verification を担当させます。
host 側部署が `docker exec` で本作業を隠し持ち続ける運用は避けます。

## remote Codex install と auth

新しい SSH host では、bootstrap flow が remote Codex の確認、install、device auth を扱います。
前提はできるだけ小さくしています。

- remote host に `git` と基本 shell tools がある
- package install が必要な場合、passwordless sudo が使えるなら使用する
- local PC 側で ChatGPT/OpenAI にログインできる browser 環境がある

auth browser helper:

```shell
codex team auth-browser login
codex team auth-browser status
codex team auth-browser authorize CODE-12345
```

remote の `codex login --device-auth` で得た device code を local helper に渡して認証します。
自動 device auth が規定回数失敗した場合のみ、明示的に許可された bootstrap path で
local auth state copy fallback を使います。

## Jobs と Waits

PID を追える長時間コマンドは `job` にします。

```shell
codex team job --team team-xxxxxxxxxxxxxx start \
  --owner runtime \
  --task 1 \
  --node runtime \
  --cwd /workspace \
  -- bash -lc "pytest -q"

codex team job --team team-xxxxxxxxxxxxxx status job-1
codex team job --team team-xxxxxxxxxxxxxx logs job-1 --tail 80
```

外部 API、MCP、手動承認、非 PID の完了条件などは `wait` にします。

```shell
codex team wait --team team-xxxxxxxxxxxxxx add "deep research request" \
  --owner research \
  --task 1 \
  --condition "MCP result artifact exists and contains all required sections" \
  --progress "request submitted"

codex team wait --team team-xxxxxxxxxxxxxx list
codex team wait --team team-xxxxxxxxxxxxxx set wait-1 \
  --status completed \
  --progress "artifact written" \
  --evidence /path/to/result.md
```

機械判定できる wait では `AUTO_CHECK` を使うよう lead prompt で指示しています。

## Member Journal

各部署には machine journal と AI digest journal が作られます。

```text
~/.codex/teams/<team-id>/member_journals/<member>.jsonl
~/.codex/teams/<team-id>/member_journals/<member>.md
~/.codex/teams/<team-id>/member_journals/<member>.digest.jsonl
~/.codex/teams/<team-id>/member_journals/<member>.digest.md
~/.codex/teams/<team-id>/member_journals/<member>.digest.prompt.md
~/.codex/teams/<team-id>/member_journals/<member>.digest.log
```

machine journal は task、job、wait、mailbox、event、last output から生成されます。
AI digest journal は task / job / wait / member の完了、blocked、failed などの
milestone で生成され、「この部署は何を考え、何に詰まり、何を進め、次に何が必要か」を
自然文でまとめます。

remote / SSH / Docker node には、team state sync の一部として journal が読み取り用に配られます。
各部署の prompt には、自分と関係部署の journal を確認して背景を理解し、必要なら具体的に議論するよう
指示しています。

## Token Usage

Dashboard の Token Usage では、feature / cell ごとに以下を確認できます。

- `input`: turn に投入された総入力 token
- `cached`: prompt cache に乗った入力 token
- `uncached`: cache されなかった入力 token
- `output`: 生成 token

主な feature:

- `team_message`: 通常の部署間連絡、lead 指示、報告、handoff、blocker 相談
- `side_channel_reply`: 本流 turn を止めない短い返信
- `lead_initial`: team 起動直後の lead 初回判断
- `lead_turn`: lead の通常判断
- `lead_tick`: lead の定期 orchestration tick
- `department_start`: 部署起動時の初回 turn
- `idle_wakeup`: 部署単位の idle 起床
- `department_heartbeat`: 進捗、blocker、成果物、相談事項の定期報告

`cached / uncached` を分けて見ることで、実際に無駄が多いのか、長い共有コンテキストが
cache hit しているだけなのかを判断しやすくしています。

## 開発メモ

Teams 実装は `codex-rs/cli/src/team_cmd.rs` から
`codex-rs/cli/src/team_cmd/` 配下へ分割しています。現時点では既存の private item 関係を
保つため `include!` ベースです。

主な分割:

- `prelude.rs`: 型、import、共通 helper
- `interactive.rs`: `codex team` 対話 lead
- `design.rs`: team design / department planning
- `runtime.rs`: runtime state と keep-alive
- `relay.rs`: message relay
- `app_events.rs`: app-server 実行、turn、side-channel、event
- `nodes_auth.rs`: node、remote bootstrap、auth helper
- `journals.rs`: machine journal / digest journal
- `tasks_nodes.rs`: task、node、member 操作
- `jobs_waits.rs`: job / wait 操作
- `ui.rs`: Web dashboard
- `cleanup.rs`: cleanup / list / status 系
- `prompts_storage.rs`: runtime prompt / storage helper
- `audit_status.rs`: audit / status helper
- `tests.rs`: unit tests

## 確認コマンド

```shell
cd /home/yukimaru/codex/codex-rs
cargo fmt
cargo build -p codex-cli
cargo test -p codex-cli status_text_warns_when_runtime_stopped_with_open_work -- --nocapture
```

## ライセンス

このフォークは元リポジトリと同じく Apache-2.0 License です。詳細は `LICENSE` を参照してください。
