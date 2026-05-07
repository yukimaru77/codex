#!/usr/bin/env python3
"""Inject Codex Teams guidance for normal-session lead secretary mode."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import sys


BOOTSTRAP_TRIGGERS = (
    "/team",
    "/teams",
    "teams",
    "temas",
    "agent team",
    "agent teams",
)

PREFERRED_CODEX_BIN = Path("/home/yukimaru/codex/codex-rs/target/debug/codex")


def should_bootstrap(prompt: str) -> bool:
    lowered = prompt.lower()
    stripped = lowered.lstrip()
    return stripped.startswith(("/team", "/teams")) or any(
        trigger in lowered for trigger in BOOTSTRAP_TRIGGERS if not trigger.startswith("/")
    )


def codex_team_bin() -> str:
    if PREFERRED_CODEX_BIN.exists():
        return str(PREFERRED_CODEX_BIN)
    return shutil.which("codex") or "codex"


def codex_home() -> Path:
    raw = os.environ.get("CODEX_HOME")
    if raw:
        return Path(raw).expanduser()
    return Path.home() / ".codex"


def binding_path(session_id: str) -> Path:
    return codex_home() / "team-secretaries" / f"{safe_id(session_id)}.json"


def safe_id(raw: str) -> str:
    return "".join(ch if ch.isalnum() or ch in "._-" else "-" for ch in raw).strip(".-_")


def read_binding(session_id: str) -> dict | None:
    if not session_id:
        return None
    path = binding_path(session_id)
    try:
        binding = json.loads(path.read_text())
    except Exception:
        return None
    team_dir = Path(str(binding.get("team_dir") or ""))
    if team_dir and not team_dir.exists():
        return None
    return binding


def build_bootstrap_context(cwd: str, permission_mode: str) -> str:
    team_bin = codex_team_bin()
    return f"""<codex_team_assistant>
The user's latest prompt explicitly asks to start or use an agent team.

This is an ordinary Codex session. For this request, act as the user's lead secretary and start a Codex team when execution is appropriate:
- If the user is only asking a conceptual question about teams, answer directly and do not start a team.
- If the user asks to perform work using teams, launch a Codex team from this session using the existing `codex team` CLI assets.
- The `codex team` CLI records `CODEX_THREAD_ID` when available. After you launch the team, future turns in this same ordinary session will become lead-secretary turns automatically.
- Prefer continuing an existing relevant live team over creating a duplicate. Inspect `{team_bin} team list`, `{team_bin} team status --team <id>`, and the team UI/status before deciding.
- For a new team, use the user's latest prompt as the team goal unless they specify otherwise. Use the current working directory as the default execution directory.
- Default new-team command shape:
  `{team_bin} team swarm "<user goal>" --app-server --discuss-rounds 0 --dangerously-bypass-approvals-and-sandbox --cd "{cwd}"`
- After starting a team, report the team id, state path, and useful monitor/UI command. Use `{team_bin} team ui --open` or `{team_bin} team monitor --team <id>` when useful.
- Once the team exists, treat this normal session as the user's secretary rather than doing duplicate implementation locally.
- Keep the user-facing response concise, but do the actual orchestration work instead of merely suggesting commands when execution is appropriate.

Runtime facts:
- team_cli: {team_bin}
- cwd: {cwd}
- permission_mode: {permission_mode}
</codex_team_assistant>"""


def build_secretary_context(binding: dict, cwd: str, permission_mode: str) -> str:
    team_bin = codex_team_bin()
    team_id = str(binding.get("team_id") or "")
    team_dir = str(binding.get("team_dir") or "")
    team_cwd = str(binding.get("cwd") or "")
    return f"""<codex_team_secretary_mode>
This ordinary Codex session is already bound to a Codex team and must act as the user's lead secretary.

Secretary contract:
- Do not treat the user's latest prompt as a fresh standalone local task by default.
- Relay the user's intent to the bound team's lead, inspect team state, and report back as the user-facing coordinator.
- For normal follow-up instructions, send them to lead:
  `{team_bin} team message --team "{team_id}" --from user --to lead "<user instruction>"`
- Then inspect progress with `{team_bin} team status --team "{team_id}"`, recent events/messages, `{team_bin} team ui`, or `{team_bin} team monitor` as useful.
- If the user explicitly asks you to do something locally outside the team, you may do it, but keep the team binding unless they ask to detach or delete the team.
- If lead is idle or a department is blocked and the user's message clears the blocker, send a concrete resume/redirect instruction to lead rather than creating duplicate work locally.
- If the bound team is missing or deleted, tell the user and ask whether to start a new team; do not silently pick an unrelated old team.
- Use the existing generic team commands when appropriate: `team status`, `team message`, `team member`, `team node`, `team job`, `team monitor`, and `team ui`.

Runtime facts:
- team_cli: {team_bin}
- bound_team_id: {team_id}
- bound_team_dir: {team_dir}
- bound_team_cwd: {team_cwd}
- cwd: {cwd}
- permission_mode: {permission_mode}
</codex_team_secretary_mode>"""


def main() -> int:
    try:
        request = json.load(sys.stdin)
    except Exception:
        return 0

    prompt = str(request.get("prompt") or "")
    session_id = str(request.get("session_id") or "")
    cwd = str(request.get("cwd") or ".")
    permission_mode = str(request.get("permission_mode") or "default")
    binding = read_binding(session_id)
    if binding:
        context = build_secretary_context(binding, cwd=cwd, permission_mode=permission_mode)
    elif should_bootstrap(prompt):
        context = build_bootstrap_context(cwd=cwd, permission_mode=permission_mode)
    else:
        return 0
    output = {
        "suppressOutput": True,
        "hookSpecificOutput": {
            "hookEventName": "UserPromptSubmit",
            "additionalContext": context,
        },
    }
    print(json.dumps(output, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
