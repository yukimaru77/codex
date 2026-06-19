#!/usr/bin/env bash
set -Eeuo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
ARTIFACT_BASE="${RUNNER_TEMP:-$REPO_ROOT/artifacts}"
ARTIFACT_DIR="${ENV_SWITCH_ARTIFACT_DIR:-$ARTIFACT_BASE/env-switch-autocompile}"
LOG_DIR="$ARTIFACT_DIR/logs"
PROMPT_FILE="$ARTIFACT_DIR/env-switch-e2e-prompt.md"
OUTPUT_FILE="$LOG_DIR/env-switch-e2e-output.log"

mkdir -p "$LOG_DIR"

cat > "$PROMPT_FILE" <<'EOF'
Create a goal for this E2E validation and pursue it to completion:

Validate the env_switch feature end to end with subagents only. The main agent should coordinate and summarize but must not run the remote SSH, Docker, or benchmark commands directly.

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

The intended pass condition is not a specific benchmark speed. The pass condition is that `/goal`/goal-state behavior, subagent delegation, SSH env_switch, nested Docker env_switch, remote provisioning, and environment-aware exec all work together through real work.

If env_switch cannot register a target, report the exact fallback reason and fail the E2E instead of silently continuing with repeated raw ssh/docker wrappers.
EOF

if [ -n "${ENV_SWITCH_E2E_CMD:-}" ]; then
  ENV_SWITCH_E2E_PROMPT="$PROMPT_FILE" bash -lc "$ENV_SWITCH_E2E_CMD" 2>&1 | tee "$OUTPUT_FILE"
  exit "${PIPESTATUS[0]}"
fi

cd "$REPO_ROOT"

cargo run --manifest-path codex-rs/Cargo.toml -p codex-cli --bin codex -- \
  exec \
  --sandbox danger-full-access \
  --ask-for-approval never \
  --enable env_switch \
  - < "$PROMPT_FILE" 2>&1 | tee "$OUTPUT_FILE"
exit "${PIPESTATUS[0]}"
