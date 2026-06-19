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
CMUX_MODEL_LOADING_TIMEOUT_SECS="${ENV_SWITCH_CMUX_MODEL_LOADING_TIMEOUT_SECS:-180}"
CMUX_CREATE_ATTEMPTS="${ENV_SWITCH_CMUX_CREATE_ATTEMPTS:-3}"
CMUX_CODEX_READY_ATTEMPTS="${ENV_SWITCH_CMUX_CODEX_READY_ATTEMPTS:-120}"
CMUX_KEEP_WORKSPACE="${ENV_SWITCH_KEEP_CMUX_WORKSPACE:-false}"
CMUX_WINDOW_REF="${ENV_SWITCH_CMUX_WINDOW:-}"
CMUX_WINDOW_ARGS=()
CMUX_BIN="${ENV_SWITCH_CMUX_BIN:-}"
CMUX_USER_ID="${ENV_SWITCH_CMUX_USER_ID:-$(id -u)}"
CMUX_USE_ASUSER="${ENV_SWITCH_CMUX_USE_ASUSER:-}"
if [ -z "$CMUX_USE_ASUSER" ] && [ "$(uname -s)" = "Darwin" ]; then
  CMUX_USE_ASUSER=true
fi

rm -rf "$ARTIFACT_DIR"
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

cmux_cli() {
  local cmux_bin="${CMUX_BIN:-cmux}"
  if [ "$CMUX_USE_ASUSER" = "true" ]; then
    launchctl asuser "$CMUX_USER_ID" /usr/bin/env \
      CMUX_SOCKET_PATH="${CMUX_SOCKET_PATH:-}" \
      CMUX_SOCKET_PASSWORD="${CMUX_SOCKET_PASSWORD:-}" \
      CMUX_QUIET="${CMUX_QUIET:-}" \
      "$cmux_bin" "$@"
  else
    CMUX_SOCKET_PATH="${CMUX_SOCKET_PATH:-}" \
      CMUX_SOCKET_PASSWORD="${CMUX_SOCKET_PASSWORD:-}" \
      CMUX_QUIET="${CMUX_QUIET:-}" \
      "$cmux_bin" "$@"
  fi
}

configure_cmux_context() {
  if [ -z "${CMUX_SOCKET_PATH:-}" ]; then
    local support_dir="$HOME/Library/Application Support/cmux"
    local last_socket_file="$support_dir/last-socket-path"
    if [ -s "$last_socket_file" ]; then
      local candidate
      candidate="$(tr -d '\r\n' < "$last_socket_file")"
      if [ -S "$candidate" ]; then
        export CMUX_SOCKET_PATH="$candidate"
      fi
    fi
    if [ -z "${CMUX_SOCKET_PATH:-}" ]; then
      local uid_socket="$support_dir/cmux-$(id -u).sock"
      if [ -S "$uid_socket" ]; then
        export CMUX_SOCKET_PATH="$uid_socket"
      fi
    fi
  fi

  if [ -z "$CMUX_WINDOW_REF" ]; then
    CMUX_WINDOW_REF="$(CMUX_QUIET=1 cmux_cli current-window 2>/dev/null || true)"
  fi
  if [ -n "$CMUX_WINDOW_REF" ]; then
    CMUX_WINDOW_ARGS=(--window "$CMUX_WINDOW_REF")
  fi
}

log_cmux_diagnostics() {
  local output_file="$1"
  local support_dir="$HOME/Library/Application Support/cmux"
  {
    echo "==> cmux diagnostics"
    echo "cmux_bin=${CMUX_BIN:-$(command -v cmux || true)}"
    echo "CMUX_USE_ASUSER=$CMUX_USE_ASUSER"
    echo "CMUX_USER_ID=$CMUX_USER_ID"
    cmux_cli version || true
    echo "CMUX_SOCKET_PATH=${CMUX_SOCKET_PATH:-}"
    echo "CMUX_WINDOW_REF=${CMUX_WINDOW_REF:-}"
    if [ -d "$support_dir" ]; then
      ls -la "$support_dir" | sed -n '1,80p'
    fi
    CMUX_QUIET=1 cmux_cli ping || true
    CMUX_QUIET=1 cmux_cli list-windows || true
  } 2>&1 | tee -a "$output_file"
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

Before editing:
- Read and understand the relevant current upstream code paths first. Do not
  start by mechanically applying the old patch.
- Compare the previous env-switch diff against the current code and identify
  where upstream APIs or ownership boundaries changed.
- Use subagents when they help with parallel code reading, compatibility
  investigation, or test failure triage. Keep their tasks scoped and verify
  their conclusions before editing.

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
    surface="$(CMUX_QUIET=1 cmux_cli list-pane-surfaces --workspace "$workspace" "${CMUX_WINDOW_ARGS[@]}" --id-format both 2>/dev/null |
      awk '
        match($0, /[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}/) {
          print substr($0, RSTART, RLENGTH)
          exit
        }
        match($0, /surface:[0-9]+/) {
          print substr($0, RSTART, RLENGTH)
          exit
        }
      ' || true)"
    [ -n "$surface" ] && break
    sleep 1
  done
  printf '%s\n' "$surface"
}

resolve_cmux_workspace() {
  local workspace="$1"
  local workspace_name="$2"
  local line=""

  line="$(CMUX_QUIET=1 cmux_cli workspace list "${CMUX_WINDOW_ARGS[@]}" --id-format both 2>/dev/null |
    awk -v workspace="$workspace" '$0 ~ workspace { print; exit }' || true)"
  if [ -z "$line" ]; then
    line="$(CMUX_QUIET=1 cmux_cli workspace list "${CMUX_WINDOW_ARGS[@]}" --id-format both 2>/dev/null |
      awk -v workspace_name="$workspace_name" 'index($0, workspace_name) { print; exit }' || true)"
  fi

  local workspace_uuid=""
  workspace_uuid="$(printf '%s\n' "$line" |
    rg -o '[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}' |
    head -1 || true)"
  if [ -n "$workspace_uuid" ]; then
    printf '%s\n' "$workspace_uuid"
  else
    printf '%s\n' "$workspace"
  fi
}

cmux_workspace_exists() {
  local workspace="$1"
  CMUX_QUIET=1 cmux_cli list-panes --workspace "$workspace" "${CMUX_WINDOW_ARGS[@]}" >/dev/null 2>&1
}

prime_codex_surface() {
  local workspace="$1"
  local surface="$2"
  local screen=""
  for _ in $(seq 1 "$CMUX_CODEX_READY_ATTEMPTS"); do
    screen="$(CMUX_QUIET=1 cmux_cli read-screen --workspace "$workspace" --surface "$surface" "${CMUX_WINDOW_ARGS[@]}" --scrollback --lines 80 2>/dev/null || true)"
    if printf '%s\n' "$screen" | rg -qi 'press enter|continue|confirm|trust|first run|welcome'; then
      CMUX_QUIET=1 cmux_cli send --workspace "$workspace" --surface "$surface" "${CMUX_WINDOW_ARGS[@]}" "\n" >/dev/null 2>&1 || true
    fi
    if printf '%s\n' "$screen" | rg -q 'OpenAI Codex' &&
      printf '%s\n' "$screen" | rg -q '^[[:space:]]*›' &&
      ! printf '%s\n' "$screen" | rg -q 'Starting MCP servers'; then
      return 0
    fi
    sleep 2
  done
  return 1
}

collect_cmux_transcripts() {
  local workspace="$1"
  local prefix="$2"
  local surfaces_file="$ARTIFACT_DIR/$prefix-surfaces.txt"
  CMUX_QUIET=1 cmux_cli list-pane-surfaces --workspace "$workspace" "${CMUX_WINDOW_ARGS[@]}" --id-format both > "$surfaces_file" 2>/dev/null || true
  while IFS= read -r surface; do
    [ -n "$surface" ] || continue
    local surface_file="${surface//:/-}"
    CMUX_QUIET=1 cmux_cli read-screen --workspace "$workspace" --surface "$surface" "${CMUX_WINDOW_ARGS[@]}" --scrollback --lines 5000 \
      > "$ARTIFACT_DIR/$prefix-$surface_file.txt" 2>/dev/null || true
  done < <(
    awk '
      match($0, /[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}/) {
        print substr($0, RSTART, RLENGTH)
        next
      }
      match($0, /surface:[0-9]+/) {
        print substr($0, RSTART, RLENGTH)
      }
    ' "$surfaces_file" || true
  )
}

cmux_transcripts_match() {
  local prefix="$1"
  local pattern="$2"
  rg -qi "$pattern" "$ARTIFACT_DIR"/"$prefix"-*.txt 2>/dev/null
}

send_file_to_cmux() {
  local workspace="$1"
  local surface="$2"
  local file="$3"
  local chunk_size="${ENV_SWITCH_CMUX_SEND_CHUNK_SIZE:-2000}"
  local chunk=""
  while IFS= read -r -n "$chunk_size" chunk || [ -n "$chunk" ]; do
    CMUX_QUIET=1 cmux_cli send --workspace "$workspace" --surface "$surface" "${CMUX_WINDOW_ARGS[@]}" "$chunk" >/dev/null
    chunk=""
  done < "$file"
  CMUX_QUIET=1 cmux_cli send --workspace "$workspace" --surface "$surface" "${CMUX_WINDOW_ARGS[@]}" "\n" >/dev/null
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

  if [ -z "$CMUX_BIN" ]; then
    CMUX_BIN="$(command -v cmux || true)"
  fi
  if [ -z "$CMUX_BIN" ]; then
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

  : > "$output_file"
  configure_cmux_context
  log_cmux_diagnostics "$output_file"
  start_heartbeat "cmux /goal $attempt_name"

  local workspace=""
  local create_status=1
  for create_attempt in $(seq 1 "$CMUX_CREATE_ATTEMPTS"); do
    echo "==> creating cmux workspace (attempt $create_attempt/$CMUX_CREATE_ATTEMPTS): $workspace_name" | tee -a "$output_file"
    local create_output
    set +e
    create_output="$(CMUX_QUIET=1 cmux_cli workspace create \
      "${CMUX_WINDOW_ARGS[@]}" \
      --name "$workspace_name" \
      --description "env_switch port coordinator for ${GITHUB_RUN_ID:-local}" \
      --cwd "$REPO_ROOT" \
      --focus false 2>&1)"
    create_status=$?
    set -e
    printf '%s\n' "$create_output" | tee -a "$output_file"
    workspace="$(printf '%s\n' "$create_output" | rg -o 'workspace:[0-9]+' | tail -1 || true)"
    if [ "$create_status" -eq 0 ] && [ -n "$workspace" ]; then
      break
    fi
    if [ "$create_attempt" -lt "$CMUX_CREATE_ATTEMPTS" ]; then
      sleep 5
    fi
  done
  if [ "$create_status" -ne 0 ]; then
    stop_heartbeat
    return "$create_status"
  fi
  if [ -z "$workspace" ]; then
    echo "failed to determine cmux workspace ref" | tee -a "$output_file"
    stop_heartbeat
    return 1
  fi
  printf '%s\n' "$workspace" > "$workspace_file"
  workspace="$(resolve_cmux_workspace "$workspace" "$workspace_name")"
  printf '%s\n' "$workspace" > "$workspace_file"

  local surface
  surface="$(resolve_cmux_surface "$workspace")"
  if [ -z "$surface" ]; then
    echo "failed to determine cmux surface for $workspace" | tee -a "$output_file"
    stop_heartbeat
    return 1
  fi

  sleep 2
  echo "==> launching codex in $workspace $surface" | tee -a "$output_file"
  if ! CMUX_QUIET=1 cmux_cli send --workspace "$workspace" --surface "$surface" "${CMUX_WINDOW_ARGS[@]}" "$codex_command" >/dev/null ||
    ! CMUX_QUIET=1 cmux_cli send --workspace "$workspace" --surface "$surface" "${CMUX_WINDOW_ARGS[@]}" "\n" >/dev/null; then
    echo "failed to launch codex in $workspace $surface" | tee -a "$output_file"
    if [ "$CMUX_KEEP_WORKSPACE" != "true" ]; then
      CMUX_QUIET=1 cmux_cli close-workspace --workspace "$workspace" "${CMUX_WINDOW_ARGS[@]}" >/dev/null 2>&1 || true
    fi
    stop_heartbeat
    return 1
  fi

  if ! prime_codex_surface "$workspace" "$surface"; then
    echo "codex did not become ready in $workspace $surface" | tee -a "$output_file"
    collect_cmux_transcripts "$workspace" "$transcript_prefix"
    if [ "$CMUX_KEEP_WORKSPACE" != "true" ]; then
      CMUX_QUIET=1 cmux_cli close-workspace --workspace "$workspace" "${CMUX_WINDOW_ARGS[@]}" >/dev/null 2>&1 || true
    fi
    stop_heartbeat
    return 1
  fi

  local send_prompt_file="$ARTIFACT_DIR/$attempt_name-goal-one-line.txt"
  tr '\n' ' ' < "$prompt_file" | sed -E 's/[[:space:]]+/ /g' > "$send_prompt_file"
  echo "==> sending /goal prompt to $workspace $surface" | tee -a "$output_file"
  send_file_to_cmux "$workspace" "$surface" "$send_prompt_file"

  local deadline=$((SECONDS + CMUX_TIMEOUT_SECS))
  local model_loading_since=""
  local result=""
  while [ "$SECONDS" -lt "$deadline" ]; do
    if ! cmux_workspace_exists "$workspace"; then
      echo "cmux workspace disappeared: $workspace" | tee -a "$output_file"
      result="workspace_missing"
      break
    fi
    collect_cmux_transcripts "$workspace" "$transcript_prefix"
    if cmux_transcripts_match "$transcript_prefix" "You've hit your usage limit|Goal hit usage limits|5h limit:.*0% left"; then
      echo "cmux /goal autocompile hit Codex usage limits" | tee -a "$output_file"
      result="usage_limited"
      break
    fi
    if cmux_transcripts_match "$transcript_prefix" 'model:[[:space:]]+loading'; then
      if [ -z "$model_loading_since" ]; then
        model_loading_since="$SECONDS"
      elif [ $((SECONDS - model_loading_since)) -ge "$CMUX_MODEL_LOADING_TIMEOUT_SECS" ]; then
        echo "cmux /goal autocompile stayed at model loading for ${CMUX_MODEL_LOADING_TIMEOUT_SECS}s" | tee -a "$output_file"
        result="model_loading_timeout"
        break
      fi
    else
      model_loading_since=""
    fi
    if rg -q '^ENV_SWITCH_PORT_PASS$' "$ARTIFACT_DIR"/"$transcript_prefix"-*.txt 2>/dev/null; then
      if [ "${RUN_ENV_SWITCH_E2E:-true}" != "true" ] || rg -q '^ENV_SWITCH_E2E_PASS$' "$ARTIFACT_DIR"/"$transcript_prefix"-*.txt 2>/dev/null; then
        result="pass"
        break
      fi
    fi
    if rg -q '^ENV_SWITCH_PORT_FAIL$|^ENV_SWITCH_E2E_FAIL$' "$ARTIFACT_DIR"/"$transcript_prefix"-*.txt 2>/dev/null; then
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
    usage_limited)
      echo "cmux /goal autocompile stopped because Codex usage limits were reached" | tee -a "$output_file"
      status=75
      ;;
    model_loading_timeout)
      echo "cmux /goal autocompile stopped because model loading did not complete" | tee -a "$output_file"
      status=124
      ;;
    workspace_missing)
      echo "cmux /goal autocompile stopped because the cmux workspace disappeared" | tee -a "$output_file"
      status=1
      ;;
    *)
      echo "cmux /goal autocompile timed out after ${CMUX_TIMEOUT_SECS}s" | tee -a "$output_file"
      status=124
      ;;
  esac

  if [ "$CMUX_KEEP_WORKSPACE" != "true" ]; then
    CMUX_QUIET=1 cmux_cli close-workspace --workspace "$workspace" "${CMUX_WINDOW_ARGS[@]}" >/dev/null 2>&1 || true
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
    if ! rg -q '^ENV_SWITCH_E2E_PASS$' "$ARTIFACT_DIR"/"attempt-$attempt-cmux"-*.txt 2>/dev/null; then
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
