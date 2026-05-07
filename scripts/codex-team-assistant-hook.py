#!/usr/bin/env python3
"""Inject Codex Teams guidance when a normal Codex prompt asks for teams."""

from __future__ import annotations

import json
import sys


TEAM_TRIGGERS = (
    "/team",
    "/teams",
    "agent team",
    "agent teams",
    "teamsを使用",
    "teamを使用",
    "teamsを使",
    "teamを使",
    "teamsで",
    "teamで",
    "チームを使",
    "部署を",
    "複数セッション",
    "オーケストレーション",
)


def should_inject(prompt: str) -> bool:
    lowered = prompt.lower()
    return any(trigger.lower() in lowered for trigger in TEAM_TRIGGERS)


def build_context(cwd: str, permission_mode: str) -> str:
    return f"""<codex_team_assistant>
The user's latest prompt appears to mention Codex Teams or multi-session orchestration.

This is an ordinary Codex session, so act as the user's lead secretary when the user asks to do work with teams:
- If the user is only asking a conceptual question about teams, answer directly and do not start a team.
- If the user asks to perform work using teams, launch or continue a Codex team from this session using the existing `codex team` CLI assets.
- Treat the current session as the user-facing secretary: start the team, keep the user informed, inspect status/events/messages, and relay follow-up instructions to the team lead.
- Prefer continuing an existing relevant live team over creating a duplicate. Inspect `codex team list`, `codex team status --team <id>`, and the team UI/status before deciding.
- For a new team, use the user's latest prompt as the team goal unless they specify otherwise. Use the current working directory as the default execution directory.
- Default new-team command shape:
  `codex team swarm "<user goal>" --app-server --discuss-rounds 0 --dangerously-bypass-approvals-and-sandbox --cd "{cwd}"`
- After starting a team, report the team id, state path, and useful monitor/UI command. Use `codex team ui --open` or `codex team monitor --team <id>` when useful.
- For follow-up user instructions to an existing team, send them to lead:
  `codex team message --team <id> --from user --to lead "<instruction>"`
- Use the existing generic team commands when appropriate: `team status`, `team message`, `team member`, `team node`, `team job`, `team monitor`, and `team ui`.
- If a user writes `/team`, `/teams`, `/goal`, or similar in normal chat, interpret it as a natural-language team-control intent. Native slash-command parsing is not required.
- Keep the user-facing response concise, but do the actual orchestration work instead of merely suggesting commands when execution is appropriate.

Runtime facts:
- cwd: {cwd}
- permission_mode: {permission_mode}
</codex_team_assistant>"""


def main() -> int:
    try:
        request = json.load(sys.stdin)
    except Exception:
        return 0

    prompt = str(request.get("prompt") or "")
    if not should_inject(prompt):
        return 0

    context = build_context(
        cwd=str(request.get("cwd") or "."),
        permission_mode=str(request.get("permission_mode") or "default"),
    )
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
