#!/usr/bin/env bash
set -Eeuo pipefail

if [ "$#" -ne 4 ]; then
  echo "usage: $0 <target_tag> <previous_branch> <new_branch> <previous_tag>" >&2
  exit 64
fi

TARGET_TAG="$1"
PREVIOUS_BRANCH="$2"
NEW_BRANCH="$3"
PREVIOUS_TAG="$4"

REPO_ROOT="$(git rev-parse --show-toplevel)"
ARTIFACT_BASE="${RUNNER_TEMP:-$REPO_ROOT/artifacts}"
ARTIFACT_DIR="${ENV_SWITCH_ARTIFACT_DIR:-$ARTIFACT_BASE/env-switch-autocompile}"
LOG_DIR="$ARTIFACT_DIR/logs"
MAX_ATTEMPTS="${ENV_SWITCH_CODEX_ATTEMPTS:-3}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
E2E_SCRIPT="$ARTIFACT_DIR/env-switch-e2e.sh"
HEARTBEAT_SECONDS="${ENV_SWITCH_HEARTBEAT_SECONDS:-60}"
HEARTBEAT_PID=""

mkdir -p "$LOG_DIR"
cp "$SCRIPT_DIR/env-switch-e2e.sh" "$E2E_SCRIPT"
chmod +x "$E2E_SCRIPT"

cd "$REPO_ROOT"

start_heartbeat() {
  local label="$1"
  (
    while true; do
      sleep "$HEARTBEAT_SECONDS"
      printf '[env-switch-autocompile] heartbeat: %s still running at %s\n' \
        "$label" "$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
    done
  ) &
  HEARTBEAT_PID=$!
}

stop_heartbeat() {
  local pid="$HEARTBEAT_PID"
  HEARTBEAT_PID=""
  if [ -n "$pid" ]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}

run_logged() {
  local name="$1"
  shift
  local logfile="$LOG_DIR/$name.log"
  echo "==> $name: $*" | tee "$logfile"
  start_heartbeat "$name"
  set +e
  "$@" 2>&1 | tee -a "$logfile"
  local status=${PIPESTATUS[0]}
  set -e
  stop_heartbeat
  echo "==> $name exit: $status" | tee -a "$logfile"
  return "$status"
}

run_logged_shell() {
  local name="$1"
  shift
  local logfile="$LOG_DIR/$name.log"
  echo "==> $name: $*" | tee "$logfile"
  start_heartbeat "$name"
  set +e
  bash -lc "$*" 2>&1 | tee -a "$logfile"
  local status=${PIPESTATUS[0]}
  set -e
  stop_heartbeat
  echo "==> $name exit: $status" | tee -a "$logfile"
  return "$status"
}

latest_failure_context() {
  {
    echo "# Latest failure context"
    echo
    for file in "$LOG_DIR"/*.log; do
      [ -e "$file" ] || continue
      echo "## $(basename "$file")"
      tail -200 "$file"
      echo
    done
  } > "$ARTIFACT_DIR/latest-failure-context.md"
}

write_port_prompt() {
  local attempt="$1"
  local prompt_file="$ARTIFACT_DIR/port-prompt-$attempt.md"
  cat > "$prompt_file" <<EOF
You are porting the env_switch runtime execution-target prototype to a new upstream Codex Rust release.

Target upstream tag: $TARGET_TAG
New branch: $NEW_BRANCH
Previous env-switch branch: $PREVIOUS_BRANCH
Previous upstream base tag: $PREVIOUS_TAG

Context files in this run:
- $ARTIFACT_DIR/previous-env-switch.diff
- $ARTIFACT_DIR/previous-env-switch-commits.txt
- $ARTIFACT_DIR/latest-failure-context.md, if present

Goal:
Port the env_switch implementation from the previous env-switch branch onto $TARGET_TAG with the smallest coherent change.

Feature intent:
- Runtime execution targets for local, SSH, Docker, and nested SSH > Docker environments.
- env_switch registers targets and makes them the default for compatible tool calls.
- env_status/env_list recover visible environment IDs after compaction or uncertainty.
- exec_command, apply_patch, and view_image can target an explicit environment_id and otherwise use the env_switch default.
- Subagents can inherit or use parent-visible environment metadata without leaking unrelated thread environments.
- TUI status shows active non-local target badges.
- Raw ssh/docker commands should produce lightweight advisory guidance when the feature is enabled.
- Remote provisioning is versioned and isolated from any user-installed codex binary on the remote host.

Required local verification:
- Run just fmt from codex-rs after code changes.
- Run the workflow's targeted unit/integration/TUI matrix. It intentionally
  avoids the full codex-core suite because the self-hosted runner E2E gate is
  the primary acceptance test and the complete core suite has unrelated local
  helper-binary and timeout noise in this environment.
- Do not run cargo test directly.
- Follow AGENTS.md instructions in this repository.

Compatibility notes from the rust-v0.141 port path:
- ToolExecutor implementations use boxed futures rather than async_trait.
- TurnEnvironment fields are private on newer upstreams; use constructors and
  accessors rather than struct literals where required.
- PathUri cwd values should be constructed through the upstream helpers.
- Clone the EnvironmentManager Arc before moving it into per-thread state.
- Remote shell handling may need to convert Option<Shell> to the expected path
  or command representation before invoking upstream helpers.

If the repository is in the middle of a rebase or has conflicts, resolve them and continue the port.
Keep changes focused on env_switch and compatibility with the new upstream tag.
EOF

  if [ -f "$ARTIFACT_DIR/latest-failure-context.md" ]; then
    {
      echo
      echo "The previous attempt failed. Use this failure context:"
      echo
      cat "$ARTIFACT_DIR/latest-failure-context.md"
    } >> "$prompt_file"
  fi
  printf '%s\n' "$prompt_file"
}

run_codex_prompt() {
  local prompt_file="$1"
  local output_file="$2"

  if [ -n "${CODEX_AUTOCOMPILE_CMD:-}" ]; then
    start_heartbeat "codex autocompile command"
    set +e
    CODEX_AUTOCOMPILE_PROMPT="$prompt_file" bash -lc "$CODEX_AUTOCOMPILE_CMD" \
      2>&1 | tee "$output_file"
    local status=${PIPESTATUS[0]}
    set -e
    stop_heartbeat
    return "$status"
  fi

  start_heartbeat "codex attempt $(basename "$prompt_file")"
  set +e
  codex --sandbox danger-full-access --ask-for-approval never exec - < "$prompt_file" \
    2>&1 | tee "$output_file"
  local status=${PIPESTATUS[0]}
  set -e
  stop_heartbeat
  return "$status"
}

validate_worktree() {
  local attempt="$1"
  run_logged_shell "attempt-$attempt-just-fmt" "cd codex-rs && just fmt" || return 1
  run_logged_shell "attempt-$attempt-test-apply-patch" "cd codex-rs && just test -p codex-apply-patch" || return 1
  run_logged_shell "attempt-$attempt-test-app-server-protocol" "cd codex-rs && just test -p codex-app-server-protocol" || return 1
  run_logged_shell "attempt-$attempt-test-exec-server-environment" "cd codex-rs && just test -p codex-exec-server environment" || return 1
  run_logged_shell "attempt-$attempt-test-exec-server-provision" "cd codex-rs && just test -p codex-exec-server provision" || return 1
  run_logged_shell "attempt-$attempt-test-core-env-switch" "cd codex-rs && just test -p codex-core env_switch" || return 1
  run_logged_shell "attempt-$attempt-test-core-env-status" "cd codex-rs && just test -p codex-core env_status" || return 1
  run_logged_shell "attempt-$attempt-test-core-environment-selection" "cd codex-rs && just test -p codex-core environment_selection" || return 1
  run_logged_shell "attempt-$attempt-test-core-remote-advisory" "cd codex-rs && just test -p codex-core remote_command_advisory" || return 1
  run_logged_shell "attempt-$attempt-test-core-unified-exec-env-switch" "cd codex-rs && just test -p codex-core unified_exec_advises_env_switch" || return 1
  run_logged_shell "attempt-$attempt-test-tui-env-switch" "cd codex-rs && just test -p codex-tui env_switch" || return 1

  if [ "${ENV_SWITCH_FULL_CRATE_TESTS:-false}" = "true" ]; then
    run_logged_shell "attempt-$attempt-test-core-full" "cd codex-rs && just test -p codex-core" || return 1
    run_logged_shell "attempt-$attempt-test-tui-full" "cd codex-rs && just test -p codex-tui" || return 1
  fi

  if [ "${RUN_ENV_SWITCH_E2E:-true}" = "true" ]; then
    run_logged "attempt-$attempt-e2e" "$E2E_SCRIPT" || return 1
  fi
}

git fetch --no-tags --prune origin \
  "+refs/heads/main:refs/remotes/origin/main" \
  "+refs/heads/$PREVIOUS_BRANCH:refs/remotes/origin/$PREVIOUS_BRANCH"
git fetch --no-tags upstream \
  "+refs/tags/$TARGET_TAG:refs/tags/$TARGET_TAG" \
  "+refs/tags/$PREVIOUS_TAG:refs/tags/$PREVIOUS_TAG"

PREVIOUS_REF="origin/$PREVIOUS_BRANCH"
if ! git rev-parse --verify --quiet "$PREVIOUS_REF" >/dev/null; then
  echo "Previous branch not found: $PREVIOUS_REF" >&2
  exit 1
fi
if ! git rev-parse --verify --quiet "$TARGET_TAG" >/dev/null; then
  echo "Target tag not found locally after fetch: $TARGET_TAG" >&2
  exit 1
fi
if ! git rev-parse --verify --quiet "$PREVIOUS_TAG" >/dev/null; then
  echo "Previous base tag not found locally after fetch: $PREVIOUS_TAG" >&2
  exit 1
fi

git diff --binary "$PREVIOUS_TAG..$PREVIOUS_REF" > "$ARTIFACT_DIR/previous-env-switch.diff"
git log --oneline --reverse "$PREVIOUS_TAG..$PREVIOUS_REF" > "$ARTIFACT_DIR/previous-env-switch-commits.txt"

git switch -C "$NEW_BRANCH" "$PREVIOUS_REF"

set +e
git rebase --onto "$TARGET_TAG" "$PREVIOUS_TAG"
rebase_status=$?
set -e
if [ "$rebase_status" -ne 0 ]; then
  echo "Initial rebase stopped with conflicts; Codex will resolve them." | tee "$LOG_DIR/initial-rebase.log"
fi

success=false
for attempt in $(seq 1 "$MAX_ATTEMPTS"); do
  prompt_file="$(write_port_prompt "$attempt")"
  set +e
  run_codex_prompt "$prompt_file" "$LOG_DIR/attempt-$attempt-codex.log"
  codex_status=$?
  set -e
  if [ "$codex_status" -ne 0 ]; then
    echo "Codex attempt $attempt exited with $codex_status" | tee -a "$LOG_DIR/attempt-$attempt-codex.log"
  fi

  if git rev-parse --verify --quiet REBASE_HEAD >/dev/null; then
    set +e
    GIT_EDITOR=true git rebase --continue 2>&1 | tee "$LOG_DIR/attempt-$attempt-rebase-continue.log"
    rebase_continue_status=${PIPESTATUS[0]}
    set -e
    if [ "$rebase_continue_status" -ne 0 ]; then
      latest_failure_context
      continue
    fi
  fi

  if validate_worktree "$attempt"; then
    success=true
    break
  fi
  latest_failure_context
done

if [ "$success" != "true" ]; then
  echo "env-switch autocompile did not pass after $MAX_ATTEMPTS attempts" >&2
  exit 1
fi

if [ -n "$(git status --porcelain)" ]; then
  git add -A
  git commit -m "Port env_switch to $TARGET_TAG"
fi

git status --short > "$ARTIFACT_DIR/final-git-status.txt"
git diff --stat "$TARGET_TAG..HEAD" > "$ARTIFACT_DIR/final-diff-stat.txt"
git diff --binary "$TARGET_TAG..HEAD" > "$ARTIFACT_DIR/final-diff.patch"

git push origin "$NEW_BRANCH" --force-with-lease

cat > "$ARTIFACT_DIR/pr-body.md" <<EOF
Ports the env_switch runtime execution-target prototype to \`$TARGET_TAG\`.

Previous branch: \`$PREVIOUS_BRANCH\`
Previous base: \`$PREVIOUS_TAG\`
New branch: \`$NEW_BRANCH\`

Validation performed by \`env-switch-autocompile\`:

- \`cd codex-rs && just fmt\`
- targeted \`just test\` matrix for env_switch-affected crates:
  \`codex-apply-patch\`, \`codex-app-server-protocol\`,
  \`codex-exec-server\`, \`codex-core\`, and \`codex-tui\`
- remote E2E: \`${RUN_ENV_SWITCH_E2E:-true}\`

Artifacts include:

- previous env-switch diff and commit list
- Codex attempt logs
- test logs
- E2E transcript
- final diff patch and stat
EOF
