#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="${ENV_SWITCH_E2E_REPO_ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"
CODEX_BIN="${ENV_SWITCH_E2E_CODEX_BIN:-$REPO_ROOT/codex-rs/target/debug/codex}"
ARTIFACT_DIR="${ENV_SWITCH_E2E_ARTIFACT_DIR:-$REPO_ROOT/env-switch-e2e-artifacts}"
PROMPT_FILE="${ENV_SWITCH_E2E_PROMPT_FILE:-$ARTIFACT_DIR/e2e-prompt.txt}"
READY_TIMEOUT_SECS="${ENV_SWITCH_E2E_READY_TIMEOUT_SECS:-240}"
RUN_TIMEOUT_SECS="${ENV_SWITCH_E2E_RUN_TIMEOUT_SECS:-7200}"
QUEUED_TIMEOUT_SECS="${ENV_SWITCH_E2E_QUEUED_TIMEOUT_SECS:-60}"
SURFACE_ATTEMPTS="${ENV_SWITCH_E2E_SURFACE_ATTEMPTS:-3}"
POLL_SECS="${ENV_SWITCH_E2E_POLL_SECS:-10}"
CMUX_BIN="${ENV_SWITCH_CMUX_BIN:-cmux}"

mkdir -p "$ARTIFACT_DIR"

cmux_cli() {
  CMUX_QUIET="${CMUX_QUIET:-1}" "$CMUX_BIN" "$@"
}

json_field() {
  local expr="$1"
  python3 -c '
import json
import sys

doc = json.load(sys.stdin)
cur = doc
for part in sys.argv[1].split("."):
    cur = cur.get(part, "") if isinstance(cur, dict) else ""
print(cur)
' "$expr"
}

write_default_prompt() {
  if [ -s "$PROMPT_FILE" ]; then
    return
  fi
  cat > "$PROMPT_FILE" <<'EOF'
/goal Validate env_switch end to end with subagents only. This E2E exists to prove that the goal command works, that subagents keep working after only the subagents switch environments, that SSH and Docker nesting work, and that the run is not merely passing by using raw shell ssh/docker wrappers. The main agent should coordinate and summarize but must not run remote SSH, Docker, or benchmark commands directly. Use exactly two subagents: Agent A targets ssh saitou, Agent B targets ssh saitou-h200. Each subagent must start by switching to its SSH target with env_switch; create or reuse a uniquely named GPU-capable Docker container on that SSH host; switch into the nested target with env_switch so subsequent commands run in an environment id shaped like ssh:<host>>docker:<container>; run a PyTorch CPU vs GPU matrix multiplication benchmark inside the nested Docker target; use the same matrix size, warmups, and repetitions for CPU and GPU on both hosts; and report transcript evidence including environment_id, container name/id, GPU model, PyTorch version, CUDA availability, benchmark parameters, CPU timing, and GPU timing. The pass condition is that /goal behavior, subagent delegation, SSH env_switch, nested Docker env_switch, remote provisioning, and environment-aware exec all work together through real work. If env_switch cannot register a target, report the exact fallback reason and fail instead of silently continuing with repeated raw ssh/docker wrappers. If raw ssh/docker shell wrappers are used to bypass env_switch, this E2E fails. When validation passes, finish with a line containing exactly ENV_SWITCH_E2E_PASS. If validation cannot pass, finish with a line containing exactly ENV_SWITCH_E2E_FAIL.
EOF
}

send_file_to_cmux() {
  local workspace="$1"
  local surface="$2"
  local file="$3"
  local chunk_size="${ENV_SWITCH_CMUX_SEND_CHUNK_SIZE:-2000}"
  local chunk=""
  while IFS= read -r -n "$chunk_size" chunk || [ -n "$chunk" ]; do
    cmux_cli send --workspace "$workspace" --surface "$surface" "$chunk" >/dev/null
    chunk=""
  done < "$file"
  cmux_cli send --workspace "$workspace" --surface "$surface" "\n" >/dev/null
}

surface_screen() {
  local workspace="$1"
  local surface="$2"
  local lines="${3:-120}"
  cmux_cli read-screen --workspace "$workspace" --surface "$surface" --lines "$lines" 2>/dev/null || true
}

collect_surface() {
  local workspace="$1"
  local surface="$2"
  local name="$3"
  cmux_cli read-screen --workspace "$workspace" --surface "$surface" --scrollback --lines 5000 \
    > "$ARTIFACT_DIR/$name.txt" 2>/dev/null || true
}

wait_for_codex_ready() {
  local workspace="$1"
  local surface="$2"
  local deadline=$((SECONDS + READY_TIMEOUT_SECS))
  local screen=""

  while [ "$SECONDS" -lt "$deadline" ]; do
    screen="$(surface_screen "$workspace" "$surface" 100)"
    printf '%s\n' "$screen" > "$ARTIFACT_DIR/current-ready-screen.txt"

    if printf '%s\n' "$screen" | rg -qi 'press enter|continue|confirm|trust|first run|welcome'; then
      cmux_cli send --workspace "$workspace" --surface "$surface" "\n" >/dev/null 2>&1 || true
    fi

    if printf '%s\n' "$screen" | rg -q 'OpenAI Codex' &&
      printf '%s\n' "$screen" | rg -q '^[[:space:]]*›' &&
      ! printf '%s\n' "$screen" | rg -q 'Starting MCP servers|model:[[:space:]]+loading|Queued follow-up inputs'; then
      return 0
    fi
    sleep 2
  done

  return 1
}

monitor_e2e() {
  local workspace="$1"
  local surface="$2"
  local name="$3"
  local deadline=$((SECONDS + RUN_TIMEOUT_SECS))
  local queued_since=""
  local screen=""

  while [ "$SECONDS" -lt "$deadline" ]; do
    collect_surface "$workspace" "$surface" "$name"

    if rg -q '^ENV_SWITCH_E2E_PASS$' "$ARTIFACT_DIR/$name.txt"; then
      printf 'ENV_SWITCH_E2E_PASS\n'
      return 0
    fi
    if rg -q '^ENV_SWITCH_E2E_FAIL$' "$ARTIFACT_DIR/$name.txt"; then
      printf 'ENV_SWITCH_E2E_FAIL\n'
      return 1
    fi
    if rg -qi "You've hit your usage limit|Goal hit usage limits|5h limit:.*0% left" "$ARTIFACT_DIR/$name.txt"; then
      printf 'ENV_SWITCH_E2E_FAIL\n'
      echo "usage limit reached during E2E" >&2
      return 75
    fi

    screen="$(surface_screen "$workspace" "$surface" 140)"
    if printf '%s\n' "$screen" | rg -q 'Queued follow-up inputs'; then
      if [ -z "$queued_since" ]; then
        queued_since="$SECONDS"
      elif [ $((SECONDS - queued_since)) -ge "$QUEUED_TIMEOUT_SECS" ]; then
        echo "E2E prompt stayed queued for ${QUEUED_TIMEOUT_SECS}s on $surface" >&2
        return 124
      fi
    else
      queued_since=""
    fi

    sleep "$POLL_SECS"
  done

  echo "E2E timed out after ${RUN_TIMEOUT_SECS}s on $surface" >&2
  return 124
}

main() {
  if [ ! -x "$CODEX_BIN" ]; then
    echo "built codex binary not found or not executable: $CODEX_BIN" >&2
    return 1
  fi
  write_default_prompt

  local identify_json
  identify_json="$(cmux_cli identify --json)"
  printf '%s\n' "$identify_json" > "$ARTIFACT_DIR/cmux-identify.json"

  local workspace pane
  workspace="$(printf '%s\n' "$identify_json" | json_field caller.workspace_ref)"
  pane="$(printf '%s\n' "$identify_json" | json_field caller.pane_ref)"

  if [ -z "$workspace" ] || [ -z "$pane" ]; then
    echo "could not identify current cmux workspace/pane" >&2
    return 1
  fi

  local prompt_one_line="$ARTIFACT_DIR/e2e-prompt-one-line.txt"
  tr '\n' ' ' < "$PROMPT_FILE" | sed -E 's/[[:space:]]+/ /g' > "$prompt_one_line"

  for attempt in $(seq 1 "$SURFACE_ATTEMPTS"); do
    echo "==> E2E surface attempt $attempt/$SURFACE_ATTEMPTS in $workspace"
    local create_output surface
    create_output="$(cmux_cli new-surface --type terminal --workspace "$workspace" --pane "$pane" --focus true 2>&1)"
    printf '%s\n' "$create_output" | tee "$ARTIFACT_DIR/e2e-surface-$attempt-create.txt"
    surface="$(printf '%s\n' "$create_output" | rg -o 'surface:[0-9]+' | tail -1 || true)"
    if [ -z "$surface" ]; then
      echo "could not create E2E surface in $workspace $pane" >&2
      continue
    fi

    cmux_cli send --workspace "$workspace" --surface "$surface" \
      "$CODEX_BIN --sandbox danger-full-access --ask-for-approval never --enable env_switch --no-alt-screen" >/dev/null
    cmux_cli send --workspace "$workspace" --surface "$surface" "\n" >/dev/null

    if ! wait_for_codex_ready "$workspace" "$surface"; then
      echo "Codex did not become ready on $surface; retrying with a new surface" >&2
      collect_surface "$workspace" "$surface" "e2e-attempt-$attempt-not-ready"
      cmux_cli close-surface --surface "$surface" >/dev/null 2>&1 || true
      continue
    fi

    send_file_to_cmux "$workspace" "$surface" "$prompt_one_line"
    set +e
    monitor_e2e "$workspace" "$surface" "e2e-attempt-$attempt-transcript"
    local status=$?
    set -e
    if [ "$status" -eq 0 ]; then
      echo "E2E passed on $surface"
      return 0
    fi
    if [ "$status" -eq 75 ]; then
      return "$status"
    fi

    collect_surface "$workspace" "$surface" "e2e-attempt-$attempt-failed"
    cmux_cli close-surface --surface "$surface" >/dev/null 2>&1 || true
  done

  printf 'ENV_SWITCH_E2E_FAIL\n'
  return 1
}

main "$@"
