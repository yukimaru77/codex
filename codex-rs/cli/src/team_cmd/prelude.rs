use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use chrono::DateTime;
use chrono::FixedOffset;
use chrono::Local;
use chrono::LocalResult;
use chrono::NaiveDateTime;
use chrono::SecondsFormat;
use chrono::TimeZone;
use chrono::Timelike;
use chrono::Utc;
use clap::Args;
use clap::Parser;
use codex_app_server_client::AppServerEvent;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SandboxMode;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadTokenUsageUpdatedNotification;
use codex_app_server_protocol::TokenUsageBreakdown;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::TurnSteerParams;
use codex_app_server_protocol::TurnSteerResponse;
use codex_app_server_protocol::UserInput as AppServerUserInput;
use codex_utils_cli::SharedCliOptions;
use regex_lite::Regex;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::fs;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;

const CODEX_TEAM_HELPER_URL: &str =
    "https://raw.githubusercontent.com/yukimaru77/codex-team-tools/main/bin/codex-team";
const MAX_DIRECT_DEVICE_AUTH_ATTEMPTS: usize = 2;
const MAX_SIDE_CHANNEL_CONTEXTS_PER_PROMPT: usize = 8;
const MAX_REACTIVE_PROMPT_MESSAGES: usize = 12;
const MAX_REACTIVE_PROMPT_MESSAGE_CHARS: usize = 900;
const MAX_RUNTIME_START_UNREAD_MAILBOX_MESSAGES: usize = 24;
const MAX_RUNTIME_START_UNREAD_MAILBOX_TAIL_MESSAGES: usize = 12;
const MAX_RUNTIME_START_UNREAD_MAILBOX_SUMMARY_MESSAGES: usize = 8;
const MIN_ACTIVE_TURN_STEER_INTERVAL_SECS: u64 = 30;
const MAX_APP_SERVER_THREAD_TOTAL_TOKENS: i64 = 180_000;
const MAX_APP_SERVER_THREAD_CONTEXT_RATIO_PERCENT: i64 = 70;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[clap(rename_all = "kebab_case")]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TeamPromptLanguage {
    #[default]
    En,
    Ja,
}

impl TeamPromptLanguage {
    pub(crate) fn cli_value(self) -> &'static str {
        match self {
            TeamPromptLanguage::En => "en",
            TeamPromptLanguage::Ja => "ja",
        }
    }

    fn is_ja(self) -> bool {
        matches!(self, TeamPromptLanguage::Ja)
    }
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex team", arg_required_else_help = false)]
pub struct TeamCli {
    #[clap(flatten)]
    interactive_shared: SharedCliOptions,

    /// Attach bare `codex team` to this existing team lead instead of creating a new team.
    #[arg(long)]
    team: Option<String>,

    /// Language for generated team runtime prompts and system messages, for example `ja`.
    #[arg(long, value_enum)]
    language: Option<TeamPromptLanguage>,

    /// If keep-alive is enabled, automatically pause after this many idle seconds. Use 0 to stay alive indefinitely.
    #[arg(long, default_value_t = 0)]
    idle_exit_after_sec: u64,

    #[command(subcommand)]
    subcommand: Option<TeamSubcommand>,
}

#[derive(Debug, clap::Subcommand)]
enum TeamSubcommand {
    /// Create a local team workspace.
    Start(StartArgs),

    /// Create a team and run members as native Codex exec sessions.
    #[clap(visible_alias = "swarm")]
    Run(RunArgs),

    /// List local team workspaces.
    List(ListArgs),

    /// Show team status.
    Status(TeamSelector),

    /// Run discussion rounds for an existing team.
    Discuss(DiscussArgs),

    /// Reattach a keep-alive app-server runtime to an existing team state.
    Runtime(RuntimeArgs),

    /// Pause a running team runtime without deleting team state.
    Stop(StopArgs),

    /// Resume a paused team runtime from existing team state.
    Resume(RuntimeArgs),

    /// Manage shared team tasks.
    Task(TaskCli),

    /// Manage shared file ownership claims.
    Ownership(OwnershipCli),

    /// Manage team departments.
    Member(MemberCli),

    /// Manage local, SSH, Docker, and remote app-server nodes.
    Node(NodeCli),

    /// Run and inspect long-lived commands on team nodes.
    Job(JobCli),

    /// Track generic long-running waits, requests, gates, or external work.
    Wait(WaitCli),

    /// Audit an autoresearch team against phase/research/runtime evidence gates.
    AutoresearchAudit(AutoresearchAuditArgs),

    /// Manage the dedicated local browser profile used for remote device auth.
    AuthBrowser(AuthBrowserCli),

    /// Send a mailbox message to a team member.
    Message(MessageArgs),

    /// Read a team member mailbox.
    Inbox(InboxArgs),

    /// Show worker logs or final messages.
    Logs(LogsArgs),

    /// Open a tmux monitor for team status, messages, events, and live output.
    Monitor(MonitorArgs),

    /// Start a very small local web UI for team runs.
    Ui(UiArgs),

    /// Delete a local team workspace.
    Cleanup(CleanupArgs),
}

#[derive(Debug, Args)]
struct StartArgs {
    /// Goal for the team run.
    #[arg(value_name = "GOAL")]
    goal: String,

    /// Team id. Defaults to a timestamped id.
    #[arg(long)]
    id: Option<String>,

    /// Add a member as NAME or NAME:ROLE. Repeatable.
    #[arg(long = "member", value_name = "NAME[:ROLE]")]
    members: Vec<String>,

    /// Add an execution node. Forms: ID=ws://HOST:PORT or ID@ssh=HOST.
    #[arg(long = "node", value_name = "NODE_SPEC")]
    nodes: Vec<String>,

    /// Initial task. Defaults to the team goal.
    #[arg(long = "task", value_name = "TASK")]
    tasks: Vec<String>,

    /// Language for generated team runtime prompts and system messages, for example `ja`.
    #[arg(long, value_enum)]
    language: Option<TeamPromptLanguage>,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[command(flatten)]
    start: StartArgs,

    /// Model passed through to each `codex exec` worker.
    #[arg(long)]
    model: Option<String>,

    /// Config profile passed through to each `codex exec` worker.
    #[arg(long)]
    profile: Option<String>,

    /// Sandbox mode passed through to each `codex exec` worker.
    #[arg(long)]
    sandbox: Option<String>,

    /// Working directory for workers. Defaults to the current directory.
    #[arg(long = "cd", value_name = "DIR")]
    cwd: Option<PathBuf>,

    /// Run each member in an isolated git worktree.
    #[arg(long, default_value_t = false)]
    worktree: bool,

    /// Pass `--dangerously-bypass-approvals-and-sandbox` to worker sessions.
    #[arg(long, default_value_t = false)]
    dangerously_bypass_approvals_and_sandbox: bool,

    /// Print worker commands and prompts without starting Codex sessions.
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Create team state and optional worktrees without starting Codex sessions.
    #[arg(long, default_value_t = false)]
    prepare_only: bool,

    /// Skip the lead synthesis Codex session after workers finish.
    #[arg(long, default_value_t = false)]
    no_synthesis: bool,

    /// Number of discussion rounds before workers start implementation. Use 0 to disable.
    #[arg(long, default_value_t = 1)]
    discuss_rounds: u32,

    /// Run members through codex app-server threads and steer active turns when team messages arrive.
    #[arg(long, default_value_t = false)]
    app_server: bool,

    /// Poll interval for app-server reactive team messages, in milliseconds.
    #[arg(long, default_value_t = 1500)]
    reactive_poll_ms: u64,

    /// Periodically resync Codex config/skills/rules/memories/plugins to active remote nodes, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 300)]
    node_sync_interval_sec: u64,

    /// Periodically have one idle department ask active/blocked departments whether help is needed, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 600)]
    idle_outreach_interval_sec: u64,

    /// Periodically warn lead about tasks that are pending/in-progress without a live owner turn or tracked job, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 60)]
    task_watchdog_interval_sec: u64,

    /// Periodically nudge the lead to inspect unfinished work and make orchestration decisions, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 180)]
    lead_tick_interval_sec: u64,

    /// Wake each department when that department has no active turn for this many seconds. Use 0 to disable.
    #[arg(long, default_value_t = 300)]
    idle_wakeup_interval_sec: u64,

    /// Periodically ask active/unfinished departments to report progress, blockers, artifacts, and coordination needs, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 300)]
    department_heartbeat_interval_sec: u64,

    /// Warn lead when an app-server turn stays active with no observed assistant output for this many seconds. Use 0 to disable.
    #[arg(long, default_value_t = 300)]
    stale_active_turn_timeout_sec: u64,

    /// Periodically write an external autoresearch audit snapshot without steering the team. Use 0 to disable.
    #[arg(long, default_value_t = 600)]
    autoresearch_audit_interval_sec: u64,

    /// Let active departments answer incoming non-system team mail through a quick forked side-channel turn.
    #[arg(long, default_value_t = true)]
    side_channel_replies: bool,

    /// Internal: start only the live lead so a Codex TUI can attach directly to it.
    #[arg(long, hide = true, default_value_t = false)]
    interactive_lead: bool,

    /// Do not keep the app-server team alive after tasks complete.
    #[arg(long, default_value_t = false)]
    no_keep_alive: bool,

    /// If keep-alive is enabled, automatically pause after this many idle seconds. Use 0 to stay alive indefinitely.
    #[arg(long, default_value_t = 0)]
    idle_exit_after_sec: u64,

    /// Connect to an existing app-server websocket instead of starting one.
    #[arg(long)]
    app_server_url: Option<String>,

    /// Ignore the registered default app-server and start a private one.
    #[arg(long, default_value_t = false)]
    no_app_server_registry: bool,

    /// Internal: reattach runtime to this existing team instead of creating a team.
    #[arg(long, hide = true)]
    resume_team: Option<String>,
}

#[derive(Debug, Args)]
struct RuntimeArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// Model used for new app-server turns.
    #[arg(long)]
    model: Option<String>,

    /// Config profile passed to spawned app-server processes.
    #[arg(long)]
    profile: Option<String>,

    /// Sandbox mode used for new app-server turns.
    #[arg(long)]
    sandbox: Option<String>,

    /// Working directory for local lead/member turns. Defaults to the current directory.
    #[arg(long = "cd", value_name = "DIR")]
    cwd: Option<PathBuf>,

    /// Pass danger-full-access and never-ask approval policy to new turns.
    #[arg(long, default_value_t = false)]
    dangerously_bypass_approvals_and_sandbox: bool,

    /// Do not keep the app-server runtime alive after tasks complete.
    #[arg(long, default_value_t = false)]
    no_keep_alive: bool,

    /// If keep-alive is enabled, automatically pause after this many idle seconds. Use 0 to stay alive indefinitely.
    #[arg(long, default_value_t = 0)]
    idle_exit_after_sec: u64,

    /// Keep an existing run.pid process alive instead of replacing it.
    #[arg(long, default_value_t = false)]
    no_replace_existing: bool,

    /// Connect to an existing app-server websocket instead of starting one.
    #[arg(long)]
    app_server_url: Option<String>,

    /// Ignore the registered default app-server and start a private one.
    #[arg(long, default_value_t = false)]
    no_app_server_registry: bool,

    /// Poll interval for app-server reactive team messages, in milliseconds.
    #[arg(long, default_value_t = 1500)]
    reactive_poll_ms: u64,

    /// Periodically resync Codex config/skills/rules/memories/plugins to active remote nodes, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 300)]
    node_sync_interval_sec: u64,

    /// Periodically have one idle department ask active/blocked departments whether help is needed, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 600)]
    idle_outreach_interval_sec: u64,

    /// Periodically warn lead about tasks that are pending/in-progress without a live owner turn or tracked job, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 60)]
    task_watchdog_interval_sec: u64,

    /// Periodically nudge the lead to inspect unfinished work and make orchestration decisions, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 180)]
    lead_tick_interval_sec: u64,

    /// Wake each department when that department has no active turn for this many seconds. Use 0 to disable.
    #[arg(long, default_value_t = 300)]
    idle_wakeup_interval_sec: u64,

    /// Periodically ask active/unfinished departments to report progress, blockers, artifacts, and coordination needs, in seconds. Use 0 to disable.
    #[arg(long, default_value_t = 300)]
    department_heartbeat_interval_sec: u64,

    /// Warn lead when an app-server turn stays active with no observed assistant output for this many seconds. Use 0 to disable.
    #[arg(long, default_value_t = 300)]
    stale_active_turn_timeout_sec: u64,

    /// Periodically write an external autoresearch audit snapshot without steering the team. Use 0 to disable.
    #[arg(long, default_value_t = 600)]
    autoresearch_audit_interval_sec: u64,

    /// Let active departments answer incoming non-system team mail through a quick forked side-channel turn.
    #[arg(long, default_value_t = true)]
    side_channel_replies: bool,

    /// Language for generated team runtime prompts and system messages. Defaults to the team's saved language.
    #[arg(long, value_enum)]
    language: Option<TeamPromptLanguage>,
}

#[derive(Debug, Args)]
struct UiArgs {
    /// HTTP listen address for the local team UI.
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: String,

    /// Default working directory used by the new-team form.
    #[arg(long)]
    default_cwd: Option<PathBuf>,

    /// Open the UI in the default browser when possible.
    #[arg(long, default_value_t = false)]
    open: bool,

    /// Do not start a shared app-server when the registry is missing or stale.
    #[arg(long, default_value_t = false)]
    no_app_server_auto_start: bool,
}

#[derive(Debug, Args)]
struct AuthBrowserCli {
    #[command(subcommand)]
    subcommand: AuthBrowserSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum AuthBrowserSubcommand {
    /// Open the dedicated browser profile so the user can log in once.
    Login(AuthBrowserLoginArgs),

    /// Check whether the dedicated browser profile and CDP automation are usable.
    Status(AuthBrowserStatusArgs),

    /// Enter a Codex device-auth code in the dedicated browser.
    Authorize(AuthBrowserAuthorizeArgs),
}

#[derive(Debug, Args)]
struct AuthBrowserLoginArgs {
    /// URL to open for the one-time manual login.
    #[arg(long, default_value = "https://auth.openai.com/codex/device")]
    url: String,

    /// Browser profile directory. Defaults to a Chromium-accessible data directory.
    #[arg(long)]
    profile: Option<PathBuf>,

    /// X11 display to use. Defaults to DISPLAY, then :1 when available.
    #[arg(long)]
    display: Option<String>,
}

#[derive(Debug, Args)]
struct AuthBrowserStatusArgs {
    /// Browser profile directory. Defaults to a Chromium-accessible data directory.
    #[arg(long)]
    profile: Option<PathBuf>,

    /// X11 display to use. Defaults to DISPLAY, then :1 when available.
    #[arg(long)]
    display: Option<String>,
}

#[derive(Debug, Args)]
struct AuthBrowserAuthorizeArgs {
    /// Nine-character Codex device code. Hyphens are accepted.
    #[arg(value_name = "CODE")]
    code: String,

    /// Codex device-auth URL.
    #[arg(long, default_value = "https://auth.openai.com/codex/device")]
    url: String,

    /// Browser profile directory. Defaults to a Chromium-accessible data directory.
    #[arg(long)]
    profile: Option<PathBuf>,

    /// X11 display to use. Defaults to DISPLAY, then :1 when available.
    #[arg(long)]
    display: Option<String>,
}

#[derive(Debug, Args)]
struct TeamSelector {
    /// Team id. Defaults to the most recently updated team.
    #[arg(long)]
    team: Option<String>,
}

#[derive(Debug, Args)]
struct ListArgs {
    /// Print machine-readable JSON.
    #[arg(long, default_value_t = false)]
    json: bool,

    /// Only show teams whose runtime is still alive.
    #[arg(long, default_value_t = false)]
    live_only: bool,

    /// Pause idle keep-alive runtimes after this many seconds in stop(idle). Use 0 to only list.
    #[arg(long, alias = "exit-stop-after-sec", default_value_t = 0)]
    pause_idle_after_sec: u64,
}

#[derive(Debug, Args)]
struct DiscussArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// Number of discussion rounds to run.
    #[arg(long, default_value_t = 1)]
    rounds: u32,

    /// Model passed through to each `codex exec` discussion turn.
    #[arg(long)]
    model: Option<String>,

    /// Config profile passed through to each `codex exec` discussion turn.
    #[arg(long)]
    profile: Option<String>,

    /// Sandbox mode passed through to each `codex exec` discussion turn.
    #[arg(long)]
    sandbox: Option<String>,

    /// Working directory for discussion turns. Defaults to the current directory.
    #[arg(long = "cd", value_name = "DIR")]
    cwd: Option<PathBuf>,

    /// Pass `--dangerously-bypass-approvals-and-sandbox` to discussion turns.
    #[arg(long, default_value_t = false)]
    dangerously_bypass_approvals_and_sandbox: bool,

    /// Print discussion prompts without starting Codex sessions.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex team task")]
struct TaskCli {
    #[command(flatten)]
    selector: TeamSelector,

    #[command(subcommand)]
    subcommand: TaskSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum TaskSubcommand {
    /// Add a task to the shared task list.
    Add(TaskAddArgs),

    /// Claim an unassigned ready task and move it to in_progress.
    Claim(TaskClaimArgs),

    /// List tasks.
    List,

    /// Update task owner, status, dependencies, or result.
    Set(TaskSetArgs),
}

#[derive(Debug, Args)]
struct TaskAddArgs {
    /// Task subject.
    #[arg(value_name = "SUBJECT")]
    subject: String,

    /// Longer task description.
    #[arg(long, default_value = "")]
    description: String,

    /// Assign to a member.
    #[arg(long)]
    owner: Option<String>,

    /// Task id this task depends on. Repeatable.
    #[arg(long = "depends-on")]
    depends_on: Vec<String>,
}

#[derive(Debug, Args)]
struct TaskSetArgs {
    /// Task id.
    #[arg(value_name = "TASK_ID")]
    id: String,

    /// New task status.
    #[arg(long)]
    status: Option<TaskStatus>,

    /// New owner. Use --clear-owner to unassign.
    #[arg(long)]
    owner: Option<String>,

    /// Clear the current owner.
    #[arg(long, default_value_t = false)]
    clear_owner: bool,

    /// Replace the dependency list with these task id(s). Repeatable.
    #[arg(long = "depends-on")]
    depends_on: Vec<String>,

    /// Clear all dependencies.
    #[arg(long, default_value_t = false)]
    clear_depends: bool,

    /// Result or summary for the task.
    #[arg(long)]
    result: Option<String>,
}

#[derive(Debug, Args)]
struct TaskClaimArgs {
    /// Ready task id. Defaults to the first unassigned ready task.
    #[arg(value_name = "TASK_ID")]
    id: Option<String>,

    /// Claim as this member. Defaults to CODEX_TEAM_MEMBER or lead.
    #[arg(long)]
    owner: Option<String>,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex team ownership")]
struct OwnershipCli {
    #[command(flatten)]
    selector: TeamSelector,

    #[command(subcommand)]
    subcommand: OwnershipSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum OwnershipSubcommand {
    /// List file ownership claims.
    List,

    /// Claim a path before editing it.
    Claim(OwnershipClaimArgs),

    /// Release a path after handoff or completion.
    Release(OwnershipReleaseArgs),
}

#[derive(Debug, Args)]
struct OwnershipClaimArgs {
    /// Repository-relative or workspace-relative file path.
    #[arg(value_name = "PATH")]
    path: String,

    /// Owner member. Defaults to CODEX_TEAM_MEMBER or lead.
    #[arg(long)]
    owner: Option<String>,

    /// Short reason, handoff note, or editing scope.
    #[arg(long, default_value = "")]
    note: String,

    /// Replace an existing claim owned by another member.
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(Debug, Args)]
struct OwnershipReleaseArgs {
    /// Repository-relative or workspace-relative file path.
    #[arg(value_name = "PATH")]
    path: String,

    /// Releasing member. Defaults to CODEX_TEAM_MEMBER or lead.
    #[arg(long)]
    owner: Option<String>,

    /// Allow lead or explicit owner to release another member's claim.
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex team member")]
struct MemberCli {
    #[command(flatten)]
    selector: TeamSelector,

    #[command(subcommand)]
    subcommand: MemberSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum MemberSubcommand {
    /// List departments.
    List,

    /// Add a department. App-server runs will start it on the next poll.
    Add(MemberAddArgs),

    /// Move a department out of active work while keeping it available for handoffs.
    Standby(MemberStandbyArgs),

    /// Bring a standby department back online.
    Resume(MemberResumeArgs),
}

#[derive(Debug, Args)]
struct MemberAddArgs {
    /// Department as NAME or NAME:ROLE.
    #[arg(value_name = "NAME[:ROLE]")]
    member: String,

    /// Node where this department should run.
    #[arg(long)]
    node: Option<String>,

    /// Mission for the new department.
    #[arg(long, default_value = "")]
    mission: String,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex team node")]
struct NodeCli {
    #[command(flatten)]
    selector: TeamSelector,

    #[command(subcommand)]
    subcommand: NodeSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum NodeSubcommand {
    /// List execution nodes for this team.
    List,

    /// Inspect capabilities and runtime facts for an execution node.
    Inspect(NodeInspectArgs),

    /// Create a Docker container locally or on an SSH host and register it as a node.
    CreateDocker(NodeCreateDockerArgs),

    /// Sync selected Codex assets to a node's CODEX_HOME.
    SyncAssets(NodeSyncAssetsArgs),

    /// Sync a local file or directory to a node path for artifact handoff.
    SyncPath(NodeSyncPathArgs),

    /// Pull a node-side file or directory into a local path for artifact consumption.
    PullPath(NodePullPathArgs),

    /// Register or update an execution node.
    Add(NodeAddArgs),

    /// Remove an execution node that is not assigned to a member.
    Remove(NodeRemoveArgs),
}

#[derive(Debug, Args)]
struct NodeAddArgs {
    /// Node id used by members, for example remote-linux or gpu-container.
    #[arg(value_name = "ID")]
    id: String,

    /// Node kind.
    #[arg(long, value_enum, default_value = "manual")]
    kind: TeamNodeKind,

    /// Reachable app-server websocket URL. For SSH nodes this should usually be a local forwarded URL.
    #[arg(long)]
    url: Option<String>,

    /// SSH host such as user@example.com.
    #[arg(long)]
    host: Option<String>,

    /// Docker container name or id.
    #[arg(long)]
    container: Option<String>,

    /// Working directory on that node.
    #[arg(long)]
    cwd: Option<String>,

    /// Operator note or bootstrap instructions.
    #[arg(long, default_value = "")]
    note: String,
}

#[derive(Debug, Args)]
struct NodeRemoveArgs {
    /// Node id.
    #[arg(value_name = "ID")]
    id: String,

    /// Remove even if members reference it.
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(Debug, Args)]
struct NodeInspectArgs {
    /// Node id. Omit to inspect every registered node.
    #[arg(value_name = "ID")]
    id: Option<String>,

    /// Print raw key/value facts only.
    #[arg(long, default_value_t = false)]
    raw: bool,
}

#[derive(Debug, Args)]
struct NodeCreateDockerArgs {
    /// Node id to register.
    #[arg(value_name = "ID")]
    id: String,

    /// Create the container on this SSH host instead of locally.
    #[arg(long)]
    host: Option<String>,

    /// Docker image.
    #[arg(long, default_value = "ubuntu:22.04")]
    image: String,

    /// Container name. Defaults to codex-team-TEAM-ID-NODE-ID.
    #[arg(long)]
    container: Option<String>,

    /// Container working directory.
    #[arg(long, default_value = "/workspace")]
    cwd: String,

    /// Bind mount in HOST_PATH:CONTAINER_PATH form. Repeatable.
    #[arg(long = "mount", value_name = "HOST:CONTAINER")]
    mounts: Vec<String>,

    /// Publish port in HOST_PORT:CONTAINER_PORT form. Repeatable.
    #[arg(long = "port", value_name = "HOST:CONTAINER")]
    ports: Vec<String>,

    /// Environment variable in KEY=VALUE form. Repeatable.
    #[arg(long = "env", value_name = "KEY=VALUE")]
    env: Vec<String>,

    /// Add --gpus all.
    #[arg(long, default_value_t = false)]
    gpus: bool,

    /// Remove an existing container with the same name before creating it.
    #[arg(long, default_value_t = false)]
    replace: bool,

    /// Command used to keep the container alive.
    #[arg(long, default_value = "sleep infinity")]
    command: String,

    /// Operator note.
    #[arg(
        long,
        default_value = "Managed Docker node created by codex team node create-docker."
    )]
    note: String,
}

#[derive(Debug, Args)]
struct NodeSyncAssetsArgs {
    /// Node id.
    #[arg(value_name = "ID")]
    id: String,

    /// Destination CODEX_HOME on the node.
    #[arg(long, default_value = "$HOME/.codex")]
    dest: String,

    /// Also sync auth.json. Use only when this is acceptable for the target node.
    #[arg(long, default_value_t = false)]
    include_auth: bool,

    /// Print the generated command without running it.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct NodeSyncPathArgs {
    /// Node id.
    #[arg(value_name = "ID")]
    id: String,

    /// Local file or directory to send.
    #[arg(long)]
    src: PathBuf,

    /// Destination path on the node.
    #[arg(long)]
    dest: String,

    /// Replace an existing destination path after backing it up.
    #[arg(long, default_value_t = false)]
    replace: bool,

    /// Print the generated command without running it.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct NodePullPathArgs {
    /// Node id.
    #[arg(value_name = "ID")]
    id: String,

    /// File or directory path on the node to pull.
    #[arg(long)]
    src: String,

    /// Local destination path.
    #[arg(long)]
    dest: PathBuf,

    /// Replace an existing local destination path after backing it up.
    #[arg(long, default_value_t = false)]
    replace: bool,

    /// Print the generated command without running it.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex team job")]
struct JobCli {
    #[command(flatten)]
    selector: TeamSelector,

    #[command(subcommand)]
    subcommand: JobSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum JobSubcommand {
    /// List jobs.
    List(JobListArgs),

    /// Start a background command on a node.
    Start(JobStartArgs),

    /// Show status for a job and refresh it from the node when possible.
    Status(JobSelectArgs),

    /// Print the stored job log.
    Logs(JobLogsArgs),

    /// Stop a running job.
    Stop(JobSelectArgs),

    /// Register an artifact path produced by a job.
    Artifact(JobArtifactArgs),
}

#[derive(Debug, Default, Args)]
struct JobListArgs {
    /// Show only jobs owned by this department/member.
    #[arg(long)]
    owner: Option<String>,

    /// Show only jobs linked to this task id.
    #[arg(long)]
    task: Option<String>,

    /// Show only jobs with this status.
    #[arg(long)]
    status: Option<TeamJobStatus>,

    /// Show at most this many jobs. When set, the newest matching jobs are shown.
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Debug, Args)]
struct JobStartArgs {
    /// Job id. Defaults to job-N.
    #[arg(long)]
    id: Option<String>,

    /// Node where the command should run.
    #[arg(long, default_value = "local")]
    node: String,

    /// Working directory for the command. Defaults to the node cwd or current directory.
    #[arg(long)]
    cwd: Option<String>,

    /// Human-readable note.
    #[arg(long, default_value = "")]
    note: String,

    /// Department/member that owns this job. Defaults to CODEX_TEAM_MEMBER, then lead.
    #[arg(long)]
    owner: Option<String>,

    /// Task id this job is executing or verifying.
    #[arg(long)]
    task: Option<String>,

    /// Command and arguments to run. Use `--` before the command.
    #[arg(required = true, trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct JobSelectArgs {
    /// Job id.
    #[arg(value_name = "ID")]
    id: String,
}

#[derive(Debug, Args)]
struct JobLogsArgs {
    /// Job id.
    #[arg(value_name = "ID")]
    id: String,

    /// Number of trailing log lines to print.
    #[arg(long)]
    tail: Option<usize>,
}

#[derive(Debug, Args)]
struct JobArtifactArgs {
    /// Job id.
    #[arg(value_name = "ID")]
    id: String,

    /// Artifact path on the node.
    #[arg(value_name = "PATH")]
    path: String,

    /// Optional note.
    #[arg(long, default_value = "")]
    note: String,
}

#[derive(Debug, Parser)]
#[command(bin_name = "codex team wait")]
struct WaitCli {
    #[command(flatten)]
    selector: TeamSelector,

    #[command(subcommand)]
    subcommand: WaitSubcommand,
}

#[derive(Debug, clap::Subcommand)]
enum WaitSubcommand {
    /// Register a generic wait item with a concrete completion condition.
    Add(WaitAddArgs),

    /// List wait items.
    List(WaitListArgs),

    /// Update wait status, progress, or evidence.
    Set(WaitSetArgs),
}

#[derive(Debug, Args)]
struct WaitAddArgs {
    /// Human-readable wait title.
    #[arg(value_name = "TITLE")]
    title: String,

    /// Department/member responsible for checking and closing this wait.
    #[arg(long)]
    owner: Option<String>,

    /// Task id this wait gates or informs.
    #[arg(long)]
    task: Option<String>,

    /// Node/site where the work or wait is happening, if any.
    #[arg(long)]
    node: Option<String>,

    /// Concrete condition that proves this wait is finished.
    #[arg(long, default_value = "")]
    condition: String,

    /// Current status.
    #[arg(long, default_value = "waiting")]
    status: TeamWaitStatus,

    /// Initial progress note, request id, URL, log path, or checkpoint.
    #[arg(long, default_value = "")]
    progress: String,

    /// Evidence path or URL proving completion/failure.
    #[arg(long)]
    evidence: Option<String>,
}

#[derive(Debug, Default, Args)]
struct WaitListArgs {
    /// Show only waits owned by this department/member.
    #[arg(long)]
    owner: Option<String>,

    /// Show only waits linked to this task id.
    #[arg(long)]
    task: Option<String>,

    /// Show only waits with this status.
    #[arg(long)]
    status: Option<TeamWaitStatus>,

    /// Show at most this many waits. When set, the newest matching waits are shown.
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Debug, Args)]
struct WaitSetArgs {
    /// Wait id.
    #[arg(value_name = "ID")]
    id: String,

    /// New status.
    #[arg(long)]
    status: Option<TeamWaitStatus>,

    /// Replace progress note, request id, log path, URL, or checkpoint.
    #[arg(long)]
    progress: Option<String>,

    /// Replace evidence path or URL.
    #[arg(long)]
    evidence: Option<String>,

    /// Clear current evidence.
    #[arg(long, default_value_t = false)]
    clear_evidence: bool,
}

#[derive(Debug, Args)]
struct MemberStandbyArgs {
    /// Department name.
    #[arg(value_name = "NAME")]
    member: String,

    /// Reason or handoff note.
    #[arg(long, default_value = "")]
    reason: String,
}

#[derive(Debug, Args)]
struct MemberResumeArgs {
    /// Department name.
    #[arg(value_name = "NAME")]
    member: String,

    /// Optional new task/mission to assign when resuming.
    #[arg(long)]
    mission: Option<String>,
}

#[derive(Debug, Args)]
struct MessageArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// Sender member name. Defaults to CODEX_TEAM_MEMBER or lead.
    #[arg(long)]
    from: Option<String>,

    /// Recipient member name, `all`, or comma-separated member names.
    #[arg(value_name = "TO")]
    to: String,

    /// Message body.
    #[arg(value_name = "MESSAGE")]
    message: String,
}

#[derive(Debug, Args)]
struct InboxArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// Member inbox to read. Defaults to CODEX_TEAM_MEMBER or lead.
    #[arg(value_name = "MEMBER")]
    member: Option<String>,
}

#[derive(Debug, Args)]
struct LogsArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// Member log to read. Omit to list available logs.
    member: Option<String>,

    /// Show the worker's final assistant message instead of stdout/stderr log.
    #[arg(long, default_value_t = false)]
    last_message: bool,

    /// Show live app-server assistant stream instead of stdout/stderr log.
    #[arg(long, default_value_t = false)]
    live: bool,
}

#[derive(Debug, Args)]
struct MonitorArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// tmux session name. Defaults to codex-team-TEAM_ID.
    #[arg(long)]
    session: Option<String>,

    /// Attach to the tmux session after creating it.
    #[arg(long, default_value_t = false)]
    attach: bool,

    /// Kill an existing tmux monitor session with the same name first.
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[derive(Debug, Args)]
struct CleanupArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// Delete without a confirmation prompt.
    #[arg(long, default_value_t = false)]
    force: bool,

    /// Delete every local team whose runtime is already exiting.
    #[arg(long, default_value_t = false)]
    exiting: bool,

    /// Also delete this team's state directory from registered SSH/Docker nodes.
    #[arg(long, default_value_t = false)]
    remote_state: bool,

    /// Also stop and remove registered Docker/ssh-docker containers for this team.
    #[arg(long, default_value_t = false)]
    containers: bool,

    /// Continue deleting local state even if a remote cleanup operation fails.
    #[arg(long, default_value_t = false)]
    ignore_remote_errors: bool,

    /// Print what would be deleted without deleting anything.
    #[arg(long, default_value_t = false)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct StopArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// Pause every running/idle team runtime.
    #[arg(long, default_value_t = false)]
    all: bool,

    /// Leave the registered local app-server process running.
    #[arg(long, default_value_t = false)]
    keep_local_app_server: bool,

    /// Do not try to stop SSH/Docker node app-servers.
    #[arg(long, default_value_t = false)]
    no_remote_nodes: bool,
}

#[derive(Debug, Args)]
struct AutoresearchAuditArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// Write the audit report into the team state directory and print its path.
    #[arg(long, default_value_t = false)]
    write: bool,

    /// Custom report output path. Implies --write.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamConfig {
    version: u32,
    id: String,
    goal: String,
    lead: String,
    members: Vec<TeamMember>,
    #[serde(default)]
    language: Option<TeamPromptLanguage>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamMember {
    name: String,
    role: String,
    status: MemberStatus,
    joined_at: String,
    thread_id: Option<String>,
    workspace_path: Option<String>,
    #[serde(default)]
    node: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamNode {
    id: String,
    kind: TeamNodeKind,
    url: Option<String>,
    host: Option<String>,
    container: Option<String>,
    cwd: Option<String>,
    status: TeamNodeStatus,
    note: String,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[clap(rename_all = "kebab_case")]
#[serde(rename_all = "snake_case")]
enum TeamNodeKind {
    Local,
    Manual,
    Ssh,
    Docker,
    SshDocker,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TeamNodeStatus {
    Pending,
    Online,
    Offline,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MemberStatus {
    Online,
    Running,
    Standby,
    Completed,
    Failed,
    Offline,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamTask {
    id: String,
    subject: String,
    description: String,
    owner: Option<String>,
    status: TaskStatus,
    depends_on: Vec<String>,
    result: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct FileOwnership {
    path: String,
    owner: String,
    note: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamJob {
    id: String,
    node: String,
    command: String,
    cwd: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    status: TeamJobStatus,
    pid: Option<String>,
    log_path: String,
    exit_path: String,
    exit_code: Option<i32>,
    note: String,
    artifacts: Vec<TeamArtifact>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamWait {
    id: String,
    title: String,
    #[serde(default)]
    owner: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    node: Option<String>,
    condition: String,
    status: TeamWaitStatus,
    progress: String,
    #[serde(default)]
    evidence: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamArtifact {
    path: String,
    note: String,
    created_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[clap(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
enum TeamJobStatus {
    Running,
    Completed,
    Failed,
    Stopped,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[clap(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
enum TeamWaitStatus {
    Waiting,
    Running,
    Polling,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for TeamWaitStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TeamWaitStatus::Waiting => "waiting",
            TeamWaitStatus::Running => "running",
            TeamWaitStatus::Polling => "polling",
            TeamWaitStatus::Blocked => "blocked",
            TeamWaitStatus::Completed => "completed",
            TeamWaitStatus::Failed => "failed",
            TeamWaitStatus::Cancelled => "cancelled",
        })
    }
}

impl TeamWaitStatus {
    fn is_open(&self) -> bool {
        matches!(
            self,
            TeamWaitStatus::Waiting
                | TeamWaitStatus::Running
                | TeamWaitStatus::Polling
                | TeamWaitStatus::Blocked
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, clap::ValueEnum)]
#[clap(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Pending,
    Waiting,
    Ready,
    #[value(name = "in_progress", alias = "in-progress")]
    InProgress,
    Blocked,
    Review,
    Completed,
    Failed,
    Cancelled,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            TaskStatus::Pending => "pending",
            TaskStatus::Waiting => "waiting",
            TaskStatus::Ready => "ready",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Review => "review",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct MailMessage {
    from: String,
    to: String,
    message: String,
    timestamp: String,
    read: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SideChannelContextStatus {
    Pending,
    Injected,
    Acknowledged,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SideChannelContextRecord {
    id: String,
    member: String,
    node: String,
    source_thread: String,
    side_thread: String,
    side_turn: String,
    recipients: Vec<String>,
    incoming_summary: String,
    reply: String,
    created_at: String,
    status: SideChannelContextStatus,
    injected_turns: Vec<String>,
    injected_at: Option<String>,
    acknowledged_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct Event<'a> {
    event: &'a str,
    timestamp: String,
    team: &'a str,
    data: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct TeamEventRecord {
    event: String,
    timestamp: String,
    data: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct UiRealtimeSnapshot {
    team: String,
    generated_at: String,
    members: Vec<UiRealtimeMember>,
    events: Vec<String>,
    messages: Vec<UiRealtimeMessage>,
}

#[derive(Debug, Serialize)]
struct UiRealtimeMember {
    name: String,
    role: String,
    status: String,
    task_status: String,
    node: String,
    location: String,
    unread: usize,
    direct_unread: usize,
    cooldown: String,
    thread: String,
    live: String,
    last: String,
    inbox_tail: String,
}

#[derive(Debug, Serialize)]
struct UiRealtimeMessage {
    timestamp: String,
    from: String,
    to: String,
    message: String,
}

#[derive(Debug, Serialize)]
struct UiDebugTimeline {
    team: String,
    generated_at: String,
    items: Vec<UiDebugTimelineItem>,
}

#[derive(Clone, Debug, Serialize)]
struct UiDebugTimelineItem {
    timestamp: String,
    kind: String,
    title: String,
    actor: String,
    target: String,
    body: String,
    meta: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamTurnUsageIndexRecord {
    timestamp: String,
    member: String,
    role: String,
    node: String,
    thread: String,
    turn: String,
    category: String,
    source_event: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamTokenUsageRecord {
    timestamp: String,
    member: String,
    role: String,
    node: String,
    thread: String,
    turn: String,
    category: String,
    source: String,
    total: TeamTokenUsageBreakdown,
    last: TeamTokenUsageBreakdown,
    model_context_window: Option<i64>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
struct TeamTokenUsageBreakdown {
    total_tokens: i64,
    input_tokens: i64,
    cached_input_tokens: i64,
    output_tokens: i64,
    reasoning_output_tokens: i64,
}

impl TeamTokenUsageBreakdown {
    fn add_assign(&mut self, other: Self) {
        self.total_tokens += other.total_tokens;
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
    }

    fn uncached_input_tokens(&self) -> i64 {
        self.input_tokens
            .saturating_sub(self.cached_input_tokens)
            .max(0)
    }
}

impl From<TokenUsageBreakdown> for TeamTokenUsageBreakdown {
    fn from(value: TokenUsageBreakdown) -> Self {
        Self {
            total_tokens: value.total_tokens,
            input_tokens: value.input_tokens,
            cached_input_tokens: value.cached_input_tokens,
            output_tokens: value.output_tokens,
            reasoning_output_tokens: value.reasoning_output_tokens,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AppServerRegistry {
    url: String,
    pid: u32,
    updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct TeamSecretaryBinding {
    session_id: String,
    team_id: String,
    team_dir: String,
    cwd: String,
    role: String,
    created_at: String,
    updated_at: String,
}

impl TeamCli {
    pub(crate) fn is_interactive_entrypoint(&self) -> bool {
        self.subcommand.is_none()
    }

    pub(crate) fn into_interactive_parts(
        self,
    ) -> (
        SharedCliOptions,
        Option<String>,
        Option<TeamPromptLanguage>,
        u64,
    ) {
        (
            self.interactive_shared,
            self.team,
            self.language,
            self.idle_exit_after_sec,
        )
    }

    pub async fn run(self) -> Result<()> {
        let codex_home =
            codex_core::config::find_codex_home().context("failed to resolve CODEX_HOME")?;
        let root = codex_home.join("teams");

        match self.subcommand {
            None => bail!("interactive `codex team` must be launched by the top-level CLI"),
            Some(TeamSubcommand::Start(args)) => {
                let (team_id, team_dir) = create_team(&root, args)?;
                ensure_container_node_departments(&team_dir)?;
                println!("Created team `{team_id}`");
                println!("State: {}", team_dir.display());
                Ok(())
            }
            Some(TeamSubcommand::Run(args)) => {
                if args.app_server {
                    run_team_app_server(&root, args).await
                } else {
                    run_team(&root, args)
                }
            }
            Some(TeamSubcommand::List(args)) => list_teams(&root, args),
            Some(TeamSubcommand::Status(selector)) => {
                let team_dir = resolve_team_dir(&root, selector.team.as_deref())?;
                auto_promote_dependency_waits(&team_dir)?;
                print_status(&team_dir)
            }
            Some(TeamSubcommand::Discuss(args)) => discuss_team(&root, args),
            Some(TeamSubcommand::Runtime(args)) => {
                run_team_app_server(&root, runtime_args_to_run_args(args, &root)?).await
            }
            Some(TeamSubcommand::Stop(args)) => stop_team_runtime(&root, args),
            Some(TeamSubcommand::Resume(args)) => {
                run_team_app_server(&root, runtime_args_to_run_args(args, &root)?).await
            }
            Some(TeamSubcommand::Task(cli)) => run_task(&root, cli),
            Some(TeamSubcommand::Ownership(cli)) => run_ownership(&root, cli),
            Some(TeamSubcommand::Member(cli)) => run_member(&root, cli),
            Some(TeamSubcommand::Node(cli)) => run_node(&root, cli),
            Some(TeamSubcommand::Job(cli)) => run_job(&root, cli),
            Some(TeamSubcommand::Wait(cli)) => run_wait(&root, cli),
            Some(TeamSubcommand::AutoresearchAudit(args)) => run_autoresearch_audit(&root, args),
            Some(TeamSubcommand::AuthBrowser(cli)) => run_auth_browser(&codex_home, cli),
            Some(TeamSubcommand::Message(args)) => send_message(&root, args),
            Some(TeamSubcommand::Inbox(args)) => read_inbox(&root, args),
            Some(TeamSubcommand::Logs(args)) => read_logs(&root, args),
            Some(TeamSubcommand::Monitor(args)) => start_tmux_monitor(&root, args),
            Some(TeamSubcommand::Ui(args)) => start_team_ui(&root, args),
            Some(TeamSubcommand::Cleanup(args)) => cleanup_team(&root, args),
        }
    }
}

