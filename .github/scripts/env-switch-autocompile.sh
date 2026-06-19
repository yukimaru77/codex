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
HEARTBEAT_SECONDS="${ENV_SWITCH_HEARTBEAT_SECONDS:-60}"
HEARTBEAT_PID=""
CMUX_POLL_SECS="${ENV_SWITCH_CMUX_POLL_SECS:-30}"
CMUX_TIMEOUT_SECS="${ENV_SWITCH_CMUX_TIMEOUT_SECS:-21600}"
CMUX_KEEP_WORKSPACE="${ENV_SWITCH_KEEP_CMUX_WORKSPACE:-false}"

mkdir -p "$LOG_DIR"

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
/goal Port the env_switch runtime execution-target prototype to a new upstream Codex Rust release.

Target upstream tag: $TARGET_TAG
New branch: $NEW_BRANCH
Previous env-switch branch: $PREVIOUS_BRANCH
Previous upstream base tag: $PREVIOUS_TAG

Context files in this run:
- $ARTIFACT_DIR/previous-env-switch.diff
- $ARTIFACT_DIR/previous-env-switch-commits.txt
- $ARTIFACT_DIR/latest-failure-context.md, if present

Goal:
Create/complete the $NEW_BRANCH implementation starting from $TARGET_TAG.
$PREVIOUS_BRANCH compared with $PREVIOUS_TAG is the working, production-like
reference implementation. It has been tested manually and should be treated as
the canonical feature behavior. Use that diff and commit list as the primary
reference, but adapt the code to the current upstream APIs instead of blindly
copying old code.

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

RUN_ENV_SWITCH_E2E: ${RUN_ENV_SWITCH_E2E:-true}

Required manual E2E as part of this same goal when RUN_ENV_SWITCH_E2E is true:
- If RUN_ENV_SWITCH_E2E is false, skip this E2E section and do not emit
  ENV_SWITCH_E2E_PASS.
- After the implementation builds, build the interactive Codex binary from this
  worktree with:
  cargo build --manifest-path codex-rs/Cargo.toml -p codex-cli --bin codex
- In cmux, open a new tab/surface in this same workspace. Do not create a new
  workspace for the E2E tab.
  Use cmux's tab/surface commands such as \`cmux new-surface --type terminal\`
  in the current workspace; do not use \`cmux new-workspace\` for this E2E.
- In that new tab/surface, run the built binary:
  $REPO_ROOT/codex-rs/target/debug/codex --sandbox danger-full-access --ask-for-approval never --enable env_switch --no-alt-screen
- If Codex startup asks for a harmless confirmation or first-run prompt, answer
  it appropriately and continue.
- Send this E2E prompt to that built Codex:

  /goal Validate env_switch end to end with subagents only. This E2E exists to prove that the goal command works, that subagents keep working after only the subagents switch environments, that SSH and Docker nesting work, and that the run is not merely passing by using raw shell ssh/docker wrappers. The main agent should coordinate and summarize but must not run remote SSH, Docker, or benchmark commands directly. Use exactly two subagents: Agent A targets ssh saitou, Agent B targets ssh saitou-h200. Each subagent must start by switching to its SSH target with env_switch; create or reuse a uniquely named GPU-capable Docker container on that SSH host; switch into the nested target with env_switch so subsequent commands run in an environment id shaped like ssh:<host>>docker:<container>; run a PyTorch CPU vs GPU matrix multiplication benchmark inside the nested Docker target; use the same matrix size, warmups, and repetitions for CPU and GPU on both hosts; and report transcript evidence including environment_id, container name/id, GPU model, PyTorch version, CUDA availability, benchmark parameters, CPU timing, and GPU timing. The pass condition is that /goal behavior, subagent delegation, SSH env_switch, nested Docker env_switch, remote provisioning, and environment-aware exec all work together through real work. If env_switch cannot register a target, report the exact fallback reason and fail instead of silently continuing with repeated raw ssh/docker wrappers. If raw ssh/docker shell wrappers are used to bypass env_switch, this E2E fails. When validation passes, finish with a line containing exactly ENV_SWITCH_E2E_PASS. If validation cannot pass, finish with a line containing exactly ENV_SWITCH_E2E_FAIL.

- The overall port is not complete until the E2E tab transcript contains
  ENV_SWITCH_E2E_PASS.

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

When everything is complete, finish your final answer with a line containing exactly:
ENV_SWITCH_PORT_PASS

If you cannot complete the port, finish with a line containing exactly:
ENV_SWITCH_PORT_FAIL
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

resolve_cmux_surface() {
  local workspace="$1"
  local surface=""
  for _ in $(seq 1 60); do
    surface="$(CMUX_QUIET=1 cmux list-pane-surfaces --workspace "$workspace" 2>/dev/null | rg -o 'surface:[0-9]+' | head -1 || true)"
    [ -n "$surface" ] && break
    sleep 1
  done
  printf '%s\n' "$surface"
}

prime_codex_surface() {
  local workspace="$1"
  local surface="$2"
  local screen=""
  for _ in $(seq 1 8); do
    screen="$(CMUX_QUIET=1 cmux read-screen --workspace "$workspace" --surface "$surface" --scrollback --lines 80 2>/dev/null || true)"
    if printf '%s\n' "$screen" | rg -qi 'press enter|continue|confirm|trust|first run|welcome'; then
      CMUX_QUIET=1 cmux send --workspace "$workspace" --surface "$surface" "\n" >/dev/null 2>&1 || true
    fi
    if printf '%s\n' "$screen" | rg -q 'Codex|codex|Type|ask|model|›|>'; then
      return 0
    fi
    sleep 2
  done
  return 0
}

collect_cmux_transcripts() {
  local workspace="$1"
  local prefix="$2"
  local surfaces_file="$ARTIFACT_DIR/$prefix-surfaces.txt"
  CMUX_QUIET=1 cmux list-pane-surfaces --workspace "$workspace" > "$surfaces_file" 2>/dev/null || true
  while IFS= read -r surface; do
    [ -n "$surface" ] || continue
    CMUX_QUIET=1 cmux read-screen --workspace "$workspace" --surface "$surface" --scrollback --lines 5000 \
      > "$ARTIFACT_DIR/$prefix-$surface.txt" 2>/dev/null || true
  done < <(rg -o 'surface:[0-9]+' "$surfaces_file" || true)
}

send_file_to_cmux() {
  local workspace="$1"
  local surface="$2"
  local file="$3"
  local chunk_size="${ENV_SWITCH_CMUX_SEND_CHUNK_SIZE:-2000}"
  local chunk=""
  while IFS= read -r -n "$chunk_size" chunk || [ -n "$chunk" ]; do
    CMUX_QUIET=1 cmux send --workspace "$workspace" --surface "$surface" "$chunk" >/dev/null
    chunk=""
  done < "$file"
  CMUX_QUIET=1 cmux send --workspace "$workspace" --surface "$surface" "\n" >/dev/null
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

  if ! command -v cmux >/dev/null 2>&1; then
    echo "cmux is required for /goal autocompile but was not found on PATH" | tee "$output_file"
    return 127
  fi

  local codex_bin="${CODEX_AUTOCOMPILE_CODEX_BIN:-}"
  if [ -z "$codex_bin" ]; then
    codex_bin="$(command -v codex || true)"
  fi
  if [ -z "$codex_bin" ]; then
    echo "codex is required for /goal autocompile but was not found on PATH" | tee "$output_file"
    return 127
  fi

  local attempt_name
  attempt_name="$(basename "$prompt_file" .md)"
  local workspace_name="env-switch-port-${TARGET_TAG}-${attempt_name}-${GITHUB_RUN_ID:-local}"
  local workspace_file="$ARTIFACT_DIR/$attempt_name-workspace.txt"
  local transcript_prefix="$attempt_name-cmux"
  local codex_command="$codex_bin --sandbox danger-full-access --ask-for-approval never --no-alt-screen"

  start_heartbeat "cmux /goal $attempt_name"
  set +e
  {
    echo "==> creating cmux workspace: $workspace_name"
    CMUX_QUIET=1 cmux new-workspace \
      --name "$workspace_name" \
      --description "env_switch port coordinator for ${GITHUB_RUN_ID:-local}" \
      --cwd "$REPO_ROOT" \
      --command "$codex_command" \
      --focus false
  } 2>&1 | tee "$output_file"
  local create_status=${PIPESTATUS[0]}
  set -e
  if [ "$create_status" -ne 0 ]; then
    stop_heartbeat
    return "$create_status"
  fi

  local workspace
  workspace="$(rg -o 'workspace:[0-9]+' "$output_file" | tail -1 || true)"
  if [ -z "$workspace" ]; then
    echo "failed to determine cmux workspace ref" | tee -a "$output_file"
    stop_heartbeat
    return 1
  fi
  printf '%s\n' "$workspace" > "$workspace_file"

  local surface
  surface="$(resolve_cmux_surface "$workspace")"
  if [ -z "$surface" ]; then
    echo "failed to determine cmux surface for $workspace" | tee -a "$output_file"
    stop_heartbeat
    return 1
  fi

  prime_codex_surface "$workspace" "$surface"

  local send_prompt_file="$ARTIFACT_DIR/$attempt_name-goal-one-line.txt"
  tr '\n' ' ' < "$prompt_file" | sed -E 's/[[:space:]]+/ /g' > "$send_prompt_file"
  echo "==> sending /goal prompt to $workspace $surface" | tee -a "$output_file"
  send_file_to_cmux "$workspace" "$surface" "$send_prompt_file"

  local deadline=$((SECONDS + CMUX_TIMEOUT_SECS))
  local result=""
  while [ "$SECONDS" -lt "$deadline" ]; do
    collect_cmux_transcripts "$workspace" "$transcript_prefix"
    if rg -q '^ENV_SWITCH_PORT_PASS$' "$ARTIFACT_DIR"/"$transcript_prefix"-surface:*.txt 2>/dev/null; then
      if [ "${RUN_ENV_SWITCH_E2E:-true}" != "true" ] || rg -q '^ENV_SWITCH_E2E_PASS$' "$ARTIFACT_DIR"/"$transcript_prefix"-surface:*.txt 2>/dev/null; then
        result="pass"
        break
      fi
    fi
    if rg -q '^ENV_SWITCH_PORT_FAIL$|^ENV_SWITCH_E2E_FAIL$' "$ARTIFACT_DIR"/"$transcript_prefix"-surface:*.txt 2>/dev/null; then
      result="fail"
      break
    fi
    sleep "$CMUX_POLL_SECS"
  done

  collect_cmux_transcripts "$workspace" "$transcript_prefix"

  local status=0
  case "$result" in
    pass)
      echo "cmux /goal autocompile passed" | tee -a "$output_file"
      ;;
    fail)
      echo "cmux /goal autocompile reported failure" | tee -a "$output_file"
      status=1
      ;;
    *)
      echo "cmux /goal autocompile timed out after ${CMUX_TIMEOUT_SECS}s" | tee -a "$output_file"
      status=124
      ;;
  esac

  if [ "$CMUX_KEEP_WORKSPACE" != "true" ]; then
    CMUX_QUIET=1 cmux close-workspace --workspace "$workspace" >/dev/null 2>&1 || true
  fi

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
    if ! rg -q '^ENV_SWITCH_E2E_PASS$' "$ARTIFACT_DIR"/"attempt-$attempt-cmux"-surface:*.txt 2>/dev/null; then
      echo "missing ENV_SWITCH_E2E_PASS in cmux transcripts" | tee "$LOG_DIR/attempt-$attempt-e2e-marker.log"
      return 1
    fi
  fi
}

git fetch --no-tags --prune origin \
  "+refs/heads/main:refs/remotes/origin/main" \
  "+refs/heads/$PREVIOUS_BRANCH:refs/remotes/origin/$PREVIOUS_BRANCH"
git fetch --no-tags origin \
  "+refs/heads/$NEW_BRANCH:refs/remotes/origin/$NEW_BRANCH" >/dev/null 2>&1 || true
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

git switch -C "$NEW_BRANCH" "$TARGET_TAG"

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
- cmux coordinator and E2E tab transcripts
- final diff patch and stat
EOF
