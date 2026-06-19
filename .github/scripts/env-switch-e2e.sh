#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
ARTIFACT_BASE="${RUNNER_TEMP:-$REPO_ROOT/artifacts}"
ARTIFACT_DIR="${ENV_SWITCH_ARTIFACT_DIR:-$ARTIFACT_BASE/env-switch-autocompile}"
LOG_DIR="$ARTIFACT_DIR/logs"
PROMPT_FILE="$ARTIFACT_DIR/env-switch-e2e-prompt.md"
OUTPUT_FILE="$LOG_DIR/env-switch-e2e-output.log"
TRANSCRIPT_FILE="$ARTIFACT_DIR/env-switch-e2e-transcript.txt"
WORKSPACE_FILE="$ARTIFACT_DIR/env-switch-e2e-workspace.txt"
SCREEN_POLL_SECS="${ENV_SWITCH_E2E_POLL_SECS:-30}"
TIMEOUT_SECS="${ENV_SWITCH_E2E_TIMEOUT_SECS:-7200}"

mkdir -p "$LOG_DIR"

cat > "$PROMPT_FILE" <<'EOF'
Validate the env_switch feature end to end with subagents only.

The main agent should coordinate and summarize but must not run the remote SSH, Docker, or benchmark commands directly.

Use exactly two subagents:

1. Agent A targets `ssh saitou`.
2. Agent B targets `ssh saitou-h200`.

Each subagent must:

- Start by switching to its SSH target with env_switch.
- Create or reuse a uniquely named GPU-capable Docker container on that SSH host.
- Switch into the nested target with env_switch so subsequent commands run in an environment id shaped like `ssh:<host>>docker:<container>`.
- Run a PyTorch CPU vs GPU matrix multiplication benchmark inside the nested Docker target.
- Use the same matrix size, warmups, and repetitions for CPU and GPU on both hosts.
- Report transcript evidence: environment_id, container name/id, GPU model, PyTorch version, CUDA availability, benchmark parameters, CPU timing, and GPU timing.

The intended pass condition is not a specific benchmark speed. The pass condition is that /goal behavior, subagent delegation, SSH env_switch, nested Docker env_switch, remote provisioning, and environment-aware exec all work together through real work.

If env_switch cannot register a target, report the exact fallback reason and fail the E2E instead of silently continuing with repeated raw ssh/docker wrappers.

When the validation passes, finish your final answer with an exact line:
ENV_SWITCH_E2E_PASS

If the validation cannot pass, finish your final answer with an exact line:
ENV_SWITCH_E2E_FAIL
EOF

if [ -n "${ENV_SWITCH_E2E_CMD:-}" ]; then
  ENV_SWITCH_E2E_PROMPT="$PROMPT_FILE" bash -lc "$ENV_SWITCH_E2E_CMD" 2>&1 | tee "$OUTPUT_FILE"
  exit "${PIPESTATUS[0]}"
fi

if ! command -v cmux >/dev/null 2>&1; then
  echo "cmux is required for interactive /goal E2E but was not found on PATH" | tee "$OUTPUT_FILE"
  exit 127
fi

cd "$REPO_ROOT"

{
  echo "==> building interactive codex binary"
  cargo build --manifest-path codex-rs/Cargo.toml -p codex-cli --bin codex
} 2>&1 | tee "$OUTPUT_FILE"
build_status=${PIPESTATUS[0]}
if [ "$build_status" -ne 0 ]; then
  exit "$build_status"
fi

CODEX_BIN="$REPO_ROOT/codex-rs/target/debug/codex"
if [ ! -x "$CODEX_BIN" ]; then
  echo "built codex binary not found at $CODEX_BIN" | tee -a "$OUTPUT_FILE"
  exit 1
fi

goal_objective="$(tr '\n' ' ' < "$PROMPT_FILE" | sed -E 's/[[:space:]]+/ /g')"
workspace_name="env-switch-e2e-$(date +%Y%m%d-%H%M%S)"
codex_command="$CODEX_BIN --sandbox danger-full-access --ask-for-approval never --enable env_switch --no-alt-screen"

{
  echo "==> creating cmux workspace: $workspace_name"
  CMUX_QUIET=1 cmux new-workspace \
    --name "$workspace_name" \
    --description "env_switch /goal E2E for GitHub Actions run ${GITHUB_RUN_ID:-local}" \
    --cwd "$REPO_ROOT" \
    --command "$codex_command" \
    --focus false
} 2>&1 | tee -a "$OUTPUT_FILE"
workspace="$(tail -50 "$OUTPUT_FILE" | rg -o 'workspace:[0-9]+' | tail -1 || true)"
if [ -z "$workspace" ]; then
  echo "failed to determine cmux workspace ref" | tee -a "$OUTPUT_FILE"
  exit 1
fi
printf '%s\n' "$workspace" > "$WORKSPACE_FILE"

surface=""
for _ in $(seq 1 60); do
  surface="$(CMUX_QUIET=1 cmux list-pane-surfaces --workspace "$workspace" 2>/dev/null | rg -o 'surface:[0-9]+' | head -1 || true)"
  [ -n "$surface" ] && break
  sleep 1
done
if [ -z "$surface" ]; then
  echo "failed to determine cmux surface for $workspace" | tee -a "$OUTPUT_FILE"
  exit 1
fi

ready=false
for _ in $(seq 1 120); do
  screen="$(CMUX_QUIET=1 cmux read-screen --workspace "$workspace" --surface "$surface" --scrollback --lines 80 2>/dev/null || true)"
  if printf '%s\n' "$screen" | rg -q 'Codex|codex|Type|ask|model|›|>'; then
    ready=true
    break
  fi
  sleep 1
done
if [ "$ready" != "true" ]; then
  echo "codex screen did not look ready; sending /goal anyway" | tee -a "$OUTPUT_FILE"
fi

echo "==> sending /goal to $workspace $surface" | tee -a "$OUTPUT_FILE"
CMUX_QUIET=1 cmux send --workspace "$workspace" --surface "$surface" "/goal $goal_objective\n"

deadline=$((SECONDS + TIMEOUT_SECS))
result=""
while [ "$SECONDS" -lt "$deadline" ]; do
  CMUX_QUIET=1 cmux read-screen --workspace "$workspace" --surface "$surface" --scrollback --lines 4000 \
    > "$TRANSCRIPT_FILE" 2>/dev/null || true
  if rg -q '^ENV_SWITCH_E2E_PASS$' "$TRANSCRIPT_FILE"; then
    result="pass"
    break
  fi
  if rg -q '^ENV_SWITCH_E2E_FAIL$' "$TRANSCRIPT_FILE"; then
    result="fail"
    break
  fi
  sleep "$SCREEN_POLL_SECS"
done

CMUX_QUIET=1 cmux read-screen --workspace "$workspace" --surface "$surface" --scrollback --lines 4000 \
  > "$TRANSCRIPT_FILE" 2>/dev/null || true

if [ "${ENV_SWITCH_KEEP_CMUX_WORKSPACE:-false}" != "true" ]; then
  CMUX_QUIET=1 cmux close-workspace --workspace "$workspace" >/dev/null 2>&1 || true
fi

case "$result" in
  pass)
    echo "env_switch cmux /goal E2E passed" | tee -a "$OUTPUT_FILE"
    exit 0
    ;;
  fail)
    echo "env_switch cmux /goal E2E reported failure" | tee -a "$OUTPUT_FILE"
    exit 1
    ;;
  *)
    echo "env_switch cmux /goal E2E timed out after ${TIMEOUT_SECS}s" | tee -a "$OUTPUT_FILE"
    exit 124
    ;;
esac
