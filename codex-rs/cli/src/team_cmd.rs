// Codex Teams command implementation.
//
// This module is intentionally split into small files under `team_cmd/` while
// being included into one Rust module. The include-based structure keeps the
// existing private item relationships intact during rapid iteration, but avoids
// a single unreviewable 30k+ line file.

include!("team_cmd/prelude.rs");
include!("team_cmd/interactive.rs");
include!("team_cmd/design.rs");
include!("team_cmd/runtime.rs");
include!("team_cmd/relay.rs");
include!("team_cmd/audit_status.rs");
include!("team_cmd/nodes_auth.rs");
include!("team_cmd/app_events.rs");
include!("team_cmd/journals.rs");
include!("team_cmd/ui.rs");
include!("team_cmd/cleanup.rs");
include!("team_cmd/tasks_nodes.rs");
include!("team_cmd/jobs_waits.rs");
include!("team_cmd/prompts_storage.rs");
include!("team_cmd/tests.rs");
