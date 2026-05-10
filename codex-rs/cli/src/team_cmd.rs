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
use std::fmt;
use std::fs;
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
    List,

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

    /// Let active departments answer incoming non-system team mail through a quick forked side-channel turn.
    #[arg(long, default_value_t = true)]
    side_channel_replies: bool,

    /// Internal: start only the live lead so a Codex TUI can attach directly to it.
    #[arg(long, hide = true, default_value_t = false)]
    interactive_lead: bool,

    /// Do not keep the app-server team alive after tasks complete.
    #[arg(long, default_value_t = false)]
    no_keep_alive: bool,

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
}

#[derive(Debug, Args)]
struct StopArgs {
    #[command(flatten)]
    selector: TeamSelector,

    /// Leave the registered local app-server process running.
    #[arg(long, default_value_t = false)]
    keep_local_app_server: bool,

    /// Do not try to stop SSH/Docker node app-servers.
    #[arg(long, default_value_t = false)]
    no_remote_nodes: bool,
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
    ) -> (SharedCliOptions, Option<String>, Option<TeamPromptLanguage>) {
        (self.interactive_shared, self.team, self.language)
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
            Some(TeamSubcommand::List) => list_teams(&root),
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

fn runtime_args_to_run_args(args: RuntimeArgs, root: &Path) -> Result<RunArgs> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    if !args.no_replace_existing
        && let Some(pid) = read_team_run_pid(&team_dir)
        && pid != std::process::id()
    {
        stop_process(pid);
    }
    Ok(RunArgs {
        start: StartArgs {
            goal: format!("Reattach runtime for {}", config.id),
            id: None,
            members: Vec::new(),
            nodes: Vec::new(),
            tasks: Vec::new(),
            language: args.language.or(config.language),
        },
        model: args.model,
        profile: args.profile,
        sandbox: args.sandbox,
        cwd: args.cwd,
        worktree: false,
        dangerously_bypass_approvals_and_sandbox: args.dangerously_bypass_approvals_and_sandbox,
        dry_run: false,
        prepare_only: false,
        no_synthesis: true,
        discuss_rounds: 0,
        app_server: true,
        reactive_poll_ms: args.reactive_poll_ms,
        node_sync_interval_sec: args.node_sync_interval_sec,
        idle_outreach_interval_sec: args.idle_outreach_interval_sec,
        task_watchdog_interval_sec: args.task_watchdog_interval_sec,
        lead_tick_interval_sec: args.lead_tick_interval_sec,
        idle_wakeup_interval_sec: args.idle_wakeup_interval_sec,
        department_heartbeat_interval_sec: args.department_heartbeat_interval_sec,
        stale_active_turn_timeout_sec: args.stale_active_turn_timeout_sec,
        side_channel_replies: args.side_channel_replies,
        interactive_lead: false,
        no_keep_alive: args.no_keep_alive,
        app_server_url: args.app_server_url,
        no_app_server_registry: args.no_app_server_registry,
        resume_team: Some(config.id),
    })
}

pub(crate) struct TeamInteractiveLeadLaunch {
    pub(crate) team_id: String,
    pub(crate) team_dir: PathBuf,
    pub(crate) app_server_url: String,
    pub(crate) lead_thread_id: String,
}

pub(crate) fn launch_interactive_lead_team(
    shared: &SharedCliOptions,
    team: Option<&str>,
    language: Option<TeamPromptLanguage>,
) -> Result<TeamInteractiveLeadLaunch> {
    let codex_home =
        codex_core::config::find_codex_home().context("failed to resolve CODEX_HOME")?;
    let root = codex_home.join("teams");
    fs::create_dir_all(&root)?;
    let _app_server_child = ensure_team_ui_app_server(&root)?;
    let app_server_url = read_registered_app_server_url()?
        .filter(|url| app_server_readyz(url))
        .context("shared app-server did not become ready for interactive team lead")?;
    if let Some(team) = team {
        return launch_existing_interactive_lead_team(
            &root,
            shared,
            team,
            &app_server_url,
            language,
        );
    }
    let team_id = format!("team-{}", tokyo_now().format("%Y%m%d%H%M%S"));
    let team_dir = root.join(&team_id);
    let cwd = shared
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    let goal = "Interactive Codex team lead session. Wait for the user's first substantive team request. You are the live lead; create departments, tasks, SSH/Docker nodes, and coordination flow only after the user asks for work that needs them.";

    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("team")
        .arg("swarm")
        .arg("--id")
        .arg(&team_id)
        .arg("--app-server")
        .arg("--app-server-url")
        .arg(&app_server_url)
        .arg("--discuss-rounds")
        .arg("0")
        .arg("--interactive-lead")
        .arg("--cd")
        .arg(&cwd);
    if let Some(language) = language {
        command.arg("--language").arg(language.cli_value());
    }
    if shared.dangerously_bypass_approvals_and_sandbox {
        command.arg("--dangerously-bypass-approvals-and-sandbox");
    }
    if let Some(model) = shared.model.as_deref() {
        command.arg("--model").arg(model);
    }
    if let Some(profile) = shared.config_profile.as_deref() {
        command.arg("--profile").arg(profile);
    }
    if let Some(sandbox) = shared.sandbox_mode {
        command
            .arg("--sandbox")
            .arg(sandbox_mode_cli_arg_name(sandbox));
    }
    command.arg(goal).stdin(Stdio::null());

    let log_path = root.join("interactive-lead-runs.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let stderr = log.try_clone()?;
    let mut child = command
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("spawn interactive team lead runtime")?;

    let started_at = Instant::now();
    loop {
        if let Some(lead_thread_id) = read_team_lead_thread_id(&team_dir)? {
            append_event(
                &team_dir,
                "interactive_lead_tui_attached",
                serde_json::json!({
                    "thread": lead_thread_id,
                    "app_server_url": app_server_url,
                    "cwd": cwd,
                }),
            )?;
            return Ok(TeamInteractiveLeadLaunch {
                team_id,
                team_dir: team_dir.to_path_buf(),
                app_server_url,
                lead_thread_id,
            });
        }
        if let Some(status) = child.try_wait()? {
            bail!(
                "interactive team lead runtime exited before lead thread was ready: {status}. See {}",
                log_path.display()
            );
        }
        if started_at.elapsed() > Duration::from_secs(30) {
            bail!(
                "timed out waiting for interactive team lead thread. See {}",
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn launch_existing_interactive_lead_team(
    root: &Path,
    shared: &SharedCliOptions,
    team: &str,
    app_server_url: &str,
    language: Option<TeamPromptLanguage>,
) -> Result<TeamInteractiveLeadLaunch> {
    let team_dir = resolve_team_dir(root, Some(team))?;
    let config = load_config(&team_dir)?;
    let cwd = shared
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    let old_lead_thread_id = read_team_lead_thread_id(&team_dir)?;
    let runtime_alive = read_team_run_pid(&team_dir)
        .map(|pid| process_alive(pid) && process_looks_like_codex_team(pid))
        .unwrap_or(false);

    let mut child = if runtime_alive {
        None
    } else {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("team")
            .arg("resume")
            .arg("--team")
            .arg(&config.id)
            .arg("--app-server-url")
            .arg(app_server_url)
            .arg("--cd")
            .arg(&cwd);
        if shared.dangerously_bypass_approvals_and_sandbox {
            command.arg("--dangerously-bypass-approvals-and-sandbox");
        }
        if let Some(model) = shared.model.as_deref() {
            command.arg("--model").arg(model);
        }
        if let Some(profile) = shared.config_profile.as_deref() {
            command.arg("--profile").arg(profile);
        }
        if let Some(sandbox) = shared.sandbox_mode {
            command
                .arg("--sandbox")
                .arg(sandbox_mode_cli_arg_name(sandbox));
        }
        if let Some(language) = language {
            command.arg("--language").arg(language.cli_value());
        }
        command.stdin(Stdio::null());

        let log_path = root.join("interactive-lead-runs.log");
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("open {}", log_path.display()))?;
        let stderr = log.try_clone()?;
        let child = command
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .spawn()
            .context("spawn existing team lead runtime")?;
        Some((child, log_path))
    };

    let started_at = Instant::now();
    loop {
        if let Some(lead_thread_id) = read_team_lead_thread_id(&team_dir)?
            && (runtime_alive || old_lead_thread_id.as_deref() != Some(lead_thread_id.as_str()))
        {
            append_event(
                &team_dir,
                "interactive_lead_tui_attached",
                serde_json::json!({
                    "thread": lead_thread_id,
                    "app_server_url": app_server_url,
                    "cwd": cwd,
                    "resumed_existing_team": !runtime_alive,
                }),
            )?;
            return Ok(TeamInteractiveLeadLaunch {
                team_id: config.id,
                team_dir,
                app_server_url: app_server_url.to_string(),
                lead_thread_id,
            });
        }
        if let Some((child, log_path)) = child.as_mut()
            && let Some(status) = child.try_wait()?
        {
            bail!(
                "existing team lead runtime exited before lead thread was ready: {status}. See {}",
                log_path.display()
            );
        }
        if started_at.elapsed() > Duration::from_secs(30) {
            if runtime_alive {
                bail!("team `{}` has no lead thread to attach", config.id);
            }
            let log_path = root.join("interactive-lead-runs.log");
            bail!(
                "timed out waiting for team `{}` lead thread after resume. See {}",
                config.id,
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn read_team_lead_thread_id(team_dir: &Path) -> Result<Option<String>> {
    if !team_dir.join("config.json").exists() {
        return Ok(None);
    }
    let config = load_config(team_dir)?;
    Ok(config
        .members
        .iter()
        .find(|member| member.role == "lead")
        .and_then(|member| member.thread_id.clone())
        .filter(|thread| !thread.trim().is_empty()))
}

fn sandbox_mode_cli_arg_name(mode: codex_utils_cli::SandboxModeCliArg) -> &'static str {
    match mode {
        codex_utils_cli::SandboxModeCliArg::ReadOnly => "read-only",
        codex_utils_cli::SandboxModeCliArg::WorkspaceWrite => "workspace-write",
        codex_utils_cli::SandboxModeCliArg::DangerFullAccess => "danger-full-access",
    }
}

fn app_server_registry_path() -> Result<PathBuf> {
    let codex_home =
        codex_core::config::find_codex_home().context("failed to resolve CODEX_HOME")?;
    Ok(codex_home.join("app-server.json").to_path_buf())
}

pub(crate) fn register_app_server_transport(
    transport: &codex_app_server::AppServerTransport,
) -> Result<Option<String>> {
    let codex_app_server::AppServerTransport::WebSocket { bind_address } = transport else {
        return Ok(None);
    };
    if bind_address.port() == 0 {
        return Ok(None);
    }
    let url = format!("ws://{bind_address}");
    let path = app_server_registry_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let registry = AppServerRegistry {
        url: url.clone(),
        pid: std::process::id(),
        updated_at: now(),
    };
    let json = serde_json::to_string_pretty(&registry)?;
    fs::write(&path, format!("{json}\n")).with_context(|| format!("write {}", path.display()))?;
    Ok(Some(url))
}

pub(crate) fn clear_app_server_registry_if_matches(url: &str) -> Result<()> {
    let path = app_server_registry_path()?;
    let Ok(raw) = fs::read_to_string(&path) else {
        return Ok(());
    };
    let Ok(registry) = serde_json::from_str::<AppServerRegistry>(&raw) else {
        return Ok(());
    };
    if registry.url == url {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

fn read_registered_app_server_url() -> Result<Option<String>> {
    Ok(read_app_server_registry()?.map(|registry| registry.url))
}

fn read_app_server_registry() -> Result<Option<AppServerRegistry>> {
    let path = app_server_registry_path()?;
    let Ok(raw) = fs::read_to_string(&path) else {
        return Ok(None);
    };
    let registry: AppServerRegistry = match serde_json::from_str(&raw) {
        Ok(registry) => registry,
        Err(_) => {
            let _ = fs::remove_file(path);
            return Ok(None);
        }
    };
    let url = registry.url.trim();
    if url.is_empty() {
        return Ok(None);
    }
    Ok(Some(registry))
}

fn remove_app_server_registry() -> Result<()> {
    let path = app_server_registry_path()?;
    let _ = fs::remove_file(path);
    Ok(())
}

fn create_team(root: &Path, args: StartArgs) -> Result<(String, PathBuf)> {
    fs::create_dir_all(root)?;
    let team_id = match args.id {
        Some(id) => sanitize_id(&id),
        None => format!("team-{}", tokyo_now().format("%Y%m%d%H%M%S")),
    };
    let team_dir = root.join(&team_id);
    if team_dir.exists() {
        bail!("team `{team_id}` already exists");
    }

    fs::create_dir_all(team_dir.join("tasks"))?;
    fs::create_dir_all(team_dir.join("mailboxes"))?;
    fs::create_dir_all(team_dir.join("logs"))?;
    fs::create_dir_all(team_dir.join("last_messages"))?;
    fs::create_dir_all(team_dir.join("live_messages"))?;
    fs::create_dir_all(team_dir.join("jobs"))?;
    fs::create_dir_all(team_dir.join("waits"))?;
    write_json_atomic(
        &team_dir.join("ownerships.json"),
        &Vec::<FileOwnership>::new(),
    )?;
    let mut nodes = Vec::<TeamNode>::new();
    for raw_node in args.nodes {
        nodes.push(parse_node_spec(&raw_node, &now())?);
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    write_json_atomic(&team_dir.join("nodes.json"), &nodes)?;

    let now = now();
    let mut members = vec![TeamMember {
        name: "lead".to_string(),
        role: "lead".to_string(),
        status: MemberStatus::Online,
        joined_at: now.clone(),
        thread_id: None,
        workspace_path: None,
        node: Some("local".to_string()),
    }];
    for raw_member in args.members {
        members.push(parse_member(&raw_member, &now)?);
    }

    let config = TeamConfig {
        version: 1,
        id: team_id.clone(),
        goal: args.goal.clone(),
        lead: "lead".to_string(),
        members,
        language: args.language,
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    write_json_atomic(&team_dir.join("config.json"), &config)?;

    let initial_tasks = if args.tasks.is_empty() {
        vec![args.goal]
    } else {
        args.tasks
    };
    for subject in initial_tasks {
        create_task(
            &team_dir,
            TaskAddArgs {
                subject,
                description: String::new(),
                owner: None,
                depends_on: Vec::new(),
            },
        )?;
    }

    append_event(
        &team_dir,
        "team_started",
        serde_json::json!({ "goal": config.goal, "members": config.members }),
    )?;

    Ok((team_id, team_dir))
}

fn ensure_team_prompt_language(
    team_dir: &Path,
    override_language: Option<TeamPromptLanguage>,
) -> Result<TeamPromptLanguage> {
    let mut config = load_config(team_dir)?;
    let language = override_language.or(config.language).unwrap_or_default();
    if config.language != Some(language) {
        config.language = Some(language);
        config.updated_at = now();
        write_json_atomic(&team_dir.join("config.json"), &config)?;
        append_event(
            team_dir,
            "team_prompt_language_updated",
            serde_json::json!({ "language": language.cli_value() }),
        )?;
    }
    Ok(language)
}

fn apply_natural_language_defaults(args: &mut StartArgs) {
    if args.members.is_empty() {
        apply_department_design(args, fallback_department_design(&args.goal));
        return;
    }

    if args.tasks.is_empty() {
        let goal = args.goal.clone();
        args.tasks = args
            .members
            .iter()
            .map(|member| {
                let name = member.split(':').next().unwrap_or(member);
                format!("Department mission for {name}: contribute to {goal}")
            })
            .collect();
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LeadDepartmentDesign {
    #[serde(default)]
    nodes: Vec<LeadNodeDesign>,
    departments: Vec<LeadDepartment>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LeadDepartment {
    name: String,
    role: String,
    mission: String,
    #[serde(default)]
    node: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LeadNodeDesign {
    id: String,
    kind: TeamNodeKind,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    container: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    note: String,
}

fn should_use_lead_department_design(args: &StartArgs) -> bool {
    args.members.is_empty() && args.tasks.is_empty()
}

fn fallback_department_design(goal: &str) -> LeadDepartmentDesign {
    LeadDepartmentDesign {
        nodes: Vec::new(),
        departments: vec![
            LeadDepartment {
                name: "product".to_string(),
                role: "product".to_string(),
                mission: format!(
                    "Clarify the user goal, scope the deliverable, and make product decisions for: {goal}"
                ),
                node: None,
            },
            LeadDepartment {
                name: "engineering".to_string(),
                role: "engineering".to_string(),
                mission: format!(
                    "Implement the primary technical deliverable, using internal subagents or tools when useful, for: {goal}"
                ),
                node: None,
            },
            LeadDepartment {
                name: "quality".to_string(),
                role: "quality".to_string(),
                mission: format!(
                    "Review, test, and identify risks or missing behavior for: {goal}"
                ),
                node: None,
            },
        ],
    }
}

fn apply_department_design(args: &mut StartArgs, design: LeadDepartmentDesign) {
    merge_lead_node_designs(args, &design.nodes);
    let valid_node_ids = args
        .nodes
        .iter()
        .map(|raw| node_spec_id(raw))
        .collect::<HashSet<_>>();
    let mut seen_departments = HashSet::<String>::new();
    let mut departments = design
        .departments
        .into_iter()
        .filter(|department| {
            let name = sanitize_id(&department.name);
            let role = sanitize_role(&department.role);
            if name.is_empty() || name == "lead" || role == "lead" {
                return false;
            }
            if !seen_departments.insert(name) {
                return false;
            }
            let Some(node) = department.node.as_deref().map(sanitize_id) else {
                return true;
            };
            node.is_empty() || node == "local" || valid_node_ids.contains(&node)
        })
        .collect::<Vec<_>>();
    if departments.is_empty() {
        departments = fallback_department_design(&args.goal).departments;
    }
    departments.truncate(6);

    args.members = departments
        .iter()
        .map(|department| {
            let name = sanitize_id(&department.name);
            let role = sanitize_role(&department.role);
            let node = department.node.as_deref().map(sanitize_id).filter(|node| {
                !node.is_empty()
                    && (node == "local"
                        || args
                            .nodes
                            .iter()
                            .any(|raw| node_spec_id(raw) == node.as_str()))
            });
            match node {
                Some(node) if node != "local" => format!("{name}:{role}@{node}"),
                _ => format!("{name}:{role}"),
            }
        })
        .collect();
    args.tasks = departments
        .iter()
        .map(|department| {
            format!(
                "Department mission for {}: {}\n\nOperate as one department-level Codex session. Proactively coordinate with related departments: broadcast your initial plan, ask even small uncertainty questions before committing to a risky choice, report failures with proposed next steps, and hand off artifacts to their consumers. If the mission is broad, research-heavy, implementation-heavy, or review-heavy, actively use available subagent/agent tools, skills, MCP servers, and internal decomposition inside this department instead of doing all work in one main thread or asking the lead to create duplicate peer departments for load balancing.",
                sanitize_id(&department.name),
                department.mission
            )
        })
        .collect();
}

#[allow(clippy::too_many_arguments)]
fn run_lead_department_design(
    codex_exe: &Path,
    cwd: &Path,
    goal: &str,
    placement_candidates: &[LeadNodeDesign],
    language: TeamPromptLanguage,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<LeadDepartmentDesign> {
    let output =
        tempfile::NamedTempFile::new().context("create lead department design temp file")?;
    let output_path = output.path().to_path_buf();
    let prompt = build_lead_department_design_prompt(goal, placement_candidates, language);
    let mut command = Command::new(codex_exe);
    command
        .arg("exec")
        .arg("-C")
        .arg(cwd)
        .arg("-o")
        .arg(&output_path);
    if let Some(model) = model {
        command.arg("--model").arg(model);
    }
    if let Some(profile) = profile {
        command.arg("--profile").arg(profile);
    }
    if let Some(sandbox) = sandbox {
        command.arg("--sandbox").arg(sandbox);
    }
    if dangerously_bypass_approvals_and_sandbox {
        command.arg("--dangerously-bypass-approvals-and-sandbox");
    }
    let status = command
        .arg(prompt)
        .status()
        .context("run lead department design Codex turn")?;
    if !status.success() {
        bail!("lead department design failed with status {status}");
    }
    let raw = fs::read_to_string(&output_path)
        .with_context(|| format!("read {}", output_path.display()))?;
    parse_lead_department_design(&raw)
}

fn build_lead_department_design_prompt(
    goal: &str,
    placement_candidates: &[LeadNodeDesign],
    language: TeamPromptLanguage,
) -> String {
    let candidates = if placement_candidates.is_empty() {
        "(none explicitly provided by CLI/UI advanced options)".to_string()
    } else {
        placement_candidates
            .iter()
            .map(|node| {
                format!(
                    "- id={} kind={:?} host={} container={} cwd={} note={}",
                    sanitize_id(&node.id),
                    node.kind,
                    node.host.as_deref().unwrap_or(""),
                    node.container.as_deref().unwrap_or(""),
                    node.cwd.as_deref().unwrap_or(""),
                    node.note
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    if language.is_ja() {
        return format!(
            r#"あなたは、ユーザーの依頼を直接聞く lead agent です。ユーザーは実質的に社長です。あなたの仕事は、依頼全体を理解し、運用計画を決め、正しい実行場所に部署を作ることです。あなたはオーケストレーターであり、実装作業者でも、単純な作業分配係でもありません。

ユーザーの goal:
{goal}

CLI/UI の advanced option から明示された配置候補だけ:
{candidates}

ユーザーの自然言語 goal を注意深く読み、配置を自分で推論してください:
- ユーザーが SSH host、remote machine、compute server、Docker container、その他 execution site を普通の文章で名指しした場合、それは運用要件として扱ってください。
- local/remote の境界を守ってください。ユーザーが「research は local、build/run は SSH 計算サーバー」と言った場合、research は local、build/run は SSH node の host-side department に分けてください。
- ユーザーに low-level flag、node spec、Docker mount、port、GPU flag、department placement を要求しないでください。それらは lead が決めることです。
- 明示的な CLI/UI 配置候補は advanced option 由来の hint にすぎません。自然言語 goal が authoritative です。
- host/container 名が明確な場合、ユーザーの表記をそのまま使ってください。日本語の助詞や周辺文を host 名に含めないでください。
- Docker node は具体的な既存 container を意味します。「Docker image を build」「container を作成」のような文言だけから Docker/container node を捏造しないでください。まず正しい host に planning/build/container-creation work を割り当ててください。container-internal department は、実 container が存在し node 登録された後に自動追加されます。

小さな部署構成を設計してください:
- 部署は 2 から 5 個にしてください。
- 各部署は、明確な ownership domain を持つ peer Codex session です。
- `lead` 部署は含めないでください。live lead は部署一覧の外に既に存在します。
- workload balancing だけを目的に重複部署を作らないでください。
- 部署の作業が重い場合、その部署が利用可能な subagent/agent tools、skills、MCP servers、または内部分解を使うべきです。
- 部署は、自分の execution site で必要な task tool や library を install できます。環境セットアップだけを理由に peer department を増やさないでください。
- goal が public/open-source model、dataset、package、API、service に依存する場合、research/ops は transitive runtime artifact や model dependency が現在の環境で実際に access 可能か確認してください。未提供の gated credential が必要な新しい選択肢より、少し新規性が低くても end-to-end で動く選択肢を優先してください。
- product、engineering、design、quality、research、docs、ops、security、data などの domain ownership を優先してください。
- department name と role は lowercase ASCII identifier にしてください。
- 配置も department design の一部として決めてください。local department は `"node": "local"` または省略を使ってください。ユーザー要求が到達可能な SSH site を明確に呼ぶ場合は SSH node を使ってください。
- ユーザー goal が到達可能な SSH host または既存 Docker/container execution site を名指しする場合、`nodes` に含めてください。
- SSH/Docker node に department を割り当てるのは、bootstrap に必要な情報が揃っている場合だけです。SSH には `host`、Docker には `container`、SSH Docker には `host` と `container` が必要です。

有効な JSON だけを返してください。Markdown や commentary は不要です:
{{
  "nodes": [
    {{
      "id": "saitou",
      "kind": "ssh",
      "host": "saitou",
      "container": null,
      "cwd": null,
      "note": "ユーザーが要求した remote SSH site。"
    }}
  ],
  "departments": [
    {{
      "name": "product",
      "role": "product",
      "mission": "Scope, requirements, product decisions を担当する。",
      "node": "local"
    }}
  ]
}}
"#,
            goal = goal,
            candidates = candidates,
        );
    }

    format!(
        r#"You are the lead agent directly listening to the user's request. The user is effectively the president/CEO. Your job is to understand the whole request, decide the operating plan, and create departments at the right execution sites. You are an orchestrator, not an implementation worker and not a simple worker balancer.

User goal:
{goal}

Explicit placement candidates from CLI/UI advanced options only:
{candidates}

Read the user goal carefully and infer placement from the natural language yourself:
- If the user names an SSH host, remote machine, compute server, Docker container, or other execution site, treat that as an operational requirement even when it is written in ordinary prose.
- Preserve local/remote boundaries. If the user says research is local but build/run happens on an SSH compute server, keep research local and create host-side departments on that SSH node for build/run work.
- Do not require the user to provide low-level flags, node specs, Docker mount specs, ports, GPU flags, or department placement. Those are lead-owned decisions.
- Explicit CLI/UI placement candidates are only hints from advanced options. The natural-language goal is authoritative.
- Use the user's exact host/container names when they are clearly named. Do not parse Japanese particles or surrounding prose as part of a host name.
- A Docker node means a concrete, already-existing container. Do not invent Docker/container nodes from phrases like "build a Docker image" or "create a Docker container"; assign planning/build/container-creation work to the correct host first. Container-internal departments are added automatically only after the real container exists and has been registered as a node.

Design a small department structure:
- Create 2 to 5 departments.
- Each department is one peer Codex session with a clear ownership domain.
- Do not include a `lead` department. The live lead already exists outside your department list.
- Do not create duplicate departments just to balance workload.
- If a department's work is heavy, that department should use available subagent/agent tools, skills, MCP servers, or its own internal decomposition.
- Departments are allowed to install missing task tools and libraries in their own execution site when that is the best way to complete or verify the work. Do not create extra peer departments just because an environment needs setup.
- If the goal depends on a public external model, dataset, package, or service, research/ops must verify that all required runtime artifacts and transitive model dependencies are actually accessible in the current environment. Prefer a slightly less novel option that can run end-to-end over a newer option that requires unprovided gated credentials.
- Prefer domain ownership such as product, engineering, design, quality, research, docs, ops, security, data, etc.
- Use lowercase ASCII identifiers for department names and roles.
- Decide placement as part of the department design. Use `"node": "local"` or omit `node` for local departments. Use an SSH node when the user's request clearly calls for a reachable SSH site.
- Include nodes in `nodes` when the user goal names a reachable SSH host or an already-existing Docker/container execution site.
- Do not assign a department to an SSH/Docker node unless the node has enough information to bootstrap: SSH needs `host`; Docker needs `container`; SSH Docker needs both `host` and `container`.

Return only valid JSON, no Markdown, no commentary:
{{
  "nodes": [
    {{
      "id": "saitou",
      "kind": "ssh",
      "host": "saitou",
      "container": null,
      "cwd": null,
      "note": "Remote SSH site requested by the user."
    }}
  ],
  "departments": [
    {{
      "name": "product",
      "role": "product",
      "mission": "Own scope, requirements, and product decisions.",
      "node": "local"
    }}
  ]
}}
"#,
        goal = goal,
        candidates = candidates,
    )
}

fn parse_lead_department_design(raw: &str) -> Result<LeadDepartmentDesign> {
    let json = extract_json_object(raw).unwrap_or(raw);
    let mut design: LeadDepartmentDesign =
        serde_json::from_str(json).context("parse lead department design JSON")?;
    design.departments.retain(|department| {
        !department.name.trim().is_empty()
            && !department.role.trim().is_empty()
            && !department.mission.trim().is_empty()
    });
    if design.departments.is_empty() {
        bail!("lead department design did not include any valid departments");
    }
    Ok(design)
}

fn lead_placement_candidates_from_start(args: &StartArgs) -> Result<Vec<LeadNodeDesign>> {
    let mut candidates = Vec::new();
    let now = now();
    for raw in &args.nodes {
        let node = parse_node_spec(raw, &now)?;
        candidates.push(LeadNodeDesign {
            id: node.id,
            kind: node.kind,
            host: node.host,
            container: node.container,
            cwd: node.cwd,
            note: node.note,
        });
    }
    Ok(candidates)
}

fn merge_lead_node_designs(args: &mut StartArgs, nodes: &[LeadNodeDesign]) {
    for node in nodes {
        let Some(spec) = lead_node_design_to_spec(node) else {
            continue;
        };
        let id = node_spec_id(&spec);
        if id.is_empty() || id == "local" {
            continue;
        }
        if !args
            .nodes
            .iter()
            .any(|existing| node_spec_id(existing) == id)
        {
            args.nodes.push(spec);
        }
    }
}

fn merge_lead_node_metadata(team_dir: &Path, designs: &[LeadNodeDesign]) -> Result<()> {
    if designs.is_empty() {
        return Ok(());
    }
    let mut nodes = load_nodes(team_dir)?;
    let mut changed = false;
    for design in designs {
        let id = sanitize_id(&design.id);
        let Some(node) = nodes.iter_mut().find(|node| node.id == id) else {
            continue;
        };
        if node.cwd.is_none() && design.cwd.is_some() {
            node.cwd = design.cwd.clone();
            changed = true;
        }
        if node.note.trim().is_empty() && !design.note.trim().is_empty() {
            node.note = design.note.clone();
            changed = true;
        }
        if changed {
            node.updated_at = now();
        }
    }
    if changed {
        write_nodes(team_dir, &nodes)?;
    }
    Ok(())
}

fn lead_node_design_to_spec(node: &LeadNodeDesign) -> Option<String> {
    let id = sanitize_id(&node.id);
    if id.is_empty() || id == "local" {
        return None;
    }
    match &node.kind {
        TeamNodeKind::Local => None,
        TeamNodeKind::Manual => None,
        TeamNodeKind::Ssh => node
            .host
            .as_deref()
            .filter(|host| !host.trim().is_empty())
            .map(|host| format!("{id}@ssh={}", host.trim())),
        TeamNodeKind::Docker => node
            .container
            .as_deref()
            .filter(|container| !container.trim().is_empty())
            .filter(|container| docker_container_exists(None, container.trim()))
            .map(|container| format!("{id}@docker={}", container.trim())),
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref()?.trim();
            let container = node.container.as_deref()?.trim();
            if host.is_empty() || container.is_empty() {
                None
            } else if !docker_container_exists(Some(host), container) {
                None
            } else {
                Some(format!("{id}@ssh-docker={host}:{container}"))
            }
        }
    }
}

fn docker_container_exists(host: Option<&str>, container: &str) -> bool {
    let command = format!("docker inspect {} >/dev/null 2>&1", shell_quote(container));
    let status = match host {
        Some(host) => Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(host)
            .arg(format!("bash -lc {}", shell_quote(&command)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
        None => Command::new("bash")
            .arg("-lc")
            .arg(command)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    };
    matches!(status, Ok(status) if status.success())
}

fn node_spec_id(raw: &str) -> String {
    let left = raw.split_once('=').map(|(left, _)| left).unwrap_or(raw);
    let id = left.split_once('@').map(|(id, _)| id).unwrap_or(left);
    sanitize_id(id)
}

fn extract_json_object(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&raw[start..=end])
}

fn run_team(root: &Path, mut args: RunArgs) -> Result<()> {
    if args.app_server_url.is_some() {
        bail!("--app-server-url is only supported with --app-server");
    }
    apply_natural_language_defaults(&mut args.start);
    let (team_id, team_dir) = create_team(root, args.start)?;
    println!("Created team `{team_id}`");
    println!("State: {}", team_dir.display());
    write_team_run_pid(&team_dir, std::process::id())?;

    assign_unowned_tasks_round_robin(&team_dir)?;
    let config = load_config(&team_dir)?;
    let tasks = load_tasks(&team_dir)?;
    let workers: Vec<TeamMember> = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .cloned()
        .collect();
    if workers.is_empty() {
        bail!("team `{team_id}` has no worker members; add --member NAME[:ROLE]");
    }

    let cwd = args
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    bind_parent_codex_session_to_team(root, &team_id, &team_dir, &cwd)?;
    let codex_exe = std::env::current_exe().context("resolve current Codex executable")?;

    if args.prepare_only {
        if args.worktree {
            for member in &workers {
                let assigned = tasks
                    .iter()
                    .any(|task| task.owner.as_deref() == Some(member.name.as_str()));
                if assigned {
                    let _ = prepare_member_worktree(&team_dir, &cwd, &team_id, member)?;
                }
            }
        }
        print_status(&team_dir)?;
        return Ok(());
    }

    if args.dry_run {
        print_discussion_dry_run(&team_dir, args.discuss_rounds, &cwd, &codex_exe)?;
        for member in &workers {
            let prompt = build_worker_prompt(&config, &tasks, member);
            println!("--- {} ({}) ---", member.name, member.role);
            println!("{} exec -C {} <prompt>", codex_exe.display(), cwd.display());
            println!("{prompt}");
        }
        return Ok(());
    }

    run_discussion_rounds(
        &team_dir,
        &team_id,
        &cwd,
        &codex_exe,
        args.discuss_rounds,
        args.model.as_deref(),
        args.profile.as_deref(),
        args.sandbox.as_deref(),
        args.dangerously_bypass_approvals_and_sandbox,
    )?;

    let mut children = Vec::new();
    for member in &workers {
        let assigned = tasks
            .iter()
            .any(|task| task.owner.as_deref() == Some(member.name.as_str()));
        if !assigned {
            continue;
        }

        set_member_status(&team_dir, &member.name, MemberStatus::Running)?;
        mark_member_tasks(&team_dir, &member.name, TaskStatus::InProgress)?;

        let worker_cwd = if args.worktree {
            prepare_member_worktree(&team_dir, &cwd, &team_id, member)?
        } else {
            cwd.clone()
        };

        let log_path = team_dir.join("logs").join(format!("{}.log", member.name));
        let last_message_path = team_dir
            .join("last_messages")
            .join(format!("{}.md", member.name));
        let stdout = fs::File::create(&log_path)
            .with_context(|| format!("create {}", log_path.display()))?;
        let stderr = stdout.try_clone()?;
        let prompt = build_worker_prompt(&config, &tasks, member);

        let mut command = Command::new(&codex_exe);
        command
            .arg("exec")
            .arg("-C")
            .arg(&worker_cwd)
            .arg("-o")
            .arg(&last_message_path)
            .env("CODEX_TEAM_ID", &team_id)
            .env("CODEX_TEAM_MEMBER", &member.name)
            .env("CODEX_TEAM_ROLE", &member.role)
            .env("CODEX_TEAM_CLI", &codex_exe)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        if let Some(model) = args.model.as_deref() {
            command.arg("--model").arg(model);
        }
        if let Some(profile) = args.profile.as_deref() {
            command.arg("--profile").arg(profile);
        }
        if let Some(sandbox) = args.sandbox.as_deref() {
            command.arg("--sandbox").arg(sandbox);
        }
        if args.dangerously_bypass_approvals_and_sandbox {
            command.arg("--dangerously-bypass-approvals-and-sandbox");
        }
        command.arg(prompt);

        append_event(
            &team_dir,
            "member_started",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "cwd": worker_cwd,
                "log": log_path,
                "lastMessage": last_message_path,
            }),
        )?;

        let child = command
            .spawn()
            .with_context(|| format!("spawn Codex worker `{}`", member.name))?;
        children.push((member.name.clone(), child));
    }

    if children.is_empty() {
        bail!("no workers had assigned tasks");
    }

    let mut failed = false;
    for (member_name, mut child) in children {
        let status = child
            .wait()
            .with_context(|| format!("wait for Codex worker `{member_name}`"))?;
        if status.success() {
            set_member_status(&team_dir, &member_name, MemberStatus::Completed)?;
            complete_member_tasks_if_active(&team_dir, &member_name)?;
            append_event(
                &team_dir,
                "member_completed",
                serde_json::json!({ "member": member_name, "status": status.code() }),
            )?;
        } else {
            failed = true;
            set_member_status(&team_dir, &member_name, MemberStatus::Failed)?;
            append_event(
                &team_dir,
                "member_failed",
                serde_json::json!({ "member": member_name, "status": status.code() }),
            )?;
        }
    }

    print_status(&team_dir)?;
    if failed {
        bail!("one or more team members failed");
    }
    if !args.no_synthesis {
        run_lead_synthesis(
            &team_dir,
            &team_id,
            &cwd,
            &codex_exe,
            args.model.as_deref(),
            args.profile.as_deref(),
            args.sandbox.as_deref(),
            args.dangerously_bypass_approvals_and_sandbox,
        )?;
    }
    Ok(())
}

async fn run_team_app_server(root: &Path, mut args: RunArgs) -> Result<()> {
    let resume_team = args.resume_team.clone();
    let use_lead_department_design = resume_team.is_none()
        && !args.interactive_lead
        && should_use_lead_department_design(&args.start);
    let cwd = args
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    let codex_exe = std::env::current_exe().context("resolve current Codex executable")?;
    let lead_department_design = if use_lead_department_design
        && !args.dry_run
        && !args.prepare_only
    {
        let design = run_lead_department_design(
            &codex_exe,
            &cwd,
            &args.start.goal,
            &lead_placement_candidates_from_start(&args.start)?,
            args.start.language.unwrap_or_default(),
            args.model.as_deref(),
            args.profile.as_deref(),
            args.sandbox.as_deref(),
            args.dangerously_bypass_approvals_and_sandbox,
        )
        .with_context(|| "lead failed to design departments")?;
        apply_department_design(&mut args.start, design.clone());
        Some(design)
    } else {
        if use_lead_department_design && args.dry_run {
            println!("Dry run: lead would design departments from the goal before team creation.");
        }
        if !args.interactive_lead {
            apply_natural_language_defaults(&mut args.start);
        }
        None
    };

    let requested_language = args.start.language;
    let (team_id, team_dir) = if let Some(team) = resume_team.as_deref() {
        let team_dir = resolve_team_dir(root, Some(team))?;
        let config = load_config(&team_dir)?;
        println!("Reattached app-server runtime to team `{}`", config.id);
        println!("State: {}", team_dir.display());
        append_event(
            &team_dir,
            "app_server_runtime_reattached",
            serde_json::json!({
                "pid": std::process::id(),
            }),
        )?;
        (config.id, team_dir)
    } else {
        let (team_id, team_dir) = create_team(root, args.start)?;
        println!("Created app-server team `{team_id}`");
        println!("State: {}", team_dir.display());
        (team_id, team_dir)
    };
    let prompt_language = ensure_team_prompt_language(&team_dir, requested_language)?;
    write_team_run_pid(&team_dir, std::process::id())?;
    bind_parent_codex_session_to_team(root, &team_id, &team_dir, &cwd)?;
    if let Some(design) = lead_department_design.as_ref() {
        merge_lead_node_metadata(&team_dir, &design.nodes)?;
        append_event(
            &team_dir,
            "lead_department_design",
            serde_json::json!({ "nodes": &design.nodes, "departments": &design.departments }),
        )?;
    }

    assign_unowned_tasks_round_robin(&team_dir)?;
    ensure_container_node_departments(&team_dir)?;
    let mut config = load_config(&team_dir)?;
    let tasks = load_tasks(&team_dir)?;
    let workers = team_workers(&config);
    if workers.is_empty() && !args.interactive_lead {
        bail!("team `{team_id}` has no worker members; add --member NAME[:ROLE]");
    }
    if args.prepare_only {
        if args.worktree {
            for member in &workers {
                let assigned = tasks
                    .iter()
                    .any(|task| task.owner.as_deref() == Some(member.name.as_str()));
                if assigned {
                    let _ = prepare_member_worktree(&team_dir, &cwd, &team_id, member)?;
                }
            }
        }
        print_status(&team_dir)?;
        return Ok(());
    }

    if args.dry_run {
        println!("App-server mode dry run.");
        println!(
            "{} app-server --listen ws://127.0.0.1:<port>",
            codex_exe.display()
        );
        print_discussion_dry_run(&team_dir, args.discuss_rounds, &cwd, &codex_exe)?;
        if let Some(lead_member) = config.members.iter().find(|member| member.role == "lead") {
            let prompt = build_app_server_lead_prompt(
                &config,
                &tasks,
                lead_member,
                &codex_exe,
                prompt_language,
            );
            println!(
                "--- app-server lead thread: {} ({}) ---",
                lead_member.name, lead_member.role
            );
            println!("{prompt}");
        }
        for member in &workers {
            let mut dry_nodes = load_nodes(&team_dir)?;
            ensure_local_node(&mut dry_nodes);
            let prompt = build_app_server_worker_prompt(
                &config,
                &tasks,
                member,
                &codex_exe,
                &dry_nodes,
                prompt_language,
            );
            println!("--- app-server turn: {} ({}) ---", member.name, member.role);
            println!("{prompt}");
        }
        return Ok(());
    }

    if args.discuss_rounds > 0 {
        run_discussion_rounds(
            &team_dir,
            &team_id,
            &cwd,
            &codex_exe,
            args.discuss_rounds,
            args.model.as_deref(),
            args.profile.as_deref(),
            args.sandbox.as_deref(),
            args.dangerously_bypass_approvals_and_sandbox,
        )?;
    }

    let relay = TeamRelayServer::spawn(team_dir.clone())?;
    append_event(
        &team_dir,
        "team_relay_started",
        serde_json::json!({
            "url": relay.local_url(),
        }),
    )?;

    let registered_app_server_url = if args.app_server_url.is_none() && !args.no_app_server_registry
    {
        read_registered_app_server_url()?
    } else {
        None
    };
    let requested_app_server_url = args
        .app_server_url
        .clone()
        .or_else(|| registered_app_server_url.clone());
    let using_registered_app_server =
        args.app_server_url.is_none() && registered_app_server_url.is_some();

    let mut app_server = None;
    let mut node_clients = HashMap::<String, TeamAppServerNodeClient>::new();
    let mut node_processes = Vec::<NodeAppServerProcess>::new();
    let app_server_url;
    let app_server_external;
    let app_server_source;
    if let Some(url) = requested_app_server_url {
        let connect_attempts = if using_registered_app_server { 2 } else { 50 };
        match connect_team_app_server_with_attempts(&url, connect_attempts).await {
            Ok(connected_client) => {
                app_server_url = url;
                app_server_external = true;
                app_server_source = if using_registered_app_server {
                    "registry"
                } else {
                    "explicit"
                };
                node_clients.insert(
                    "local".to_string(),
                    TeamAppServerNodeClient {
                        client: connected_client,
                        request_counter: 1,
                    },
                );
            }
            Err(err) if using_registered_app_server => {
                append_event(
                    &team_dir,
                    "app_server_registry_unavailable",
                    serde_json::json!({
                        "url": url,
                        "error": err.to_string(),
                    }),
                )?;
                let _ = clear_app_server_registry_if_matches(&url);
                eprintln!(
                    "Registered app-server `{url}` is unavailable; starting a private app-server."
                );
                let spawned =
                    BackgroundTeamAppServer::spawn(&codex_exe, &team_dir, args.profile.as_deref())?;
                app_server_url = spawned.url.clone();
                app_server = Some(spawned);
                app_server_external = false;
                app_server_source = "spawned";
                let connected_client = connect_team_app_server(&app_server_url).await?;
                node_clients.insert(
                    "local".to_string(),
                    TeamAppServerNodeClient {
                        client: connected_client,
                        request_counter: 1,
                    },
                );
            }
            Err(err) => return Err(err),
        }
    } else {
        let spawned =
            BackgroundTeamAppServer::spawn(&codex_exe, &team_dir, args.profile.as_deref())?;
        app_server_url = spawned.url.clone();
        app_server = Some(spawned);
        app_server_external = false;
        app_server_source = "spawned";
        let connected_client = connect_team_app_server(&app_server_url).await?;
        node_clients.insert(
            "local".to_string(),
            TeamAppServerNodeClient {
                client: connected_client,
                request_counter: 1,
            },
        );
    }
    append_event(
        &team_dir,
        "app_server_connected",
        serde_json::json!({
            "url": app_server_url,
            "external": app_server_external,
            "source": app_server_source,
        }),
    )?;
    set_node_connection(
        &team_dir,
        "local",
        TeamNodeStatus::Online,
        Some(app_server_url.clone()),
    )?;
    let mut nodes = load_nodes(&team_dir)?;
    ensure_local_node(&mut nodes);
    let mut needed_node_ids = vec!["local".to_string()];
    for member in &workers {
        let assigned = tasks.iter().any(|task| {
            task.owner.as_deref() == Some(member.name.as_str())
                && task_status_can_start_turn(task.status)
        });
        if assigned {
            let node_id = member_node_id(member);
            if !needed_node_ids.contains(&node_id) {
                needed_node_ids.push(node_id);
            }
        }
    }
    for node_id in needed_node_ids {
        if node_id == "local" || node_clients.contains_key(&node_id) {
            continue;
        }
        let node = nodes
            .iter()
            .find(|node| node.id == node_id)
            .cloned()
            .with_context(|| format!("node `{node_id}` is not registered"))?;
        let (url, child) = resolve_or_spawn_node_app_server(&team_dir, &node, relay.port())?;
        if let Some(child) = child {
            node_processes.push(child);
        }
        let connected_client = connect_team_app_server(&url)
            .await
            .with_context(|| format!("connect app-server node `{node_id}` at `{url}`"))?;
        append_event(
            &team_dir,
            "app_server_node_connected",
            serde_json::json!({
                "node": node_id,
                "kind": node.kind,
                "url": url,
                "source": "node",
            }),
        )?;
        set_node_connection(
            &team_dir,
            &node_id,
            TeamNodeStatus::Online,
            Some(url.clone()),
        )?;
        node_clients.insert(
            node_id,
            TeamAppServerNodeClient {
                client: connected_client,
                request_counter: 1,
            },
        );
    }
    let mut active = HashMap::<String, AppServerMemberRun>::new();
    let mut thread_to_member = HashMap::<String, String>::new();
    let mut side_replies = HashMap::<String, AppServerSideReply>::new();
    let mut assistant_buffers = HashMap::<String, String>::new();

    let sandbox = app_server_sandbox(
        args.sandbox.as_deref(),
        args.dangerously_bypass_approvals_and_sandbox,
    )?;
    let approval_policy = if args.dangerously_bypass_approvals_and_sandbox {
        Some(AskForApproval::Never)
    } else {
        None
    };

    let lead_member = config
        .members
        .iter()
        .find(|member| member.role == "lead")
        .cloned()
        .context("team has no lead member")?;
    let lead_node_id = "local".to_string();
    let lead_client = node_clients
        .get_mut(&lead_node_id)
        .context("local app-server client missing for lead")?;
    let lead_thread: ThreadStartResponse = lead_client
        .client
        .request_typed(ClientRequest::ThreadStart {
            request_id: next_request_id(&mut lead_client.request_counter),
            params: ThreadStartParams {
                model: args.model.clone(),
                cwd: Some(cwd.display().to_string()),
                sandbox,
                approval_policy,
                ephemeral: Some(false),
                ..ThreadStartParams::default()
            },
        })
        .await
        .map_err(|err| anyhow!(err))?;
    set_member_thread(&team_dir, &lead_member.name, &lead_thread.thread.id)?;
    set_member_workspace(&team_dir, &lead_member.name, &cwd)?;
    thread_to_member.insert(
        thread_key(&lead_node_id, &lead_thread.thread.id),
        lead_member.name.clone(),
    );
    assistant_buffers.insert(lead_member.name.clone(), String::new());
    active.insert(
        lead_member.name.clone(),
        AppServerMemberRun {
            member: lead_member.clone(),
            node_id: lead_node_id.clone(),
            cwd: cwd.clone(),
            thread_id: lead_thread.thread.id.clone(),
            turn_id: String::new(),
            completed: true,
            failed: false,
            standby_after_turn: false,
            team_message_scan_offset: 0,
            last_activity_at: Instant::now(),
            last_activity_kind: "thread_started".to_string(),
            last_stale_notice_at: None,
            retry_not_before: recent_usage_limit_retry_not_before(&team_dir, &lead_member.name)?,
            side_context_ids: Vec::new(),
        },
    );
    println!("Started lead thread={}", lead_thread.thread.id);
    append_event(
        &team_dir,
        "app_server_lead_thread_started",
        serde_json::json!({
            "member": lead_member.name,
            "thread": lead_thread.thread.id,
            "cwd": cwd,
        }),
    )?;

    let mut started_workers = 0usize;
    for member in &workers {
        let assigned = tasks.iter().any(|task| {
            task.owner.as_deref() == Some(member.name.as_str())
                && task_status_can_start_turn(task.status)
        });
        if !assigned {
            continue;
        }
        if let Some(remaining) = recent_usage_limit_retry_remaining(&team_dir, &member.name)? {
            append_event(
                &team_dir,
                "app_server_member_start_deferred",
                serde_json::json!({
                    "member": member.name,
                    "node": member_node_id(member),
                    "reason": "recent app-server/model usage-limit cooldown",
                    "retry_after_sec": remaining.as_secs(),
                }),
            )?;
            set_member_status(&team_dir, &member.name, MemberStatus::Standby)?;
            continue;
        }

        set_member_status(&team_dir, &member.name, MemberStatus::Running)?;
        mark_member_tasks(&team_dir, &member.name, TaskStatus::InProgress)?;

        let node_id = member_node_id(member);
        if node_id != "local" && args.worktree {
            bail!(
                "--worktree is not supported for remote node member `{}` yet",
                member.name
            );
        }
        let worker_cwd = if node_id != "local" {
            app_server_member_cwd(&node_id, &nodes, &cwd)
        } else if args.worktree {
            prepare_member_worktree(&team_dir, &cwd, &team_id, member)?
        } else {
            cwd.clone()
        };

        let node_client = node_clients
            .get_mut(&node_id)
            .with_context(|| format!("app-server client missing for node `{node_id}`"))?;
        let thread: ThreadStartResponse = node_client
            .client
            .request_typed(ClientRequest::ThreadStart {
                request_id: next_request_id(&mut node_client.request_counter),
                params: ThreadStartParams {
                    model: args.model.clone(),
                    cwd: Some(worker_cwd.display().to_string()),
                    sandbox,
                    approval_policy,
                    ephemeral: Some(false),
                    ..ThreadStartParams::default()
                },
            })
            .await
            .map_err(|err| anyhow!(err))?;
        set_member_thread(&team_dir, &member.name, &thread.thread.id)?;
        set_member_workspace(&team_dir, &member.name, &worker_cwd)?;

        let prompt = build_app_server_worker_prompt(
            &config,
            &tasks,
            member,
            &codex_exe,
            &nodes,
            prompt_language,
        );
        let turn: TurnStartResponse = node_client
            .client
            .request_typed(ClientRequest::TurnStart {
                request_id: next_request_id(&mut node_client.request_counter),
                params: TurnStartParams {
                    thread_id: thread.thread.id.clone(),
                    input: vec![text_input(prompt)],
                    cwd: Some(worker_cwd.clone()),
                    model: args.model.clone(),
                    approval_policy,
                    sandbox_policy: if args.dangerously_bypass_approvals_and_sandbox {
                        Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess)
                    } else {
                        None
                    },
                    ..TurnStartParams::default()
                },
            })
            .await
            .map_err(|err| anyhow!(err))?;

        println!(
            "Started {} ({}) thread={} turn={}",
            member.name, member.role, thread.thread.id, turn.turn.id
        );
        append_event(
            &team_dir,
            "app_server_member_started",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "thread": thread.thread.id,
                "turn": turn.turn.id,
                "node": node_id,
                "cwd": worker_cwd,
            }),
        )?;

        thread_to_member.insert(thread_key(&node_id, &thread.thread.id), member.name.clone());
        assistant_buffers.insert(member.name.clone(), String::new());
        active.insert(
            member.name.clone(),
            AppServerMemberRun {
                member: member.clone(),
                node_id,
                cwd: worker_cwd,
                thread_id: thread.thread.id,
                turn_id: turn.turn.id,
                completed: false,
                failed: false,
                standby_after_turn: false,
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "turn_started".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );
        started_workers += 1;
    }

    if started_workers == 0 {
        if args.interactive_lead {
            append_event(
                &team_dir,
                "app_server_interactive_lead_only",
                serde_json::json!({
                    "message": "lead-only interactive team runtime; departments will be added after user instruction"
                }),
            )?;
        } else if args.resume_team.is_some() {
            append_event(
                &team_dir,
                "app_server_runtime_no_startable_workers",
                serde_json::json!({
                    "message": "reattached runtime has no startable worker tasks; keep-alive will wait for messages, dependency changes, or dynamic members"
                }),
            )?;
        } else {
            bail!("no workers had assigned tasks");
        }
    }

    let lead_prompt =
        build_app_server_lead_prompt(&config, &tasks, &lead_member, &codex_exe, prompt_language);
    start_app_server_member_turn(
        &mut node_clients,
        &team_dir,
        &mut active,
        &lead_member.name,
        lead_prompt,
        &cwd,
        args.model.clone(),
        approval_policy,
        args.dangerously_bypass_approvals_and_sandbox,
        "app_server_lead_started",
    )
    .await?;
    normalize_stale_running_members_without_active_turns(&team_dir, &active)?;
    config = load_config(&team_dir)?;

    let mut mailbox_counts = current_mailbox_counts(&team_dir, &config.members, &tasks)?;
    let poll_interval = Duration::from_millis(args.reactive_poll_ms.max(250));
    let node_sync_interval = if args.node_sync_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.node_sync_interval_sec.max(30)))
    };
    let idle_outreach_interval = if args.idle_outreach_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.idle_outreach_interval_sec.max(60)))
    };
    let task_watchdog_interval = if args.task_watchdog_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.task_watchdog_interval_sec.max(30)))
    };
    let lead_tick_interval = if args.lead_tick_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.lead_tick_interval_sec.max(60)))
    };
    let idle_wakeup_interval = if args.idle_wakeup_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(args.idle_wakeup_interval_sec.max(60)))
    };
    let department_heartbeat_interval = if args.department_heartbeat_interval_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(
            args.department_heartbeat_interval_sec.max(60),
        ))
    };
    let stale_active_turn_timeout = if args.stale_active_turn_timeout_sec == 0 {
        None
    } else {
        Some(Duration::from_secs(
            args.stale_active_turn_timeout_sec.max(120),
        ))
    };
    let mut last_node_asset_sync = HashMap::<String, Instant>::new();
    let mut last_idle_outreach = Instant::now();
    let mut idle_outreach_cursor = 0_usize;
    let mut last_task_watchdog = Instant::now();
    let mut task_watchdog_warned = HashSet::<String>::new();
    let mut last_lead_tick = lead_tick_interval
        .map(|interval| Instant::now() - interval)
        .unwrap_or_else(Instant::now);
    let mut member_idle_since = HashMap::<String, Instant>::new();
    let mut member_last_idle_wakeup = HashMap::<String, Instant>::new();
    let mut last_idle_wakeup_batch = Instant::now();
    if let Some(wakeup_interval) = idle_wakeup_interval {
        seed_department_idle_wakeup_cooldowns(
            &team_dir,
            &mut member_last_idle_wakeup,
            &mut last_idle_wakeup_batch,
            wakeup_interval,
        )?;
    }
    let mut idle_wakeup_cursor = 0_usize;
    let mut department_heartbeats = HashMap::<String, Instant>::new();
    let mut last_stale_active_turn_check = Instant::now();
    let mut last_job_refresh = Instant::now() - Duration::from_secs(15);
    let mut contract_input_sync_attempts = HashSet::<String>::new();
    let mut keep_alive_idle_reported = false;
    #[cfg(unix)]
    let hangup_task = {
        let mut hangup_signal =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
                .context("install team runtime SIGHUP handler")?;
        let team_dir = team_dir.clone();
        tokio::spawn(async move {
            while hangup_signal.recv().await.is_some() {
                let _ = append_event(
                    &team_dir,
                    "app_server_keep_alive_hangup_ignored",
                    serde_json::json!({ "reason": "sighup" }),
                );
            }
        })
    };

    loop {
        let has_running_turn = active.values().any(|run| !run.completed);
        let has_unstarted_member = has_unstarted_app_server_members(&team_dir, &active)?;
        let team_is_idle = !has_running_turn && !has_unstarted_member;
        if team_is_idle {
            if !args.no_keep_alive {
                if !keep_alive_idle_reported {
                    println!(
                        "Team {} is idle and staying alive. Send messages or member changes; press Ctrl-C to stop.",
                        team_id
                    );
                    append_event(
                        &team_dir,
                        "app_server_keep_alive_idle",
                        serde_json::json!({ "message": "team idle; waiting for messages or dynamic member changes" }),
                    )?;
                    keep_alive_idle_reported = true;
                }
            } else {
                break;
            }
        } else {
            keep_alive_idle_reported = false;
        }
        tokio::select! {
            _ = tokio::signal::ctrl_c(), if !args.no_keep_alive => {
                append_event(
                    &team_dir,
                    "app_server_keep_alive_stopped",
                    serde_json::json!({ "reason": "ctrl_c" }),
                )?;
                break;
            }
            _ = tokio::time::sleep(poll_interval) => {
                drain_app_server_events(
                    &mut node_clients,
                    &team_dir,
                    &mut active,
                    &mut side_replies,
                    &thread_to_member,
                    &mut assistant_buffers,
                ).await?;
                nodes = load_nodes(&team_dir)?;
                ensure_local_node(&mut nodes);
                ensure_container_node_departments(&team_dir)?;
                nodes = load_nodes(&team_dir)?;
                ensure_local_node(&mut nodes);
                if let Some(sync_interval) = node_sync_interval {
                    if let Err(err) = maybe_sync_remote_node_assets(
                        &team_dir,
                        &nodes,
                        &node_clients,
                        &mut last_node_asset_sync,
                        sync_interval,
                    ) {
                        record_runtime_loop_error(&team_dir, "node_asset_sync", err)?;
                    }
                }
                sync_removed_app_server_nodes(
                    &mut node_clients,
                    &mut node_processes,
                    &nodes,
                    &team_dir,
                    &active,
                ).await?;
                if last_job_refresh.elapsed() >= Duration::from_secs(15) {
                    last_job_refresh = Instant::now();
                    if let Err(err) = refresh_running_team_jobs(&team_dir) {
                        record_runtime_loop_error(&team_dir, "refresh_running_team_jobs", err)?;
                    }
                }
                if let Err(err) = auto_promote_dependency_waits(&team_dir) {
                    record_runtime_loop_error(&team_dir, "auto_promote_dependency_waits", err)?;
                }
                if let Err(err) = assign_unowned_tasks_round_robin(&team_dir) {
                    record_runtime_loop_error(&team_dir, "assign_unowned_tasks_round_robin", err)?;
                }
                if let Err(err) = maybe_sync_contract_declared_inputs(
                    &team_dir,
                    &config,
                    &nodes,
                    &mut contract_input_sync_attempts,
                ) {
                    record_runtime_loop_error(&team_dir, "contract_declared_input_sync", err)?;
                }
                sync_dynamic_app_server_members(
                    &mut node_clients,
                    &nodes,
                    &team_dir,
                    &mut config,
                    &mut active,
                    &mut thread_to_member,
                    &mut assistant_buffers,
                    &mut mailbox_counts,
                    &mut node_processes,
                    &cwd,
                    args.model.clone(),
                    sandbox,
                    approval_policy,
                    args.dangerously_bypass_approvals_and_sandbox,
                    &codex_exe,
                    relay.port(),
                    prompt_language,
                ).await?;
                steer_new_team_messages(
                    &mut node_clients,
                    &team_dir,
                    &config.members,
                    &mut active,
                    &mut side_replies,
                    &mut mailbox_counts,
                    &cwd,
                    args.model.clone(),
                    approval_policy,
                    args.dangerously_bypass_approvals_and_sandbox,
                    &codex_exe,
                    args.side_channel_replies,
                    prompt_language,
                ).await?;
                if let Some(outreach_interval) = idle_outreach_interval {
                    if let Err(err) = maybe_send_idle_department_outreach(
                        &team_dir,
                        &config,
                        &active,
                        &mut last_idle_outreach,
                        &mut idle_outreach_cursor,
                        outreach_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "idle_department_outreach", err)?;
                    }
                }
                if let Some(watchdog_interval) = task_watchdog_interval {
                    if let Err(err) = maybe_warn_unattended_tasks(
                        &team_dir,
                        &config,
                        &active,
                        &mut last_task_watchdog,
                        &mut task_watchdog_warned,
                        watchdog_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "task_watchdog", err)?;
                    }
                }
                config = load_config(&team_dir)?;
                if let Some(tick_interval) = lead_tick_interval {
                    if let Err(err) = maybe_send_lead_autonomy_tick(
                        &team_dir,
                        &config,
                        &active,
                        &mut last_lead_tick,
                        tick_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "lead_autonomy_tick", err)?;
                    }
                }
                if let Some(wakeup_interval) = idle_wakeup_interval {
                    if let Err(err) = maybe_send_department_idle_wakeups(
                        &team_dir,
                        &config,
                        &active,
                        &mut member_idle_since,
                        &mut member_last_idle_wakeup,
                        &mut last_idle_wakeup_batch,
                        &mut idle_wakeup_cursor,
                        wakeup_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "department_idle_wakeup", err)?;
                    }
                }
                if let Some(heartbeat_interval) = department_heartbeat_interval {
                    if let Err(err) = maybe_send_department_heartbeats(
                        &team_dir,
                        &config,
                        &active,
                        &mut department_heartbeats,
                        &member_last_idle_wakeup,
                        heartbeat_interval,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "department_heartbeat", err)?;
                    }
                }
                if let Some(stale_timeout) = stale_active_turn_timeout {
                    if let Err(err) = maybe_warn_stale_active_turns(
                        &team_dir,
                        &config,
                        &mut active,
                        &mut last_stale_active_turn_check,
                        Duration::from_secs(30),
                        stale_timeout,
                        prompt_language,
                    ) {
                        record_runtime_loop_error(&team_dir, "stale_active_turn_check", err)?;
                    }
                }
            }
        }
    }

    if !args.no_synthesis
        && let Some(lead_run) = active.get(&lead_member.name)
        && lead_run.completed
    {
        let prompt = build_app_server_lead_final_prompt(&config, &team_dir, prompt_language)?;
        start_app_server_member_turn(
            &mut node_clients,
            &team_dir,
            &mut active,
            &lead_member.name,
            prompt,
            &cwd,
            args.model.clone(),
            approval_policy,
            args.dangerously_bypass_approvals_and_sandbox,
            "app_server_lead_synthesis_started",
        )
        .await?;
        while active
            .get(&lead_member.name)
            .map(|run| !run.completed)
            .unwrap_or(false)
        {
            drain_app_server_events(
                &mut node_clients,
                &team_dir,
                &mut active,
                &mut side_replies,
                &thread_to_member,
                &mut assistant_buffers,
            )
            .await?;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    let failed = active
        .values()
        .any(|run| run.member.role != "lead" && run.failed);
    for run in active.values() {
        let last_message_path = team_dir
            .join("last_messages")
            .join(format!("{}.md", run.member.name));
        let text = assistant_buffers
            .get(&run.member.name)
            .cloned()
            .unwrap_or_default();
        write_text_atomic(&last_message_path, &text)?;
    }
    if let Some(summary) = assistant_buffers.get(&lead_member.name)
        && !summary.trim().is_empty()
    {
        write_text_atomic(&team_dir.join("summary.md"), summary)?;
    }

    print_status(&team_dir)?;
    for (_node_id, node_client) in node_clients {
        node_client
            .client
            .shutdown()
            .await
            .context("shutdown app-server client")?;
    }
    for process in node_processes {
        process.stop();
    }
    #[cfg(unix)]
    hangup_task.abort();
    drop(app_server);

    if failed {
        bail!("one or more app-server team members failed");
    }
    Ok(())
}

struct BackgroundTeamAppServer {
    process: Child,
    url: String,
}

impl BackgroundTeamAppServer {
    fn spawn(codex_exe: &Path, team_dir: &Path, profile: Option<&str>) -> Result<Self> {
        let listener =
            TcpListener::bind("127.0.0.1:0").context("reserve local app-server websocket port")?;
        let addr = listener.local_addr()?;
        drop(listener);

        let url = format!("ws://{addr}");
        let log_path = team_dir.join("logs").join("app-server.log");
        let stderr = fs::File::create(&log_path)
            .with_context(|| format!("create {}", log_path.display()))?;
        let mut command = Command::new(codex_exe);
        if let Some(profile) = profile {
            command.arg("--profile").arg(profile);
        }
        let process = command
            .arg("app-server")
            .arg("--listen")
            .arg(&url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(stderr))
            .spawn()
            .with_context(|| format!("spawn `{}` app-server", codex_exe.display()))?;
        Ok(Self { process, url })
    }
}

impl Drop for BackgroundTeamAppServer {
    fn drop(&mut self) {
        if matches!(self.process.try_wait(), Ok(Some(_))) {
            return;
        }
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

struct TeamRelayServer {
    addr: std::net::SocketAddr,
}

impl TeamRelayServer {
    fn spawn(team_dir: PathBuf) -> Result<Self> {
        let listener = TcpListener::bind("0.0.0.0:0").context("bind team relay server")?;
        let addr = listener.local_addr()?;
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                if let Err(err) = handle_team_relay_request(&team_dir, &mut stream) {
                    let _ = write_http_response(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain; charset=utf-8",
                        &format!("{err:#}\n"),
                    );
                }
            }
        });
        Ok(Self { addr })
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }

    fn local_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.addr.port())
    }
}

fn handle_team_relay_request(team_dir: &Path, stream: &mut std::net::TcpStream) -> Result<()> {
    let request = read_http_request(stream)?;
    validate_relay_team(team_dir, &request)?;
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/status") => {
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                &format_status_text(team_dir)?,
            )?;
        }
        ("GET", "/inbox") => {
            let member = request
                .query
                .get("member")
                .filter(|value| !value.trim().is_empty())
                .context("missing member")?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                &format_inbox_text(team_dir, member)?,
            )?;
        }
        ("POST", "/message") => {
            let form = parse_form(&request.body);
            let from = form_value(&form, "from")?;
            let to = form_value(&form, "to")?;
            let message = form_value(&form, "message")?;
            let recipients = send_team_message_to_dir(team_dir, &from, &to, &message)?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                &format!("Message sent to {}\n", recipients.join(",")),
            )?;
        }
        ("GET", "/task/list") => {
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                &format_tasks_text(team_dir)?,
            )?;
        }
        ("POST", "/task/set") => {
            let form = parse_form(&request.body);
            let id = form_value(&form, "id")?;
            let status = form
                .get("status")
                .filter(|value| !value.trim().is_empty())
                .map(|value| parse_task_status(value))
                .transpose()?;
            update_task(
                team_dir,
                TaskSetArgs {
                    id: id.clone(),
                    status,
                    owner: form
                        .get("owner")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    clear_owner: form
                        .get("clear_owner")
                        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes")),
                    depends_on: Vec::new(),
                    clear_depends: false,
                    result: form
                        .get("result")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                },
            )?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "Task updated\n",
            )?;
        }
        ("GET", "/ownership/list") => {
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                &format_ownerships_text(team_dir)?,
            )?;
        }
        ("POST", "/ownership/claim") => {
            let form = parse_form(&request.body);
            claim_ownership(
                team_dir,
                OwnershipClaimArgs {
                    path: form_value(&form, "path")?,
                    owner: Some(form_value(&form, "owner")?),
                    note: form.get("note").cloned().unwrap_or_default(),
                    force: form
                        .get("force")
                        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes")),
                },
            )?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "Ownership claimed\n",
            )?;
        }
        ("POST", "/ownership/release") => {
            let form = parse_form(&request.body);
            release_ownership(
                team_dir,
                OwnershipReleaseArgs {
                    path: form_value(&form, "path")?,
                    owner: Some(form_value(&form, "owner")?),
                    force: form
                        .get("force")
                        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes")),
                },
            )?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "Ownership released\n",
            )?;
        }
        ("GET", "/job/list") => {
            let list_args = JobListArgs {
                owner: request
                    .query
                    .get("owner")
                    .filter(|value| !value.trim().is_empty())
                    .cloned(),
                task: request
                    .query
                    .get("task")
                    .filter(|value| !value.trim().is_empty())
                    .cloned(),
                status: request
                    .query
                    .get("status")
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| parse_job_status(value))
                    .transpose()?,
                limit: request
                    .query
                    .get("limit")
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| value.parse::<usize>())
                    .transpose()
                    .context("invalid job list limit")?,
            };
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                &format_jobs_text_filtered(team_dir, &list_args)?,
            )?;
        }
        ("POST", "/job/start") => {
            let form = parse_form(&request.body);
            let command = form_value(&form, "command")?;
            start_team_job(
                team_dir,
                JobStartArgs {
                    id: form
                        .get("id")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    node: form
                        .get("node")
                        .filter(|value| !value.trim().is_empty())
                        .cloned()
                        .unwrap_or_else(|| "local".to_string()),
                    cwd: form
                        .get("cwd")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    note: form.get("note").cloned().unwrap_or_default(),
                    owner: form
                        .get("owner")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    task: form
                        .get("task")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    command: vec!["bash".to_string(), "-lc".to_string(), command],
                },
            )?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "Job started\n",
            )?;
        }
        ("GET", "/job/status") => {
            let id = request.query.get("id").context("missing id")?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                &format_job_status_text(team_dir, id)?,
            )?;
        }
        ("GET", "/job/logs") => {
            let id = request.query.get("id").context("missing id")?;
            let tail = request
                .query
                .get("tail")
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.parse::<usize>())
                .transpose()
                .context("invalid tail")?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                &job_logs_text(team_dir, id, tail)?,
            )?;
        }
        ("POST", "/job/stop") => {
            let form = parse_form(&request.body);
            let id = form_value(&form, "id")?;
            stop_team_job(team_dir, &id)?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "Job stopped\n",
            )?;
        }
        ("POST", "/job/artifact") => {
            let form = parse_form(&request.body);
            add_job_artifact(
                team_dir,
                JobArtifactArgs {
                    id: form_value(&form, "id")?,
                    path: form_value(&form, "path")?,
                    note: form.get("note").cloned().unwrap_or_default(),
                },
            )?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "Artifact registered\n",
            )?;
        }
        _ => {
            write_http_response(
                stream,
                "404 Not Found",
                "text/plain; charset=utf-8",
                "not found\n",
            )?;
        }
    }
    Ok(())
}

fn validate_relay_team(team_dir: &Path, request: &HttpRequest) -> Result<()> {
    let Some(requested_team) = request.query.get("team").filter(|value| !value.is_empty()) else {
        return Ok(());
    };
    let config = load_config(team_dir)?;
    if requested_team != &config.id {
        bail!(
            "relay is bound to team `{}`, not `{}`",
            config.id,
            requested_team
        );
    }
    Ok(())
}

fn send_team_message_to_dir(
    team_dir: &Path,
    from: &str,
    to: &str,
    message: &str,
) -> Result<Vec<String>> {
    let mut config = load_config(team_dir)?;
    let from = sanitize_id(from);
    if from != "system" && from != "user" {
        ensure_member_exists(&config, &from)?;
    }
    let recipients = resolve_message_recipients(&config, &from, to)?;
    for recipient in &recipients {
        let msg = MailMessage {
            from: from.clone(),
            to: recipient.clone(),
            message: message.to_string(),
            timestamp: now(),
            read: false,
        };
        append_jsonl(&mailbox_path(team_dir, &msg.to), &msg)?;
    }
    append_event(
        team_dir,
        "message_sent",
        serde_json::json!({
            "from": from,
            "to": recipients,
            "message": message,
            "source": "team_relay",
        }),
    )?;
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)?;
    Ok(recipients)
}

fn format_status_text(team_dir: &Path) -> Result<String> {
    auto_promote_dependency_waits(team_dir)?;
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let mut out = String::new();
    out.push_str(&format!("Team: {}\n", config.id));
    out.push_str(&format!("Goal: {}\n", config.goal));
    out.push_str(&format!("Members: {}\n", config.members.len()));
    for member in &config.members {
        let task_status = member_task_status_summary(&tasks, &member.name);
        let mail = mailbox_unread_counts(team_dir, &member.name)?;
        out.push_str(&format!(
            "  {} ({}) session={:?} tasks={} node={} unread={} direct={}\n",
            member.name,
            member.role,
            member.status,
            task_status,
            member.node.as_deref().unwrap_or("local"),
            mail.unread,
            mail.direct_unread
        ));
    }
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    out.push_str(&format!("Nodes: {}\n", nodes.len()));
    for node in nodes {
        out.push_str(&format!("{}\n", format_node_status_line(&node)));
    }
    let cooldowns = format_usage_limit_cooldowns(team_dir, &config)?;
    if !cooldowns.is_empty() {
        out.push_str(&cooldowns);
    }
    let waits = load_waits(team_dir)?;
    let open_waits = waits.iter().filter(|wait| wait.status.is_open()).count();
    if open_waits > 0 {
        out.push_str(&format!(
            "Waits: {open_waits} open, {} total\n",
            waits.len()
        ));
        for wait in waits.iter().filter(|wait| wait.status.is_open()).take(12) {
            out.push_str(&format_wait_line(wait));
            out.push('\n');
        }
    }
    out.push_str(&format!("Tasks: {}\n", format_task_status_counts(&tasks)));
    out.push_str(&format_tasks_text(team_dir)?);
    let ownerships = format_ownerships_text(team_dir)?;
    if !ownerships.trim().is_empty() && !ownerships.starts_with("No ownership") {
        out.push_str(&format!("Ownerships:\n{ownerships}"));
    }
    Ok(out)
}

fn format_tasks_text(team_dir: &Path) -> Result<String> {
    auto_promote_dependency_waits(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    if tasks.is_empty() {
        return Ok("No tasks found.\n".to_string());
    }
    let mut out = String::new();
    for task in tasks {
        out.push_str(&format_task_line(&task));
        out.push('\n');
    }
    Ok(out)
}

fn format_task_status_counts(tasks: &[TeamTask]) -> String {
    let completed = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Completed))
        .count();
    let open = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Waiting
                    | TaskStatus::Ready
                    | TaskStatus::InProgress
                    | TaskStatus::Blocked
                    | TaskStatus::Review
            )
        })
        .count();
    let cancelled = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Cancelled))
        .count();
    let failed = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Failed))
        .count();
    format!(
        "{completed} completed, {open} open, {cancelled} cancelled, {failed} failed, {} total",
        tasks.len()
    )
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MailboxUnreadCounts {
    unread: usize,
    direct_unread: usize,
}

fn mailbox_unread_counts(team_dir: &Path, member_name: &str) -> Result<MailboxUnreadCounts> {
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, member_name))?;
    let unread = messages.iter().filter(|message| !message.read).count();
    let direct_unread = messages
        .iter()
        .filter(|message| !message.read && message.from != "system")
        .count();
    Ok(MailboxUnreadCounts {
        unread,
        direct_unread,
    })
}

fn format_node_status_line(node: &TeamNode) -> String {
    let (age, stale) = format_node_last_seen_age(&node.updated_at);
    format!(
        "  {} {:?} {:?} url={} last_seen={} age={}{}",
        node.id,
        node.kind,
        node.status,
        node.url.as_deref().unwrap_or(""),
        node.updated_at,
        age,
        if stale { " stale" } else { "" }
    )
}

fn format_usage_limit_cooldowns(team_dir: &Path, config: &TeamConfig) -> Result<String> {
    let mut lines = Vec::new();
    for member in &config.members {
        if let Some(remaining) = recent_usage_limit_retry_remaining(team_dir, &member.name)? {
            lines.push(format!(
                "  {} usage_limit retry_in={}",
                member.name,
                format_compact_duration(remaining.as_secs())
            ));
        }
    }
    if lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(format!("Cooldowns:\n{}\n", lines.join("\n")))
    }
}

fn format_node_last_seen_age(updated_at: &str) -> (String, bool) {
    const STALE_AFTER_SEC: i64 = 10 * 60;
    let Ok(timestamp) = DateTime::parse_from_rfc3339(updated_at) else {
        return ("unknown".to_string(), true);
    };
    let age = Utc::now()
        .signed_duration_since(timestamp.with_timezone(&Utc))
        .num_seconds()
        .max(0);
    (format_compact_duration(age as u64), age >= STALE_AFTER_SEC)
}

fn format_compact_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m{secs}s")
    } else {
        format!("{secs}s")
    }
}

fn member_task_status_summary(tasks: &[TeamTask], member_name: &str) -> String {
    let mut owned = tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(member_name))
        .filter(|task| {
            !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
        })
        .collect::<Vec<_>>();
    owned.sort_by(|a, b| a.id.cmp(&b.id));
    if owned.is_empty() {
        return "no_open_tasks".to_string();
    }
    owned
        .into_iter()
        .take(4)
        .map(|task| format!("#{}:{}", task.id, task.status))
        .collect::<Vec<_>>()
        .join(",")
}

fn format_task_line(task: &TeamTask) -> String {
    let owner = task
        .owner
        .as_ref()
        .map(|owner| format!("@{owner}"))
        .unwrap_or_default();
    let deps = if task.depends_on.is_empty() {
        String::new()
    } else {
        format!(" deps:{}", task.depends_on.join(","))
    };
    format!(
        "  {:>3} {:<11} {:<16} {}{}",
        task.id, task.status, owner, task.subject, deps
    )
}

fn format_wait_line(wait: &TeamWait) -> String {
    format!(
        "  {:<8} {:<9} owner={:<12} task={:<6} {}",
        wait.id,
        wait.status,
        wait.owner.as_deref().unwrap_or("-"),
        wait.task_id.as_deref().unwrap_or("-"),
        wait.title
    )
}

fn format_inbox_text(team_dir: &Path, member: &str) -> Result<String> {
    let config = load_config(team_dir)?;
    let member = sanitize_id(member);
    ensure_member_exists(&config, &member)?;
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, &member))?;
    if messages.is_empty() {
        return Ok(format!("Inbox for `{member}` is empty.\n"));
    }
    let mut out = String::new();
    for msg in messages {
        out.push_str(&format!(
            "[{}] {} -> {}: {}\n",
            msg.timestamp, msg.from, msg.to, msg.message
        ));
    }
    Ok(out)
}

fn format_ownerships_text(team_dir: &Path) -> Result<String> {
    let ownerships = load_ownerships(team_dir)?;
    if ownerships.is_empty() {
        return Ok("No ownership claims.\n".to_string());
    }
    let mut out = String::new();
    for ownership in ownerships {
        let note = if ownership.note.trim().is_empty() {
            String::new()
        } else {
            format!("  {}", ownership.note)
        };
        out.push_str(&format!(
            "  {:<24} {}{}\n",
            format!("@{}", ownership.owner),
            ownership.path,
            note
        ));
    }
    Ok(out)
}

fn parse_task_status(value: &str) -> Result<TaskStatus> {
    match value.trim().replace('-', "_").as_str() {
        "pending" => Ok(TaskStatus::Pending),
        "waiting" | "wait" => Ok(TaskStatus::Waiting),
        "ready" => Ok(TaskStatus::Ready),
        "in_progress" => Ok(TaskStatus::InProgress),
        "blocked" => Ok(TaskStatus::Blocked),
        "review" => Ok(TaskStatus::Review),
        "completed" => Ok(TaskStatus::Completed),
        "failed" => Ok(TaskStatus::Failed),
        "cancelled" | "canceled" => Ok(TaskStatus::Cancelled),
        other => bail!("unsupported task status `{other}`"),
    }
}

fn parse_job_status(value: &str) -> Result<TeamJobStatus> {
    match value.trim().replace('-', "_").as_str() {
        "running" => Ok(TeamJobStatus::Running),
        "completed" => Ok(TeamJobStatus::Completed),
        "failed" => Ok(TeamJobStatus::Failed),
        "stopped" => Ok(TeamJobStatus::Stopped),
        "unknown" => Ok(TeamJobStatus::Unknown),
        other => bail!("unsupported job status `{other}`"),
    }
}

fn parse_wait_status(value: &str) -> Result<TeamWaitStatus> {
    match value.trim().replace('-', "_").as_str() {
        "waiting" | "wait" => Ok(TeamWaitStatus::Waiting),
        "running" => Ok(TeamWaitStatus::Running),
        "polling" | "pending" => Ok(TeamWaitStatus::Polling),
        "blocked" => Ok(TeamWaitStatus::Blocked),
        "completed" | "complete" | "done" => Ok(TeamWaitStatus::Completed),
        "failed" | "failure" => Ok(TeamWaitStatus::Failed),
        "cancelled" | "canceled" => Ok(TeamWaitStatus::Cancelled),
        other => bail!("unsupported wait status `{other}`"),
    }
}

fn resolve_or_spawn_node_app_server(
    team_dir: &Path,
    node: &TeamNode,
    relay_port: u16,
) -> Result<(String, Option<NodeAppServerProcess>)> {
    if let Some(url) = node.url.clone()
        && app_server_readyz(&url)
    {
        if matches!(node.kind, TeamNodeKind::Local | TeamNodeKind::Manual) {
            return Ok((url, None));
        }
        append_event(
            team_dir,
            "app_server_node_rebootstrap_for_current_relay",
            serde_json::json!({
                "node": node.id,
                "old_url": url,
                "reason": "non-local codex-team helper needs a fresh reverse relay for this runtime",
            }),
        )?;
    }
    if matches!(
        node.kind,
        TeamNodeKind::Ssh | TeamNodeKind::Docker | TeamNodeKind::SshDocker
    ) {
        match sync_codex_assets_to_node(node, "$HOME/.codex", false) {
            Ok(paths) => {
                let _ = append_event(
                    team_dir,
                    "node_assets_synced_before_app_server",
                    serde_json::json!({ "node": node.id, "paths": paths }),
                );
            }
            Err(err) => {
                let _ = append_event(
                    team_dir,
                    "node_assets_sync_failed_before_app_server",
                    serde_json::json!({ "node": node.id, "error": err.to_string() }),
                );
            }
        }
    }
    let mut direct_auth_failures = 0_usize;
    let spawn_result = loop {
        let spawn_result = match &node.kind {
            TeamNodeKind::Ssh => spawn_ssh_node_app_server(team_dir, node, relay_port),
            TeamNodeKind::Manual | TeamNodeKind::Local => {
                let url = node
                    .url
                    .clone()
                    .with_context(|| format!("node `{}` has no app-server URL", node.id))?;
                Ok((url, None))
            }
            TeamNodeKind::Docker => spawn_docker_node_app_server(team_dir, node, relay_port),
            TeamNodeKind::SshDocker => spawn_ssh_docker_node_app_server(team_dir, node, relay_port),
        };
        match spawn_result {
            Err(err)
                if matches!(
                    node.kind,
                    TeamNodeKind::Ssh | TeamNodeKind::Docker | TeamNodeKind::SshDocker
                ) && node_auth_log_indicates_direct_auth_failure(team_dir, node)
                    && direct_auth_failures + 1 < MAX_DIRECT_DEVICE_AUTH_ATTEMPTS =>
            {
                direct_auth_failures += 1;
                append_event(
                    team_dir,
                    "node_direct_device_auth_retry",
                    serde_json::json!({
                        "node": node.id,
                        "attempt": direct_auth_failures,
                        "max_attempts": MAX_DIRECT_DEVICE_AUTH_ATTEMPTS,
                        "reason": err.to_string(),
                    }),
                )?;
                continue;
            }
            other => break other,
        }
    };
    match spawn_result {
        Ok(result) => Ok(result),
        Err(first_err)
            if matches!(
                node.kind,
                TeamNodeKind::Ssh | TeamNodeKind::Docker | TeamNodeKind::SshDocker
            ) && node_auth_log_indicates_auth(team_dir, node) =>
        {
            append_event(
                team_dir,
                "node_auth_copy_fallback_started",
                serde_json::json!({
                    "node": node.id,
                    "direct_device_auth_failures": if node_auth_log_indicates_direct_auth_failure(team_dir, node) {
                        direct_auth_failures + 1
                    } else {
                        direct_auth_failures
                    },
                    "max_direct_device_auth_attempts": MAX_DIRECT_DEVICE_AUTH_ATTEMPTS,
                    "reason": first_err.to_string(),
                }),
            )?;
            match sync_codex_assets_to_node(node, "$HOME/.codex", true) {
                Ok(paths) => {
                    append_event(
                        team_dir,
                        "node_auth_copy_fallback_synced",
                        serde_json::json!({ "node": node.id, "paths": paths }),
                    )?;
                    match &node.kind {
                        TeamNodeKind::Ssh => spawn_ssh_node_app_server(team_dir, node, relay_port),
                        TeamNodeKind::Docker => {
                            spawn_docker_node_app_server(team_dir, node, relay_port)
                        }
                        TeamNodeKind::SshDocker => {
                            spawn_ssh_docker_node_app_server(team_dir, node, relay_port)
                        }
                        TeamNodeKind::Manual | TeamNodeKind::Local => unreachable!(),
                    }
                }
                Err(sync_err) => Err(first_err).with_context(|| {
                    format!(
                        "auth copy fallback for node `{}` also failed: {sync_err}",
                        node.id
                    )
                }),
            }
        }
        Err(err) => Err(err),
    }
}

fn node_auth_log_indicates_auth(team_dir: &Path, node: &TeamNode) -> bool {
    let path = team_dir.join("logs").join(format!("node-{}.log", node.id));
    let Ok(log) = fs::read_to_string(path) else {
        return false;
    };
    let lower = log.to_ascii_lowercase();
    lower.contains("auth.openai.com")
        || lower.contains("device")
        || lower.contains("login --device-auth")
        || lower.contains("sign in")
        || lower.contains("not authenticated")
}

fn node_auth_log_indicates_direct_auth_failure(team_dir: &Path, node: &TeamNode) -> bool {
    let path = team_dir.join("logs").join(format!("node-{}.log", node.id));
    fs::read_to_string(path)
        .map(|log| log.contains("[codex-team direct-device-auth ok=false"))
        .unwrap_or(false)
}

fn spawn_ssh_node_app_server(
    team_dir: &Path,
    node: &TeamNode,
    relay_port: u16,
) -> Result<(String, Option<NodeAppServerProcess>)> {
    let host = node
        .host
        .as_deref()
        .with_context(|| format!("ssh node `{}` needs --host", node.id))?;
    let listener = TcpListener::bind("127.0.0.1:0").context("reserve ssh app-server port")?;
    let local_addr = listener.local_addr()?;
    drop(listener);
    let local_port = local_addr.port();
    let remote_port = local_port;
    let remote_relay_port = reserve_ephemeral_port().context("reserve ssh relay port")?;
    let relay_url = format!("http://127.0.0.1:{remote_relay_port}");
    let config = load_config(team_dir)?;
    let url = format!("ws://127.0.0.1:{local_port}");
    let log_path = team_dir.join("logs").join(format!("node-{}.log", node.id));
    let stderr =
        fs::File::create(&log_path).with_context(|| format!("create {}", log_path.display()))?;
    let stdout = stderr.try_clone()?;
    let remote_script = remote_app_server_bootstrap_script(
        &config.id,
        &relay_url,
        &format!("ws://127.0.0.1:{remote_port}"),
    );
    let child = Command::new("ssh")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-L")
        .arg(format!("{local_port}:127.0.0.1:{remote_port}"))
        .arg("-R")
        .arg(format!("{remote_relay_port}:127.0.0.1:{relay_port}"))
        .arg(host)
        .arg(format!("bash -lc {}", shell_quote(&remote_script)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| format!("spawn ssh app-server node `{}` on `{host}`", node.id))?;
    let mut auth_attempted = false;
    for _ in 0..300 {
        if app_server_readyz(&url) {
            return Ok((
                url,
                Some(NodeAppServerProcess {
                    node_id: node.id.clone(),
                    child,
                    cleanup: Some(NodeCleanup::Ssh {
                        host: host.to_string(),
                        remote_port,
                    }),
                }),
            ));
        }
        if try_authorize_codex_device_from_log(team_dir, &node.id, &log_path, &mut auth_attempted)?
        {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "ssh app-server node `{}` direct device auth failed; see {}",
                node.id,
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();
    bail!(
        "ssh app-server node `{}` did not become ready at {}; see {}",
        node.id,
        url,
        log_path.display()
    )
}

fn spawn_docker_node_app_server(
    team_dir: &Path,
    node: &TeamNode,
    relay_port: u16,
) -> Result<(String, Option<NodeAppServerProcess>)> {
    let container = node
        .container
        .as_deref()
        .with_context(|| format!("docker node `{}` needs --container", node.id))?;
    let listener = TcpListener::bind("127.0.0.1:0").context("reserve docker app-server port")?;
    let local_port = listener.local_addr()?.port();
    drop(listener);
    let remote_port = local_port;
    let container_ip = docker_inspect_value(
        None,
        container,
        "{{range.NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
    )?;
    if container_ip.trim().is_empty() {
        bail!("docker node `{}` has no reachable container IP", node.id);
    }
    let gateway = docker_inspect_value(
        None,
        container,
        "{{range.NetworkSettings.Networks}}{{.Gateway}}{{end}}",
    )?;
    let relay_url = format!("http://{}:{relay_port}", gateway.trim());
    let config = load_config(team_dir)?;
    let url = format!("ws://{}:{remote_port}", container_ip.trim());
    let log_path = team_dir.join("logs").join(format!("node-{}.log", node.id));
    let stderr =
        fs::File::create(&log_path).with_context(|| format!("create {}", log_path.display()))?;
    let stdout = stderr.try_clone()?;
    let remote_script = remote_app_server_bootstrap_script(
        &config.id,
        &relay_url,
        &format!("ws://0.0.0.0:{remote_port}"),
    );
    let child = Command::new("docker")
        .arg("exec")
        .arg("-i")
        .arg(container)
        .arg("bash")
        .arg("-lc")
        .arg(remote_script)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| {
            format!(
                "spawn docker app-server node `{}` in `{container}`",
                node.id
            )
        })?;
    let mut auth_attempted = false;
    for _ in 0..300 {
        if app_server_readyz(&url) {
            return Ok((
                url,
                Some(NodeAppServerProcess {
                    node_id: node.id.clone(),
                    child,
                    cleanup: Some(NodeCleanup::Docker {
                        container: container.to_string(),
                        remote_port,
                    }),
                }),
            ));
        }
        if try_authorize_codex_device_from_log(team_dir, &node.id, &log_path, &mut auth_attempted)?
        {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "docker app-server node `{}` direct device auth failed; see {}",
                node.id,
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();
    bail!(
        "docker app-server node `{}` did not become ready at {}; see {}",
        node.id,
        url,
        log_path.display()
    )
}

fn spawn_ssh_docker_node_app_server(
    team_dir: &Path,
    node: &TeamNode,
    relay_port: u16,
) -> Result<(String, Option<NodeAppServerProcess>)> {
    let host = node
        .host
        .as_deref()
        .with_context(|| format!("ssh-docker node `{}` needs --host", node.id))?;
    let container = node
        .container
        .as_deref()
        .with_context(|| format!("ssh-docker node `{}` needs --container", node.id))?;
    let listener = TcpListener::bind("127.0.0.1:0").context("reserve ssh docker port")?;
    let local_port = listener.local_addr()?.port();
    drop(listener);
    let remote_port = local_port;
    let remote_relay_port = reserve_ephemeral_port().context("reserve ssh docker relay port")?;
    let container_ip = docker_inspect_value(
        Some(host),
        container,
        "{{range.NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
    )?;
    let network_mode = docker_inspect_value(Some(host), container, "{{.HostConfig.NetworkMode}}")?;
    let gateway = docker_inspect_value(
        Some(host),
        container,
        "{{range.NetworkSettings.Networks}}{{.Gateway}}{{end}}",
    )?;
    let target_host = if container_ip.trim().is_empty() && network_mode.trim() == "host" {
        "127.0.0.1".to_string()
    } else if container_ip.trim().is_empty() {
        bail!(
            "ssh-docker node `{}` has no reachable container IP",
            node.id
        )
    } else {
        container_ip.trim().to_string()
    };
    let relay_url = if network_mode.trim() == "host" {
        format!("http://127.0.0.1:{remote_relay_port}")
    } else {
        let gateway = gateway.trim();
        if gateway.is_empty() {
            bail!("ssh-docker node `{}` has no docker gateway", node.id);
        }
        format!("http://{gateway}:{remote_relay_port}")
    };
    let config = load_config(team_dir)?;
    let url = format!("ws://127.0.0.1:{local_port}");
    let log_path = team_dir.join("logs").join(format!("node-{}.log", node.id));
    let stderr =
        fs::File::create(&log_path).with_context(|| format!("create {}", log_path.display()))?;
    let stdout = stderr.try_clone()?;
    let remote_script = remote_app_server_bootstrap_script(
        &config.id,
        &relay_url,
        &format!("ws://0.0.0.0:{remote_port}"),
    );
    let remote_command = ssh_docker_remote_command(
        container,
        &remote_script,
        remote_relay_port,
        if network_mode.trim() == "host" {
            None
        } else {
            Some(gateway.trim())
        },
    );
    let child = Command::new("ssh")
        .arg("-o")
        .arg("ExitOnForwardFailure=yes")
        .arg("-L")
        .arg(format!("{local_port}:{target_host}:{remote_port}"))
        .arg("-R")
        .arg(format!("{remote_relay_port}:127.0.0.1:{relay_port}"))
        .arg(host)
        .arg(format!("bash -lc {}", shell_quote(&remote_command)))
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .with_context(|| {
            format!(
                "spawn ssh-docker app-server node `{}` on `{host}` container `{container}`",
                node.id
            )
        })?;
    let mut auth_attempted = false;
    for _ in 0..300 {
        if app_server_readyz(&url) {
            return Ok((
                url,
                Some(NodeAppServerProcess {
                    node_id: node.id.clone(),
                    child,
                    cleanup: Some(NodeCleanup::SshDocker {
                        host: host.to_string(),
                        container: container.to_string(),
                        remote_port,
                    }),
                }),
            ));
        }
        if try_authorize_codex_device_from_log(team_dir, &node.id, &log_path, &mut auth_attempted)?
        {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "ssh-docker app-server node `{}` direct device auth failed; see {}",
                node.id,
                log_path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();
    bail!(
        "ssh-docker app-server node `{}` did not become ready at {}; see {}",
        node.id,
        url,
        log_path.display()
    )
}

fn remote_app_server_bootstrap_script(team_id: &str, relay_url: &str, listen_url: &str) -> String {
    format!(
        r#"set -euo pipefail
install_prefix=""
if command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
  install_prefix="sudo -n"
elif [ "$(id -u)" = "0" ]; then
  install_prefix=""
fi
if ! command -v curl >/dev/null 2>&1 || ! command -v tar >/dev/null 2>&1 || ! command -v bash >/dev/null 2>&1 || ! command -v git >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
  if [ -n "$install_prefix" ] || [ "$(id -u)" = "0" ]; then
    if command -v apt-get >/dev/null 2>&1; then
      $install_prefix apt-get update -y
      $install_prefix apt-get install -y curl tar ca-certificates bash git python3 procps findutils coreutils
    elif command -v apk >/dev/null 2>&1; then
      $install_prefix apk add --no-cache curl tar ca-certificates bash git python3 procps findutils coreutils
    elif command -v dnf >/dev/null 2>&1; then
      $install_prefix dnf install -y curl tar ca-certificates bash git python3 procps-ng findutils coreutils
    elif command -v yum >/dev/null 2>&1; then
      $install_prefix yum install -y curl tar ca-certificates bash git python3 procps-ng findutils coreutils
    fi
  fi
fi
if [ -z "${{HOME:-}}" ]; then
  export HOME=/root
fi
codex_version_ok() {{
  candidate="$1"
  [ -x "$candidate" ] || return 1
  version="$("$candidate" --version 2>/dev/null | awk '{{print $2}}' | tail -n 1)"
  [ -n "$version" ] || return 1
  [ "$(printf '%s\n%s\n' "0.130.0" "$version" | sort -V | head -n 1)" = "0.130.0" ]
}}
CODEX_BIN=""
for candidate in "$(command -v codex 2>/dev/null || true)" "$HOME/.codex/bin/codex" "$HOME/.local/bin/codex" "$HOME/bin/codex"; do
  if [ -n "$candidate" ] && codex_version_ok "$candidate"; then
    CODEX_BIN="$candidate"
    break
  fi
done
if [ -z "$CODEX_BIN" ]; then
  mkdir -p "$HOME/bin"
  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64) artifact="codex-x86_64-unknown-linux-musl" ;;
    aarch64|arm64) artifact="codex-aarch64-unknown-linux-musl" ;;
    *) echo "CODEX_TEAM_BOOTSTRAP_UNSUPPORTED_ARCH: $arch" >&2; exit 127 ;;
  esac
  curl -fsSL "https://github.com/openai/codex/releases/latest/download/${{artifact}}.tar.gz" -o "$tmpdir/codex.tgz"
  tar -xzf "$tmpdir/codex.tgz" -C "$tmpdir"
  install -m 0755 "$tmpdir/$artifact" "$HOME/bin/codex"
  CODEX_BIN="$HOME/bin/codex"
fi
mkdir -p "$HOME/bin"
helper_real="$HOME/bin/.codex-team-real"
curl -fsSL {helper_url} -o "$helper_real"
chmod 0755 "$helper_real"
cat > "$HOME/bin/codex-team" <<CODEX_TEAM_WRAPPER
#!/usr/bin/env bash
set -euo pipefail
export CODEX_TEAM_ID=\${{CODEX_TEAM_ID:-{team_id}}}
export CODEX_TEAM_RELAY_URL=\${{CODEX_TEAM_RELAY_URL:-{relay_url}}}
if [ -z "\${{CODEX_TEAM_MEMBER:-}}" ]; then
  export CODEX_TEAM_MEMBER=lead
fi
script_dir="\$(CDPATH= cd -- "\$(dirname -- "\$0")" && pwd)"
helper_real="\${{CODEX_TEAM_HELPER_REAL:-\$script_dir/.codex-team-real}}"
if command -v timeout >/dev/null 2>&1; then
  exec timeout "\${{CODEX_TEAM_HELPER_TIMEOUT:-30s}}" "\$helper_real" "\$@"
fi
exec "\$helper_real" "\$@"
CODEX_TEAM_WRAPPER
chmod 0755 "$HOME/bin/codex-team"
if [ "$(id -u)" = "0" ] && [ -d /usr/local/bin ]; then
  install -m 0755 "$helper_real" /usr/local/bin/.codex-team-real || true
  install -m 0755 "$HOME/bin/codex-team" /usr/local/bin/codex-team || true
elif command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then
  sudo -n install -m 0755 "$helper_real" /usr/local/bin/.codex-team-real || true
  sudo -n install -m 0755 "$HOME/bin/codex-team" /usr/local/bin/codex-team || true
fi
cd "$HOME"
export PATH="$HOME/bin:/usr/local/bin:/root/bin:$PATH"
export CODEX_TEAM_ID={team_id}
export CODEX_TEAM_RELAY_URL={relay_url}
if [ ! -s "$HOME/.codex/auth.json" ]; then
  "$CODEX_BIN" login --device-auth
fi
"$CODEX_BIN" app-server --listen {listen_url} &
child="$!"
trap 'kill "$child" 2>/dev/null || true; wait "$child" 2>/dev/null || true' EXIT HUP INT TERM
wait "$child"
"#,
        helper_url = shell_quote(CODEX_TEAM_HELPER_URL),
        team_id = shell_quote(team_id),
        relay_url = shell_quote(relay_url),
        listen_url = listen_url,
    )
}

fn ssh_docker_remote_command(
    container: &str,
    container_script: &str,
    relay_port: u16,
    gateway_bind: Option<&str>,
) -> String {
    let mut command = String::from("set -euo pipefail\n");
    if let Some(bind_addr) = gateway_bind.filter(|value| !value.trim().is_empty()) {
        command.push_str(&format!(
            r#"fwd_pid=""
if command -v python3 >/dev/null 2>&1; then
  CODEX_TEAM_DOCKER_RELAY_BIND={bind_addr} CODEX_TEAM_RELAY_PORT={relay_port} python3 -c {python_code} &
  fwd_pid="$!"
fi
cleanup() {{
  if [ -n "$fwd_pid" ]; then
    kill "$fwd_pid" 2>/dev/null || true
    wait "$fwd_pid" 2>/dev/null || true
  fi
}}
trap cleanup EXIT HUP INT TERM
"#,
            bind_addr = shell_quote(bind_addr),
            relay_port = relay_port,
            python_code = shell_quote(SSH_DOCKER_RELAY_FORWARDER_PY),
        ));
    }
    command.push_str(&format!(
        "docker exec -i {} bash -lc {}\n",
        shell_quote(container),
        shell_quote(container_script)
    ));
    command
}

const SSH_DOCKER_RELAY_FORWARDER_PY: &str = r#"
import os, socket, threading
bind = os.environ["CODEX_TEAM_DOCKER_RELAY_BIND"]
port = int(os.environ["CODEX_TEAM_RELAY_PORT"])
def pump(src, dst):
    try:
        while True:
            data = src.recv(65536)
            if not data:
                break
            dst.sendall(data)
    except OSError:
        pass
    finally:
        try:
            src.close()
        except OSError:
            pass
        try:
            dst.close()
        except OSError:
            pass
server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind((bind, port))
server.listen(64)
while True:
    client, _ = server.accept()
    upstream = socket.create_connection(("127.0.0.1", port))
    threading.Thread(target=pump, args=(client, upstream), daemon=True).start()
    threading.Thread(target=pump, args=(upstream, client), daemon=True).start()
"#;

fn docker_inspect_value(host: Option<&str>, container: &str, template: &str) -> Result<String> {
    let command = format!(
        "docker inspect -f {} {}",
        shell_quote(template),
        shell_quote(container)
    );
    let output = match host {
        Some(host) => Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(host)
            .arg(command)
            .output()
            .with_context(|| format!("inspect docker container `{container}` on `{host}`"))?,
        None => Command::new("sh")
            .arg("-lc")
            .arg(command)
            .output()
            .with_context(|| format!("inspect docker container `{container}`"))?,
    };
    if !output.status.success() {
        bail!(
            "docker inspect failed for `{container}`: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn try_authorize_codex_device_from_log(
    team_dir: &Path,
    node_id: &str,
    log_path: &Path,
    attempted: &mut bool,
) -> Result<bool> {
    if *attempted || !log_path.exists() {
        return Ok(false);
    }
    let log = fs::read_to_string(log_path).unwrap_or_default();
    let Some((url, code)) = parse_codex_device_auth_from_log(&log)? else {
        return Ok(false);
    };
    *attempted = true;
    match authorize_codex_device_with_auth_browser(&url, &code) {
        Ok(auth_log) => {
            append_text(
                log_path,
                &format!(
                    "\n[codex-team direct-device-auth ok=true url={} code=***]\n{}\n",
                    url,
                    auth_log.join("\n")
                ),
            )?;
            append_event(
                team_dir,
                "node_direct_device_auth_completed",
                serde_json::json!({
                    "node": node_id,
                    "url": url,
                    "log": log_path.display().to_string(),
                }),
            )?;
        }
        Err(err) => {
            append_text(
                log_path,
                &format!(
                    "\n[codex-team direct-device-auth ok=false url={} code=***]\n{err:#}\n",
                    url
                ),
            )?;
            return Ok(false);
        }
    }
    Ok(false)
}

fn run_auth_browser(codex_home: &Path, cli: AuthBrowserCli) -> Result<()> {
    match cli.subcommand {
        AuthBrowserSubcommand::Login(args) => {
            let profile = auth_browser_profile_dir(codex_home, args.profile.as_deref());
            open_auth_browser_login_window(
                codex_home,
                &profile,
                args.display.as_deref(),
                &args.url,
            )?;
            println!("Opened Codex Teams auth browser.");
            println!("Profile: {}", profile.display());
            println!("URL: {}", args.url);
            println!();
            println!(
                "Log in to OpenAI/ChatGPT in that browser once. Future remote device-auth prompts can then be completed automatically."
            );
            Ok(())
        }
        AuthBrowserSubcommand::Status(args) => {
            let profile = auth_browser_profile_dir(codex_home, args.profile.as_deref());
            println!("Codex Teams auth browser");
            println!("Profile: {}", profile.display());
            println!("Profile exists: {}", profile.exists());
            match find_auth_browser_binary() {
                Some(binary) => println!("Browser: {binary}"),
                None => println!("Browser: not found"),
            }
            match auth_browser_display(args.display.as_deref()) {
                Ok(display) => println!("Display: {display}"),
                Err(err) => println!("Display: unavailable ({err})"),
            }
            match read_auth_browser_endpoint(codex_home) {
                Some(endpoint) => {
                    println!("Saved endpoint: {endpoint}");
                    match cdp_http_from_ws(&endpoint).and_then(|http| {
                        cdp_version(&http)?;
                        Ok(http)
                    }) {
                        Ok(http) => println!("CDP: active ({http})"),
                        Err(err) => println!("CDP: stale or unavailable ({err})"),
                    }
                }
                None => println!("Saved endpoint: none"),
            }
            if auth_browser_profile_is_running(&profile) {
                println!("Profile process: running");
            } else {
                println!("Profile process: not running");
            }
            if command_exists("node") {
                println!("Node.js: available");
            } else {
                println!("Node.js: not found");
            }
            Ok(())
        }
        AuthBrowserSubcommand::Authorize(args) => {
            let code = normalize_codex_device_code(&args.code)?;
            let log = authorize_codex_device_with_auth_browser_config(
                &args.url,
                &code,
                args.profile.as_deref(),
                args.display.as_deref(),
            )?;
            println!("Codex device auth completed.");
            for line in log {
                println!("{line}");
            }
            Ok(())
        }
    }
}

struct AuthBrowserSession {
    ws_url: String,
    http_url: String,
    log_path: PathBuf,
    profile_dir: PathBuf,
}

fn auth_browser_root(codex_home: &Path) -> PathBuf {
    codex_home.join("team-auth-browser")
}

fn auth_browser_profile_dir(codex_home: &Path, override_profile: Option<&Path>) -> PathBuf {
    override_profile
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_auth_browser_profile_dir(codex_home))
}

fn default_auth_browser_profile_dir(codex_home: &Path) -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let snap_chromium_common = home.join("snap/chromium/common");
        if snap_chromium_common.is_dir() {
            return snap_chromium_common.join("codex-team-auth-browser/chromium-profile");
        }

        if let Some(xdg_data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from)
            && !xdg_data_home.as_os_str().is_empty()
        {
            return xdg_data_home.join("codex/team-auth-browser/chromium-profile");
        }

        return home.join(".local/share/codex/team-auth-browser/chromium-profile");
    }

    auth_browser_root(codex_home).join("chromium-profile")
}

fn auth_browser_endpoint_path(codex_home: &Path) -> PathBuf {
    auth_browser_root(codex_home).join("endpoint.env")
}

fn auth_browser_log_path(codex_home: &Path) -> PathBuf {
    auth_browser_root(codex_home).join("chromium.log")
}

fn find_auth_browser_binary() -> Option<String> {
    for candidate in [
        "chromium-browser",
        "chromium",
        "google-chrome",
        "google-chrome-stable",
    ] {
        if command_exists(candidate) {
            return Some(candidate.to_string());
        }
    }
    None
}

fn auth_browser_display(override_display: Option<&str>) -> Result<String> {
    if let Some(display) = override_display.filter(|value| !value.trim().is_empty()) {
        return Ok(display.to_string());
    }
    if let Ok(display) = std::env::var("DISPLAY")
        && !display.trim().is_empty()
    {
        return Ok(display);
    }
    let status = Command::new("sh")
        .arg("-lc")
        .arg("DISPLAY=:1 xwininfo -root >/dev/null 2>&1")
        .status();
    if status.map(|status| status.success()).unwrap_or(false) {
        return Ok(":1".to_string());
    }
    bail!("DISPLAY is not set and DISPLAY=:1 is not available")
}

fn read_auth_browser_endpoint(codex_home: &Path) -> Option<String> {
    let path = auth_browser_endpoint_path(codex_home);
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if let Some(ws) = line.strip_prefix("ws=") {
            return Some(ws.trim().to_string());
        }
        if line.starts_with("ws://") {
            return Some(line.to_string());
        }
    }
    None
}

fn open_auth_browser_login_window(
    codex_home: &Path,
    profile_dir: &Path,
    override_display: Option<&str>,
    url: &str,
) -> Result<()> {
    fs::create_dir_all(auth_browser_root(codex_home))?;
    fs::create_dir_all(profile_dir)?;
    let _ = fs::remove_file(auth_browser_endpoint_path(codex_home));
    let display = auth_browser_display(override_display)?;
    let binary = find_auth_browser_binary()
        .context("Chromium/Chrome was not found; install chromium-browser or chromium")?;
    let log_path = auth_browser_log_path(codex_home);
    let log_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .with_context(|| format!("open auth browser log {}", log_path.display()))?;
    let mut command = Command::new(binary);
    command
        .env("DISPLAY", &display)
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file));
    command
        .spawn()
        .with_context(|| {
            format!(
                "start Codex Teams auth browser for manual login. If Chromium reports a stale SingletonLock, close existing auth-browser windows or remove {}",
                profile_dir.display()
            )
        })?;
    write_text_atomic(
        &auth_browser_root(codex_home).join("login.env"),
        &format!(
            "profile={}\nlog={}\nurl={}\nupdated_at={}\n",
            profile_dir.display(),
            log_path.display(),
            url,
            now()
        ),
    )?;
    Ok(())
}

fn ensure_auth_browser_cdp(
    codex_home: &Path,
    override_profile: Option<&Path>,
    override_display: Option<&str>,
    initial_url: Option<&str>,
) -> Result<AuthBrowserSession> {
    let profile_dir = auth_browser_profile_dir(codex_home, override_profile);
    let log_path = auth_browser_log_path(codex_home);
    if let Some(ws_url) = read_auth_browser_endpoint(codex_home)
        && let Ok(http_url) = cdp_http_from_ws(&ws_url)
        && cdp_version(&http_url).is_ok()
    {
        return Ok(AuthBrowserSession {
            ws_url,
            http_url,
            log_path,
            profile_dir,
        });
    }

    fs::create_dir_all(auth_browser_root(codex_home))?;
    fs::create_dir_all(&profile_dir)?;
    if auth_browser_profile_is_running(&profile_dir) {
        bail!(
            "auth browser profile is already open without an active CDP endpoint; close the Codex Teams auth-browser window normally, then retry"
        );
    }
    let display = auth_browser_display(override_display)?;
    let binary = find_auth_browser_binary()
        .context("Chromium/Chrome was not found; install chromium-browser or chromium")?;
    let url = initial_url.unwrap_or("about:blank");
    let log_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .with_context(|| format!("open auth browser log {}", log_path.display()))?;
    let mut command = Command::new(binary);
    command
        .env("DISPLAY", &display)
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile_dir.display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-default-apps")
        .arg("--disable-sync")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file.try_clone()?))
        .stderr(Stdio::from(log_file));
    let child = command.spawn().context("start Codex Teams auth browser")?;
    let pid = child.id();
    drop(child);

    let start = Instant::now();
    let mut ws_url = None;
    while start.elapsed() < Duration::from_secs(20) {
        if let Ok(log) = fs::read_to_string(&log_path)
            && let Some(ws) = parse_auth_browser_ws_from_log(&log)?
        {
            ws_url = Some(ws);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let ws_url = ws_url.with_context(|| {
        let log = fs::read_to_string(&log_path).unwrap_or_default();
        format!(
            "auth browser did not expose a DevTools endpoint; log:\n{}",
            log.lines().take(80).collect::<Vec<_>>().join("\n")
        )
    })?;
    let http_url = cdp_http_from_ws(&ws_url)?;
    cdp_version(&http_url)?;
    write_text_atomic(
        &auth_browser_endpoint_path(codex_home),
        &format!(
            "ws={ws_url}\nhttp={http_url}\npid={pid}\nprofile={}\nlog={}\nupdated_at={}\n",
            profile_dir.display(),
            log_path.display(),
            now()
        ),
    )?;
    Ok(AuthBrowserSession {
        ws_url,
        http_url,
        log_path,
        profile_dir,
    })
}

fn auth_browser_profile_is_running(profile_dir: &Path) -> bool {
    let pattern = format!("--user-data-dir={}", profile_dir.display());
    Command::new("pgrep")
        .arg("-f")
        .arg("--")
        .arg(&pattern)
        .output()
        .map(|output| output.status.success() && !output.stdout.is_empty())
        .unwrap_or(false)
}

fn parse_auth_browser_ws_from_log(log: &str) -> Result<Option<String>> {
    Ok(
        Regex::new(r"DevTools listening on (ws://127\.0\.0\.1:[0-9]+/[^\s]+)")?
            .captures_iter(log)
            .last()
            .and_then(|captures| captures.get(1).map(|mat| mat.as_str().to_string())),
    )
}

fn cdp_http_from_ws(ws_url: &str) -> Result<String> {
    let captures = Regex::new(r"^ws://127\.0\.0\.1:([0-9]+)/")?
        .captures(ws_url)
        .with_context(|| format!("unsupported DevTools endpoint `{ws_url}`"))?;
    Ok(format!("http://127.0.0.1:{}", &captures[1]))
}

fn cdp_version(http_url: &str) -> Result<String> {
    let body = http_get_loopback(&format!("{http_url}/json/version"), Duration::from_secs(2))?;
    if !body.contains("webSocketDebuggerUrl") && !body.contains("\"Browser\"") {
        bail!("CDP /json/version response did not look valid");
    }
    Ok(body)
}

fn http_get_loopback(url: &str, timeout: Duration) -> Result<String> {
    let captures = Regex::new(r"^http://127\.0\.0\.1:([0-9]+)(/.*)$")?
        .captures(url)
        .with_context(|| format!("only loopback HTTP URLs are supported, got `{url}`"))?;
    let port: u16 = captures[1].parse()?;
    let path = captures.get(2).map(|m| m.as_str()).unwrap_or("/");
    let mut stream = TcpStream::connect(("127.0.0.1", port))
        .with_context(|| format!("connect to CDP port {port}"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) && !bytes.is_empty() =>
            {
                break;
            }
            Err(err) => return Err(err).context("read CDP HTTP response"),
        }
    }
    let response = String::from_utf8_lossy(&bytes).to_string();
    if !response.starts_with("HTTP/1.1 200") && !response.starts_with("HTTP/1.0 200") {
        let status = response.lines().next().unwrap_or("<empty response>");
        bail!("CDP HTTP request failed: {status}");
    }
    Ok(response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or(response))
}

fn normalize_codex_device_code(code: &str) -> Result<String> {
    let code = code
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase();
    if !Regex::new(r"^[A-Z0-9]{9}$")?.is_match(&code) {
        bail!("Codex device code must contain exactly 9 letters/digits");
    }
    Ok(code)
}

fn authorize_codex_device_with_auth_browser(url: &str, code: &str) -> Result<Vec<String>> {
    authorize_codex_device_with_auth_browser_config(url, code, None, None)
}

fn authorize_codex_device_with_auth_browser_config(
    url: &str,
    code: &str,
    override_profile: Option<&Path>,
    override_display: Option<&str>,
) -> Result<Vec<String>> {
    let codex_home =
        codex_core::config::find_codex_home().context("failed to resolve CODEX_HOME")?;
    let code = normalize_codex_device_code(code)?;
    let profile = auth_browser_profile_dir(&codex_home, override_profile);
    let display = auth_browser_display(override_display)?;
    let output = run_auth_browser_os_authorize(&codex_home, &profile, &display, url, &code)?;
    let mut log = vec![
        format!("auth-browser profile={}", profile.display()),
        "auth-browser automation=os-window".to_string(),
    ];
    log.extend(output.lines().map(str::to_string));
    Ok(log)
}

fn run_auth_browser_os_authorize(
    codex_home: &Path,
    profile_dir: &Path,
    display: &str,
    url: &str,
    code: &str,
) -> Result<String> {
    for command in ["xdotool"] {
        if !command_exists(command) {
            bail!("`{command}` is required for non-CDP auth-browser automation");
        }
    }
    let binary = find_auth_browser_binary()
        .context("Chromium/Chrome was not found; install chromium-browser or chromium")?;
    fs::create_dir_all(auth_browser_root(codex_home))?;
    fs::create_dir_all(profile_dir)?;
    let _ = fs::remove_file(auth_browser_endpoint_path(codex_home));
    let extension_dir = profile_dir
        .parent()
        .map(|parent| parent.join("authorize-extension"))
        .unwrap_or_else(|| auth_browser_root(codex_home).join("authorize-extension"));
    write_auth_browser_authorize_extension(&extension_dir, code)?;
    let log_path = auth_browser_root(codex_home).join("os-authorize.log");
    let shell = format!(
        r#"
set -euo pipefail
export DISPLAY={display}
PROFILE={profile}
URL={url}
BROWSER={browser}
EXT_DIR={extension_dir}
LOG={log_path}
mkdir -p "$(dirname "$LOG")" "$PROFILE"
: > "$LOG"
for pid in $(pgrep -f -- "--user-data-dir=$PROFILE" 2>/dev/null || true); do
  kill "$pid" >/dev/null 2>&1 || true
done
sleep 1
"$BROWSER" --user-data-dir="$PROFILE" --no-first-run --no-default-browser-check --disable-extensions-except="$EXT_DIR" --load-extension="$EXT_DIR" "$URL" >>"$LOG" 2>&1 &
sleep 2
find_window() {{
  for _ in $(seq 1 40); do
    ids="$(xdotool search --class chromium 2>/dev/null || true)"
    for id in $ids; do
      pid="$(xdotool getwindowpid "$id" 2>/dev/null || true)"
      [ -n "$pid" ] || continue
      cmd="$(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null || true)"
      case "$cmd" in
        *"--user-data-dir=$PROFILE"*) echo "$id"; return 0 ;;
      esac
    done
    sleep 0.25
  done
  return 1
}}
WIN="$(find_window)"
echo "window=$WIN" | tee -a "$LOG"
xdotool windowactivate "$WIN"
sleep 0.3
typed_code=0
for step in $(seq 0 179); do
  title="$(xdotool getwindowname "$WIN" 2>/dev/null || true)"
  echo "step $step title=$title" | tee -a "$LOG"
  case "$title" in
    *"codex-auth:success"*) echo "device auth completed" | tee -a "$LOG"; cat "$LOG"; exit 0 ;;
    *"codex-auth:invalid"*) echo "device code was rejected or expired" | tee -a "$LOG"; cat "$LOG"; exit 2 ;;
  esac
  xdotool windowactivate "$WIN"
  sleep 0.7
done
echo "Codex device auth did not complete before timeout" | tee -a "$LOG"
cat "$LOG"
exit 4
"#,
        display = shell_quote(display),
        profile = shell_quote(&profile_dir.display().to_string()),
        url = shell_quote(url),
        browser = shell_quote(&binary),
        extension_dir = shell_quote(&extension_dir.display().to_string()),
        log_path = shell_quote(&log_path.display().to_string()),
    );
    run_shell_capture(&shell, "run auth-browser OS automation")
}

fn write_auth_browser_authorize_extension(extension_dir: &Path, code: &str) -> Result<()> {
    fs::create_dir_all(extension_dir)?;
    write_text_atomic(
        &extension_dir.join("manifest.json"),
        r#"{
  "manifest_version": 3,
  "name": "Codex Team Auth Browser",
  "version": "0.1.0",
  "content_scripts": [
    {
      "matches": ["<all_urls>"],
      "js": ["content.js"],
      "run_at": "document_idle",
      "all_frames": true
    }
  ]
}
"#,
    )?;
    write_text_atomic(
        &extension_dir.join("content.js"),
        &auth_browser_os_authorize_script(code),
    )?;
    Ok(())
}

fn auth_browser_os_authorize_script(code: &str) -> String {
    let code_json = serde_json::to_string(code).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        r#"(()=>{{
if(window.__codexTeamAuthBrowser)return;
window.__codexTeamAuthBrowser=true;
const CODE={code_json};
const RX={{success:/You may close this page|このページを閉じても問題ありません|Device authorized|認証が完了/i,invalid:/Invalid code|コードが無効|expired|期限切れ/i,bot:/セキュリティ検証|悪意のあるボット|not a robot|ロボットではありません|security verification/i}};
const text=()=>document.body?document.body.innerText:"";
const visible=e=>!!(e&&e.offsetParent!==null);
const norm=s=>(s||"").replace(/\s+/g," ").trim();
const setValue=(el,value)=>{{
  const proto=el instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;
  const setter=Object.getOwnPropertyDescriptor(proto,'value')?.set;
  if(setter)setter.call(el,value);else el.value=value;
  el.dispatchEvent(new Event('input',{{bubbles:true}}));
  el.dispatchEvent(new Event('change',{{bubbles:true}}));
}};
const click=(rx)=>{{
  const nodes=[...document.querySelectorAll('button,a,[role="button"],[role="link"],input[type="button"],input[type="submit"],div[tabindex],span[tabindex]')];
  for(const n of nodes){{
    const label=norm(n.innerText||n.value||n.getAttribute('aria-label')||n.getAttribute('title'));
    if(visible(n)&&rx.test(label)){{n.click();return true;}}
  }}
  return false;
}};
const focusCode=()=>{{
  const inputs=[...document.querySelectorAll('input,textarea')].filter(e=>visible(e)&&!['hidden','checkbox','radio','submit','button'].includes((e.type||'').toLowerCase()));
  if(!inputs.length)return false;
  const codeLike=inputs.find(e=>/(code|コード|one-time|ワンタイム)/i.test([e.name,e.id,e.placeholder,e.getAttribute('aria-label')].join(' ')))||inputs[0];
  codeLike.focus();
  try{{codeLike.select();}}catch{{}}
  setValue(codeLike,CODE);
  return true;
}};
const mark=s=>{{if(window.top===window)document.title='codex-auth:'+s;return s;}};
const tick=()=>{{
const t=text(), u=location.href, title=document.title;
if(RX.success.test(t))return mark('success');
if(RX.invalid.test(t))return mark('invalid');
if(RX.bot.test(t))return mark('bot-check');
if(/accounts\.google\.com/i.test(u)){{
  if(click(/Continue|続行|Next|次へ/i))return mark('google-continue');
  const acct=[...document.querySelectorAll('[data-identifier],[data-email],div[role="link"],li[role="link"]')].filter(visible);
  if(acct.length>=1){{acct[0].click();return mark('google-account');}}
  return mark('google-wait');
}}
if(/callback\/google|api\/accounts\/callback\/google|prompt=none/i.test(u)||/Loading|読み込み|しばらくお待ちください/i.test(title+t))return mark('wait');
if(/oauth\/authorize/i.test(u))return mark('wait-oauth');
if(/sign-in-with-chatgpt\/codex\/consent|consent/i.test(u+t)){{
  if(click(/Continue|続行|続ける|Sign in|サインイン|Authorize|許可|Allow|許可する/i))return mark('consent');
}}
if(/log-in|log in|ログイン|メールアドレス|Email address/i.test(u+t)){{
  if(click(/Continue with Google|Sign in with Google|Googleで続行|Google で続行/i))return mark('google-clicked');
  return mark('login-needs-google');
}}
if(focusCode()){{click(/Continue|続行|続ける|Submit|送信|Authorize|許可|Allow|許可する/i);return mark('type-code');}}
if(click(/Continue|続行|続ける|Submit|送信|Authorize|許可|Allow|許可する/i))return mark('clicked');
return mark('unknown');
}};
setInterval(tick,700);
tick();
}})();
"#
    )
}

fn run_auth_browser_authorize_script(cdp_http_url: &str, url: &str, code: &str) -> Result<String> {
    run_auth_browser_node_script(
        r#"
const cdp = process.argv[1];
const authUrl = process.argv[2];
const code = process.argv[3];
const { chromium } = loadPlaywrightCore();

async function bodyText(page) {
  return await page.locator('body').innerText({ timeout: 1500 }).catch(() => '');
}

async function clickVisibleByName(page, pattern) {
  const candidates = [
    page.getByRole('button', { name: pattern }),
    page.getByRole('link', { name: pattern }),
    page.getByText(pattern),
  ];
  for (const locator of candidates) {
    const count = await locator.count().catch(() => 0);
    for (let i = 0; i < Math.min(count, 5); i++) {
      const item = locator.nth(i);
      if (await item.isVisible().catch(() => false)) {
        await item.click({ timeout: 5000 });
        return true;
      }
    }
  }
  return false;
}

async function visibleInputs(page) {
  const locator = page.locator('input');
  const count = await locator.count().catch(() => 0);
  const out = [];
  for (let i = 0; i < Math.min(count, 20); i++) {
    const item = locator.nth(i);
    if (await item.isVisible().catch(() => false)) out.push(item);
  }
  return out;
}

async function clickSingleVisibleAccountCandidate(page) {
  const locator = page.locator('[data-identifier], [data-email], div[role="link"], li[role="link"]');
  const count = await locator.count().catch(() => 0);
  const visible = [];
  for (let i = 0; i < Math.min(count, 20); i++) {
    const item = locator.nth(i);
    if (await item.isVisible().catch(() => false)) visible.push(item);
  }
  if (visible.length === 1) {
    await visible[0].click({ timeout: 5000 });
    return true;
  }
  return false;
}

(async () => {
  const browser = await chromium.connectOverCDP(cdp);
  const context = browser.contexts()[0] || await browser.newContext();
  const page = await context.newPage();
  await page.goto(authUrl, { waitUntil: 'domcontentloaded', timeout: 30000 }).catch(() => {});
  let typed = false;
  let lastUrl = '';
  let sameUrlSteps = 0;
  let reloadedOauthAuthorize = false;
  for (let step = 0; step < 180; step++) {
    await page.waitForTimeout(500);
    const currentUrl = page.url();
    const title = await page.title().catch(() => '');
    const text = await bodyText(page);
    console.log(`step ${step}: ${title} ${currentUrl}`);
    if (currentUrl === lastUrl) {
      sameUrlSteps += 1;
    } else {
      sameUrlSteps = 0;
      lastUrl = currentUrl;
    }

    if (/You may close this page|このページを閉じても問題ありません|Device authorized|認証が完了/i.test(text)) {
      console.log('device auth completed');
      await page.close().catch(() => {});
      await browser.close();
      return;
    }
    if (/Invalid code|コードが無効|expired|期限切れ/i.test(text)) {
      throw new Error('device code was rejected or expired');
    }
    if (/accounts\.google\.com/i.test(currentUrl)) {
      if (await clickVisibleByName(page, /Continue|続行|Next|次へ/i)) {
        console.log('continued Google sign-in');
        continue;
      }
      if (await clickSingleVisibleAccountCandidate(page)) {
        console.log('selected the only visible Google account candidate');
        continue;
      }
    }
    if (/callback\/google|api\/accounts\/callback\/google|prompt=none/i.test(currentUrl) || /Loading|読み込み/i.test(title)) {
      console.log('waiting for Google callback');
      continue;
    }
    if (/oauth\/authorize/i.test(currentUrl) && /しばらくお待ちください|please wait|Loading/i.test(title + '\n' + text)) {
      if (!reloadedOauthAuthorize && sameUrlSteps >= 20) {
        reloadedOauthAuthorize = true;
        console.log('oauth authorize wait is stale; reloading once');
        await page.reload({ waitUntil: 'domcontentloaded', timeout: 30000 }).catch(() => {});
      } else {
        console.log('waiting for OAuth authorize redirect');
      }
      continue;
    }
    if (/sign-in-with-chatgpt\/codex\/consent|consent/i.test(currentUrl + '\n' + text)) {
      if (await clickVisibleByName(page, /Continue|続行|続ける|Sign in|サインイン|Authorize|許可|Allow|許可する/i)) {
        console.log('accepted Codex consent');
        continue;
      }
    }
    if (/log-in|log in|ログイン|メールアドレス|Email address/i.test(currentUrl + '\n' + text)) {
      if (await clickVisibleByName(page, /Continue with Google|Sign in with Google|Googleで続行|Google で続行/i)) {
        console.log('clicked Google sign-in');
        continue;
      }
      throw new Error('auth browser needs Google sign-in but the Google continuation button was not found; run `codex team auth-browser login` and complete Google sign-in once');
    }

    if (/choose-an-account|Select account|アカウントを選択/i.test(currentUrl + '\n' + text)) {
      if (await clickVisibleByName(page, /Select account|アカウントを選択|Continue|続行/i)) continue;
    }

    if (/consent|Continue|続行|続ける|Authorize|許可|Allow|許可する/i.test(currentUrl + '\n' + text)) {
      if (await clickVisibleByName(page, /Continue|続行|続ける|Authorize|許可|Allow|許可する/i)) continue;
    }

    const inputs = await visibleInputs(page);
    if (!typed && inputs.length > 0) {
      await inputs[0].click({ timeout: 5000 });
      await page.keyboard.type(code, { delay: 50 });
      typed = true;
      console.log('typed device code');
      await page.waitForTimeout(400);
      await clickVisibleByName(page, /Continue|続行|続ける|Submit|送信/i);
      continue;
    }

    if (typed) {
      await clickVisibleByName(page, /Continue|続行|続ける|Authorize|許可|Allow|許可する/i);
    }
  }
  throw new Error('Codex device auth did not complete before timeout');
})().catch((err) => {
  console.error(err && err.stack ? err.stack : String(err));
  process.exit(1);
});
"#,
        &[cdp_http_url, url, code],
        Duration::from_secs(90),
    )
}

fn run_auth_browser_node_script(script: &str, args: &[&str], timeout: Duration) -> Result<String> {
    if !command_exists("node") {
        bail!("Node.js is required for auth-browser CDP automation");
    }
    let prelude = r#"
function loadPlaywrightCore() {
  const path = require('path');
  const childProcess = require('child_process');
  const candidates = [];
  function add(candidate) {
    if (candidate && !candidates.includes(candidate)) candidates.push(candidate);
  }
  try { add(require.resolve('playwright-core')); } catch {}
  try {
    const globalRoot = childProcess.execSync('npm root -g', { encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }).trim();
    add(path.join(globalRoot, 'playwright-core'));
    add(path.join(globalRoot, '@playwright/cli/node_modules/playwright-core'));
    add(path.join(globalRoot, 'playwright/node_modules/playwright-core'));
  } catch {}
  if (process.env.HOME) {
    add(path.join(process.env.HOME, '.npm-global/lib/node_modules/playwright-core'));
    add(path.join(process.env.HOME, '.npm-global/lib/node_modules/@playwright/cli/node_modules/playwright-core'));
  }
  for (const candidate of candidates) {
    try { return require(candidate); } catch {}
  }
  throw new Error('playwright-core module was not found; install playwright-cli or playwright-core');
}
"#;
    let output = Command::new("timeout")
        .arg(format!("{}s", timeout.as_secs()))
        .arg("node")
        .arg("-e")
        .arg(format!("{prelude}\n{script}"))
        .args(args)
        .output()
        .or_else(|_| {
            Command::new("node")
                .arg("-e")
                .arg(format!("{prelude}\n{script}"))
                .args(args)
                .output()
        })
        .context("run auth-browser Node.js automation")?;
    if !output.status.success() {
        bail!(
            "auth-browser automation failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_codex_device_auth_from_log(log: &str) -> Result<Option<(String, String)>> {
    if !log.contains("auth.openai.com") && !log.to_ascii_lowercase().contains("device") {
        return Ok(None);
    }
    let url = Regex::new(r"https://auth\.openai\.com/[^\s\)]+")?
        .find(log)
        .map(|mat| mat.as_str().to_string())
        .unwrap_or_else(|| "https://auth.openai.com/codex/device".to_string());
    let code = Regex::new(r"\b([A-Z0-9]{4})-([A-Z0-9]{4,5})\b")?
        .captures(log)
        .and_then(|captures| {
            Some(format!(
                "{}{}",
                captures.get(1)?.as_str(),
                captures.get(2)?.as_str()
            ))
        })
        .or_else(|| {
            Regex::new(r"\b([A-Z0-9]{9})\b")
                .ok()?
                .captures(log)
                .and_then(|captures| captures.get(1).map(|mat| mat.as_str().to_string()))
        })
        .map(|code| code.replace('-', "").to_ascii_uppercase());
    let Some(code) = code else {
        return Ok(None);
    };
    if !Regex::new(r"^[A-Z0-9]{9}$")?.is_match(&code) {
        return Ok(None);
    }
    Ok(Some((url, code)))
}

fn command_exists(command: &str) -> bool {
    Command::new("sh")
        .arg("-lc")
        .arg(format!(
            "command -v {} >/dev/null 2>&1",
            shell_quote(command)
        ))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn shell_quote_path(path: &Path) -> String {
    shell_quote(&path.display().to_string())
}

fn reserve_ephemeral_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").context("reserve ephemeral port")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn run_shell_command(command: &str, context: &str) -> Result<()> {
    let output = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .output()
        .with_context(|| context.to_string())?;
    if !output.status.success() {
        bail!(
            "{context} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn run_shell_capture(command: &str, context: &str) -> Result<String> {
    let output = Command::new("bash")
        .arg("-lc")
        .arg(command)
        .output()
        .with_context(|| context.to_string())?;
    if !output.status.success() {
        bail!(
            "{context} failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_ssh_command(host: &str, command: &str) -> Result<String> {
    let output = Command::new("ssh")
        .arg(host)
        .arg(format!("bash -lc {}", shell_quote(command)))
        .output()
        .with_context(|| format!("run ssh command on `{host}`"))?;
    if !output.status.success() {
        bail!(
            "ssh command on `{host}` failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout).trim(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_node_command_capture(node: &TeamNode, command: &str) -> Result<String> {
    match node.kind {
        TeamNodeKind::Local => run_shell_capture(command, "run local node command"),
        TeamNodeKind::Ssh => {
            let host = node.host.as_deref().context("ssh node needs host")?;
            run_ssh_command(host, command)
        }
        TeamNodeKind::Docker => {
            let container = node
                .container
                .as_deref()
                .context("docker node needs container")?;
            run_shell_capture(
                &format!(
                    "docker exec {} bash -lc {}",
                    shell_quote(container),
                    shell_quote(command)
                ),
                "run docker node command",
            )
        }
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref().context("ssh-docker node needs host")?;
            let container = node
                .container
                .as_deref()
                .context("ssh-docker node needs container")?;
            run_ssh_command(
                host,
                &format!(
                    "docker exec {} bash -lc {}",
                    shell_quote(container),
                    shell_quote(command)
                ),
            )
        }
        TeamNodeKind::Manual => bail!("manual node command execution is not supported"),
    }
}

fn collect_node_facts(node: &TeamNode) -> Result<String> {
    let script = r#"printf 'hostname=%s\n' "$(hostname 2>/dev/null || true)"
printf 'user=%s\n' "$(id -un 2>/dev/null || true)"
printf 'uid=%s\n' "$(id -u 2>/dev/null || true)"
printf 'pwd=%s\n' "$(pwd 2>/dev/null || true)"
printf 'uname=%s\n' "$(uname -a 2>/dev/null || true)"
printf 'codex_path=%s\n' "$(command -v codex 2>/dev/null || true)"
printf 'codex_version=%s\n' "$(codex --version 2>/dev/null || true)"
printf 'codex_team_path=%s\n' "$(command -v codex-team 2>/dev/null || true)"
printf 'docker_path=%s\n' "$(command -v docker 2>/dev/null || true)"
printf 'docker_version=%s\n' "$(docker --version 2>/dev/null || true)"
printf 'sudo_passwordless=%s\n' "$(if command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then echo yes; else echo no; fi)"
printf 'package_managers=%s\n' "$(for cmd in apt-get apk dnf yum brew pacman zypper; do command -v "$cmd" >/dev/null 2>&1 && printf '%s ' "$cmd"; done)"
printf 'node_path=%s\n' "$(command -v node 2>/dev/null || true)"
printf 'node_version=%s\n' "$(node --version 2>/dev/null || true)"
printf 'npm_path=%s\n' "$(command -v npm 2>/dev/null || true)"
printf 'npm_version=%s\n' "$(npm --version 2>/dev/null || true)"
printf 'python3_path=%s\n' "$(command -v python3 2>/dev/null || true)"
printf 'python3_version=%s\n' "$(python3 --version 2>/dev/null || true)"
printf 'pip_path=%s\n' "$(command -v pip3 2>/dev/null || command -v pip 2>/dev/null || true)"
printf 'rg_path=%s\n' "$(command -v rg 2>/dev/null || true)"
printf 'git_path=%s\n' "$(command -v git 2>/dev/null || true)"
printf 'chromium_path=%s\n' "$(command -v chromium 2>/dev/null || command -v chromium-browser 2>/dev/null || command -v google-chrome 2>/dev/null || true)"
printf 'nvidia_smi_path=%s\n' "$(command -v nvidia-smi 2>/dev/null || true)"
if command -v nvidia-smi >/dev/null 2>&1; then
  printf 'gpu_summary=%s\n' "$(nvidia-smi --query-gpu=name,memory.total,memory.free,driver_version --format=csv,noheader 2>/dev/null | paste -sd ';' -)"
else
  printf 'gpu_summary=\n'
fi
printf 'disk_pwd=%s\n' "$(df -h . 2>/dev/null | tail -n 1 | tr -s ' ' || true)"
"#;
    run_node_command_capture(node, script)
}

struct NodeAppServerProcess {
    node_id: String,
    child: Child,
    cleanup: Option<NodeCleanup>,
}

enum NodeCleanup {
    Ssh {
        host: String,
        remote_port: u16,
    },
    Docker {
        container: String,
        remote_port: u16,
    },
    SshDocker {
        host: String,
        container: String,
        remote_port: u16,
    },
}

impl NodeAppServerProcess {
    fn stop(mut self) {
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        match self.cleanup {
            Some(NodeCleanup::Ssh { host, remote_port }) => {
                let pattern = format!("[c]odex app-server --listen ws://127.0.0.1:{remote_port}");
                let _ = Command::new("ssh")
                    .arg("-o")
                    .arg("BatchMode=yes")
                    .arg(host)
                    .arg(format!("pkill -f {}", shell_quote(&pattern)))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            Some(NodeCleanup::Docker {
                container,
                remote_port,
            }) => {
                let pattern = format!("[c]odex app-server --listen ws://0.0.0.0:{remote_port}");
                let _ = Command::new("docker")
                    .arg("exec")
                    .arg(container)
                    .arg("pkill")
                    .arg("-f")
                    .arg(pattern)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            Some(NodeCleanup::SshDocker {
                host,
                container,
                remote_port,
            }) => {
                let pattern = format!("[c]odex app-server --listen ws://0.0.0.0:{remote_port}");
                let command = format!(
                    "docker exec {} pkill -f {}",
                    shell_quote(&container),
                    shell_quote(&pattern)
                );
                let _ = Command::new("ssh")
                    .arg("-o")
                    .arg("BatchMode=yes")
                    .arg(host)
                    .arg(command)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            None => {}
        }
    }
}

#[derive(Clone)]
struct AppServerMemberRun {
    member: TeamMember,
    node_id: String,
    cwd: PathBuf,
    thread_id: String,
    turn_id: String,
    completed: bool,
    failed: bool,
    standby_after_turn: bool,
    team_message_scan_offset: usize,
    last_activity_at: Instant,
    last_activity_kind: String,
    last_stale_notice_at: Option<Instant>,
    retry_not_before: Option<Instant>,
    side_context_ids: Vec<String>,
}

struct TeamAppServerNodeClient {
    client: RemoteAppServerClient,
    request_counter: i64,
}

struct AppServerSideReply {
    member: TeamMember,
    node_id: String,
    source_thread_id: String,
    side_thread_id: String,
    turn_id: String,
    recipients: Vec<String>,
    messages: Vec<MailMessage>,
    buffer: String,
    started_at: Instant,
}

#[allow(clippy::too_many_arguments)]
async fn start_app_server_member_turn(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    member_name: &str,
    prompt: String,
    _cwd: &Path,
    model: Option<String>,
    approval_policy: Option<AskForApproval>,
    dangerously_bypass_approvals_and_sandbox: bool,
    event_name: &str,
) -> Result<bool> {
    let Some(run) = active.get_mut(member_name) else {
        bail!("member `{member_name}` has no app-server thread");
    };
    if let Some(remaining) = app_server_retry_remaining(run) {
        append_event(
            team_dir,
            "app_server_member_turn_start_deferred",
            serde_json::json!({
                "member": member_name,
                "node": run.node_id.clone(),
                "thread": run.thread_id.clone(),
                "reason": "temporary app-server/model usage-limit cooldown",
                "retry_after_sec": remaining.as_secs(),
                "event": event_name,
            }),
        )?;
        set_member_status(team_dir, member_name, MemberStatus::Standby)?;
        return Ok(false);
    }
    let Some(node_client) = node_clients.get_mut(&run.node_id) else {
        append_event(
            team_dir,
            "app_server_member_turn_start_skipped",
            serde_json::json!({
                "member": member_name,
                "node": run.node_id,
                "thread": run.thread_id.clone(),
                "reason": "node client missing",
                "event": event_name,
            }),
        )?;
        block_member_tasks_if_active(
            team_dir,
            member_name,
            "Member could not be resumed because its app-server node client is missing.",
        )?;
        run.completed = true;
        run.failed = false;
        run.standby_after_turn = false;
        set_member_status(team_dir, member_name, MemberStatus::Standby)?;
        return Ok(false);
    };
    let turn_cwd = run.cwd.clone();
    let language = load_config(team_dir)?.language.unwrap_or_default();
    let (prompt, side_context_ids) =
        append_side_channel_context_prompt(team_dir, member_name, "", prompt, language)?;
    let turn: TurnStartResponse = node_client
        .client
        .request_typed(ClientRequest::TurnStart {
            request_id: next_request_id(&mut node_client.request_counter),
            params: TurnStartParams {
                thread_id: run.thread_id.clone(),
                input: vec![text_input(prompt)],
                cwd: Some(turn_cwd.clone()),
                model,
                approval_policy,
                sandbox_policy: if dangerously_bypass_approvals_and_sandbox {
                    Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess)
                } else {
                    None
                },
                ..TurnStartParams::default()
            },
        })
        .await
        .map_err(|err| anyhow!(err))?;
    run.turn_id = turn.turn.id.clone();
    run.completed = false;
    run.failed = false;
    run.standby_after_turn = false;
    run.retry_not_before = None;
    run.last_activity_at = Instant::now();
    run.last_activity_kind = "turn_started".to_string();
    run.last_stale_notice_at = None;
    run.side_context_ids = side_context_ids;
    reset_member_live_message_for_new_turn(team_dir, member_name, &turn.turn.id)?;
    set_member_status(team_dir, member_name, MemberStatus::Running)?;
    mark_side_channel_contexts_injected(
        team_dir,
        member_name,
        &run.side_context_ids,
        &turn.turn.id,
    )?;
    append_event(
        team_dir,
        event_name,
        serde_json::json!({
            "member": member_name,
            "node": run.node_id.clone(),
            "thread": run.thread_id.clone(),
            "turn": turn.turn.id,
            "cwd": turn_cwd,
        }),
    )?;
    Ok(true)
}

async fn connect_team_app_server(url: &str) -> Result<RemoteAppServerClient> {
    connect_team_app_server_with_attempts(url, 50).await
}

async fn connect_team_app_server_with_attempts(
    url: &str,
    attempts: usize,
) -> Result<RemoteAppServerClient> {
    let mut last_error = None;
    for _ in 0..attempts.max(1) {
        match RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
            websocket_url: url.to_string(),
            auth_token: None,
            client_name: "codex_team".to_string(),
            client_version: env!("CARGO_PKG_VERSION").to_string(),
            experimental_api: true,
            opt_out_notification_methods: vec![
                "command/exec/outputDelta".to_string(),
                "item/commandExecution/outputDelta".to_string(),
                "item/fileChange/outputDelta".to_string(),
                "item/reasoning/summaryTextDelta".to_string(),
                "item/reasoning/textDelta".to_string(),
            ],
            channel_capacity: 256,
        })
        .await
        {
            Ok(client) => return Ok(client),
            Err(err) => {
                last_error = Some(err);
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    Err(anyhow!(
        "failed to connect to app-server at `{}`: {}",
        url,
        last_error
            .map(|err| err.to_string())
            .unwrap_or_else(|| "unknown error".to_string())
    ))
}

fn next_request_id(counter: &mut i64) -> RequestId {
    let request_id = *counter;
    *counter += 1;
    RequestId::Integer(request_id)
}

fn app_server_usage_limit_cooldown(error: Option<&str>) -> Option<Duration> {
    let error = error?;
    let normalized = error.to_ascii_lowercase();
    if !(normalized.contains("usage limit")
        || normalized.contains("purchase more credits")
        || normalized.contains("try again at"))
    {
        return None;
    }
    Some(usage_limit_cooldown_from_error(
        error,
        &[
            Local::now().time().num_seconds_from_midnight(),
            Utc::now().time().num_seconds_from_midnight(),
        ],
    ))
}

fn usage_limit_cooldown_from_error(error: &str, now_secs_candidates: &[u32]) -> Duration {
    usage_limit_cooldown_from_error_at(error, Local::now(), now_secs_candidates)
}

fn usage_limit_cooldown_from_error_at(
    error: &str,
    now_local: DateTime<Local>,
    now_secs_candidates: &[u32],
) -> Duration {
    const DEFAULT_USAGE_LIMIT_COOLDOWN_SEC: u64 = 45 * 60;
    const RETRY_TIME_JUST_PASSED_GRACE_SEC: u32 = 10 * 60;
    const RETRY_TIME_JUST_PASSED_BACKOFF_SEC: u32 = 5 * 60;
    if let Some(retry_at) = parse_usage_limit_retry_datetime(error) {
        let delta = retry_at.signed_duration_since(now_local);
        if delta.num_seconds() > 0 {
            return Duration::from_secs(delta.num_seconds().max(60) as u64);
        }
        if (-delta).num_seconds() <= i64::from(RETRY_TIME_JUST_PASSED_GRACE_SEC) {
            return Duration::from_secs(u64::from(RETRY_TIME_JUST_PASSED_BACKOFF_SEC));
        }
    }
    match parse_usage_limit_retry_time_secs(error) {
        Some(retry_secs) => {
            let delta = now_secs_candidates
                .iter()
                .map(|now_secs| {
                    let now_secs = now_secs % (24 * 60 * 60);
                    if retry_secs >= now_secs {
                        retry_secs - now_secs
                    } else if now_secs - retry_secs <= RETRY_TIME_JUST_PASSED_GRACE_SEC {
                        RETRY_TIME_JUST_PASSED_BACKOFF_SEC
                    } else {
                        24 * 60 * 60 - now_secs + retry_secs
                    }
                })
                .min()
                .unwrap_or(DEFAULT_USAGE_LIMIT_COOLDOWN_SEC as u32);
            Duration::from_secs(u64::from(delta.max(60)))
        }
        None => Duration::from_secs(DEFAULT_USAGE_LIMIT_COOLDOWN_SEC),
    }
}

fn parse_usage_limit_retry_datetime(error: &str) -> Option<DateTime<Local>> {
    let lower = error.to_ascii_lowercase();
    let marker = "try again at";
    let start = lower.find(marker)? + marker.len();
    let original = error.get(start..)?.trim_start_matches(|ch: char| {
        ch.is_ascii_whitespace() || matches!(ch, ':' | '-' | '.' | ',')
    });
    let cleaned = clean_usage_limit_datetime_text(original);
    let naive = NaiveDateTime::parse_from_str(&cleaned, "%B %d, %Y %I:%M %p")
        .or_else(|_| NaiveDateTime::parse_from_str(&cleaned, "%b %d, %Y %I:%M %p"))
        .ok()?;
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Some(value),
        LocalResult::Ambiguous(a, b) => Some(a.min(b)),
        LocalResult::None => None,
    }
}

fn clean_usage_limit_datetime_text(value: &str) -> String {
    let mut cleaned = value
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(|ch: char| matches!(ch, '.' | ';'))
        .to_string();
    for suffix in ["st", "nd", "rd", "th"] {
        cleaned = remove_ordinal_suffix(&cleaned, suffix);
    }
    cleaned
}

fn remove_ordinal_suffix(value: &str, suffix: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(index) = rest.to_ascii_lowercase().find(suffix) {
        let (before, after_suffix) = rest.split_at(index);
        let after = &after_suffix[suffix.len()..];
        let previous_is_digit = before
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_ascii_digit());
        let next_is_boundary = after
            .chars()
            .next()
            .is_none_or(|ch| !ch.is_ascii_alphanumeric());
        out.push_str(before);
        if !(previous_is_digit && next_is_boundary) {
            out.push_str(&after_suffix[..suffix.len()]);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

fn parse_usage_limit_retry_time_secs(error: &str) -> Option<u32> {
    let lower = error.to_ascii_lowercase();
    let marker = "try again at";
    let start = lower.find(marker)? + marker.len();
    let mut rest = lower[start..].trim_start_matches(|ch: char| {
        ch.is_ascii_whitespace() || matches!(ch, ':' | '-' | '.' | ',')
    });
    if rest.is_empty() {
        return None;
    }

    let hour_len = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .map(char::len_utf8)
        .sum::<usize>();
    if hour_len == 0 {
        return None;
    }
    let hour = rest[..hour_len].parse::<u32>().ok()?;
    rest = &rest[hour_len..];

    let mut minute = 0;
    if let Some(after_colon) = rest.strip_prefix(':') {
        let minute_len = after_colon
            .chars()
            .take_while(|ch| ch.is_ascii_digit())
            .map(char::len_utf8)
            .sum::<usize>();
        if minute_len == 0 {
            return None;
        }
        minute = after_colon[..minute_len].parse::<u32>().ok()?;
        rest = &after_colon[minute_len..];
    }
    if minute >= 60 {
        None
    } else {
        let suffix =
            rest.trim_start_matches(|ch: char| ch.is_ascii_whitespace() || matches!(ch, '.' | ','));
        let hour_24 = if suffix.starts_with("am") {
            if !(1..=12).contains(&hour) {
                return None;
            }
            hour % 12
        } else if suffix.starts_with("pm") {
            if !(1..=12).contains(&hour) {
                return None;
            }
            (hour % 12) + 12
        } else {
            if hour >= 24 {
                return None;
            }
            hour
        };
        Some(hour_24 * 60 * 60 + minute * 60)
    }
}

fn app_server_retry_remaining(run: &AppServerMemberRun) -> Option<Duration> {
    let retry_not_before = run.retry_not_before?;
    retry_not_before.checked_duration_since(Instant::now())
}

fn active_run_usage_limit_remaining(
    active: &HashMap<String, AppServerMemberRun>,
    member_name: &str,
) -> Option<Duration> {
    active.get(member_name).and_then(app_server_retry_remaining)
}

fn should_suppress_empty_department_ping_during_cooldown(
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    member_name: &str,
    has_open_tasks: bool,
    has_active_turn: bool,
) -> Option<Duration> {
    if has_open_tasks || has_active_turn {
        return None;
    }
    active_run_usage_limit_remaining(active, member_name)
        .or_else(|| active_run_usage_limit_remaining(active, &config.lead))
}

fn recent_usage_limit_retry_not_before(
    team_dir: &Path,
    member_name: &str,
) -> Result<Option<Instant>> {
    Ok(recent_usage_limit_retry_remaining(team_dir, member_name)?
        .map(|remaining| Instant::now() + remaining))
}

fn recent_usage_limit_retry_remaining(
    team_dir: &Path,
    member_name: &str,
) -> Result<Option<Duration>> {
    let auth_json = codex_core::config::find_codex_home()
        .ok()
        .map(|home| home.join("auth.json"));
    recent_usage_limit_retry_remaining_with_auth(team_dir, member_name, auth_json.as_deref())
}

fn recent_usage_limit_retry_remaining_with_auth(
    team_dir: &Path,
    member_name: &str,
    auth_json: Option<&Path>,
) -> Result<Option<Duration>> {
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    let member_node = usage_limit_member_node_id(team_dir, member_name)?;
    let now_utc = Utc::now();
    for event in events.into_iter().rev().take(300) {
        if event.event != "app_server_member_usage_limited" {
            continue;
        }
        if event.data.get("member").and_then(|value| value.as_str()) != Some(member_name) {
            continue;
        }
        let event_time = match DateTime::parse_from_rfc3339(&event.timestamp) {
            Ok(value) => value.with_timezone(&Utc),
            Err(_) => continue,
        };
        if auth_json_was_modified_after(auth_json, event_time)? {
            return Ok(None);
        }
        if node_device_auth_completed_after(team_dir, member_node.as_deref(), event_time)? {
            return Ok(None);
        }
        let elapsed = now_utc.signed_duration_since(event_time);
        let elapsed_secs = elapsed.num_seconds().max(0) as u64;
        let mut cooldown = event
            .data
            .get("retry_after_sec")
            .and_then(|value| value.as_u64())
            .map(Duration::from_secs);
        if let Some(error) = event.data.get("error").and_then(|value| value.as_str()) {
            let event_local = event_time.with_timezone(&Local);
            let parsed = usage_limit_cooldown_from_error_at(
                error,
                event_local,
                &[
                    event_local.time().num_seconds_from_midnight(),
                    event_time.time().num_seconds_from_midnight(),
                ],
            );
            cooldown = Some(cooldown.map_or(parsed, |existing| existing.max(parsed)));
        }
        let Some(cooldown) = cooldown else {
            continue;
        };
        if cooldown.as_secs() > elapsed_secs {
            return Ok(Some(Duration::from_secs(cooldown.as_secs() - elapsed_secs)));
        }
        return Ok(None);
    }
    Ok(None)
}

fn usage_limit_member_node_id(team_dir: &Path, member_name: &str) -> Result<Option<String>> {
    let config_path = team_dir.join("config.json");
    let Ok(config) = read_json::<TeamConfig>(&config_path) else {
        return Ok(None);
    };
    Ok(config
        .members
        .iter()
        .find(|member| member.name == member_name)
        .map(member_node_id))
}

fn node_device_auth_completed_after(
    team_dir: &Path,
    node_id: Option<&str>,
    event_time: DateTime<Utc>,
) -> Result<bool> {
    let Some(node_id) = node_id else {
        return Ok(false);
    };
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    for event in events.into_iter().rev().take(300) {
        if event.event != "node_direct_device_auth_completed"
            && event.event != "node_auth_copy_fallback_synced"
        {
            continue;
        }
        if event.data.get("node").and_then(|value| value.as_str()) != Some(node_id) {
            continue;
        }
        let event_time_auth = match DateTime::parse_from_rfc3339(&event.timestamp) {
            Ok(value) => value.with_timezone(&Utc),
            Err(_) => continue,
        };
        if event_time_auth > event_time {
            return Ok(true);
        }
        return Ok(false);
    }
    Ok(false)
}

fn auth_json_was_modified_after(
    auth_json: Option<&Path>,
    event_time: DateTime<Utc>,
) -> Result<bool> {
    let Some(auth_json) = auth_json else {
        return Ok(false);
    };
    let Ok(metadata) = fs::metadata(auth_json) else {
        return Ok(false);
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(false);
    };
    let modified_utc: DateTime<Utc> = modified.into();
    Ok(modified_utc > event_time)
}

fn member_node_id(member: &TeamMember) -> String {
    member
        .node
        .clone()
        .filter(|node| !node.trim().is_empty())
        .unwrap_or_else(|| "local".to_string())
}

fn app_server_member_cwd(node_id: &str, nodes: &[TeamNode], local_cwd: &Path) -> PathBuf {
    if node_id == "local" {
        return local_cwd.to_path_buf();
    }
    nodes
        .iter()
        .find(|node| node.id == node_id)
        .and_then(|node| node.cwd.as_deref())
        .filter(|cwd| !cwd.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn thread_key(node_id: &str, thread_id: &str) -> String {
    format!("{node_id}:{thread_id}")
}

fn text_input(text: String) -> AppServerUserInput {
    AppServerUserInput::Text {
        text,
        text_elements: Vec::new(),
    }
}

fn app_server_sandbox(
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<Option<SandboxMode>> {
    if dangerously_bypass_approvals_and_sandbox {
        return Ok(Some(SandboxMode::DangerFullAccess));
    }
    match sandbox {
        None => Ok(None),
        Some("read-only" | "readonly" | "read_only") => Ok(Some(SandboxMode::ReadOnly)),
        Some("workspace-write" | "workspace_write") => Ok(Some(SandboxMode::WorkspaceWrite)),
        Some("danger-full-access" | "danger_full_access") => {
            Ok(Some(SandboxMode::DangerFullAccess))
        }
        Some(value) => bail!("unsupported app-server sandbox mode `{value}`"),
    }
}

async fn drain_app_server_events(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    thread_to_member: &HashMap<String, String>,
    assistant_buffers: &mut HashMap<String, String>,
) -> Result<()> {
    let node_ids = node_clients.keys().cloned().collect::<Vec<_>>();
    for node_id in node_ids {
        loop {
            let Some(node_client) = node_clients.get_mut(&node_id) else {
                break;
            };
            let event = match tokio::time::timeout(
                Duration::from_millis(1),
                node_client.client.next_event(),
            )
            .await
            {
                Ok(Some(event)) => event,
                Ok(None) => {
                    if node_id == "local" {
                        bail!("app-server node `{node_id}` disconnected");
                    }
                    append_event(
                        team_dir,
                        "app_server_node_disconnected",
                        serde_json::json!({
                            "node": node_id,
                            "reason": "event stream closed",
                        }),
                    )?;
                    node_clients.remove(&node_id);
                    requeue_app_server_node_members(
                        team_dir,
                        active,
                        &node_id,
                        "app-server event stream closed; restarting node session",
                    )?;
                    remove_side_replies_for_node(team_dir, side_replies, &node_id)?;
                    break;
                }
                Err(_) => break,
            };
            if let AppServerEvent::Disconnected { message } = &event {
                if node_id == "local" {
                    bail!("app-server disconnected: {message}");
                }
                append_event(
                    team_dir,
                    "app_server_node_disconnected",
                    serde_json::json!({
                        "node": node_id,
                        "reason": message,
                    }),
                )?;
                node_clients.remove(&node_id);
                requeue_app_server_node_members(
                    team_dir,
                    active,
                    &node_id,
                    &format!("app-server disconnected: {message}; restarting node session"),
                )?;
                remove_side_replies_for_node(team_dir, side_replies, &node_id)?;
                break;
            }
            handle_app_server_event(
                &mut node_client.client,
                &node_id,
                event,
                team_dir,
                active,
                side_replies,
                thread_to_member,
                assistant_buffers,
            )
            .await?;
        }
    }
    Ok(())
}

fn requeue_app_server_node_members(
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    node_id: &str,
    reason: &str,
) -> Result<()> {
    let member_names = active
        .iter()
        .filter(|(_, run)| run.node_id == node_id && run.member.role != "lead")
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    for member_name in member_names {
        active.remove(&member_name);
        set_member_status(team_dir, &member_name, MemberStatus::Online)?;
        append_event(
            team_dir,
            "app_server_member_requeued",
            serde_json::json!({
                "member": member_name,
                "node": node_id,
                "reason": reason,
            }),
        )?;
    }
    Ok(())
}

fn remove_side_replies_for_node(
    team_dir: &Path,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    node_id: &str,
) -> Result<()> {
    let removed = side_replies
        .iter()
        .filter(|(_, reply)| reply.node_id == node_id)
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in removed {
        if let Some(reply) = side_replies.remove(&key) {
            append_event(
                team_dir,
                "app_server_side_channel_reply_dropped",
                serde_json::json!({
                    "member": reply.member.name,
                    "node": reply.node_id,
                    "side_thread": reply.side_thread_id,
                    "turn": reply.turn_id,
                    "reason": "node disconnected",
                }),
            )?;
        }
    }
    Ok(())
}

async fn handle_app_server_event(
    client: &mut RemoteAppServerClient,
    node_id: &str,
    event: AppServerEvent,
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    thread_to_member: &HashMap<String, String>,
    assistant_buffers: &mut HashMap<String, String>,
) -> Result<()> {
    match event {
        AppServerEvent::ServerNotification(ServerNotification::AgentMessageDelta(delta)) => {
            let key = thread_key(node_id, &delta.thread_id);
            if let Some(reply) = side_replies.get_mut(&key) {
                reply.buffer.push_str(&delta.delta);
                append_text(
                    &team_dir
                        .join("live_messages")
                        .join(format!("{}.side.md", sanitize_id(&reply.member.name))),
                    &delta.delta,
                )?;
            } else if let Some(member) = thread_to_member.get(&key) {
                if let Some(run) = active.get_mut(member) {
                    run.last_activity_at = Instant::now();
                    run.last_activity_kind = "agent_message_delta".to_string();
                }
                assistant_buffers
                    .entry(member.clone())
                    .or_default()
                    .push_str(&delta.delta);
                ingest_team_signal_lines(team_dir, member, active, assistant_buffers, false)?;
                append_text(
                    &team_dir
                        .join("live_messages")
                        .join(format!("{}.md", sanitize_id(member))),
                    &delta.delta,
                )?;
            }
        }
        AppServerEvent::ServerNotification(ServerNotification::TurnStarted(started)) => {
            let key = thread_key(node_id, &started.thread_id);
            if let Some(member) = thread_to_member.get(&key)
                && let Some(run) = active.get_mut(member)
            {
                if reset_member_turn_buffer_if_new(run, assistant_buffers, member, &started.turn.id)
                {
                    reset_member_live_message_for_new_turn(team_dir, member, &started.turn.id)?;
                }
                run.turn_id = started.turn.id.clone();
                run.completed = false;
                run.failed = false;
                run.retry_not_before = None;
                run.last_activity_at = Instant::now();
                run.last_activity_kind = "external_turn_started".to_string();
                run.last_stale_notice_at = None;
                set_member_status(team_dir, member, MemberStatus::Running)?;
                append_event(
                    team_dir,
                    "app_server_member_external_turn_started",
                    serde_json::json!({
                        "member": member,
                        "node": node_id,
                        "thread": started.thread_id,
                        "turn": started.turn.id,
                    }),
                )?;
            }
        }
        AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(completed)) => {
            let key = thread_key(node_id, &completed.thread_id);
            if side_replies.contains_key(&key) {
                handle_app_server_side_reply_completed(team_dir, side_replies, node_id, completed)?;
            } else {
                handle_app_server_turn_completed(
                    team_dir,
                    active,
                    thread_to_member,
                    assistant_buffers,
                    node_id,
                    completed,
                )?;
            }
        }
        AppServerEvent::ServerRequest(request) => {
            reject_app_server_request(client, request).await?;
        }
        AppServerEvent::Disconnected { message } => {
            bail!("app-server disconnected: {message}");
        }
        AppServerEvent::Lagged { skipped } => {
            append_event(
                team_dir,
                "app_server_events_lagged",
                serde_json::json!({ "skipped": skipped }),
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn reset_member_turn_buffer_if_new(
    run: &mut AppServerMemberRun,
    assistant_buffers: &mut HashMap<String, String>,
    member_name: &str,
    new_turn_id: &str,
) -> bool {
    if run.turn_id == new_turn_id {
        return false;
    }
    assistant_buffers.insert(member_name.to_string(), String::new());
    run.team_message_scan_offset = 0;
    true
}

fn reset_member_live_message_for_new_turn(
    team_dir: &Path,
    member_name: &str,
    new_turn_id: &str,
) -> Result<()> {
    let live_path = team_dir
        .join("live_messages")
        .join(format!("{}.md", sanitize_id(member_name)));
    if let Ok(previous_live) = fs::read_to_string(&live_path)
        && !previous_live.trim().is_empty()
    {
        let last_path = team_dir
            .join("last_messages")
            .join(format!("{}.md", sanitize_id(member_name)));
        write_text_atomic(&last_path, &previous_live)?;
    }
    write_text_atomic(
        &live_path,
        &format!("## Turn {turn}\n\n", turn = new_turn_id),
    )?;
    Ok(())
}

fn handle_app_server_turn_completed(
    team_dir: &Path,
    active: &mut HashMap<String, AppServerMemberRun>,
    thread_to_member: &HashMap<String, String>,
    assistant_buffers: &HashMap<String, String>,
    node_id: &str,
    completed: TurnCompletedNotification,
) -> Result<()> {
    let Some(member_name) = thread_to_member.get(&thread_key(node_id, &completed.thread_id)) else {
        return Ok(());
    };
    let Some(run) = active.get_mut(member_name) else {
        return Ok(());
    };
    run.completed = true;
    run.last_activity_at = Instant::now();
    run.last_activity_kind = "turn_completed".to_string();
    run.last_stale_notice_at = None;
    match completed.turn.status {
        TurnStatus::Completed => {
            run.retry_not_before = None;
            if run.member.role == "lead" {
                set_member_status(team_dir, member_name, MemberStatus::Online)?;
            } else if member_turn_reports_blocked(assistant_buffers, member_name)
                && member_has_active_tasks(team_dir, member_name)?
            {
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                block_member_tasks_if_active(
                    team_dir,
                    member_name,
                    "Worker turn ended while waiting on a team gate or handoff.",
                )?;
                append_event(
                    team_dir,
                    "app_server_member_blocked",
                    serde_json::json!({
                        "member": member_name,
                        "node": node_id,
                        "thread": completed.thread_id,
                        "turn": completed.turn.id,
                        "reason": "turn output reported blocked/waiting",
                    }),
                )?;
            } else if run.standby_after_turn
                || member_status(team_dir, member_name)? == Some(MemberStatus::Standby)
            {
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                if member_has_active_tasks(team_dir, member_name)? {
                    block_member_tasks_if_active(
                        team_dir,
                        member_name,
                        "Member was moved to standby before this mission was completed.",
                    )?;
                    append_event(
                        team_dir,
                        "app_server_member_standby_blocked",
                        serde_json::json!({
                            "member": member_name,
                            "node": node_id,
                            "thread": completed.thread_id,
                            "turn": completed.turn.id,
                        }),
                    )?;
                }
                run.standby_after_turn = false;
            } else if member_has_active_tasks(team_dir, member_name)?
                && let Some(checklist_issue) = member_turn_active_task_completion_issue(
                    team_dir,
                    assistant_buffers,
                    member_name,
                )?
            {
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                block_member_tasks_if_active(
                    team_dir,
                    member_name,
                    &format!(
                        "Worker turn ended without acceptable TEAM_COMPLETION_CHECKLIST handoff evidence: {checklist_issue}."
                    ),
                )?;
                append_event(
                    team_dir,
                    "app_server_member_completion_checklist_missing",
                    serde_json::json!({
                        "member": member_name,
                        "node": node_id,
                        "thread": completed.thread_id,
                        "turn": completed.turn.id,
                        "issue": checklist_issue,
                    }),
                )?;
            } else {
                set_member_status(team_dir, member_name, MemberStatus::Completed)?;
                complete_member_tasks_if_active(team_dir, member_name)?;
            }
            append_event(
                team_dir,
                if run.member.role == "lead" {
                    "app_server_lead_completed"
                } else {
                    "app_server_member_completed"
                },
                serde_json::json!({
                    "member": member_name,
                    "node": node_id,
                    "thread": completed.thread_id,
                    "turn": completed.turn.id,
                }),
            )?;
            acknowledge_side_channel_contexts(
                team_dir,
                member_name,
                &run.side_context_ids,
                &completed.turn.id,
            )?;
            run.side_context_ids.clear();
        }
        _ => {
            let status = format!("{:?}", completed.turn.status);
            let error = completed.turn.error.map(|err| err.message);
            if let Some(cooldown) = app_server_usage_limit_cooldown(error.as_deref()) {
                run.failed = false;
                run.retry_not_before = Some(Instant::now() + cooldown);
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                append_event(
                    team_dir,
                    "app_server_member_usage_limited",
                    serde_json::json!({
                        "member": member_name,
                        "node": node_id,
                        "thread": completed.thread_id,
                        "turn": completed.turn.id,
                        "status": status,
                        "error": error,
                        "retry_after_sec": cooldown.as_secs(),
                    }),
                )?;
            } else {
                run.failed = true;
                run.retry_not_before = None;
                set_member_status(team_dir, member_name, MemberStatus::Failed)?;
                append_event(
                    team_dir,
                    "app_server_member_failed",
                    serde_json::json!({
                        "member": member_name,
                        "node": node_id,
                        "thread": completed.thread_id,
                        "turn": completed.turn.id,
                        "status": status,
                        "error": error,
                    }),
                )?;
            }
        }
    }
    ingest_team_signal_lines(team_dir, member_name, active, assistant_buffers, true)?;
    Ok(())
}

fn handle_app_server_side_reply_completed(
    team_dir: &Path,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    node_id: &str,
    completed: TurnCompletedNotification,
) -> Result<()> {
    let key = thread_key(node_id, &completed.thread_id);
    let Some(reply) = side_replies.remove(&key) else {
        return Ok(());
    };
    let elapsed = reply.started_at.elapsed().as_secs();
    match completed.turn.status {
        TurnStatus::Completed => {
            let language = load_config(team_dir)?.language.unwrap_or_default();
            let body = side_reply_message_body(&reply, language);
            if body.trim().is_empty() {
                append_event(
                    team_dir,
                    "app_server_side_channel_reply_empty",
                    serde_json::json!({
                        "member": reply.member.name,
                        "node": reply.node_id,
                        "source_thread": reply.source_thread_id,
                        "side_thread": reply.side_thread_id,
                        "turn": completed.turn.id,
                        "elapsed_sec": elapsed,
                    }),
                )?;
                return Ok(());
            }
            for recipient in &reply.recipients {
                send_team_message_to_dir(team_dir, &reply.member.name, recipient, &body)?;
            }
            let handoff = if language.is_ja() {
                format!(
                    "Side-channel reply: あなたの main turn が busy の間に短い返信を送りました。\n\n宛先: {}\n\n処理した受信 message:\n{}\n\n送信した返信:\n{}\n\nこの返信で生じた約束や制約を、実務上可能なタイミングで main work に取り込んでください。action が必要な場合以外は重複 chat を避けてください。",
                    reply.recipients.join(", "),
                    summarize_side_reply_messages(&reply.messages, language),
                    body
                )
            } else {
                format!(
                    "Side-channel reply sent while your main turn was busy.\n\nRecipients: {}\n\nIncoming messages handled:\n{}\n\nReply sent:\n{}\n\nIncorporate any resulting commitments or constraints into your main work when practical. Avoid duplicate chat unless action is needed.",
                    reply.recipients.join(", "),
                    summarize_side_reply_messages(&reply.messages, language),
                    body
                )
            };
            record_side_channel_context(
                team_dir,
                &reply,
                completed.turn.id.clone(),
                &body,
                language,
            )?;
            send_team_message_to_dir(team_dir, "system", &reply.member.name, &handoff)?;
            append_event(
                team_dir,
                "app_server_side_channel_reply_completed",
                serde_json::json!({
                    "member": reply.member.name,
                    "node": reply.node_id,
                    "source_thread": reply.source_thread_id,
                    "side_thread": reply.side_thread_id,
                    "turn": completed.turn.id,
                    "recipients": reply.recipients,
                    "messages": reply.messages.len(),
                    "elapsed_sec": elapsed,
                }),
            )?;
        }
        _ => {
            append_event(
                team_dir,
                "app_server_side_channel_reply_failed",
                serde_json::json!({
                    "member": reply.member.name,
                    "node": reply.node_id,
                    "source_thread": reply.source_thread_id,
                    "side_thread": reply.side_thread_id,
                    "turn": completed.turn.id,
                    "status": format!("{:?}", completed.turn.status),
                    "error": completed.turn.error.map(|err| err.message),
                    "elapsed_sec": elapsed,
                }),
            )?;
        }
    }
    Ok(())
}

fn side_channel_context_path(team_dir: &Path, member_name: &str) -> PathBuf {
    team_dir
        .join("side_channel_contexts")
        .join(format!("{}.jsonl", sanitize_id(member_name)))
}

fn record_side_channel_context(
    team_dir: &Path,
    reply: &AppServerSideReply,
    side_turn: String,
    body: &str,
    language: TeamPromptLanguage,
) -> Result<()> {
    let id = sanitize_id(&format!(
        "sidectx-{}-{}-{}",
        reply.member.name, reply.side_thread_id, side_turn
    ));
    let record = SideChannelContextRecord {
        id: id.clone(),
        member: reply.member.name.clone(),
        node: reply.node_id.clone(),
        source_thread: reply.source_thread_id.clone(),
        side_thread: reply.side_thread_id.clone(),
        side_turn,
        recipients: reply.recipients.clone(),
        incoming_summary: summarize_side_reply_messages(&reply.messages, language),
        reply: body.to_string(),
        created_at: now(),
        status: SideChannelContextStatus::Pending,
        injected_turns: Vec::new(),
        injected_at: None,
        acknowledged_at: None,
    };
    append_jsonl(
        &side_channel_context_path(team_dir, &reply.member.name),
        &record,
    )?;
    append_event(
        team_dir,
        "side_channel_context_pending",
        serde_json::json!({
            "member": reply.member.name,
            "node": reply.node_id,
            "source_thread": reply.source_thread_id,
            "side_thread": reply.side_thread_id,
            "side_turn": record.side_turn.clone(),
            "context_id": id,
            "recipients": reply.recipients.clone(),
        }),
    )?;
    Ok(())
}

fn pending_side_channel_contexts_for_turn(
    team_dir: &Path,
    member_name: &str,
    turn_id: &str,
) -> Result<Vec<SideChannelContextRecord>> {
    Ok(
        read_jsonl::<SideChannelContextRecord>(&side_channel_context_path(team_dir, member_name))?
            .into_iter()
            .filter(|record| {
                record.status != SideChannelContextStatus::Acknowledged
                    && !record.injected_turns.iter().any(|id| id == turn_id)
            })
            .collect(),
    )
}

fn append_side_channel_context_prompt(
    team_dir: &Path,
    member_name: &str,
    turn_id: &str,
    prompt: String,
    language: TeamPromptLanguage,
) -> Result<(String, Vec<String>)> {
    let contexts = pending_side_channel_contexts_for_turn(team_dir, member_name, turn_id)?;
    if contexts.is_empty() {
        return Ok((prompt, Vec::new()));
    }
    let mut out = prompt;
    if language.is_ja() {
        out.push_str("\n\nあなたの main turn に未反映の side-channel context があります:\n");
        out.push_str(
            "この main thread が busy の間に、あなた名義で短い side-channel 返信が送信されました。team message で明示的に訂正しない限り、team に見える約束や制約として扱ってください。\n",
        );
        out.push_str(
            "続行する前に、現在の plan と artifact をこれらの side-channel commitment と突き合わせてください。停止、fail closed、claim scope 変更、evidence 保持、handoff 更新などを約束している場合は、その更新を実施して機械可読 artifact/manifest を検証するか、理由付きで撤回/訂正する team message を直ちに送ってください。side-channel commitment と矛盾する古い artifact に依存したり、handoff/task 完了をしないでください。\n",
        );
    } else {
        out.push_str("\n\nPending side-channel context for your main turn:\n");
        out.push_str(
            "The following fast side-channel replies were sent as you while this main thread was busy. Treat them as team-visible commitments or constraints unless you explicitly correct them with a team message.\n",
        );
        out.push_str(
            "Before you continue, reconcile your current plan and artifacts against these side-channel commitments. If a side-channel reply promised to stop, fail closed, change claim scope, preserve evidence, or update a handoff, you must either perform that update and verify the resulting machine-readable artifacts/manifests, or immediately send a team message explicitly retracting/correcting the side-channel reply with the reason. Do not hand off, complete a task, or rely on stale artifacts that contradict a side-channel commitment.\n",
        );
    }
    for context in &contexts {
        if language.is_ja() {
            out.push_str(&format!(
                "\n[{}]\n宛先: {}\n処理した受信 message:\n{}\nすでに送信済みの返信:\n{}\n",
                context.id,
                context.recipients.join(", "),
                context.incoming_summary,
                context.reply
            ));
        } else {
            out.push_str(&format!(
                "\n[{}]\nRecipients: {}\nIncoming handled:\n{}\nReply already sent:\n{}\n",
                context.id,
                context.recipients.join(", "),
                context.incoming_summary,
                context.reply
            ));
        }
    }
    if language.is_ja() {
        out.push_str("\n続行前に、これらの制約を取り込み、検証してください。\n");
    } else {
        out.push_str("\nIncorporate and verify these constraints before continuing.\n");
    }
    Ok((
        out,
        contexts.into_iter().map(|context| context.id).collect(),
    ))
}

fn mark_side_channel_contexts_injected(
    team_dir: &Path,
    member_name: &str,
    context_ids: &[String],
    turn_id: &str,
) -> Result<()> {
    if context_ids.is_empty() {
        return Ok(());
    }
    let path = side_channel_context_path(team_dir, member_name);
    let mut records = read_jsonl::<SideChannelContextRecord>(&path)?;
    let mut changed = false;
    for record in &mut records {
        if context_ids.iter().any(|id| id == &record.id) {
            record.status = SideChannelContextStatus::Injected;
            if !record.injected_turns.iter().any(|id| id == turn_id) {
                record.injected_turns.push(turn_id.to_string());
            }
            record.injected_at = Some(now());
            changed = true;
        }
    }
    if changed {
        write_jsonl_atomic(&path, &records)?;
        append_event(
            team_dir,
            "side_channel_context_injected",
            serde_json::json!({
                "member": member_name,
                "turn": turn_id,
                "context_ids": context_ids,
            }),
        )?;
    }
    Ok(())
}

fn acknowledge_side_channel_contexts(
    team_dir: &Path,
    member_name: &str,
    context_ids: &[String],
    turn_id: &str,
) -> Result<()> {
    if context_ids.is_empty() {
        return Ok(());
    }
    let path = side_channel_context_path(team_dir, member_name);
    let mut records = read_jsonl::<SideChannelContextRecord>(&path)?;
    let mut acknowledged = Vec::new();
    for record in &mut records {
        if context_ids.iter().any(|id| id == &record.id)
            && record.status != SideChannelContextStatus::Acknowledged
        {
            record.status = SideChannelContextStatus::Acknowledged;
            record.acknowledged_at = Some(now());
            acknowledged.push(record.id.clone());
        }
    }
    if !acknowledged.is_empty() {
        write_jsonl_atomic(&path, &records)?;
        append_event(
            team_dir,
            "side_channel_context_acknowledged",
            serde_json::json!({
                "member": member_name,
                "turn": turn_id,
                "context_ids": acknowledged,
            }),
        )?;
    }
    Ok(())
}

fn merge_side_context_ids(run: &mut AppServerMemberRun, context_ids: &[String]) {
    for context_id in context_ids {
        if !run.side_context_ids.iter().any(|id| id == context_id) {
            run.side_context_ids.push(context_id.clone());
        }
    }
}

fn side_reply_message_body(reply: &AppServerSideReply, language: TeamPromptLanguage) -> String {
    let body = reply.buffer.trim();
    if body.is_empty() {
        return String::new();
    }
    if language.is_ja() {
        format!(
            "@{} からの side-channel 速報返信です。main turn は継続中です:\n\n{}",
            reply.member.name, body
        )
    } else {
        format!(
            "Quick side-channel reply from @{} while my main turn continues:\n\n{}",
            reply.member.name, body
        )
    }
}

fn summarize_side_reply_messages(messages: &[MailMessage], language: TeamPromptLanguage) -> String {
    messages
        .iter()
        .map(|message| {
            if language.is_ja() {
                format!(
                    "- @{} から {}: {}",
                    message.from,
                    message.timestamp,
                    compact_one_line(&message.message, 500)
                )
            } else {
                format!(
                    "- from @{} at {}: {}",
                    message.from,
                    message.timestamp,
                    compact_one_line(&message.message, 500)
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn compact_one_line(value: &str, max_chars: usize) -> String {
    let mut compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > max_chars {
        compact = compact.chars().take(max_chars).collect::<String>();
        compact.push_str("...");
    }
    compact
}

fn member_turn_reports_blocked(
    assistant_buffers: &HashMap<String, String>,
    member_name: &str,
) -> bool {
    let Some(text) = assistant_buffers.get(member_name) else {
        return false;
    };
    let tail = text
        .chars()
        .rev()
        .take(5000)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_lowercase();
    let blocked_markers = [
        "blocked on",
        "blocked by",
        "waiting on",
        "waiting for",
        "wait for",
        "blocked until",
        "blocked pending",
        "pending lead clearance",
        "pending explicit lead",
        "until explicit lead",
        "requires lead clearance",
        "require lead clearance",
        "awaiting lead clearance",
        "holding until",
        "hold until",
        "paused until",
        "gate wait",
        "gate remains",
        "remains gated",
        "not started",
        "no model-specific",
        "handoff待ち",
        "結果待ち",
        "研究待ち",
        "ゲート待ち",
        "未着",
        "待機",
    ];
    blocked_markers.iter().any(|marker| tail.contains(marker))
}

#[cfg(test)]
fn member_turn_has_completion_checklist(
    assistant_buffers: &HashMap<String, String>,
    member_name: &str,
) -> bool {
    member_turn_completion_checklist_issue(assistant_buffers, member_name).is_none()
}

fn member_turn_completion_checklist_issue(
    assistant_buffers: &HashMap<String, String>,
    member_name: &str,
) -> Option<String> {
    let Some(text) = assistant_buffers.get(member_name) else {
        return Some("no assistant output was captured".to_string());
    };
    let tail = text
        .chars()
        .rev()
        .take(8000)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_lowercase();
    if !tail.contains("team_completion_checklist:") {
        return Some("missing TEAM_COMPLETION_CHECKLIST marker".to_string());
    }
    for field in [
        "artifacts:",
        "verification:",
        "messages_sent:",
        "consumers_notified:",
        "blockers_or_limits:",
    ] {
        if !tail.contains(field) {
            return Some(format!("missing `{field}` field"));
        }
    }
    let messages_sent = checklist_field_value(&tail, "messages_sent:");
    if checklist_value_is_empty_or_unknown(messages_sent.as_deref()) {
        return Some(
            "messages_sent is empty/unknown; final handoff message was not evidenced".to_string(),
        );
    }
    let consumers_notified = checklist_field_value(&tail, "consumers_notified:");
    if checklist_value_is_empty_or_unknown(consumers_notified.as_deref()) {
        return Some(
            "consumers_notified is empty/unknown; artifact consumers were not evidenced"
                .to_string(),
        );
    }
    None
}

fn member_turn_active_task_completion_issue(
    team_dir: &Path,
    assistant_buffers: &HashMap<String, String>,
    member_name: &str,
) -> Result<Option<String>> {
    if let Some(issue) = member_turn_completion_checklist_issue(assistant_buffers, member_name) {
        return Ok(Some(issue));
    }
    let Some(text) = assistant_buffers.get(member_name) else {
        return Ok(Some("no assistant output was captured".to_string()));
    };
    let tail = text
        .chars()
        .rev()
        .take(12000)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>()
        .to_lowercase();
    let checklist = tail
        .rfind("team_completion_checklist:")
        .map(|idx| &tail[idx..])
        .unwrap_or(&tail);
    if checklist_value_is_empty_or_unknown(
        checklist_field_value(checklist, "artifacts:").as_deref(),
    ) {
        return Ok(Some(
            "artifacts is empty/unknown; final handoff artifacts were not evidenced".to_string(),
        ));
    }
    if checklist_value_is_empty_or_unknown(
        checklist_field_value(checklist, "verification:").as_deref(),
    ) {
        return Ok(Some(
            "verification is empty/unknown; final handoff verification was not evidenced"
                .to_string(),
        ));
    }

    let tasks = load_tasks(team_dir)?;
    for task in tasks.into_iter().filter(|task| {
        task.owner.as_deref() == Some(member_name)
            && matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Ready
                    | TaskStatus::InProgress
                    | TaskStatus::Review
            )
    }) {
        let required = task_required_declared_non_local_output_paths(team_dir, &task)?;
        if required.is_empty() {
            continue;
        }
        let missing = required
            .iter()
            .filter(|path| !checklist.contains(&path.to_ascii_lowercase()))
            .take(3)
            .cloned()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Ok(Some(format!(
                "final checklist does not cite required non-local task output path(s): {}",
                missing.join(", ")
            )));
        }
    }
    Ok(None)
}

fn checklist_field_value(text: &str, field: &str) -> Option<String> {
    let start = text.find(field)? + field.len();
    let rest = &text[start..];
    let end = rest
        .find('\n')
        .map(|idx| start + idx)
        .unwrap_or_else(|| text.len());
    Some(
        text[start..end]
            .trim()
            .trim_start_matches('-')
            .trim()
            .to_string(),
    )
}

fn checklist_value_is_empty_or_unknown(value: Option<&str>) -> bool {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return true;
    };
    let value = value.to_ascii_lowercase();
    matches!(
        value.as_str(),
        "none" | "n/a" | "na" | "unknown" | "missing" | "tbd" | "not sure"
    ) || ["none", "n/a", "na", "unknown", "missing", "tbd", "not sure"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn member_has_active_tasks(team_dir: &Path, member_name: &str) -> Result<bool> {
    Ok(load_tasks(team_dir)?.iter().any(|task| {
        task.owner.as_deref() == Some(member_name)
            && matches!(task.status, TaskStatus::InProgress | TaskStatus::Review)
    }))
}

fn ingest_team_signal_lines(
    team_dir: &Path,
    member_name: &str,
    active: &mut HashMap<String, AppServerMemberRun>,
    assistant_buffers: &HashMap<String, String>,
    final_flush: bool,
) -> Result<()> {
    let Some(run) = active.get_mut(member_name) else {
        return Ok(());
    };
    let Some(buffer) = assistant_buffers.get(member_name) else {
        return Ok(());
    };
    let offset = run.team_message_scan_offset.min(buffer.len());
    let new_text = &buffer[offset..];
    let scan_end = if final_flush {
        buffer.len()
    } else {
        let complete_len = new_text.rfind('\n').map(|idx| idx + 1).unwrap_or(0);
        offset + complete_len
    };
    if scan_end <= offset {
        return Ok(());
    }
    let new_text = &buffer[offset..scan_end];
    run.team_message_scan_offset = scan_end;
    let config = load_config(team_dir)?;
    for line in new_text.lines() {
        let Some((to, message)) = parse_team_message_line(line) else {
            continue;
        };
        let recipients = resolve_message_recipients(&config, member_name, &to)?;
        for recipient in &recipients {
            let msg = MailMessage {
                from: member_name.to_string(),
                to: recipient.clone(),
                message: message.clone(),
                timestamp: now(),
                read: false,
            };
            append_jsonl(&mailbox_path(team_dir, &msg.to), &msg)?;
        }
        append_event(
            team_dir,
            "team_message_ingested",
            serde_json::json!({
                "from": member_name,
                "to": recipients,
                "message": message,
                "source": "assistant_text",
            }),
        )?;
    }
    for line in new_text.lines() {
        let task_update = match parse_team_task_line(line) {
            Ok(Some(task_update)) => task_update,
            Ok(None) => continue,
            Err(err) => {
                append_event(
                    team_dir,
                    "team_task_parse_failed",
                    serde_json::json!({
                        "from": member_name,
                        "line": line.trim().chars().take(500).collect::<String>(),
                        "error": err.to_string(),
                        "source": "assistant_text",
                    }),
                )?;
                continue;
            }
        };
        let changed = set_task_status_if_open(
            team_dir,
            &task_update.id,
            task_update.status,
            task_update.result.as_deref(),
        )?;
        append_event(
            team_dir,
            "team_task_ingested",
            serde_json::json!({
                "from": member_name,
                "task": task_update.id,
                "status": task_update.status.to_string(),
                "result": task_update.result,
                "changed": changed,
                "source": "assistant_text",
            }),
        )?;
    }
    for line in new_text.lines() {
        let wait_update = match parse_team_wait_line(line) {
            Ok(Some(wait_update)) => wait_update,
            Ok(None) => continue,
            Err(err) => {
                append_event(
                    team_dir,
                    "team_wait_parse_failed",
                    serde_json::json!({
                        "from": member_name,
                        "line": line.trim().chars().take(500).collect::<String>(),
                        "error": err.to_string(),
                        "source": "assistant_text",
                    }),
                )?;
                continue;
            }
        };
        match ingest_team_wait_fallback(team_dir, member_name, wait_update) {
            Ok(wait_id) => append_event(
                team_dir,
                "team_wait_ingested",
                serde_json::json!({
                    "from": member_name,
                    "wait": wait_id,
                    "source": "assistant_text",
                }),
            )?,
            Err(err) => append_event(
                team_dir,
                "team_wait_ingest_failed",
                serde_json::json!({
                    "from": member_name,
                    "line": line.trim().chars().take(500).collect::<String>(),
                    "error": err.to_string(),
                    "source": "assistant_text",
                }),
            )?,
        }
    }
    for line in new_text.lines() {
        let node_args = match parse_team_node_line(line) {
            Ok(Some(node_args)) => node_args,
            Ok(None) => continue,
            Err(err) => {
                append_event(
                    team_dir,
                    "team_node_parse_failed",
                    serde_json::json!({
                        "from": member_name,
                        "line": line.trim().chars().take(500).collect::<String>(),
                        "error": err.to_string(),
                        "source": "assistant_text",
                    }),
                )?;
                continue;
            }
        };
        let node_id = node_args.id.clone();
        match add_team_node(team_dir, node_args) {
            Ok(()) => {
                ensure_container_node_departments(team_dir)?;
                append_event(
                    team_dir,
                    "team_node_ingested",
                    serde_json::json!({
                        "from": member_name,
                        "node": node_id,
                        "source": "assistant_text",
                    }),
                )?;
            }
            Err(err) => {
                append_event(
                    team_dir,
                    "team_node_ingest_failed",
                    serde_json::json!({
                        "from": member_name,
                        "node": node_id,
                        "error": err.to_string(),
                        "source": "assistant_text",
                    }),
                )?;
            }
        }
    }
    Ok(())
}

struct TeamTaskFallback {
    id: String,
    status: TaskStatus,
    result: Option<String>,
}

struct TeamWaitFallback {
    id: Option<String>,
    title: String,
    status: TeamWaitStatus,
    task_id: Option<String>,
    condition: String,
    progress: String,
    evidence: Option<String>,
}

fn parse_team_message_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let marker = line.find("TEAM_MESSAGE ")?;
    let rest = &line[marker + "TEAM_MESSAGE ".len()..];
    let rest = rest.strip_prefix("to=")?;
    let (to, message) = rest.split_once(':')?;
    let to = to.trim().to_string();
    let message = message.trim();
    if to.is_empty() || message.is_empty() {
        return None;
    }
    Some((to, message.to_string()))
}

fn parse_team_task_line(line: &str) -> Result<Option<TeamTaskFallback>> {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("TEAM_TASK ") else {
        return Ok(None);
    };
    let (fields_text, result) = match rest.split_once(" result=") {
        Some((fields, result)) => (
            fields,
            Some(
                result
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string(),
            ),
        ),
        None => (rest, None),
    };
    let mut fields = HashMap::<String, String>::new();
    for token in fields_text.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        fields.insert(
            key.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    let id = fields
        .remove("id")
        .filter(|value| !value.trim().is_empty())
        .context("TEAM_TASK needs id=<task-id>")?;
    if id.contains('<') || id.contains('>') {
        bail!("TEAM_TASK id must be concrete, not a placeholder");
    }
    let status = fields
        .remove("status")
        .filter(|value| !value.trim().is_empty())
        .context("TEAM_TASK needs status=<status>")?;
    Ok(Some(TeamTaskFallback {
        id,
        status: parse_task_status(&status)?,
        result: result.filter(|value| !value.trim().is_empty()),
    }))
}

fn parse_team_wait_line(line: &str) -> Result<Option<TeamWaitFallback>> {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("TEAM_WAIT ") else {
        return Ok(None);
    };
    let parts = rest.split(" | ").collect::<Vec<_>>();
    let head = parts.first().copied().unwrap_or_default();
    let mut fields = HashMap::<String, String>::new();
    for token in head.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        fields.insert(
            key.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    for part in parts.into_iter().skip(1) {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        fields.insert(
            key.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    let title = fields
        .remove("title")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "external wait".to_string());
    let status = fields
        .remove("status")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "waiting".to_string());
    let id = fields.remove("id").filter(|value| !value.trim().is_empty());
    if id
        .as_deref()
        .is_some_and(|id| id.contains('<') || id.contains('>'))
    {
        bail!("TEAM_WAIT id must be concrete, not a placeholder");
    }
    let task_id = fields
        .remove("task")
        .filter(|value| !value.trim().is_empty());
    if task_id
        .as_deref()
        .is_some_and(|task| task.contains('<') || task.contains('>'))
    {
        bail!("TEAM_WAIT task must be concrete, not a placeholder");
    }
    Ok(Some(TeamWaitFallback {
        id,
        title,
        status: parse_wait_status(&status)?,
        task_id,
        condition: fields.remove("condition").unwrap_or_default(),
        progress: fields.remove("progress").unwrap_or_default(),
        evidence: fields
            .remove("evidence")
            .filter(|value| !value.trim().is_empty()),
    }))
}

fn ingest_team_wait_fallback(
    team_dir: &Path,
    member_name: &str,
    wait_update: TeamWaitFallback,
) -> Result<String> {
    let config = load_config(team_dir)?;
    ensure_member_exists(&config, member_name)?;
    if let Some(wait_id) = wait_update.id.as_deref()
        && wait_path(team_dir, wait_id).exists()
    {
        set_team_wait(
            team_dir,
            WaitSetArgs {
                id: wait_id.to_string(),
                status: Some(wait_update.status),
                progress: Some(wait_update.progress),
                evidence: wait_update.evidence,
                clear_evidence: false,
            },
        )?;
        return Ok(wait_id.to_string());
    }

    let id = wait_update
        .id
        .unwrap_or_else(|| allocate_wait_id(team_dir).unwrap_or_else(|_| "wait-1".to_string()));
    if wait_path(team_dir, &id).exists() {
        bail!("wait `{id}` already exists");
    }
    if let Some(task_id) = wait_update.task_id.as_deref() {
        let tasks = load_tasks(team_dir)?;
        if !tasks.iter().any(|task| task.id == task_id) {
            bail!("task `{task_id}` does not exist");
        }
        set_task_status_if_open(
            team_dir,
            task_id,
            TaskStatus::Waiting,
            Some(&format!("Waiting on `{id}`: {}", wait_update.title)),
        )?;
    }
    let now = now();
    let wait = TeamWait {
        id: id.clone(),
        title: wait_update.title,
        owner: Some(member_name.to_string()),
        task_id: wait_update.task_id,
        node: None,
        condition: wait_update.condition,
        status: wait_update.status,
        progress: wait_update.progress,
        evidence: wait_update.evidence,
        created_at: now.clone(),
        updated_at: now,
    };
    fs::create_dir_all(waits_dir(team_dir))?;
    write_json_atomic(&wait_path(team_dir, &id), &wait)?;
    Ok(id)
}

fn parse_team_node_line(line: &str) -> Result<Option<NodeAddArgs>> {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("TEAM_NODE ") else {
        return Ok(None);
    };
    let mut fields = HashMap::<String, String>::new();
    for token in rest.split_whitespace() {
        let Some((key, value)) = token.split_once('=') else {
            continue;
        };
        fields.insert(
            key.trim().to_ascii_lowercase(),
            value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string(),
        );
    }
    let id = fields
        .remove("id")
        .filter(|value| !value.trim().is_empty())
        .context("TEAM_NODE needs id=<node-id>")?;
    if id.contains('<') || id.contains('>') {
        bail!("TEAM_NODE id must be concrete, not a placeholder");
    }
    let kind = match fields
        .remove("kind")
        .unwrap_or_else(|| "docker".to_string())
        .replace('_', "-")
        .as_str()
    {
        "docker" => TeamNodeKind::Docker,
        "ssh-docker" => TeamNodeKind::SshDocker,
        other => bail!("TEAM_NODE unsupported kind `{other}`"),
    };
    let host = fields
        .remove("host")
        .filter(|value| !value.is_empty() && value != "-");
    if matches!(kind, TeamNodeKind::SshDocker) && host.is_none() {
        bail!("TEAM_NODE kind=ssh-docker needs host=<ssh-host>");
    }
    let container = fields
        .remove("container")
        .filter(|value| !value.trim().is_empty())
        .context("TEAM_NODE needs container=<container-name>")?;
    if container.contains('<') || container.contains('>') {
        bail!("TEAM_NODE container must be concrete, not a placeholder");
    }
    let cwd = fields
        .remove("cwd")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some("/workspace".to_string()));
    let note = fields
        .remove("note")
        .unwrap_or_else(|| "Docker node reported by a team department.".to_string())
        .replace('_', " ");
    Ok(Some(NodeAddArgs {
        id,
        kind,
        url: None,
        host,
        container: Some(container),
        cwd,
        note,
    }))
}

async fn reject_app_server_request(
    client: &mut RemoteAppServerClient,
    request: ServerRequest,
) -> Result<()> {
    let request_id = request.id().clone();
    client
        .reject_server_request(
            request_id,
            JSONRPCErrorError {
                code: -32000,
                message: "codex team app-server mode does not handle interactive approvals; rerun with --dangerously-bypass-approvals-and-sandbox or a non-interactive permission profile".to_string(),
                data: None,
            },
        )
        .await
        .context("reject app-server server request")
}

#[allow(clippy::too_many_arguments)]
async fn sync_dynamic_app_server_members(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    nodes: &[TeamNode],
    team_dir: &Path,
    config: &mut TeamConfig,
    active: &mut HashMap<String, AppServerMemberRun>,
    thread_to_member: &mut HashMap<String, String>,
    assistant_buffers: &mut HashMap<String, String>,
    mailbox_counts: &mut HashMap<String, usize>,
    node_processes: &mut Vec<NodeAppServerProcess>,
    cwd: &Path,
    model: Option<String>,
    sandbox: Option<SandboxMode>,
    approval_policy: Option<AskForApproval>,
    dangerously_bypass_approvals_and_sandbox: bool,
    codex_exe: &Path,
    relay_port: u16,
    language: TeamPromptLanguage,
) -> Result<()> {
    let latest = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    for member in latest.members.iter().filter(|member| member.role != "lead") {
        if !matches!(member.status, MemberStatus::Online | MemberStatus::Running) {
            continue;
        }
        let has_active_task = tasks.iter().any(|task| {
            task.owner.as_deref() == Some(member.name.as_str())
                && task_status_can_start_turn(task.status)
        });
        if !has_active_task {
            continue;
        }
        if let Some(remaining) = recent_usage_limit_retry_remaining(team_dir, &member.name)? {
            append_event(
                team_dir,
                "app_server_dynamic_member_start_deferred",
                serde_json::json!({
                    "member": member.name,
                    "node": member_node_id(member),
                    "reason": "recent app-server/model usage-limit cooldown",
                    "retry_after_sec": remaining.as_secs(),
                }),
            )?;
            set_member_status(team_dir, &member.name, MemberStatus::Standby)?;
            continue;
        }
        if let Some(existing) = active.get(&member.name) {
            if !existing.completed {
                continue;
            }
            let old_node_id = existing.node_id.clone();
            let old_thread_id = existing.thread_id.clone();
            let old_turn_id = existing.turn_id.clone();
            thread_to_member.remove(&thread_key(&old_node_id, &old_thread_id));
            assistant_buffers.remove(&member.name);
            active.remove(&member.name);
            append_event(
                team_dir,
                "app_server_completed_member_restarting",
                serde_json::json!({
                    "member": member.name,
                    "old_node": old_node_id,
                    "old_thread": old_thread_id,
                    "old_turn": old_turn_id,
                    "reason": "member is online/running with unfinished assigned task",
                }),
            )?;
        }

        set_member_status(team_dir, &member.name, MemberStatus::Running)?;
        mark_member_tasks(team_dir, &member.name, TaskStatus::InProgress)?;
        let node_id = member_node_id(member);
        if !node_clients.contains_key(&node_id) {
            let node = nodes
                .iter()
                .find(|node| node.id == node_id)
                .cloned()
                .with_context(|| format!("node `{node_id}` is not registered"))?;
            let (url, process) = match resolve_or_spawn_node_app_server(team_dir, &node, relay_port)
            {
                Ok(result) => result,
                Err(err) => {
                    append_event(
                        team_dir,
                        "app_server_node_reconnect_failed",
                        serde_json::json!({
                            "node": node_id,
                            "member": member.name,
                            "error": err.to_string(),
                        }),
                    )?;
                    set_member_status(team_dir, &member.name, MemberStatus::Online)?;
                    continue;
                }
            };
            if let Some(process) = process {
                node_processes.push(process);
            }
            let connected_client = match connect_team_app_server(&url).await {
                Ok(client) => client,
                Err(err) => {
                    append_event(
                        team_dir,
                        "app_server_node_reconnect_failed",
                        serde_json::json!({
                            "node": node_id,
                            "member": member.name,
                            "url": url,
                            "error": err.to_string(),
                        }),
                    )?;
                    set_member_status(team_dir, &member.name, MemberStatus::Online)?;
                    continue;
                }
            };
            append_event(
                team_dir,
                "app_server_node_connected",
                serde_json::json!({
                    "node": node_id,
                    "kind": node.kind,
                    "url": url,
                    "source": "dynamic_member",
                }),
            )?;
            set_node_connection(
                team_dir,
                &node_id,
                TeamNodeStatus::Online,
                Some(url.clone()),
            )?;
            node_clients.insert(
                node_id.clone(),
                TeamAppServerNodeClient {
                    client: connected_client,
                    request_counter: 1,
                },
            );
        }
        let member_cwd = app_server_member_cwd(&node_id, nodes, cwd);
        let node_client = node_clients
            .get_mut(&node_id)
            .with_context(|| format!("app-server client missing for node `{node_id}`"))?;
        let thread: ThreadStartResponse = node_client
            .client
            .request_typed(ClientRequest::ThreadStart {
                request_id: next_request_id(&mut node_client.request_counter),
                params: ThreadStartParams {
                    model: model.clone(),
                    cwd: Some(member_cwd.display().to_string()),
                    sandbox,
                    approval_policy,
                    ephemeral: Some(false),
                    ..ThreadStartParams::default()
                },
            })
            .await
            .map_err(|err| anyhow!(err))?;
        set_member_thread(team_dir, &member.name, &thread.thread.id)?;
        set_member_workspace(team_dir, &member.name, &member_cwd)?;

        let current_config = load_config(team_dir)?;
        let current_tasks = load_tasks(team_dir)?;
        let prompt = build_app_server_worker_prompt(
            &current_config,
            &current_tasks,
            member,
            codex_exe,
            nodes,
            language,
        );
        let turn: TurnStartResponse = node_client
            .client
            .request_typed(ClientRequest::TurnStart {
                request_id: next_request_id(&mut node_client.request_counter),
                params: TurnStartParams {
                    thread_id: thread.thread.id.clone(),
                    input: vec![text_input(prompt)],
                    cwd: Some(member_cwd.clone()),
                    model: model.clone(),
                    approval_policy,
                    sandbox_policy: if dangerously_bypass_approvals_and_sandbox {
                        Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess)
                    } else {
                        None
                    },
                    ..TurnStartParams::default()
                },
            })
            .await
            .map_err(|err| anyhow!(err))?;

        thread_to_member.insert(thread_key(&node_id, &thread.thread.id), member.name.clone());
        assistant_buffers.insert(member.name.clone(), String::new());
        mailbox_counts
            .entry(member.name.clone())
            .or_insert(mailbox_seen_count(&read_jsonl::<MailMessage>(
                &mailbox_path(team_dir, &member.name),
            )?));
        active.insert(
            member.name.clone(),
            AppServerMemberRun {
                member: member.clone(),
                node_id: node_id.clone(),
                cwd: member_cwd.clone(),
                thread_id: thread.thread.id.clone(),
                turn_id: turn.turn.id.clone(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "turn_started".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );
        println!(
            "Started dynamic {} ({}) thread={} turn={}",
            member.name, member.role, thread.thread.id, turn.turn.id
        );
        append_event(
            team_dir,
            "app_server_dynamic_member_started",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "thread": thread.thread.id,
                "turn": turn.turn.id,
                "node": node_id,
                "cwd": member_cwd,
            }),
        )?;
    }
    *config = load_config(team_dir)?;
    Ok(())
}

async fn sync_removed_app_server_nodes(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    node_processes: &mut Vec<NodeAppServerProcess>,
    nodes: &[TeamNode],
    team_dir: &Path,
    active: &HashMap<String, AppServerMemberRun>,
) -> Result<()> {
    let known = nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
    let config = load_config(team_dir)?;
    let connected = node_clients.keys().cloned().collect::<Vec<_>>();
    for node_id in connected {
        if node_id == "local" || known.contains(&node_id) {
            continue;
        }
        let active_member = active.values().any(|run| {
            run.node_id == node_id
                && !run.completed
                && config
                    .members
                    .iter()
                    .find(|member| member.name == run.member.name)
                    .map(|member| {
                        !matches!(
                            member.status,
                            MemberStatus::Standby
                                | MemberStatus::Completed
                                | MemberStatus::Failed
                                | MemberStatus::Offline
                        )
                    })
                    .unwrap_or(false)
        });
        if active_member {
            append_event(
                team_dir,
                "app_server_node_remove_deferred",
                serde_json::json!({
                    "node": node_id,
                    "reason": "node still has an active member",
                }),
            )?;
            continue;
        }
        if let Some(client) = node_clients.remove(&node_id) {
            client
                .client
                .shutdown()
                .await
                .with_context(|| format!("shutdown removed node `{node_id}` client"))?;
        }
        let mut idx = 0;
        while idx < node_processes.len() {
            if node_processes[idx].node_id == node_id {
                let process = node_processes.remove(idx);
                process.stop();
            } else {
                idx += 1;
            }
        }
        append_event(
            team_dir,
            "app_server_node_disconnected",
            serde_json::json!({ "node": node_id, "reason": "node removed" }),
        )?;
    }
    Ok(())
}

fn has_unstarted_app_server_members(
    team_dir: &Path,
    active: &HashMap<String, AppServerMemberRun>,
) -> Result<bool> {
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    Ok(config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .any(|member| {
            !active.contains_key(&member.name)
                && matches!(member.status, MemberStatus::Online | MemberStatus::Running)
                && tasks.iter().any(|task| {
                    task.owner.as_deref() == Some(member.name.as_str())
                        && task_status_can_start_turn(task.status)
                })
        }))
}

fn current_mailbox_counts(
    team_dir: &Path,
    members: &[TeamMember],
    tasks: &[TeamTask],
) -> Result<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    for member in members {
        let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, &member.name))?;
        let has_open_task = tasks
            .iter()
            .any(|task| task.owner.as_deref() == Some(member.name.as_str()) && task_is_open(task));
        let count = if member.role == "lead" || has_open_task {
            mailbox_seen_count(&messages)
        } else {
            messages.len()
        };
        counts.insert(member.name.clone(), count);
    }
    Ok(counts)
}

fn mailbox_seen_count(messages: &[MailMessage]) -> usize {
    messages
        .iter()
        .position(|message| !message.read)
        .unwrap_or(messages.len())
}

#[cfg(test)]
fn mark_mailbox_messages_read(team_dir: &Path, member_name: &str, from_index: usize) -> Result<()> {
    let path = mailbox_path(team_dir, member_name);
    let mut messages = read_jsonl::<MailMessage>(&path)?;
    mark_mailbox_messages_read_range_inner(
        team_dir,
        member_name,
        &path,
        &mut messages,
        from_index,
        None,
    )
}

fn mark_mailbox_messages_read_range(
    team_dir: &Path,
    member_name: &str,
    from_index: usize,
    to_index: usize,
) -> Result<()> {
    let path = mailbox_path(team_dir, member_name);
    let mut messages = read_jsonl::<MailMessage>(&path)?;
    mark_mailbox_messages_read_range_inner(
        team_dir,
        member_name,
        &path,
        &mut messages,
        from_index,
        Some(to_index),
    )
}

fn mark_mailbox_messages_read_range_inner(
    team_dir: &Path,
    member_name: &str,
    path: &Path,
    messages: &mut [MailMessage],
    from_index: usize,
    to_index: Option<usize>,
) -> Result<()> {
    if from_index >= messages.len() {
        return Ok(());
    }
    let end = to_index.unwrap_or(messages.len()).min(messages.len());
    if from_index >= end {
        return Ok(());
    }
    let mut changed = false;
    for message in messages.iter_mut().take(end).skip(from_index) {
        if !message.read {
            message.read = true;
            changed = true;
        }
    }
    if changed {
        write_jsonl_atomic(&path, &messages)?;
        append_event(
            team_dir,
            "mailbox_messages_marked_read",
            serde_json::json!({
                "member": member_name,
                "from_index": from_index,
                "count": end.saturating_sub(from_index),
            }),
        )?;
    }
    Ok(())
}

fn maybe_send_idle_department_outreach(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    last_outreach: &mut Instant,
    cursor: &mut usize,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    if now_instant.duration_since(*last_outreach) < interval {
        return Ok(());
    }
    *last_outreach = now_instant;

    let tasks = load_tasks(team_dir)?;
    let mut helpers = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .filter(|member| {
            matches!(
                member.status,
                MemberStatus::Standby | MemberStatus::Completed
            )
        })
        .filter(|member| active.get(&member.name).is_none_or(|run| run.completed))
        .filter(|member| {
            !tasks.iter().any(|task| {
                task.owner.as_deref() == Some(member.name.as_str())
                    && matches!(
                        task.status,
                        TaskStatus::InProgress
                            | TaskStatus::Review
                            | TaskStatus::Pending
                            | TaskStatus::Ready
                            | TaskStatus::Blocked
                    )
            })
        })
        .map(|member| member.name.clone())
        .collect::<Vec<_>>();
    helpers.sort();
    helpers.dedup();
    if helpers.is_empty() {
        append_event(
            team_dir,
            "idle_outreach_skipped",
            serde_json::json!({ "reason": "no_idle_departments" }),
        )?;
        return Ok(());
    }

    let mut targets = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .filter(|member| !matches!(member.status, MemberStatus::Failed | MemberStatus::Offline))
        .filter(|member| {
            tasks.iter().any(|task| {
                task.owner.as_deref() == Some(member.name.as_str())
                    && matches!(
                        task.status,
                        TaskStatus::InProgress
                            | TaskStatus::Ready
                            | TaskStatus::Waiting
                            | TaskStatus::Blocked
                            | TaskStatus::Review
                    )
            })
        })
        .map(|member| member.name.clone())
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();

    let helper = helpers[*cursor % helpers.len()].clone();
    *cursor = cursor.wrapping_add(1);
    targets.retain(|target| target != &helper);
    if targets.is_empty() {
        append_event(
            team_dir,
            "idle_outreach_skipped",
            serde_json::json!({ "helper": helper, "reason": "no_active_or_blocked_targets" }),
        )?;
        return Ok(());
    }

    let selected_targets = targets.into_iter().take(3).collect::<Vec<_>>();
    let message = if language.is_ja() {
        format!(
            "@{helper} からの定期アイドル声かけ: 私はいま free/standby です。blocker、レビュー依頼、artifact 解釈、schema/runtime の懸念、handoff の整理など、手伝えることはありますか？役に立つなら直接返信してください。必要なら lead に、具体的な mission 付きで私を resume するよう依頼してください。問題なく進んでいるなら返信不要です。"
        )
    } else {
        format!(
            "Periodic idle outreach from @{helper}: I am currently free/standby. Do you have a blocker, review need, artifact interpretation question, schema/runtime concern, or handoff cleanup I can help with? Reply directly if useful, or ask lead to resume me with a concrete mission. No reply needed if you are unblocked."
        )
    };
    for target in &selected_targets {
        send_team_message_to_dir(team_dir, &helper, target, &message)?;
    }
    append_event(
        team_dir,
        "idle_outreach_sent",
        serde_json::json!({
            "from": helper,
            "to": selected_targets,
            "interval_sec": interval.as_secs(),
        }),
    )?;
    Ok(())
}

fn record_runtime_loop_error(team_dir: &Path, phase: &str, err: anyhow::Error) -> Result<()> {
    append_event(
        team_dir,
        "app_server_runtime_loop_nonfatal_error",
        serde_json::json!({
            "phase": phase,
            "error": format!("{err:#}"),
        }),
    )
}

fn refresh_running_team_jobs(team_dir: &Path) -> Result<()> {
    let jobs = load_jobs(team_dir)?;
    for job in jobs {
        if matches!(job.status, TeamJobStatus::Running | TeamJobStatus::Unknown) {
            refresh_job_status(team_dir, &job.id)?;
        }
    }
    Ok(())
}

fn maybe_warn_unattended_tasks(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    last_watchdog: &mut Instant,
    warned: &mut HashSet<String>,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    if now_instant.duration_since(*last_watchdog) < interval {
        return Ok(());
    }
    *last_watchdog = now_instant;

    let tasks = load_tasks(team_dir)?;
    let jobs = load_jobs(team_dir)?;
    let waits = load_waits(team_dir)?;
    let running_job_tasks = jobs
        .iter()
        .filter(|job| matches!(job.status, TeamJobStatus::Running))
        .filter_map(|job| job.task_id.as_deref())
        .collect::<HashSet<_>>();
    let open_wait_tasks = waits
        .iter()
        .filter(|wait| wait.status.is_open())
        .filter_map(|wait| wait.task_id.as_deref())
        .collect::<HashSet<_>>();
    let mut config = config.clone();
    let mut config_changed = false;
    let member_status = config
        .members
        .iter()
        .map(|member| (member.name.clone(), member.status.clone()))
        .collect::<HashMap<_, _>>();

    for task in tasks.iter().filter(|task| {
        matches!(
            task.status,
            TaskStatus::Pending
                | TaskStatus::Ready
                | TaskStatus::Waiting
                | TaskStatus::InProgress
                | TaskStatus::Review
                | TaskStatus::Blocked
        )
    }) {
        let Some(owner) = task.owner.as_deref() else {
            continue;
        };
        let active_owner_turn = active.get(owner).is_some_and(|run| !run.completed);
        let tracked_running_job = running_job_tasks.contains(task.id.as_str());
        let tracked_open_wait = open_wait_tasks.contains(task.id.as_str());
        if active_owner_turn || tracked_running_job || tracked_open_wait {
            continue;
        }
        if !task.depends_on.is_empty() && !task_dependencies_completed(task, &tasks) {
            continue;
        }
        if task_age_secs(task).is_some_and(|age| age < 90) {
            continue;
        }
        if let Some(job) = jobs.iter().find(|job| {
            matches!(job.status, TeamJobStatus::Completed)
                && !job.artifacts.is_empty()
                && job.task_id.as_deref() == Some(task.id.as_str())
                && job.owner.as_deref() == Some(owner)
        }) {
            let changed = set_task_status_if_open(
                team_dir,
                &task.id,
                TaskStatus::InProgress,
                Some(&format!(
                    "Watchdog found completed job `{}` with registered artifact(s); owner must inspect them and publish the task's final handoff/checklist or a concrete blocker before review.",
                    job.id
                )),
            )?;
            if changed {
                resume_job_owner_after_job_status_change(
                    team_dir,
                    job,
                    &task.id,
                    TaskStatus::InProgress,
                )?;
                append_event(
                    team_dir,
                    "task_watchdog_completed_artifact_revival",
                    serde_json::json!({
                        "task": task.id,
                        "owner": owner,
                        "job": job.id,
                        "artifacts": job.artifacts,
                    }),
                )?;
            }
            continue;
        }
        let status = member_status
            .get(owner)
            .map(|status| format!("{status:?}"))
            .unwrap_or_else(|| "unknown".to_string());
        let warning_key = format!("{}:{}:{}", task.id, task.status, task.updated_at);
        if !warned.insert(warning_key) {
            continue;
        }
        let proposal_lines =
            collect_recent_lead_proposals_for_task(team_dir, &config.lead, &task.id, 3)?;
        let proposal_note = if proposal_lines.is_empty() {
            String::new()
        } else if language.is_ja() {
            format!(
                "\n\nこの task に言及している最近の LEAD_PROPOSAL signal:\n{}",
                proposal_lines.join("\n")
            )
        } else {
            format!(
                "\n\nRecent LEAD_PROPOSAL signal(s) mentioning this task:\n{}",
                proposal_lines.join("\n")
            )
        };
        let message = if language.is_ja() {
            format!(
                "Task watchdog: task {} は @{owner} が owner で、状態は `{}` ですが、owner の live turn も tracked running job もありません。Owner status は {status} です。lead は @{owner} を resume するか、`team job --owner {owner} --task {}` を attach/start するか、具体的 blocker 付きで blocked にするか、evidence 付きで completed にしてください。{proposal_note}",
                task.id, task.status, task.id
            )
        } else {
            format!(
                "Task watchdog: task {} owned by @{owner} is `{}` but has no live owner turn and no tracked running job. Owner status is {status}. Resume @{owner}, attach/start a `team job --owner {owner} --task {}`, mark it blocked with a concrete blocker, or complete it with evidence.{proposal_note}",
                task.id, task.status, task.id
            )
        };
        send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
        if config.members.iter().any(|member| member.name == owner) {
            send_team_message_to_dir(team_dir, "system", owner, &message)?;
        }
        let mut reactivated_owner = false;
        if task_status_can_start_turn(task.status)
            && let Some(member) = config
                .members
                .iter_mut()
                .find(|member| member.name == owner)
            && matches!(
                member.status,
                MemberStatus::Standby | MemberStatus::Completed
            )
        {
            member.status = MemberStatus::Online;
            config.updated_at = now();
            config_changed = true;
            reactivated_owner = true;
        }
        append_event(
            team_dir,
            "task_watchdog_attention",
            serde_json::json!({
                "task": task.id,
                "owner": owner,
                "status": task.status,
                "owner_status": status,
                "reason": "no live owner turn and no tracked running job",
                "owner_reactivated": reactivated_owner,
            }),
        )?;
    }
    warn_review_tasks_missing_local_handoff_artifacts(team_dir, &config, &tasks, warned)?;
    if config_changed {
        write_json_atomic(&team_dir.join("config.json"), &config)?;
        touch_config(team_dir)?;
    }
    Ok(())
}

#[derive(Debug)]
struct ReviewArtifactIssue {
    path: String,
    issue: String,
}

fn warn_review_tasks_missing_local_handoff_artifacts(
    team_dir: &Path,
    config: &TeamConfig,
    tasks: &[TeamTask],
    warned: &mut HashSet<String>,
) -> Result<()> {
    let ownerships = load_ownerships(team_dir)?;
    if ownerships.is_empty() {
        return Ok(());
    }

    for task in tasks
        .iter()
        .filter(|task| task.status == TaskStatus::Review)
    {
        let Some(owner) = task.owner.as_deref() else {
            continue;
        };
        if task_age_secs(task).is_some_and(|age| age < 60) {
            continue;
        }
        let issues = review_task_local_artifact_issues(team_dir, task, owner, &ownerships)?;
        if issues.is_empty() {
            continue;
        }
        let issue_key = issues
            .iter()
            .map(|issue| format!("{}={}", issue.path, issue.issue))
            .collect::<Vec<_>>()
            .join("|");
        let warning_key = format!(
            "review-handoff-artifacts:{}:{}:{}",
            task.id, task.updated_at, issue_key
        );
        if !warned.insert(warning_key) {
            continue;
        }

        let issue_lines = issues
            .iter()
            .take(6)
            .map(|issue| format!("- {}: {}", issue.path, issue.issue))
            .collect::<Vec<_>>()
            .join("\n");
        let message = format!(
            "Review handoff watchdog: task {} owned by @{owner} is in `review`, but task-specific local artifact ownership path(s) do not yet contain a complete formal handoff package.\n\nIssues:\n{issue_lines}\n\nA clean review handoff needs owner artifacts such as report/JSON ledger, `sha256_manifest.txt`, and `TEAM_COMPLETION_CHECKLIST.md`, plus a final message to lead/consumers. Collection or semantic verification jobs are not enough by themselves. Lead should steer @{owner} to publish the package, report the exact blocker, or move the task back to `in_progress`/`blocked` with a concrete next checkpoint.",
            task.id
        );
        send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
        if config.members.iter().any(|member| member.name == owner) {
            send_team_message_to_dir(team_dir, "system", owner, &message)?;
        }
        append_event(
            team_dir,
            "review_handoff_artifact_attention",
            serde_json::json!({
                "task": task.id,
                "owner": owner,
                "issues": issues.iter().map(|issue| {
                    serde_json::json!({
                        "path": issue.path,
                        "issue": issue.issue,
                    })
                }).collect::<Vec<_>>(),
            }),
        )?;
    }
    Ok(())
}

fn review_task_local_artifact_issues(
    team_dir: &Path,
    task: &TeamTask,
    owner: &str,
    ownerships: &[FileOwnership],
) -> Result<Vec<ReviewArtifactIssue>> {
    let mut issues = Vec::new();
    let mut complete_handoff_seen = false;
    let mut non_local_handoff_paths = Vec::new();
    for ownership in ownerships
        .iter()
        .filter(|ownership| ownership.owner == owner)
        .filter(|ownership| ownership_mentions_task(ownership, task))
    {
        if !ownership_path_is_probably_local(team_dir, &ownership.path) {
            non_local_handoff_paths.push(ownership.path.clone());
            continue;
        }
        let path = PathBuf::from(&ownership.path);
        match inspect_local_handoff_path(
            &path,
            owner_recent_completion_checklist_message(team_dir, owner)?,
        )? {
            Some(issue) => issues.push(ReviewArtifactIssue {
                path: ownership.path.clone(),
                issue,
            }),
            None => complete_handoff_seen = true,
        }
    }
    if complete_handoff_seen {
        return Ok(Vec::new());
    }
    if issues.is_empty() && !non_local_handoff_paths.is_empty() {
        for path in non_local_handoff_paths.into_iter().take(3) {
            issues.push(ReviewArtifactIssue {
                path,
                issue: "owned handoff path is not locally inspectable; require a node-side manifest/checklist verification job or explicit blocker before accepting review".to_string(),
            });
        }
    }
    Ok(issues)
}

fn ownership_mentions_task(ownership: &FileOwnership, task: &TeamTask) -> bool {
    let haystack = format!("{} {}", ownership.path, ownership.note).to_ascii_lowercase();
    [
        format!("task {}", task.id),
        format!("task{}", task.id),
        format!("task-{}", task.id),
        format!("#{}", task.id),
    ]
    .iter()
    .any(|needle| haystack.contains(&needle.to_ascii_lowercase()))
}

fn ownership_path_is_probably_local(team_dir: &Path, raw: &str) -> bool {
    let path = Path::new(raw);
    if path.exists() || path.starts_with(team_dir) {
        return true;
    }
    if let Ok(home) = std::env::var("HOME")
        && path.starts_with(home)
    {
        return true;
    }
    false
}

fn owner_recent_completion_checklist_message(team_dir: &Path, owner: &str) -> Result<bool> {
    for mailbox_owner in [owner, "lead"] {
        let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, mailbox_owner))?;
        if messages.into_iter().rev().take(200).any(|message| {
            message.from == owner && message.message.contains("TEAM_COMPLETION_CHECKLIST")
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn inspect_local_handoff_path(
    path: &Path,
    owner_has_completion_checklist_message: bool,
) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(Some("path does not exist".to_string()));
    }
    if path.is_file() {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        if handoff_file_kind(name).is_some() {
            return Ok(None);
        }
        return Ok(Some(
            "owned file is not a recognizable handoff artifact".to_string(),
        ));
    }
    if !path.is_dir() {
        return Ok(Some(
            "path exists but is not a file or directory".to_string(),
        ));
    }

    let mut stats = HandoffArtifactStats::default();
    collect_handoff_artifact_stats(path, 0, &mut stats)?;
    if stats.files == 0 {
        return Ok(Some("directory exists but contains no files".to_string()));
    }

    let mut missing = Vec::new();
    if !stats.has_checklist && !owner_has_completion_checklist_message {
        missing.push("TEAM_COMPLETION_CHECKLIST.md");
    }
    if !stats.has_manifest {
        missing.push("sha256_manifest.txt");
    }
    if !stats.has_report {
        missing.push("report markdown/text");
    }
    if !stats.has_structured {
        missing.push("JSON/YAML ledger/report");
    }
    if missing.is_empty() {
        verify_handoff_manifests(&stats)
    } else {
        Ok(Some(format!("missing {}", missing.join(", "))))
    }
}

#[derive(Default)]
struct HandoffArtifactStats {
    files: usize,
    has_checklist: bool,
    has_manifest: bool,
    has_report: bool,
    has_structured: bool,
    manifest_paths: Vec<PathBuf>,
}

fn verify_handoff_manifests(stats: &HandoffArtifactStats) -> Result<Option<String>> {
    for manifest_path in &stats.manifest_paths {
        let metadata = fs::metadata(manifest_path)
            .with_context(|| format!("stat {}", manifest_path.display()))?;
        if metadata.len() == 0 {
            return Ok(Some(format!(
                "{} is empty",
                manifest_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("sha256_manifest.txt")
            )));
        }
        if let Some(issue) = inspect_handoff_manifest_entries(manifest_path)? {
            return Ok(Some(issue));
        }
        let Some(parent) = manifest_path.parent() else {
            continue;
        };
        let output = Command::new("sha256sum")
            .arg("-c")
            .arg(manifest_path)
            .current_dir(parent)
            .output()
            .with_context(|| format!("run sha256sum -c {}", manifest_path.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = [stdout.trim(), stderr.trim()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .take(2)
                .collect::<Vec<_>>()
                .join("; ");
            return Ok(Some(format!(
                "{} failed sha256 verification{}{}",
                manifest_path.display(),
                if detail.is_empty() { "" } else { ": " },
                detail
            )));
        }
    }
    Ok(None)
}

fn inspect_handoff_manifest_entries(manifest_path: &Path) -> Result<Option<String>> {
    let content = fs::read_to_string(manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest_name = manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sha256_manifest.txt");
    for (idx, line) in content.lines().enumerate() {
        let Some(entry_path) = parse_sha256_manifest_entry_path(line) else {
            continue;
        };
        if manifest_entry_points_to_self(&entry_path, manifest_path) {
            return Ok(Some(format!(
                "{manifest_name} includes itself on line {}; generate the manifest after all final files and exclude the manifest file",
                idx + 1
            )));
        }
        if let Some(reason) = volatile_handoff_manifest_entry_reason(&entry_path) {
            return Ok(Some(format!(
                "{manifest_name} includes volatile entry `{entry_path}` on line {} ({reason}); exclude active transcripts/logs from the final manifest or freeze them before hashing and do not append afterward",
                idx + 1
            )));
        }
    }
    Ok(None)
}

fn parse_sha256_manifest_entry_path(line: &str) -> Option<String> {
    let trimmed = line.trim_end();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let hash = trimmed.get(..64)?;
    if !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let rest = trimmed.get(64..)?.trim_start();
    let path = rest.strip_prefix('*').unwrap_or(rest).trim_start();
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn manifest_entry_points_to_self(entry_path: &str, manifest_path: &Path) -> bool {
    let entry = Path::new(entry_path);
    if entry.is_absolute() {
        return entry == manifest_path;
    }
    manifest_path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| entry_path == name)
}

fn volatile_handoff_manifest_entry_reason(entry_path: &str) -> Option<&'static str> {
    let normalized = entry_path.replace('\\', "/").to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    match name {
        "command_transcript.log" => {
            Some("command transcripts are often appended after hash generation")
        }
        "manifest_verification.log" => {
            Some("manifest verification logs are commonly written after hash generation")
        }
        "sha256_manifest.verify.log" => {
            Some("manifest verification logs are commonly written after hash generation")
        }
        "job.log" | "job_stdout.log" | "job_stderr.log" => {
            Some("job logs may still be live when final handoff manifests are generated")
        }
        _ => None,
    }
}

fn collect_handoff_artifact_stats(
    dir: &Path,
    depth: usize,
    stats: &mut HandoffArtifactStats,
) -> Result<()> {
    if depth > 2 {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_handoff_artifact_stats(&path, depth + 1, stats)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        stats.files += 1;
        if let Some(name) = path.file_name().and_then(|name| name.to_str())
            && let Some(kind) = handoff_file_kind(name)
        {
            match kind {
                HandoffFileKind::Checklist => stats.has_checklist = true,
                HandoffFileKind::Manifest => {
                    stats.has_manifest = true;
                    stats.manifest_paths.push(path.clone());
                }
                HandoffFileKind::Report => stats.has_report = true,
                HandoffFileKind::Structured => stats.has_structured = true,
            }
        }
    }
    Ok(())
}

enum HandoffFileKind {
    Checklist,
    Manifest,
    Report,
    Structured,
}

fn handoff_file_kind(name: &str) -> Option<HandoffFileKind> {
    let lower = name.to_ascii_lowercase();
    if lower == "team_completion_checklist.md" || lower.contains("completion_checklist") {
        return Some(HandoffFileKind::Checklist);
    }
    if lower == "sha256_manifest.txt"
        || lower.ends_with("_manifest.sha256")
        || lower.ends_with("manifest.sha256")
    {
        return Some(HandoffFileKind::Manifest);
    }
    if lower.ends_with(".json") || lower.ends_with(".yaml") || lower.ends_with(".yml") {
        return Some(HandoffFileKind::Structured);
    }
    if lower.ends_with(".md") || lower.ends_with(".txt") {
        return Some(HandoffFileKind::Report);
    }
    None
}

fn maybe_send_lead_autonomy_tick(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    last_tick: &mut Instant,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    if now_instant.duration_since(*last_tick) < interval {
        return Ok(());
    }
    *last_tick = now_instant;

    let tasks = load_tasks(team_dir)?;
    let waits = load_waits(team_dir)?;
    let open_tasks = tasks
        .iter()
        .filter(|task| task_is_open(task))
        .collect::<Vec<_>>();
    let open_waits = waits
        .iter()
        .filter(|wait| wait.status.is_open())
        .collect::<Vec<_>>();
    let next_action_lines = collect_recent_next_action_signals(team_dir, 8)?;
    let goal_requests_continuation = team_goal_requests_continuation(&config.goal);
    if open_tasks.is_empty()
        && open_waits.is_empty()
        && active.values().all(|run| run.completed)
        && (!goal_requests_continuation || next_action_lines.is_empty())
    {
        return Ok(());
    }
    if let Some(lead_run) = active.get(&config.lead)
        && let Some(remaining) = app_server_retry_remaining(lead_run)
    {
        append_event(
            team_dir,
            "lead_autonomy_tick_suppressed",
            serde_json::json!({
                "lead": config.lead,
                "reason": "temporary app-server/model usage-limit cooldown",
                "retry_after_sec": remaining.as_secs(),
                "open_tasks": open_tasks.len(),
                "active_turns": active.values().filter(|run| !run.completed).count(),
            }),
        )?;
        return Ok(());
    }

    let mut active_lines = active
        .values()
        .map(|run| {
            let state = if run.completed { "idle" } else { "active" };
            let quiet_for = now_instant.duration_since(run.last_activity_at).as_secs();
            format!(
                "- @{name} role={role} node={node} state={state} quiet_for={quiet_for}s last={last}",
                name = run.member.name,
                role = run.member.role,
                node = run.node_id,
                last = run.last_activity_kind
            )
        })
        .collect::<Vec<_>>();
    active_lines.sort();

    let open_task_lines = open_tasks
        .iter()
        .take(20)
        .map(|task| {
            let owner = task.owner.as_deref().unwrap_or("unassigned");
            let age = task_age_secs(task)
                .map(|age| format!("{age}s"))
                .unwrap_or_else(|| "unknown".to_string());
            format!(
                "- task {id} [{status}] @{owner} age={age}: {subject}",
                id = task.id,
                status = task.status,
                subject = task.subject
            )
        })
        .collect::<Vec<_>>();
    let omitted = open_tasks.len().saturating_sub(open_task_lines.len());
    let omitted_line = if omitted > 0 {
        format!("\n- ... {omitted} more open tasks")
    } else {
        String::new()
    };
    let open_wait_lines = open_waits
        .iter()
        .take(20)
        .map(|wait| {
            format!(
                "- wait {id} [{status}] owner=@{owner} task={task} node={node} condition={condition} progress={progress}",
                id = wait.id,
                status = wait.status,
                owner = wait.owner.as_deref().unwrap_or("unassigned"),
                task = wait.task_id.as_deref().unwrap_or("-"),
                node = wait.node.as_deref().unwrap_or("-"),
                condition = wait.condition,
                progress = wait.progress
            )
        })
        .collect::<Vec<_>>();
    let omitted_waits = open_waits.len().saturating_sub(open_wait_lines.len());
    let omitted_waits_line = if omitted_waits > 0 {
        format!("\n- ... {omitted_waits} more open waits")
    } else {
        String::new()
    };
    let proposal_lines = collect_recent_lead_proposals(team_dir, &config.lead, 5)?;
    let proposal_block = if proposal_lines.is_empty() {
        "- none".to_string()
    } else {
        proposal_lines.join("\n")
    };
    let next_action_block = if next_action_lines.is_empty() {
        "- none".to_string()
    } else {
        next_action_lines.join("\n")
    };
    let continuation_policy = if goal_requests_continuation {
        if language.is_ja() {
            "この team の goal は継続・反復を明示しています。open task/open wait がなく、監査・評価・handoff に recommended next action / next cycle がある場合、idle とみなさず、lead が次サイクルの task/owner/wait/job を作るか、ユーザー入力が必要な blocker を明示してください。".to_string()
        } else {
            "This team's goal explicitly requests continuation/iteration. If there are no open tasks or waits but audit/evaluation/handoff artifacts contain recommended next actions or a next cycle, do not treat the team as idle; lead must either create the next cycle tasks/owners/waits/jobs or record the concrete blocker requiring user input.".to_string()
        }
    } else if language.is_ja() {
        "この team の goal は明示的な継続 loop を要求していません。next action signal は参考情報として扱い、勝手に新しい改善 loop を作らず、必要ならユーザー入力待ちを明示してください。".to_string()
    } else {
        "This team's goal does not explicitly request a continuation loop. Treat next-action signals as advisory context; do not invent a new improvement loop unless the user's instructions require it, and record user-input wait when needed.".to_string()
    };
    let message = if language.is_ja() {
        format!(
            "Lead autonomy tick: あなたはこの team の意思決定オーケストレーターです。team runtime は状態を報告し、この tick を届けているだけです。runtime があなたの代わりにオーケストレーション判断をしているわけではありません。\n\n必須の lead action:\n- 未完了 task、open wait、部署 mailbox、live message、job、artifact を確認してください。\n- ユーザーの現在の task に向けて協調してください。必要なら部署を steer / resume / reassign し、具体的な artifact、blocker、handoff を求めてください。\n- 部署が完了条件を持つ待機対象を始めたら、種類に決め打ちせず `team wait add` で condition / owner / task / progress / evidence を登録させてください。PID付きコマンドは `team job`、PIDを持たない外部待機や非同期依存は `team wait` で追跡してください。\n- open wait がある task は完了扱いにしないでください。wait が completed/failed/blocked になったら owner を resume し、結果を見て handoff、次 action、または blocker を出させてください。\n- teammate が `LEAD_PROPOSAL:` を送っているなら、resume / reassign / review action として明示的に採用するか、具体的な理由付きで却下してください。\n- {continuation_policy}\n- action が必要な task がなければ、team が idle のままでよい理由、または user input 待ちである理由を明示的に記録してください。active instruction や domain skill が明示的に要求しない限り、新しい改善 loop を勝手に作らないでください。\n\nOpen tasks:\n{}{omitted_line}\n\nOpen waits:\n{}{omitted_waits_line}\n\nRecent LEAD_PROPOSAL signals:\n{proposal_block}\n\nRecent artifact next-action signals:\n{next_action_block}\n\nCurrent app-server turns:\n{}",
            if open_task_lines.is_empty() {
                "- none".to_string()
            } else {
                open_task_lines.join("\n")
            },
            if open_wait_lines.is_empty() {
                "- none".to_string()
            } else {
                open_wait_lines.join("\n")
            },
            if active_lines.is_empty() {
                "- none".to_string()
            } else {
                active_lines.join("\n")
            }
        )
    } else {
        format!(
            "Lead autonomy tick: you are the decision-making orchestrator for this team. The team runtime is only reporting state and delivering this tick; it is not making orchestration decisions for you.\n\nRequired lead action:\n- Inspect unfinished tasks, open waits, department mailboxes, live messages, jobs, and artifacts.\n- Coordinate toward the user's current task: steer, resume, reassign, or ask departments for concrete artifacts, blockers, or handoffs.\n- When a department starts a waitable item with a completion condition, do not categorize it narrowly; register it with `team wait add` including condition / owner / task / progress / evidence. Use `team job` for PID-backed commands and `team wait` for PID-less external waits or async dependencies.\n- Do not treat a task with an open wait as complete. When a wait becomes completed/failed/blocked, resume the owner to inspect the result and publish a handoff, next action, or blocker.\n- If a teammate sent `LEAD_PROPOSAL:`, explicitly accept it with a resume/reassign/review action or reject it with the concrete reason.\n- {continuation_policy}\n- If no task needs action, explicitly record why the team should remain idle or wait for user input. Do not invent a new improvement loop unless the active instructions or a domain skill explicitly require one.\n\nOpen tasks:\n{}{omitted_line}\n\nOpen waits:\n{}{omitted_waits_line}\n\nRecent LEAD_PROPOSAL signals:\n{proposal_block}\n\nRecent artifact next-action signals:\n{next_action_block}\n\nCurrent app-server turns:\n{}",
            if open_task_lines.is_empty() {
                "- none".to_string()
            } else {
                open_task_lines.join("\n")
            },
            if open_wait_lines.is_empty() {
                "- none".to_string()
            } else {
                open_wait_lines.join("\n")
            },
            if active_lines.is_empty() {
                "- none".to_string()
            } else {
                active_lines.join("\n")
            }
        )
    };
    send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
    append_event(
        team_dir,
        "lead_autonomy_tick_sent",
        serde_json::json!({
            "lead": config.lead,
            "open_tasks": open_tasks.len(),
            "open_waits": open_waits.len(),
            "next_action_signals": next_action_lines.len(),
            "goal_requests_continuation": goal_requests_continuation,
            "active_turns": active.values().filter(|run| !run.completed).count(),
        }),
    )?;
    Ok(())
}

fn team_goal_requests_continuation(goal: &str) -> bool {
    let lower = goal.to_ascii_lowercase();
    [
        "keep iterating",
        "continue until",
        "continue the autoresearch loop",
        "keep cycling",
        "next cycle",
        "rerun",
        "繰り返",
        "継続",
        "改善サイクル",
        "次サイクル",
        "永遠",
        "ずっと",
    ]
    .iter()
    .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
}

fn collect_recent_lead_proposals(team_dir: &Path, lead: &str, limit: usize) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, lead))?;
    let resolved_after = latest_lead_proposal_resolution_timestamp(team_dir, lead)?;
    let mut proposals = messages
        .iter()
        .rev()
        .filter(|message| is_real_lead_proposal_message(message))
        .filter(|message| {
            resolved_after
                .as_deref()
                .is_none_or(|cutoff| message.timestamp.as_str() > cutoff)
        })
        .take(limit)
        .map(format_lead_proposal_summary)
        .collect::<Vec<_>>();
    proposals.reverse();
    Ok(proposals)
}

fn collect_recent_lead_proposals_for_task(
    team_dir: &Path,
    lead: &str,
    task_id: &str,
    limit: usize,
) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let task_token = format!("task {task_id}");
    let task_hash_token = format!("task-{task_id}");
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, lead))?;
    let resolved_after = latest_lead_proposal_resolution_timestamp(team_dir, lead)?;
    let mut proposals = messages
        .iter()
        .rev()
        .filter(|message| is_real_lead_proposal_message(message))
        .filter(|message| {
            resolved_after
                .as_deref()
                .is_none_or(|cutoff| message.timestamp.as_str() > cutoff)
        })
        .filter(|message| {
            message
                .message
                .to_ascii_lowercase()
                .contains(&task_token.to_ascii_lowercase())
                || message
                    .message
                    .to_ascii_lowercase()
                    .contains(&task_hash_token.to_ascii_lowercase())
        })
        .take(limit)
        .map(format_lead_proposal_summary)
        .collect::<Vec<_>>();
    proposals.reverse();
    Ok(proposals)
}

fn latest_lead_proposal_resolution_timestamp(
    team_dir: &Path,
    lead: &str,
) -> Result<Option<String>> {
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    Ok(events
        .iter()
        .rev()
        .find(|event| {
            if event.event != "message_sent" {
                return false;
            }
            let from = event
                .data
                .get("from")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if from != lead {
                return false;
            }
            let message = event
                .data
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            message_mentions_lead_proposal_resolution(message)
        })
        .map(|event| event.timestamp.clone()))
}

fn message_mentions_lead_proposal_resolution(message: &str) -> bool {
    let normalized = message.to_ascii_lowercase();
    normalized.contains("lead_proposal")
        && (normalized.contains("accepted")
            || normalized.contains("addressed")
            || normalized.contains("no separate action")
            || normalized.contains("reject")
            || normalized.contains("premature"))
}

fn is_real_lead_proposal_message(message: &MailMessage) -> bool {
    let from = message.from.trim().trim_start_matches('@');
    if from.eq_ignore_ascii_case("system") {
        return false;
    }
    let text = message.message.trim_start();
    if text.starts_with("Lead autonomy tick:")
        || text.starts_with("Department heartbeat")
        || text.starts_with("Department idle wakeup")
        || text.starts_with("TASK_COMPLETION_FREEZE:")
        || text.starts_with("JOB_STATUS:")
        || text.starts_with("AUX_JOB_STATUS:")
    {
        return false;
    }
    text.contains("LEAD_PROPOSAL:")
}

fn format_lead_proposal_summary(message: &MailMessage) -> String {
    format!(
        "- [{}] @{}: {}",
        message.timestamp,
        message.from,
        compact_one_line(&message.message, 700)
    )
}

fn collect_recent_next_action_signals(team_dir: &Path, limit: usize) -> Result<Vec<String>> {
    if limit == 0 {
        return Ok(Vec::new());
    }

    let mut dirs = vec![team_dir.to_path_buf()];
    let mut candidates = Vec::new();
    for ownership in load_ownerships(team_dir)? {
        let path = PathBuf::from(&ownership.path);
        if path.is_file() {
            candidates.push(path);
        } else if path.is_dir() {
            dirs.push(path);
        }
    }
    for dir in dirs {
        collect_next_action_candidate_files(&dir, 0, &mut candidates);
    }

    candidates.sort_by(|left, right| {
        let left_mtime = left
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok();
        let right_mtime = right
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok();
        right_mtime.cmp(&left_mtime)
    });

    let mut seen = HashSet::new();
    let mut signals = Vec::new();
    for path in candidates {
        if signals.len() >= limit {
            break;
        }
        if !seen.insert(path.clone()) {
            continue;
        }
        for line in extract_next_action_lines(&path)? {
            signals.push(format!(
                "- {}: {}",
                path.display(),
                compact_one_line(&line, 700)
            ));
            if signals.len() >= limit {
                break;
            }
        }
    }
    Ok(signals)
}

fn collect_next_action_candidate_files(dir: &Path, depth: usize, candidates: &mut Vec<PathBuf>) {
    if depth > 3 || candidates.len() > 200 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_next_action_candidate_files(&path, depth + 1, candidates);
        } else if file_type.is_file() && is_next_action_candidate_file(&path) {
            candidates.push(path);
        }
        if candidates.len() > 200 {
            break;
        }
    }
}

fn is_next_action_candidate_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let has_report_name = [
        "audit",
        "report",
        "summary",
        "handoff",
        "status",
        "outcome",
        "checklist",
        "progress",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    let has_supported_extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "md" | "json" | "txt"
            )
        })
        .unwrap_or(false);
    has_report_name && has_supported_extension
}

fn extract_next_action_lines(path: &Path) -> Result<Vec<String>> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to stat next-action candidate {}", path.display()))?;
    if metadata.len() > 512 * 1024 {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read next-action candidate {}", path.display()))?;
    let mut lines = Vec::new();
    let all_lines = text.lines().collect::<Vec<_>>();
    for (idx, line) in all_lines.iter().enumerate() {
        if !line_mentions_next_action(line) {
            continue;
        }
        let mut signal = line.trim().to_string();
        if signal.ends_with(':')
            && let Some(next_line) = all_lines.iter().skip(idx + 1).find(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
        {
            signal.push(' ');
            signal.push_str(next_line.trim());
        }
        lines.push(signal);
        if lines.len() >= 4 {
            break;
        }
    }
    Ok(lines)
}

fn line_mentions_next_action(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "recommended next action",
        "recommended_next_action",
        "next action",
        "next_action",
        "next cycle",
        "next-cycle",
        "follow-up",
        "follow up",
        "次に",
        "次の",
        "次アクション",
    ]
    .iter()
    .any(|needle| lower.contains(&needle.to_ascii_lowercase()))
}

fn maybe_send_department_idle_wakeups(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    idle_since: &mut HashMap<String, Instant>,
    last_wakeup: &mut HashMap<String, Instant>,
    last_batch: &mut Instant,
    cursor: &mut usize,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    const MAX_IDLE_WAKEUPS_PER_BATCH: usize = 2;

    let now_instant = Instant::now();
    let tasks = load_tasks(team_dir)?;
    let members = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .filter(|member| !matches!(member.status, MemberStatus::Failed | MemberStatus::Offline))
        .collect::<Vec<_>>();
    if members.is_empty() {
        return Ok(());
    }
    if *cursor >= members.len() {
        *cursor = 0;
    }

    let mut eligible = Vec::new();
    for (idx, member) in members.iter().enumerate() {
        let active_run = active.get(&member.name).is_some_and(|run| !run.completed);
        if active_run {
            idle_since.remove(&member.name);
            last_wakeup.remove(&member.name);
            continue;
        }
        let member_has_open_tasks = tasks
            .iter()
            .any(|task| task.owner.as_deref() == Some(member.name.as_str()) && task_is_open(task));
        if let Some(remaining) = should_suppress_empty_department_ping_during_cooldown(
            config,
            active,
            &member.name,
            member_has_open_tasks,
            active_run,
        ) {
            if last_wakeup
                .get(&member.name)
                .is_some_and(|last| now_instant.duration_since(*last) < interval)
            {
                continue;
            }
            last_wakeup.insert(member.name.clone(), now_instant);
            idle_since.remove(&member.name);
            append_event(
                team_dir,
                "department_idle_wakeup_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "usage_limit_cooldown",
                    "retry_after_sec": remaining.as_secs(),
                }),
            )?;
            continue;
        }

        let since = idle_since.entry(member.name.clone()).or_insert(now_instant);
        let idle_for = now_instant.duration_since(*since);
        if idle_for < interval {
            continue;
        }
        if last_wakeup
            .get(&member.name)
            .is_some_and(|last| now_instant.duration_since(*last) < interval)
        {
            continue;
        }
        eligible.push(idx);
    }
    if eligible.is_empty() || now_instant.duration_since(*last_batch) < interval {
        return Ok(());
    }

    let mut sent = 0_usize;
    let mut last_sent_idx = None;
    for offset in 0..members.len() {
        if sent >= MAX_IDLE_WAKEUPS_PER_BATCH {
            break;
        }
        let idx = (*cursor + offset) % members.len();
        if !eligible.contains(&idx) {
            continue;
        }
        let member = members[idx];
        let idle_for = idle_since
            .get(&member.name)
            .map(|since| now_instant.duration_since(*since))
            .unwrap_or_default();
        last_wakeup.insert(member.name.clone(), now_instant);
        sent += 1;
        last_sent_idx = Some(idx);

        let member_tasks = tasks
            .iter()
            .filter(|task| task.owner.as_deref() == Some(member.name.as_str()))
            .filter(|task| task_is_open(task))
            .take(8)
            .map(|task| format!("- task {} [{}]: {}", task.id, task.status, task.subject))
            .collect::<Vec<_>>();
        let team_open_tasks = tasks
            .iter()
            .filter(|task| task_is_open(task))
            .take(12)
            .map(|task| {
                let owner = task.owner.as_deref().unwrap_or("-");
                format!(
                    "- task {} [{}] @{}: {}",
                    task.id, task.status, owner, task.subject
                )
            })
            .collect::<Vec<_>>();
        let message = format!(
            "{}",
            if language.is_ja() {
                format!(
                    "Department idle wakeup for @{name}: この部署には {idle}s の間 active な app-server turn がありません。\n\n必須 action:\n- 自分の inbox、担当中の open task、最近の handoff、job、artifact を読んでください。\n- 自分の blocked/pending/review task が再開可能なら、resume/reassign/review action の evidence とともに `LEAD_PROPOSAL:` を lead に送ってください。\n- 他部署の task が ready、重複、stale、owner 不明だと気づいた場合も、task id、evidence、suggested action とともに `LEAD_PROPOSAL:` を lead に送ってください。\n- action が不要なら、lead に `STAY:` と一行理由を送り、終了してください。busywork を作らないでください。\n\nYour open tasks:\n{member_tasks}\n\nTeam open tasks:\n{team_tasks}",
                    name = member.name,
                    idle = idle_for.as_secs(),
                    member_tasks = if member_tasks.is_empty() {
                        "- none".to_string()
                    } else {
                        member_tasks.join("\n")
                    },
                    team_tasks = if team_open_tasks.is_empty() {
                        "- none".to_string()
                    } else {
                        team_open_tasks.join("\n")
                    }
                )
            } else {
                format!(
                    "Department idle wakeup for @{name}: this department has had no active app-server turn for {idle}s.\n\nRequired action:\n- Read your inbox, owned open tasks, recent handoffs, jobs, and artifacts.\n- If your own blocked/pending/review task is now ready, message lead with `LEAD_PROPOSAL:` and evidence for the resume/reassign/review action.\n- If you notice another department's task is ready, duplicated, stale, or missing an owner, send lead a `LEAD_PROPOSAL:` with task id, evidence, and suggested action.\n- If no action is needed, send lead `STAY:` with a one-line reason and finish. Do not invent busywork.\n\nYour open tasks:\n{member_tasks}\n\nTeam open tasks:\n{team_tasks}",
                    name = member.name,
                    idle = idle_for.as_secs(),
                    member_tasks = if member_tasks.is_empty() {
                        "- none".to_string()
                    } else {
                        member_tasks.join("\n")
                    },
                    team_tasks = if team_open_tasks.is_empty() {
                        "- none".to_string()
                    } else {
                        team_open_tasks.join("\n")
                    }
                )
            },
        );
        send_team_message_to_dir(team_dir, "system", &member.name, &message)?;
        append_event(
            team_dir,
            "department_idle_wakeup_sent",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "node": member_node_id(member),
                "idle_for_sec": idle_for.as_secs(),
                "owned_open_tasks": member_tasks.len(),
                "batch_limit": MAX_IDLE_WAKEUPS_PER_BATCH,
            }),
        )?;
    }
    if sent > 0 {
        *last_batch = now_instant;
        if let Some(idx) = last_sent_idx {
            *cursor = (idx + 1) % members.len();
        }
    }
    Ok(())
}

fn seed_department_idle_wakeup_cooldowns(
    team_dir: &Path,
    last_wakeup: &mut HashMap<String, Instant>,
    last_batch: &mut Instant,
    interval: Duration,
) -> Result<()> {
    let events_path = team_dir.join("events.jsonl");
    if !events_path.exists() {
        return Ok(());
    }

    let events = read_jsonl::<TeamEventRecord>(&events_path)?;
    let now_utc = Utc::now();
    let now_instant = Instant::now();
    let mut newest_elapsed = None::<Duration>;

    for event in events
        .into_iter()
        .filter(|event| event.event == "department_idle_wakeup_sent")
    {
        let Ok(timestamp) = chrono::DateTime::parse_from_rfc3339(&event.timestamp) else {
            continue;
        };
        let elapsed = now_utc - timestamp.with_timezone(&Utc);
        if elapsed < chrono::Duration::zero() {
            continue;
        }
        let Ok(elapsed_std) = elapsed.to_std() else {
            continue;
        };
        if elapsed_std >= interval {
            continue;
        }
        let Some(member) = event
            .data
            .get("member")
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        last_wakeup.insert(member.to_string(), now_instant - elapsed_std);
        newest_elapsed = Some(newest_elapsed.map_or(elapsed_std, |current| {
            if elapsed_std < current {
                elapsed_std
            } else {
                current
            }
        }));
    }

    if let Some(elapsed) = newest_elapsed {
        *last_batch = now_instant - elapsed;
    }
    Ok(())
}

fn maybe_send_department_heartbeats(
    team_dir: &Path,
    config: &TeamConfig,
    active: &HashMap<String, AppServerMemberRun>,
    heartbeats: &mut HashMap<String, Instant>,
    recent_idle_wakeups: &HashMap<String, Instant>,
    interval: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    let tasks = load_tasks(team_dir)?;
    for member in config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .filter(|member| !matches!(member.status, MemberStatus::Failed | MemberStatus::Offline))
    {
        let member_tasks = tasks
            .iter()
            .filter(|task| task.owner.as_deref() == Some(member.name.as_str()))
            .filter(|task| task_is_open(task))
            .collect::<Vec<_>>();
        let active_run = active.get(&member.name).is_some_and(|run| !run.completed);
        if member_tasks.is_empty()
            && !active_run
            && !matches!(member.status, MemberStatus::Running | MemberStatus::Standby)
        {
            continue;
        }
        if let Some(remaining) = should_suppress_empty_department_ping_during_cooldown(
            config,
            active,
            &member.name,
            !member_tasks.is_empty(),
            active_run,
        ) {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "usage_limit_cooldown",
                    "retry_after_sec": remaining.as_secs(),
                }),
            )?;
            continue;
        }
        if recent_idle_wakeups
            .get(&member.name)
            .is_some_and(|last| now_instant.duration_since(*last) < interval)
        {
            let entry = heartbeats
                .entry(member.name.clone())
                .or_insert(now_instant - interval);
            if now_instant.duration_since(*entry) < interval {
                continue;
            }
            *entry = now_instant;
            append_event(
                team_dir,
                "department_heartbeat_skipped",
                serde_json::json!({
                    "member": member.name,
                    "role": member.role,
                    "node": member_node_id(member),
                    "reason": "recent_idle_wakeup",
                }),
            )?;
            continue;
        }
        let entry = heartbeats
            .entry(member.name.clone())
            .or_insert(now_instant - interval);
        if now_instant.duration_since(*entry) < interval {
            continue;
        }
        *entry = now_instant;

        let task_lines = member_tasks
            .iter()
            .take(8)
            .map(|task| format!("- task {} [{}]: {}", task.id, task.status, task.subject))
            .collect::<Vec<_>>();
        let node = member_node_id(member);
        let owned_tasks = if task_lines.is_empty() {
            "- none currently recorded, but your department is still active/standby".to_string()
        } else {
            task_lines.join("\n")
        };
        let message = if language.is_ja() {
            format!(
                "Department heartbeat for @{name}: あなたの mission または担当 task が完全に完了していない場合、今すぐ進捗を報告してください。\n\n必須 department action:\n- lead と relevant consumers に簡潔な status update を送ってください。\n- artifact/log/config/job/request path がある場合は具体的に含めてください。\n- 実行中の重い command、download、build、render、training、API/tool 待ち、remote/container 内処理などがある場合、それが team job または team wait に登録済みか明記してください。未登録なら今すぐ登録するか、登録できない具体的理由と代替 progress artifact path を lead に報告してください。\n- Docker/container node の部署は、成果がまだ未完成でも runtime workspace 内に status/progress artifact を作り、command transcript、manifest、metrics、visualization の予定 path を報告してください。\n- manifest や checksum を持つ package を作った場合、最後の追記・script修正・report/status/progress更新後に再度 `sha256sum -c` を実行し、fresh rc と現 disk hash を報告してください。live transcript、manifest check log、handoff log、progress/status file、helper/finalizer script を hash 後に追記または修正した可能性があるなら、完了扱いにせず再生成してください。\n- 他部署、MCP、remote host、Docker/container、long job、user input を待っている場合、正確な dependency と next action を書いてください。\n- 自部署または他部署の blocked/pending/review task について、gate が cleared に見える、または next owner が不明なら、自分で勝手に開始せず、evidence と suggested resume/reassign/review action を含む `LEAD_PROPOSAL:` を lead に送ってください。\n- 前回 heartbeat から進捗がない場合、黙って待たず lead に介入を求めてください。\n- 完了している場合は TEAM_COMPLETION_CHECKLIST を提示し、follow-up question に答えられる状態で残ってください。\n\nOwned open tasks:\n{owned_tasks}",
                name = member.name
            )
        } else {
            format!(
                "Department heartbeat for @{name}: if your mission or any owned task is not fully complete, report progress now.\n\nRequired department action:\n- Send lead and relevant consumers a concise status update.\n- Include concrete artifact/log/config/job/request paths when they exist.\n- If a heavy command, download, build, render, training run, API/tool wait, remote/container process, or other long operation is running, state whether it is registered as a team job or team wait. If it is not registered, register it now or report the exact reason plus a fallback progress artifact path to lead.\n- Docker/container-node departments must create a status/progress artifact in the runtime workspace even before final output exists, and report planned command transcript, manifest, metrics, and visualization paths.\n- If you produced a manifest or checksum package, rerun `sha256sum -c` after the final append, script edit, report/status/progress update and report the fresh rc plus current on-disk hashes. If a live transcript, manifest check log, handoff log, progress/status file, or helper/finalizer script may have been changed after hashing, do not complete; regenerate the package.\n- If waiting on another department, MCP, remote host, Docker/container, long job, or user input, state the exact dependency and next action.\n- If you notice any blocked/pending/review task, including another department's task, whose gate appears cleared or whose next owner is unclear, do not start it yourself; send lead a `LEAD_PROPOSAL:` message with evidence and the suggested resume/reassign/review action.\n- If you have made no progress since the previous heartbeat, ask lead for intervention instead of silently waiting.\n- If complete, provide TEAM_COMPLETION_CHECKLIST and remain available for follow-up questions.\n\nOwned open tasks:\n{owned_tasks}",
                name = member.name
            )
        };
        send_team_message_to_dir(team_dir, "system", &member.name, &message)?;
        append_event(
            team_dir,
            "department_heartbeat_sent",
            serde_json::json!({
                "member": member.name,
                "role": member.role,
                "node": node,
                "open_tasks": member_tasks.len(),
                "active_turn": active_run,
            }),
        )?;
    }
    Ok(())
}

fn maybe_warn_stale_active_turns(
    team_dir: &Path,
    config: &TeamConfig,
    active: &mut HashMap<String, AppServerMemberRun>,
    last_check: &mut Instant,
    interval: Duration,
    stale_timeout: Duration,
    language: TeamPromptLanguage,
) -> Result<()> {
    let now_instant = Instant::now();
    if now_instant.duration_since(*last_check) < interval {
        return Ok(());
    }
    *last_check = now_instant;

    let tasks = load_tasks(team_dir)?;
    for (member_name, run) in active.iter_mut() {
        if run.completed {
            continue;
        }
        let quiet_for = now_instant.duration_since(run.last_activity_at);
        if quiet_for < stale_timeout {
            continue;
        }
        if run
            .last_stale_notice_at
            .is_some_and(|last| now_instant.duration_since(last) < stale_timeout)
        {
            continue;
        }
        let repeated_stale = run.last_stale_notice_at.is_some();
        run.last_stale_notice_at = Some(now_instant);
        let member_tasks = tasks
            .iter()
            .filter(|task| task.owner.as_deref() == Some(member_name.as_str()))
            .filter(|task| task_is_open(task))
            .take(8)
            .map(|task| format!("- task {} [{}]: {}", task.id, task.status, task.subject))
            .collect::<Vec<_>>();
        let task_summary = if member_tasks.is_empty() {
            "- no open owned task recorded".to_string()
        } else {
            member_tasks.join("\n")
        };
        let escalation = if repeated_stale {
            "\n\nEscalation: this member has already received at least one stale-turn notice in this active turn. If the previous notice did not produce concrete status, artifact growth, a tracked job id, or a real blocker, do not keep repeating generic check-ins. Inspect the owned artifact path(s), jobs, and mailbox, then either steer a very specific next checkpoint, cancel/reassign/recover the task to a recovery owner, or mark it blocked with evidence. Preserve any partial files as draft-only until the recovery owner verifies and re-manifests them."
        } else {
            ""
        };
        let lead_message = if language.is_ja() {
            format!(
                "Stale active turn attention: @{member} は active な app-server turn を持っていますが、team runtime は {quiet}s の間 assistant output を観測していません。Last observed activity: {last}。これは通常の長い MCP/tool call、遅いが妥当な部署ペース、blocked remote/container operation、または wedged turn の可能性があります。\n\nあなたは lead として、部署に低品質な partial work を急がせず recovery action を決めてください。まず observability を求めてください: current subtask、tool/job/MCP/remote/container operation が実行中か、関連 request/job/log/artifact path、risks、next planned checkpoint。evidence が stuck / mis-scoped / waiting on another actor を示す場合だけ、exact next step で steer、reassign、または task を blocked にしてください。artifact、handoff message、required verification evidence が揃うまで owned task を complete とみなさないでください。{escalation}\n\nOwned open tasks:\n{tasks}",
                member = member_name,
                quiet = quiet_for.as_secs(),
                last = run.last_activity_kind,
                escalation = escalation,
                tasks = task_summary
            )
        } else {
            format!(
                "Stale active turn attention: @{member} has an app-server turn marked active, but the team runtime has observed no assistant output for {quiet}s. Last observed activity: {last}. This may be a normal long MCP/tool call, a slow but valid department pace, a blocked remote/container operation, or a wedged turn.\n\nYou are the lead and must decide the recovery action without pressuring the department to ship low-quality partial work. First ask for observability: current subtask, whether a tool/job/MCP/remote/container operation is running, relevant request/job/log/artifact paths, risks, and the next planned checkpoint. Only steer with an exact next step, reassign work, or mark the task blocked when evidence shows the work is stuck, mis-scoped, or waiting on another actor. Do not assume the owned task is complete until artifacts, handoff messages, and required verification evidence exist.{escalation}\n\nOwned open tasks:\n{tasks}",
                member = member_name,
                quiet = quiet_for.as_secs(),
                last = run.last_activity_kind,
                escalation = escalation,
                tasks = task_summary
            )
        };
        send_team_message_to_dir(team_dir, "system", &config.lead, &lead_message)?;
        if member_name != &config.lead {
            let member_message = if language.is_ja() {
                format!(
                    "Automated lead status check: あなたの現在の app-server turn では {quiet}s の間 assistant output が観測されていません。これは急げ、または品質を下げろという要求ではありません。今すぐ lead に current status を報告してください: current subtask、MCP/tool/job/remote/container work を待っているか、関連 request/job/log/artifact path、risks、next checkpoint。work が広い/重い場合、subagent/agent tools、skills、MCP servers、internal decomposition が使えるなら積極的に使い、その使い方を報告してください。blocked なら concrete evidence 付きで owned task を blocked にしてください。",
                    quiet = quiet_for.as_secs()
                )
            } else {
                format!(
                    "Automated lead status check: your current app-server turn has had no observed assistant output for {quiet}s. This is not a demand to rush or lower quality. Report current status to lead now: current subtask, whether you are waiting on MCP/tool/job/remote/container work, relevant request/job/log/artifact paths, risks, and next checkpoint. If work is broad or heavy and subagent/agent tools, skills, MCP servers, or internal decomposition are available, use them proactively and mention how. If you are blocked, mark the owned task blocked with concrete evidence.",
                    quiet = quiet_for.as_secs()
                )
            };
            send_team_message_to_dir(team_dir, &config.lead, member_name, &member_message)?;
        }
        append_event(
            team_dir,
            "stale_active_turn_attention",
            serde_json::json!({
                "member": member_name,
                "node": run.node_id.clone(),
                "thread": run.thread_id.clone(),
                "turn": run.turn_id.clone(),
                "quiet_for_sec": quiet_for.as_secs(),
                "last_activity": run.last_activity_kind.clone(),
            }),
        )?;
    }
    Ok(())
}

fn task_is_open(task: &TeamTask) -> bool {
    !matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
    )
}

fn task_status_can_start_turn(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Pending
            | TaskStatus::Waiting
            | TaskStatus::Ready
            | TaskStatus::InProgress
            | TaskStatus::Review
    )
}

fn task_age_secs(task: &TeamTask) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(&task.updated_at)
        .ok()
        .map(|updated| (Utc::now() - updated.with_timezone(&Utc)).num_seconds())
}

async fn steer_new_team_messages(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    team_dir: &Path,
    members: &[TeamMember],
    active: &mut HashMap<String, AppServerMemberRun>,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    mailbox_counts: &mut HashMap<String, usize>,
    cwd: &Path,
    model: Option<String>,
    approval_policy: Option<AskForApproval>,
    dangerously_bypass_approvals_and_sandbox: bool,
    codex_exe: &Path,
    side_channel_replies: bool,
    language: TeamPromptLanguage,
) -> Result<()> {
    let mut by_recipient = HashMap::<String, PendingMailboxDelivery>::new();
    for member in members {
        if active
            .get(&member.name)
            .and_then(app_server_retry_remaining)
            .is_some()
        {
            continue;
        }
        let Some(pending) = collect_new_active_mailbox_messages(
            team_dir,
            member,
            active.contains_key(&member.name) && !matches!(member.status, MemberStatus::Offline),
            mailbox_counts,
        )?
        else {
            continue;
        };
        if !pending.messages.is_empty() {
            by_recipient.insert(member.name.clone(), pending);
        }
    }

    for (member_name, pending) in by_recipient {
        let messages = pending.messages;
        let Some(run) = active.get(&member_name).cloned() else {
            continue;
        };
        if run.completed {
            if run.member.role == "lead" {
                let config = load_config(team_dir)?;
                let prompt = build_reactive_lead_turn_prompt(
                    &run.member,
                    &messages,
                    codex_exe,
                    &config.id,
                    language,
                );
                let started = start_app_server_member_turn(
                    node_clients,
                    team_dir,
                    active,
                    &member_name,
                    prompt,
                    cwd,
                    model.clone(),
                    approval_policy,
                    dangerously_bypass_approvals_and_sandbox,
                    "app_server_lead_reactive_started",
                )
                .await?;
                if started {
                    acknowledge_mailbox_delivery(
                        team_dir,
                        mailbox_counts,
                        &member_name,
                        pending.seen,
                        messages.len(),
                    )?;
                }
            } else {
                let config = load_config(team_dir)?;
                let status =
                    member_status(team_dir, &member_name)?.unwrap_or(MemberStatus::Completed);
                let prompt = build_reactive_member_turn_prompt(
                    &run.member,
                    &messages,
                    codex_exe,
                    &config.id,
                    matches!(status, MemberStatus::Standby),
                    language,
                );
                let started = start_app_server_member_turn(
                    node_clients,
                    team_dir,
                    active,
                    &member_name,
                    prompt,
                    cwd,
                    model.clone(),
                    approval_policy,
                    dangerously_bypass_approvals_and_sandbox,
                    "app_server_member_reactive_started",
                )
                .await?;
                if let Some(run) = active.get_mut(&member_name) {
                    run.standby_after_turn = matches!(status, MemberStatus::Standby);
                }
                if started {
                    acknowledge_mailbox_delivery(
                        team_dir,
                        mailbox_counts,
                        &member_name,
                        pending.seen,
                        messages.len(),
                    )?;
                }
            }
            continue;
        }
        let mut delivered = false;
        if side_channel_replies {
            let side_messages = messages
                .iter()
                .filter(|message| side_channel_message_needs_fast_reply(&member_name, message))
                .cloned()
                .collect::<Vec<_>>();
            if !side_messages.is_empty() {
                let side_started = start_app_server_side_channel_reply(
                    node_clients,
                    team_dir,
                    side_replies,
                    &run,
                    side_messages,
                    model.clone(),
                    approval_policy,
                    dangerously_bypass_approvals_and_sandbox,
                    language,
                )
                .await?;
                if side_started {
                    let system_messages = messages
                        .iter()
                        .filter(|message| message.from == "system")
                        .cloned()
                        .collect::<Vec<_>>();
                    if system_messages.is_empty() {
                        acknowledge_mailbox_delivery(
                            team_dir,
                            mailbox_counts,
                            &member_name,
                            pending.seen,
                            messages.len(),
                        )?;
                        continue;
                    }
                    let steer_text =
                        build_reactive_steer_prompt(&run.member, &system_messages, language);
                    let (steer_text, side_context_ids) = append_side_channel_context_prompt(
                        team_dir,
                        &member_name,
                        &run.turn_id,
                        steer_text,
                        language,
                    )?;
                    let Some(node_client) = node_clients.get_mut(&run.node_id) else {
                        append_event(
                            team_dir,
                            "app_server_turn_steer_skipped",
                            serde_json::json!({
                                "member": member_name,
                                "node": run.node_id,
                                "thread": run.thread_id.clone(),
                                "turn": run.turn_id.clone(),
                                "messages": system_messages.len(),
                                "error": "node client missing",
                            }),
                        )?;
                        continue;
                    };
                    let steer_result = node_client
                        .client
                        .request_typed::<TurnSteerResponse>(ClientRequest::TurnSteer {
                            request_id: next_request_id(&mut node_client.request_counter),
                            params: TurnSteerParams {
                                thread_id: run.thread_id.clone(),
                                input: vec![text_input(steer_text)],
                                responsesapi_client_metadata: None,
                                expected_turn_id: run.turn_id.clone(),
                            },
                        })
                        .await;
                    let steer_succeeded = steer_result.is_ok();
                    match steer_result {
                        Ok(response) => {
                            let response_turn_id = response.turn_id.clone();
                            append_turn_steer_result(
                                team_dir,
                                &member_name,
                                &run,
                                system_messages.len(),
                                Ok::<TurnSteerResponse, String>(response),
                            )?;
                            mark_side_channel_contexts_injected(
                                team_dir,
                                &member_name,
                                &side_context_ids,
                                &response_turn_id,
                            )?;
                            if let Some(run) = active.get_mut(&member_name) {
                                merge_side_context_ids(run, &side_context_ids);
                            }
                        }
                        Err(err) => append_turn_steer_result(
                            team_dir,
                            &member_name,
                            &run,
                            system_messages.len(),
                            Err(err),
                        )?,
                    }
                    if steer_succeeded {
                        acknowledge_mailbox_delivery(
                            team_dir,
                            mailbox_counts,
                            &member_name,
                            pending.seen,
                            messages.len(),
                        )?;
                    }
                    continue;
                }
            }
        }
        let steer_text = build_reactive_steer_prompt(&run.member, &messages, language);
        let (steer_text, side_context_ids) = append_side_channel_context_prompt(
            team_dir,
            &member_name,
            &run.turn_id,
            steer_text,
            language,
        )?;
        let Some(node_client) = node_clients.get_mut(&run.node_id) else {
            append_event(
                team_dir,
                "app_server_turn_steer_skipped",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id.clone(),
                    "turn": run.turn_id.clone(),
                    "messages": messages.len(),
                    "error": "node client missing",
                }),
            )?;
            continue;
        };
        let steer_result = node_client
            .client
            .request_typed::<TurnSteerResponse>(ClientRequest::TurnSteer {
                request_id: next_request_id(&mut node_client.request_counter),
                params: TurnSteerParams {
                    thread_id: run.thread_id.clone(),
                    input: vec![text_input(steer_text)],
                    responsesapi_client_metadata: None,
                    expected_turn_id: run.turn_id.clone(),
                },
            })
            .await;
        match steer_result {
            Ok(response) => {
                let response_turn_id = response.turn_id.clone();
                append_turn_steer_result(
                    team_dir,
                    &member_name,
                    &run,
                    messages.len(),
                    Ok::<TurnSteerResponse, String>(response),
                )?;
                mark_side_channel_contexts_injected(
                    team_dir,
                    &member_name,
                    &side_context_ids,
                    &response_turn_id,
                )?;
                if let Some(run) = active.get_mut(&member_name) {
                    merge_side_context_ids(run, &side_context_ids);
                }
                delivered = true;
            }
            Err(err) => {
                append_turn_steer_result(team_dir, &member_name, &run, messages.len(), Err(err))?
            }
        }
        if delivered {
            acknowledge_mailbox_delivery(
                team_dir,
                mailbox_counts,
                &member_name,
                pending.seen,
                messages.len(),
            )?;
        }
    }
    Ok(())
}

struct PendingMailboxDelivery {
    seen: usize,
    messages: Vec<MailMessage>,
}

fn collect_new_active_mailbox_messages(
    team_dir: &Path,
    member: &TeamMember,
    active: bool,
    mailbox_counts: &mut HashMap<String, usize>,
) -> Result<Option<PendingMailboxDelivery>> {
    if !active {
        return Ok(None);
    }
    let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, &member.name))?;
    let seen = mailbox_counts
        .get(&member.name)
        .copied()
        .unwrap_or_default()
        .min(messages.len());
    let new_messages = messages.into_iter().skip(seen).collect::<Vec<_>>();
    Ok(Some(PendingMailboxDelivery {
        seen,
        messages: new_messages,
    }))
}

fn acknowledge_mailbox_delivery(
    team_dir: &Path,
    mailbox_counts: &mut HashMap<String, usize>,
    member_name: &str,
    seen: usize,
    delivered_count: usize,
) -> Result<()> {
    if delivered_count == 0 {
        return Ok(());
    }
    let delivered_until = seen.saturating_add(delivered_count);
    mark_mailbox_messages_read_range(team_dir, member_name, seen, delivered_until)?;
    mailbox_counts.insert(member_name.to_string(), delivered_until);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn start_app_server_side_channel_reply(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    team_dir: &Path,
    side_replies: &mut HashMap<String, AppServerSideReply>,
    run: &AppServerMemberRun,
    messages: Vec<MailMessage>,
    model: Option<String>,
    approval_policy: Option<AskForApproval>,
    dangerously_bypass_approvals_and_sandbox: bool,
    language: TeamPromptLanguage,
) -> Result<bool> {
    let recipients = side_channel_reply_recipients(&run.member.name, &messages);
    if recipients.is_empty() {
        return Ok(false);
    }
    let Some(node_client) = node_clients.get_mut(&run.node_id) else {
        append_event(
            team_dir,
            "app_server_side_channel_reply_skipped",
            serde_json::json!({
                "member": run.member.name,
                "node": run.node_id,
                "thread": run.thread_id,
                "reason": "node client missing",
                "messages": messages.len(),
            }),
        )?;
        return Ok(false);
    };
    let fork: ThreadForkResponse = node_client
        .client
        .request_typed(ClientRequest::ThreadFork {
            request_id: next_request_id(&mut node_client.request_counter),
            params: ThreadForkParams {
                thread_id: run.thread_id.clone(),
                model: model.clone(),
                cwd: Some(run.cwd.display().to_string()),
                approval_policy: approval_policy.clone(),
                sandbox: if dangerously_bypass_approvals_and_sandbox {
                    Some(SandboxMode::DangerFullAccess)
                } else {
                    None
                },
                ephemeral: true,
                exclude_turns: true,
                ..ThreadForkParams::default()
            },
        })
        .await
        .map_err(|err| anyhow!(err))?;
    let side_thread_id = fork.thread.id.clone();
    let prompt = build_side_channel_reply_prompt(&run.member, &messages, language);
    let turn: TurnStartResponse = node_client
        .client
        .request_typed(ClientRequest::TurnStart {
            request_id: next_request_id(&mut node_client.request_counter),
            params: TurnStartParams {
                thread_id: side_thread_id.clone(),
                input: vec![text_input(prompt)],
                cwd: Some(run.cwd.clone()),
                model,
                approval_policy,
                sandbox_policy: if dangerously_bypass_approvals_and_sandbox {
                    Some(codex_app_server_protocol::SandboxPolicy::DangerFullAccess)
                } else {
                    None
                },
                ..TurnStartParams::default()
            },
        })
        .await
        .map_err(|err| anyhow!(err))?;
    side_replies.insert(
        thread_key(&run.node_id, &side_thread_id),
        AppServerSideReply {
            member: run.member.clone(),
            node_id: run.node_id.clone(),
            source_thread_id: run.thread_id.clone(),
            side_thread_id: side_thread_id.clone(),
            turn_id: turn.turn.id.clone(),
            recipients: recipients.clone(),
            messages: messages.clone(),
            buffer: String::new(),
            started_at: Instant::now(),
        },
    );
    append_event(
        team_dir,
        "app_server_side_channel_reply_started",
        serde_json::json!({
            "member": run.member.name,
            "node": run.node_id,
            "source_thread": run.thread_id,
            "side_thread": side_thread_id,
            "turn": turn.turn.id,
            "recipients": recipients,
            "messages": messages.len(),
        }),
    )?;
    Ok(true)
}

fn side_channel_message_needs_fast_reply(member_name: &str, message: &MailMessage) -> bool {
    message.from != "system"
        && message.from != member_name
        && !is_side_channel_generated_message(&message.message)
        && message_requests_fast_reply(&message.from, &message.message)
}

fn is_side_channel_generated_message(message: &str) -> bool {
    let trimmed = message.trim_start();
    trimmed.starts_with("Quick side-channel reply from @")
        || (trimmed.starts_with('@') && trimmed.contains(" からの side-channel 速報返信です"))
        || trimmed.starts_with("Side-channel reply sent while your main turn was busy.")
        || trimmed.starts_with(
            "Side-channel reply: あなたの main turn が busy の間に短い返信を送りました。",
        )
}

fn message_requests_fast_reply(from: &str, message: &str) -> bool {
    let lower = message.to_lowercase();
    if lower.contains("no reply needed") || lower.contains("no response needed") {
        return false;
    }
    if from == "user" {
        return true;
    }
    message.contains('?')
        || message.contains('？')
        || lower.contains("question")
        || lower.contains("ask ")
        || lower.contains("reply")
        || lower.contains("respond")
        || lower.contains("can you")
        || lower.contains("could you")
        || lower.contains("should ")
        || lower.contains("what ")
        || lower.contains("which ")
        || lower.contains("why ")
        || lower.contains("how ")
        || message.contains("相談")
        || message.contains("確認")
        || message.contains("質問")
        || message.contains("返事")
        || message.contains("返信")
        || message.contains("教えて")
        || message.contains("どう")
        || message.contains("何")
}

fn side_channel_reply_recipients(member_name: &str, messages: &[MailMessage]) -> Vec<String> {
    let mut recipients = messages
        .iter()
        .filter(|message| side_channel_message_needs_fast_reply(member_name, message))
        .map(|message| message.from.clone())
        .collect::<Vec<_>>();
    recipients.sort();
    recipients.dedup();
    recipients
}

fn build_side_channel_reply_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    language: TeamPromptLanguage,
) -> String {
    if language.is_ja() {
        format!(
            "あなたは Codex team における @{name} の fast side-channel responder です。\n\n部署 role: {role}\n\nmain @{name} turn はまだ実行中です。止めないでください。long job を始めないでください。この side channel で広範な実装作業をしないでください。必要なら軽い local state の確認は可能ですが、主目的は部署間の対話を滑らかに保つことです。\n\n以下の incoming team messages に対して、@{name} としてすぐに簡潔に返信してください。返信は requester に直接送られるため、「返信した」というメタ要約ではなく、実質的な答えそのものを書いてください。status を聞かれた場合は current mode、blocker 有無、request/job id、command/log path、next checkpoint、expected artifact filenames、verification gate を具体的に含めてください。main turn の作業変更が必要なら、main turn が取り込むべき commitment/constraint を明記してください。不明なら具体的な clarifying question を 1 つだけ聞くか blocker を述べてください。必要がなければ markdown code fence は使わないでください。自然文は日本語で書いてください。\n\nIncoming messages:\n{}",
            summarize_side_reply_messages(messages, language),
            name = member.name,
            role = member.role,
        )
    } else {
        format!(
            "You are @{name}'s fast side-channel responder for a Codex team.\n\nYour department role: {role}\n\nThe main @{name} turn is still running. Do not stop it, do not start long jobs, and do not perform broad implementation work in this side channel. You may inspect lightweight local state if needed, but the primary purpose is to keep inter-department discussion fluid.\n\nReply immediately and concisely as @{name} to the incoming team messages below. Your reply is sent directly to the requester, so it must be the substantive answer itself, not a meta-summary of what you did. Do not write phrases like \"I replied\", \"handed back\", \"will tell lead\", or \"status was provided\" unless you also include the actual requested facts in the same message. If the incoming message asks for status, include concrete status fields directly: current mode, blocker or none, request/job id or none, command/log path if any, next checkpoint, expected artifact filenames, and any verification gate. If the request requires the main turn to change its work, state the exact commitment or constraint that the main turn must incorporate. If you are unsure, ask one concrete clarifying question or state the blocker. Do not include markdown code fences unless necessary.\n\nIncoming messages:\n{}",
            summarize_side_reply_messages(messages, language),
            name = member.name,
            role = member.role,
        )
    }
}

fn append_turn_steer_result<E: std::fmt::Display>(
    team_dir: &Path,
    member_name: &str,
    run: &AppServerMemberRun,
    message_count: usize,
    result: std::result::Result<TurnSteerResponse, E>,
) -> Result<()> {
    match result {
        Ok(response) => {
            append_event(
                team_dir,
                "app_server_turn_steered",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id.clone(),
                    "turn": response.turn_id,
                    "messages": message_count,
                }),
            )?;
        }
        Err(err) => {
            append_event(
                team_dir,
                "app_server_turn_steer_skipped",
                serde_json::json!({
                    "member": member_name,
                    "node": run.node_id,
                    "thread": run.thread_id.clone(),
                    "turn": run.turn_id.clone(),
                    "messages": message_count,
                    "error": err.to_string(),
                }),
            )?;
        }
    }
    Ok(())
}

fn discuss_team(root: &Path, args: DiscussArgs) -> Result<()> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    let cwd = args
        .cwd
        .clone()
        .unwrap_or(std::env::current_dir().context("resolve current directory")?);
    let codex_exe = std::env::current_exe().context("resolve current Codex executable")?;
    if args.dry_run {
        print_discussion_dry_run(&team_dir, args.rounds, &cwd, &codex_exe)?;
        return Ok(());
    }
    run_discussion_rounds(
        &team_dir,
        &config.id,
        &cwd,
        &codex_exe,
        args.rounds,
        args.model.as_deref(),
        args.profile.as_deref(),
        args.sandbox.as_deref(),
        args.dangerously_bypass_approvals_and_sandbox,
    )
}

fn print_discussion_dry_run(
    team_dir: &Path,
    rounds: u32,
    cwd: &Path,
    codex_exe: &Path,
) -> Result<()> {
    if rounds == 0 {
        return Ok(());
    }
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let workers = team_workers(&config);
    for round in 1..=rounds {
        for member in &workers {
            println!("--- discuss round {round}/{rounds}: {} ---", member.name);
            println!("{} exec -C {} <prompt>", codex_exe.display(), cwd.display());
            println!(
                "{}",
                build_discussion_prompt(&config, &tasks, member, round, rounds)
            );
        }
    }
    Ok(())
}

fn run_discussion_rounds(
    team_dir: &Path,
    team_id: &str,
    cwd: &Path,
    codex_exe: &Path,
    rounds: u32,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<()> {
    if rounds == 0 {
        return Ok(());
    }
    let config = load_config(team_dir)?;
    let workers = team_workers(&config);
    if workers.is_empty() {
        bail!("team `{}` has no worker members to discuss", config.id);
    }

    append_event(
        team_dir,
        "discussion_started",
        serde_json::json!({ "rounds": rounds }),
    )?;
    send_system_message_to_members(
        team_dir,
        &config,
        "lead",
        &workers,
        &format!(
            "Discussion starting for team goal: {}. Read your inbox, share assumptions, blockers, handoffs, and review concerns.",
            config.goal
        ),
    )?;

    for round in 1..=rounds {
        let tasks = load_tasks(team_dir)?;
        for member in &workers {
            let log_path = team_dir
                .join("logs")
                .join(format!("discuss-round{round}-{}.log", member.name));
            let last_message_path = team_dir
                .join("last_messages")
                .join(format!("discuss-round{round}-{}.md", member.name));
            let prompt = build_discussion_prompt(&config, &tasks, member, round, rounds);
            append_event(
                team_dir,
                "discussion_member_started",
                serde_json::json!({ "round": round, "member": member.name }),
            )?;
            let status = run_codex_exec(
                codex_exe,
                cwd,
                team_id,
                &member.name,
                &member.role,
                &prompt,
                &log_path,
                &last_message_path,
                model,
                profile,
                sandbox,
                dangerously_bypass_approvals_and_sandbox,
            )?;
            append_event(
                team_dir,
                if status.success() {
                    "discussion_member_completed"
                } else {
                    "discussion_member_failed"
                },
                serde_json::json!({ "round": round, "member": member.name, "status": status.code() }),
            )?;
            if !status.success() {
                bail!(
                    "discussion round {round} failed for member `{}`",
                    member.name
                );
            }
        }
    }
    append_event(
        team_dir,
        "discussion_completed",
        serde_json::json!({ "rounds": rounds }),
    )?;
    Ok(())
}

fn run_lead_synthesis(
    team_dir: &Path,
    team_id: &str,
    cwd: &Path,
    codex_exe: &Path,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<()> {
    set_member_status(team_dir, "lead", MemberStatus::Running)?;
    let log_path = team_dir.join("logs").join("lead.log");
    let summary_path = team_dir.join("summary.md");
    let stdout =
        fs::File::create(&log_path).with_context(|| format!("create {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;
    let prompt = build_lead_synthesis_prompt(team_dir)?;

    let mut command = Command::new(codex_exe);
    command
        .arg("exec")
        .arg("-C")
        .arg(cwd)
        .arg("-o")
        .arg(&summary_path)
        .env("CODEX_TEAM_ID", team_id)
        .env("CODEX_TEAM_MEMBER", "lead")
        .env("CODEX_TEAM_ROLE", "lead")
        .env("CODEX_TEAM_CLI", codex_exe)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    if let Some(model) = model {
        command.arg("--model").arg(model);
    }
    if let Some(profile) = profile {
        command.arg("--profile").arg(profile);
    }
    if let Some(sandbox) = sandbox {
        command.arg("--sandbox").arg(sandbox);
    }
    if dangerously_bypass_approvals_and_sandbox {
        command.arg("--dangerously-bypass-approvals-and-sandbox");
    }
    command.arg(prompt);

    append_event(
        team_dir,
        "lead_synthesis_started",
        serde_json::json!({ "log": log_path, "summary": summary_path }),
    )?;
    let status = command.spawn()?.wait()?;
    if status.success() {
        set_member_status(team_dir, "lead", MemberStatus::Completed)?;
        append_event(
            team_dir,
            "lead_synthesis_completed",
            serde_json::json!({ "status": status.code(), "summary": summary_path }),
        )?;
        println!("Summary: {}", summary_path.display());
        Ok(())
    } else {
        set_member_status(team_dir, "lead", MemberStatus::Failed)?;
        append_event(
            team_dir,
            "lead_synthesis_failed",
            serde_json::json!({ "status": status.code() }),
        )?;
        bail!("lead synthesis failed");
    }
}

fn list_teams(root: &Path) -> Result<()> {
    let mut teams = load_team_summaries(root)?;
    if teams.is_empty() {
        println!("No teams found.");
        return Ok(());
    }
    teams.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    for team in teams {
        println!("{}  {}  {}", team.id, team.updated_at, team.goal);
    }
    Ok(())
}

fn print_status(team_dir: &Path) -> Result<()> {
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    println!("Team: {}", config.id);
    println!("Goal: {}", config.goal);
    println!("Members: {}", config.members.len());
    for member in &config.members {
        let task_status = member_task_status_summary(&tasks, &member.name);
        let mail = mailbox_unread_counts(team_dir, &member.name)?;
        println!(
            "  {} ({}) session={:?} tasks={} node={} unread={} direct={}",
            member.name,
            member.role,
            member.status,
            task_status,
            member.node.as_deref().unwrap_or("local"),
            mail.unread,
            mail.direct_unread
        );
    }
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    println!("Nodes: {}", nodes.len());
    for node in nodes {
        println!("{}", format_node_status_line(&node));
    }
    let cooldowns = format_usage_limit_cooldowns(team_dir, &config)?;
    if !cooldowns.is_empty() {
        print!("{cooldowns}");
    }
    let waits = load_waits(team_dir)?;
    let open_waits = waits.iter().filter(|wait| wait.status.is_open()).count();
    if open_waits > 0 {
        println!("Waits: {open_waits} open, {} total", waits.len());
        for wait in waits.iter().filter(|wait| wait.status.is_open()).take(12) {
            println!("{}", format_wait_line(wait));
        }
    }
    println!("Tasks: {}", format_task_status_counts(&tasks));
    for task in &tasks {
        print_task(task);
    }
    let ownerships = load_ownerships(team_dir)?;
    if !ownerships.is_empty() {
        println!("Ownerships: {}", ownerships.len());
        for ownership in ownerships {
            print_ownership(&ownership);
        }
    }
    Ok(())
}

fn run_task(root: &Path, cli: TaskCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    let _lock = lock_team_state(&team_dir)?;
    match cli.subcommand {
        TaskSubcommand::Add(args) => {
            let task = create_task(&team_dir, args)?;
            append_event(
                &team_dir,
                "task_created",
                serde_json::json!({ "task": task }),
            )?;
            auto_promote_dependency_waits(&team_dir)?;
            touch_config(&team_dir)?;
            println!("Created task {}", task.id);
            Ok(())
        }
        TaskSubcommand::Claim(args) => claim_ready_task(&team_dir, args),
        TaskSubcommand::List => {
            auto_promote_dependency_waits(&team_dir)?;
            let tasks = load_tasks(&team_dir)?;
            if tasks.is_empty() {
                println!("No tasks found.");
                return Ok(());
            }
            for task in &tasks {
                print_task(task);
            }
            Ok(())
        }
        TaskSubcommand::Set(args) => update_task(&team_dir, args),
    }
}

#[cfg(unix)]
struct TeamStateLock {
    file: fs::File,
}

#[cfg(unix)]
impl Drop for TeamStateLock {
    fn drop(&mut self) {
        // Closing the fd would also release the lock. Explicit unlock keeps
        // repeated task operations in long-lived processes straightforward.
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

#[cfg(not(unix))]
struct TeamStateLock;

fn lock_team_state(team_dir: &Path) -> Result<TeamStateLock> {
    fs::create_dir_all(team_dir)?;
    #[cfg(unix)]
    {
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(team_dir.join(".team-state.lock"))?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            bail!(
                "failed to lock team state: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(TeamStateLock { file })
    }
    #[cfg(not(unix))]
    {
        Ok(TeamStateLock)
    }
}

fn run_ownership(root: &Path, cli: OwnershipCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    match cli.subcommand {
        OwnershipSubcommand::List => {
            let ownerships = load_ownerships(&team_dir)?;
            if ownerships.is_empty() {
                println!("No ownership claims.");
                return Ok(());
            }
            for ownership in &ownerships {
                print_ownership(ownership);
            }
            Ok(())
        }
        OwnershipSubcommand::Claim(args) => claim_ownership(&team_dir, args),
        OwnershipSubcommand::Release(args) => release_ownership(&team_dir, args),
    }
}

fn run_member(root: &Path, cli: MemberCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    match cli.subcommand {
        MemberSubcommand::List => {
            let config = load_config(&team_dir)?;
            for member in &config.members {
                println!(
                    "{:<20} {:<16} {:<16} {:?}",
                    member.name,
                    member.role,
                    member.node.as_deref().unwrap_or("local"),
                    member.status
                );
            }
            Ok(())
        }
        MemberSubcommand::Add(args) => add_team_member(&team_dir, args),
        MemberSubcommand::Standby(args) => standby_team_member(&team_dir, args),
        MemberSubcommand::Resume(args) => resume_team_member(&team_dir, args),
    }
}

fn send_message(root: &Path, args: MessageArgs) -> Result<()> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let mut config = load_config(&team_dir)?;
    let from = sanitize_id(&args.from.unwrap_or_else(default_team_member_name));
    if from != "system" && from != "user" {
        ensure_member_exists(&config, &from)?;
    }
    let recipients = resolve_message_recipients(&config, &from, &args.to)?;

    for recipient in &recipients {
        let msg = MailMessage {
            from: from.clone(),
            to: recipient.clone(),
            message: args.message.clone(),
            timestamp: now(),
            read: false,
        };
        append_jsonl(&mailbox_path(&team_dir, &msg.to), &msg)?;
    }
    append_event(
        &team_dir,
        "message_sent",
        serde_json::json!({ "from": from, "to": recipients, "message": args.message }),
    )?;
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)?;
    println!("Message sent to {}", args.to);
    Ok(())
}

fn read_inbox(root: &Path, args: InboxArgs) -> Result<()> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    let member = args.member.unwrap_or_else(default_team_member_name);
    ensure_member_exists(&config, &member)?;
    let mailbox = mailbox_path(&team_dir, &member);
    let messages = read_jsonl::<MailMessage>(&mailbox)?;
    if messages.is_empty() {
        println!("Inbox for `{member}` is empty.");
        return Ok(());
    }
    for msg in messages {
        println!(
            "[{}] {} -> {}: {}",
            msg.timestamp, msg.from, msg.to, msg.message
        );
    }
    Ok(())
}

fn read_logs(root: &Path, args: LogsArgs) -> Result<()> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    if let Some(member) = args.member {
        ensure_member_exists(&config, &member)?;
        let path = if args.live {
            team_dir
                .join("live_messages")
                .join(format!("{}.md", sanitize_id(&member)))
        } else if args.last_message {
            team_dir
                .join("last_messages")
                .join(format!("{}.md", sanitize_id(&member)))
        } else {
            team_dir
                .join("logs")
                .join(format!("{}.log", sanitize_id(&member)))
        };
        if !path.exists() {
            bail!("log file does not exist: {}", path.display());
        }
        print!("{}", fs::read_to_string(&path)?);
        return Ok(());
    }

    let dir = if args.live {
        team_dir.join("live_messages")
    } else if args.last_message {
        team_dir.join("last_messages")
    } else {
        team_dir.join("logs")
    };
    if !dir.exists() {
        println!("No logs found.");
        return Ok(());
    }
    let mut entries = fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().map(|ty| ty.is_file()).unwrap_or(false))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    if entries.is_empty() {
        println!("No logs found.");
        return Ok(());
    }
    for path in entries {
        println!("{}", path.display());
    }
    Ok(())
}

fn start_tmux_monitor(root: &Path, args: MonitorArgs) -> Result<()> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    let session = args
        .session
        .unwrap_or_else(|| format!("codex-team-{}", sanitize_id(&config.id)));
    let codex_exe = std::env::current_exe().context("resolve current Codex executable")?;

    if Command::new("tmux").arg("-V").output().is_err() {
        bail!("tmux is not installed or not on PATH");
    }
    if tmux_session_exists(&session)? {
        if args.force {
            run_tmux(["kill-session", "-t", &session])?;
        } else {
            bail!("tmux session `{session}` already exists; pass --force or choose --session");
        }
    }

    let status_cmd = format!(
        "watch -n 2 '{} team status --team {}'",
        sh_quote(&codex_exe.display().to_string()),
        sh_quote(&config.id)
    );
    run_tmux([
        "new-session",
        "-d",
        "-s",
        &session,
        "-n",
        "team",
        &status_cmd,
    ])?;

    let events_cmd = format!(
        "cd {} && touch events.jsonl && tail -n 80 -f events.jsonl",
        sh_quote(&team_dir.display().to_string())
    );
    run_tmux(["split-window", "-t", &session, "-h", &events_cmd])?;

    let mail_cmd = format!(
        "cd {} && mkdir -p mailboxes && touch mailboxes/.keep && tail -n 40 -F mailboxes/*.jsonl",
        sh_quote(&team_dir.display().to_string())
    );
    run_tmux(["split-window", "-t", &session, "-v", &mail_cmd])?;

    let live_cmd = format!(
        "cd {} && mkdir -p live_messages && touch live_messages/.keep && tail -n 80 -F live_messages/*.md",
        sh_quote(&team_dir.display().to_string())
    );
    run_tmux(["select-pane", "-t", &format!("{session}:0.0")])?;
    run_tmux(["split-window", "-t", &session, "-v", &live_cmd])?;
    run_tmux(["select-layout", "-t", &session, "tiled"])?;

    println!("tmux monitor: {session}");
    println!("Attach: tmux attach -t {session}");
    println!("Team: {}", config.id);
    println!("State: {}", team_dir.display());
    if args.attach {
        let status = Command::new("tmux")
            .arg("attach")
            .arg("-t")
            .arg(&session)
            .status()
            .context("attach tmux monitor")?;
        if !status.success() {
            bail!("tmux attach failed with status {status}");
        }
    }
    Ok(())
}

fn tmux_session_exists(session: &str) -> Result<bool> {
    let status = Command::new("tmux")
        .arg("has-session")
        .arg("-t")
        .arg(session)
        .stderr(Stdio::null())
        .status()
        .context("check tmux session")?;
    Ok(status.success())
}

fn run_tmux<const N: usize>(args: [&str; N]) -> Result<()> {
    let status = Command::new("tmux")
        .args(args)
        .status()
        .context("run tmux")?;
    if !status.success() {
        bail!("tmux command failed with status {status}");
    }
    Ok(())
}

fn start_team_ui(root: &Path, args: UiArgs) -> Result<()> {
    fs::create_dir_all(root)?;
    let _ui_app_server = if args.no_app_server_auto_start {
        None
    } else {
        ensure_team_ui_app_server(root)?
    };
    let (listener, fallback_notice) = bind_team_ui_listener(&args.listen)?;
    let listen_addr = listener.local_addr().context("read team UI listen addr")?;
    let url = format!("http://{listen_addr}");
    if let Some(notice) = fallback_notice {
        println!("{notice}");
    }
    println!("Codex team UI: {url}");
    if args.open {
        let _ = Command::new("xdg-open").arg(&url).spawn();
    }
    for stream in listener.incoming() {
        let mut stream = stream.context("accept team UI connection")?;
        if let Err(err) = handle_team_ui_request(root, &args, &mut stream) {
            let body = format!("error: {err}\n");
            let _ = write_http_response(
                &mut stream,
                "500 Internal Server Error",
                "text/plain",
                &body,
            );
        }
    }
    Ok(())
}

fn bind_team_ui_listener(listen: &str) -> Result<(TcpListener, Option<String>)> {
    match TcpListener::bind(listen) {
        Ok(listener) => Ok((listener, None)),
        Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => {
            let Ok(mut addr) = listen.parse::<std::net::SocketAddr>() else {
                return Err(err).with_context(|| format!("bind {listen}"));
            };
            let requested = addr;
            addr.set_port(0);
            let listener = TcpListener::bind(addr)
                .with_context(|| format!("bind fallback team UI listener for {requested}"))?;
            let actual = listener.local_addr().context("read fallback UI addr")?;
            Ok((
                listener,
                Some(format!(
                    "Requested team UI address {requested} is already in use; using {actual} instead."
                )),
            ))
        }
        Err(err) => Err(err).with_context(|| format!("bind {listen}")),
    }
}

pub(crate) fn ensure_team_ui_app_server(root: &Path) -> Result<Option<Child>> {
    if let Some(url) = read_registered_app_server_url()? {
        if app_server_readyz(&url) {
            println!("Using registered app-server: {url}");
            return Ok(None);
        }
        eprintln!("Removing stale app-server registry: {url}");
        let _ = clear_app_server_registry_if_matches(&url);
        let _ = remove_app_server_registry();
    }

    let listener = TcpListener::bind("127.0.0.1:0").context("reserve team UI app-server port")?;
    let addr = listener.local_addr()?;
    drop(listener);
    let url = format!("ws://{addr}");
    let log_path = root.join("ui-app-server.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let stderr = log.try_clone()?;
    let mut child = Command::new(std::env::current_exe()?)
        .arg("app-server")
        .arg("--listen")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("spawn shared app-server for team UI")?;

    for _ in 0..50 {
        if app_server_readyz(&url) {
            println!("Started shared app-server: {url}");
            return Ok(Some(child));
        }
        if let Some(status) = child.try_wait()? {
            bail!("shared app-server exited early with status {status}");
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("shared app-server did not become ready at {url}");
}

fn app_server_readyz(url: &str) -> bool {
    let Some((host, port)) = parse_ws_host_port(url) else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect((host.as_str(), port)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let request =
        format!("GET /readyz HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = [0_u8; 64];
    let Ok(n) = stream.read(&mut response) else {
        return false;
    };
    String::from_utf8_lossy(&response[..n]).starts_with("HTTP/1.1 200")
}

fn parse_ws_host_port(url: &str) -> Option<(String, u16)> {
    let rest = url.strip_prefix("ws://")?;
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .filter(|value| !value.is_empty())?;
    let (host, port) = authority.rsplit_once(':')?;
    let port = port.parse::<u16>().ok()?;
    if host.is_empty() {
        return None;
    }
    Some((host.trim_matches(['[', ']']).to_string(), port))
}

fn handle_team_ui_request(
    root: &Path,
    args: &UiArgs,
    stream: &mut std::net::TcpStream,
) -> Result<()> {
    let request = read_http_request(stream)?;
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => {
            let selected = request.query.get("team").cloned();
            let selected_cwd = request.query.get("cwd").cloned();
            let selected_translation = request.query.get("translation").cloned();
            let html = render_team_ui(
                root,
                args,
                selected.as_deref(),
                selected_cwd.as_deref(),
                selected_translation.as_deref(),
            )?;
            write_http_response(stream, "200 OK", "text/html; charset=utf-8", &html)?;
        }
        ("GET", "/realtime") => {
            let team = request
                .query
                .get("team")
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .context("missing query parameter `team`")?;
            let team_dir = resolve_team_dir(root, Some(&team))?;
            let json = render_team_realtime_json(&team_dir)?;
            write_http_response(stream, "200 OK", "application/json; charset=utf-8", &json)?;
        }
        ("GET", "/debug") => {
            let team = request
                .query
                .get("team")
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .context("missing query parameter `team`")?;
            let team_dir = resolve_team_dir(root, Some(&team))?;
            let json = render_team_debug_json(&team_dir)?;
            write_http_response(stream, "200 OK", "application/json; charset=utf-8", &json)?;
        }
        ("POST", "/message") => {
            let form = parse_form(&request.body);
            let team = form_value(&form, "team")?;
            let to = form
                .get("to")
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| "lead".to_string());
            let message = form_value(&form, "message")?;
            send_message(
                root,
                MessageArgs {
                    selector: TeamSelector {
                        team: Some(team.clone()),
                    },
                    from: Some("user".to_string()),
                    to,
                    message,
                },
            )?;
            redirect_team_ui(stream, Some(&team))?;
        }
        ("POST", "/translate") => {
            let form = parse_form(&request.body);
            let team = form_value(&form, "team")?;
            let language = form
                .get("language")
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| "ja".to_string());
            let team_dir = resolve_team_dir(root, Some(&team))?;
            start_translate_team_messages(&team_dir, &language)?;
            redirect_team_ui_with_params(
                stream,
                &[("team", team.as_str()), ("translation", language.as_str())],
            )?;
        }
        ("POST", "/delete") => {
            let form = parse_form(&request.body);
            let team = form_value(&form, "team")?;
            stop_ui_team_process(root, &team)?;
            cleanup_team(
                root,
                CleanupArgs {
                    selector: TeamSelector {
                        team: Some(team.clone()),
                    },
                    force: true,
                },
            )?;
            remove_ui_team_pid(root, &team)?;
            redirect_team_ui(stream, None)?;
        }
        ("POST", "/new") => {
            let form = parse_form(&request.body);
            let goal = form_value(&form, "goal")?;
            let cwd = expand_home(
                form.get("cwd")
                    .filter(|value| !value.trim().is_empty())
                    .cloned()
                    .unwrap_or_else(|| default_ui_cwd(args)),
            );
            let app_server_url = form
                .get("app_server_url")
                .filter(|value| !value.trim().is_empty())
                .cloned();
            let team_id = form
                .get("id")
                .map(|value| sanitize_id(value))
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("team-{}", tokyo_now().format("%Y%m%d%H%M%S")));
            let members = split_ui_lines(form.get("members").map(String::as_str).unwrap_or(""));
            let nodes = split_ui_lines(form.get("nodes").map(String::as_str).unwrap_or(""));
            let discuss_rounds = form
                .get("discuss_rounds")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .unwrap_or("0")
                .to_string();
            let no_keep_alive = form.contains_key("no_keep_alive");
            let bypass_sandbox = form.contains_key("dangerously_bypass")
                || !form.contains_key("dangerously_bypass_present");
            let registered_app_server_url = read_registered_app_server_url().unwrap_or(None);
            let mut command = Command::new(std::env::current_exe()?);
            command.arg("team").arg("swarm");
            command.arg("--id").arg(&team_id);
            for node in nodes {
                command.arg("--node").arg(node);
            }
            for member in members {
                command.arg("--member").arg(member);
            }
            command
                .arg("--app-server")
                .arg("--discuss-rounds")
                .arg(discuss_rounds)
                .arg("--cd")
                .arg(cwd);
            if bypass_sandbox {
                command.arg("--dangerously-bypass-approvals-and-sandbox");
            }
            if no_keep_alive {
                command.arg("--no-keep-alive");
            }
            if let Some(app_server_url) = app_server_url {
                if registered_app_server_url.as_deref() != Some(app_server_url.as_str()) {
                    command.arg("--app-server-url").arg(app_server_url);
                }
            } else {
                command.arg("--no-app-server-registry");
            }
            command.arg(goal).stdin(Stdio::null());
            let log_path = root.join("ui-runs.log");
            let log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .with_context(|| format!("open {}", log_path.display()))?;
            let stderr = log.try_clone()?;
            command.stdout(Stdio::from(log)).stderr(Stdio::from(stderr));
            let child = command.spawn().context("spawn team run from UI")?;
            write_ui_team_pid(root, &team_id, child.id())?;
            redirect_team_ui(stream, Some(&team_id))?;
        }
        _ => {
            write_http_response(stream, "404 Not Found", "text/plain", "not found\n")?;
        }
    }
    Ok(())
}

struct HttpRequest {
    method: String,
    path: String,
    query: HashMap<String, String>,
    body: String,
}

fn read_http_request(stream: &mut std::net::TcpStream) -> Result<HttpRequest> {
    let mut buf = Vec::new();
    let mut tmp = [0_u8; 4096];
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 1024 * 1024 {
            bail!("HTTP request too large");
        }
    }
    let header_end = buf
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|idx| idx + 4)
        .context("malformed HTTP request")?;
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = headers.lines();
    let request_line = lines.next().context("empty HTTP request")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("/");
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = String::from_utf8_lossy(
        &buf[header_end..header_end + content_length.min(buf.len().saturating_sub(header_end))],
    )
    .to_string();
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), parse_form(query)),
        None => (target.to_string(), HashMap::new()),
    };
    Ok(HttpRequest {
        method,
        path,
        query,
        body,
    })
}

fn write_http_response(
    stream: &mut std::net::TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(())
}

fn redirect_team_ui(stream: &mut std::net::TcpStream, team: Option<&str>) -> Result<()> {
    let location = team
        .map(|team| format!("/?team={}", url_encode(team)))
        .unwrap_or_else(|| "/".to_string());
    write_redirect_response(stream, &location)
}

fn redirect_team_ui_with_params(
    stream: &mut std::net::TcpStream,
    params: &[(&str, &str)],
) -> Result<()> {
    let query = params
        .iter()
        .map(|(key, value)| format!("{}={}", url_encode(key), url_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    write_redirect_response(
        stream,
        &format!(
            "/{query_prefix}{query}",
            query_prefix = if query.is_empty() { "" } else { "?" }
        ),
    )
}

fn write_redirect_response(stream: &mut std::net::TcpStream, location: &str) -> Result<()> {
    let body = "redirect\n";
    write!(
        stream,
        "HTTP/1.1 303 See Other\r\nLocation: {location}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    Ok(())
}

fn render_team_ui(
    root: &Path,
    args: &UiArgs,
    selected: Option<&str>,
    selected_cwd: Option<&str>,
    selected_translation: Option<&str>,
) -> Result<String> {
    let mut teams = load_team_summaries(root)?;
    teams.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    let selected_id = selected
        .map(sanitize_id)
        .or_else(|| teams.first().map(|team| team.id.clone()));
    let selected_dir = selected_id.as_ref().map(|team| root.join(team));
    let selected_config = selected_dir.as_ref().and_then(|dir| load_config(dir).ok());
    let selected_tasks = selected_dir
        .as_ref()
        .and_then(|dir| load_tasks(dir).ok())
        .unwrap_or_default();
    let selected_nodes = selected_dir
        .as_ref()
        .and_then(|dir| load_nodes(dir).ok())
        .map(|mut nodes| {
            ensure_local_node(&mut nodes);
            nodes
        })
        .unwrap_or_else(|| {
            let mut nodes = Vec::new();
            ensure_local_node(&mut nodes);
            nodes
        });
    let selected_events = selected_dir
        .as_ref()
        .and_then(|dir| render_events_for_ui(&dir.join("events.jsonl")).ok())
        .unwrap_or_default();
    let selected_cwd = selected_cwd
        .map(|value| expand_home(value.to_string()))
        .unwrap_or_else(|| {
            args.default_cwd
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(default_home)
        });
    let registered_app_server_url = read_registered_app_server_url()?.unwrap_or_default();
    let directory_picker = render_directory_picker(selected_cwd.as_str(), selected_id.as_deref())?;
    let ui_runs_log = fs::read_to_string(root.join("ui-runs.log"))
        .ok()
        .map(|log| {
            log.lines()
                .rev()
                .take(80)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .map(html_escape)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|log| !log.trim().is_empty())
        .map(|log| format!(r#"<details><summary>UI Run Log</summary><pre>{log}</pre></details>"#))
        .unwrap_or_default();
    let team_links = teams
        .iter()
        .map(|team| {
            let active = selected_id.as_deref() == Some(team.id.as_str());
            let run_status = ui_team_run_status(root, team);
            format!(
                r#"<div class="team-wrap" data-team="{id}"><a class="team {active}" href="/?team={id}"><strong>{id}</strong><span>{goal}</span><small>{updated}</small><em class="run-state {run_class}">{run_label}</em></a></div>"#,
                active = if active { "active" } else { "" },
                id = html_escape(&team.id),
                goal = html_escape(&team.goal),
                updated = html_escape(&timestamp_for_ui(&team.updated_at)),
                run_class = run_status.css_class(),
                run_label = run_status.label(),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let detail = if let Some(config) = selected_config {
        let node_by_id = selected_nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
            .collect::<HashMap<_, _>>();
        let members = config
            .members
            .iter()
            .map(|member| {
                let task_status = member_task_status_summary(&selected_tasks, &member.name);
                let mail = selected_dir
                    .as_ref()
                    .and_then(|dir| mailbox_unread_counts(dir, &member.name).ok())
                    .unwrap_or_default();
                let cooldown = selected_dir
                    .as_ref()
                    .and_then(|dir| recent_usage_limit_retry_remaining(dir, &member.name).ok())
                    .flatten()
                    .map(|remaining| format_compact_duration(remaining.as_secs()))
                    .unwrap_or_default();
                let node_id = infer_member_node_for_ui(
                    selected_dir.as_deref(),
                    member,
                    member.node.as_deref().unwrap_or("local"),
                );
                let location = node_by_id
                    .get(node_id.as_str())
                    .map(format_node_location)
                    .unwrap_or_else(|| node_id.clone());
                format!(
                    "<tr><td>{}</td><td>{}</td><td>{:?}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td></tr>",
                    html_escape(&member.name),
                    html_escape(&member.role),
                    member.status,
                    html_escape(&task_status),
                    html_escape(&node_id),
                    html_escape(&location),
                    html_escape(&format!("{}/{}", mail.unread, mail.direct_unread)),
                    html_escape(if cooldown.is_empty() { "-" } else { &cooldown }),
                    html_escape(member.thread_id.as_deref().unwrap_or(""))
                )
            })
            .collect::<Vec<_>>()
            .join("");
        let nodes = selected_nodes
            .iter()
            .map(|node| {
                let (age, stale) = format_node_last_seen_age(&node.updated_at);
                format!(
                    "<tr><td>{}</td><td>{:?}</td><td>{:?}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    html_escape(&node.id),
                    node.kind,
                    node.status,
                    html_escape(node.url.as_deref().unwrap_or("")),
                    html_escape(&timestamp_for_ui(&node.updated_at)),
                    html_escape(&age),
                    if stale {
                        r#"<span class="pill warn">stale</span>"#.to_string()
                    } else {
                        "-".to_string()
                    },
                    html_escape(node.host.as_deref().unwrap_or("")),
                    html_escape(node.container.as_deref().unwrap_or("")),
                    html_escape(node.cwd.as_deref().unwrap_or(""))
                )
            })
            .collect::<Vec<_>>()
            .join("");
        let tasks = selected_tasks
            .iter()
            .map(|task| {
                format!(
                    "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    html_escape(&task.id),
                    html_escape(&task.status.to_string()),
                    html_escape(task.owner.as_deref().unwrap_or("")),
                    html_escape(&task.subject)
                )
            })
            .collect::<Vec<_>>()
            .join("");
        let events = selected_events
            .lines()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(html_escape)
            .collect::<Vec<_>>()
            .join("\n");
        let translation_language = selected_translation.unwrap_or("ja");
        let message_board = selected_dir
            .as_ref()
            .map(|dir| render_message_board(dir, &config.id, translation_language))
            .transpose()?
            .unwrap_or_default();
        let lead_chat = selected_dir
            .as_ref()
            .map(|dir| render_lead_chat(dir, &config.id))
            .transpose()?
            .unwrap_or_default();
        let thread_board = selected_dir
            .as_ref()
            .map(|dir| render_thread_board(dir, &config, &node_by_id))
            .transpose()?
            .unwrap_or_default();
        let realtime_view = render_realtime_view(&config.id, &config);
        let debug_timeline = render_debug_timeline_view(&config.id);
        format!(
            r#"<section><h2>{id}</h2><p>{goal}</p>
<h3>Lead Chat</h3>{lead_chat}
{realtime_view}
{debug_timeline}
<h3>Members</h3><table><tr><th>Name</th><th>Role</th><th>Session</th><th>Tasks</th><th>Node</th><th>Location</th><th>Unread/Direct</th><th>Cooldown</th><th>Thread</th></tr>{members}</table>
<h3>Nodes</h3><table><tr><th>ID</th><th>Kind</th><th>Status</th><th>URL</th><th>Last Seen</th><th>Age</th><th>Health</th><th>Host</th><th>Container</th><th>CWD</th></tr>{nodes}</table>
<h3>Tasks</h3><table><tr><th>ID</th><th>Status</th><th>Owner</th><th>Subject</th></tr>{tasks}</table>
<h3>Team Messages</h3>{message_board}
<h3>Thread Contents</h3>{thread_board}
<h3>Events</h3><pre>{events}</pre></section>"#,
            id = html_escape(&config.id),
            goal = html_escape(&config.goal),
            lead_chat = lead_chat,
            realtime_view = realtime_view,
            debug_timeline = debug_timeline,
            members = members,
            nodes = nodes,
            tasks = tasks,
            message_board = message_board,
            thread_board = thread_board,
            events = events,
        )
    } else {
        "<section><h2>No team selected</h2></section>".to_string()
    };
    Ok(format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><title>Codex Teams</title>
<style>
body{{margin:0;font:14px system-ui,sans-serif;background:#f6f7f9;color:#1b1f24}}
.app{{display:grid;grid-template-columns:320px 1fr;min-height:100vh}}
aside{{background:#fff;border-right:1px solid #d8dee4;padding:16px;overflow:auto}}
main{{padding:20px;overflow:auto}}
.team-wrap{{position:relative}}
.team{{display:block;padding:10px;border-radius:6px;color:inherit;text-decoration:none;border:1px solid transparent;margin-bottom:8px}}
.team.active{{background:#eaf2ff;border-color:#8bb8ff}}
.team span,.team small{{display:block;color:#59636e;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
.run-state{{display:inline-block;margin-top:7px;padding:1px 7px;border:1px solid #d8dee4;border-radius:999px;font-size:12px;font-style:normal;color:#59636e;background:#f6f8fa}}
.run-running{{color:#116329;background:#dafbe1;border-color:#4ac26b}}
.run-stopped{{color:#82071e;background:#ffebe9;border-color:#ff8182}}
.run-unknown{{color:#59636e;background:#f6f8fa}}
.context-menu{{display:none;position:fixed;z-index:100;background:#fff;border:1px solid #d8dee4;border-radius:6px;box-shadow:0 8px 24px rgba(27,31,36,.16);padding:6px}}
.context-menu.open{{display:block}}
.context-menu form{{margin:0;padding:0;border:0;background:transparent}}
.context-menu button{{width:170px;text-align:left;background:transparent;border:0;border-radius:4px;padding:8px 10px;color:#82071e}}
.context-menu button:hover{{background:#ffebe9}}
form{{display:grid;gap:10px;margin:12px 0;padding:12px;background:#fff;border:1px solid #d8dee4;border-radius:6px}}
label{{display:grid;gap:4px}} input,textarea{{font:inherit;padding:8px;border:1px solid #c9d1d9;border-radius:4px}} button{{width:max-content;padding:8px 12px}}
.dir-picker{{background:#fff;border:1px solid #d8dee4;border-radius:6px;padding:10px;margin:10px 0;max-height:260px;overflow:auto}}
.dir-picker a{{display:block;padding:5px 0;color:#0969da;text-decoration:none;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
.dir-current{{font-weight:600;word-break:break-all}}
table{{width:100%;border-collapse:collapse;background:#fff}} th,td{{padding:8px;border:1px solid #d8dee4;text-align:left;vertical-align:top}}
pre{{background:#111827;color:#d1d5db;padding:12px;border-radius:6px;overflow:auto;max-height:360px}}
.messages{{display:grid;gap:8px;max-height:520px;overflow:auto}}
.msg{{background:#fff;border:1px solid #d8dee4;border-radius:6px;padding:10px}}
.lead-chat .msg{{border-left:4px solid #8c959f}}
.lead-chat .chat-user{{border-left-color:#0969da}}
.lead-chat .chat-lead{{border-left-color:#1a7f37}}
.msg-meta{{display:flex;gap:8px;flex-wrap:wrap;color:#59636e;font-size:12px;margin-bottom:4px}}
.pill{{display:inline-block;background:#eef2f7;border:1px solid #d8dee4;border-radius:999px;padding:1px 7px;color:#39424e}}
.pill.warn{{background:#fff8c5;border-color:#d4a72c;color:#7d4e00}}
.hint{{margin:8px 0;color:#59636e;font-size:12px;line-height:1.4}}
.translate-form{{display:flex;align-items:end;gap:10px;flex-wrap:wrap}}
.translate-form label{{display:grid;gap:4px}}
.translation{{margin:10px 0}}
.threads{{display:grid;gap:10px}}
details{{background:#fff;border:1px solid #d8dee4;border-radius:6px;padding:10px}}
summary{{cursor:pointer;font-weight:600}}
code{{font:12px ui-monospace,SFMono-Regular,Menlo,monospace;word-break:break-all}}
.rt-card{{background:#0f172a;color:#dbeafe;border:1px solid #1e293b;border-radius:8px;margin:16px 0;overflow:hidden;box-shadow:0 12px 34px rgba(15,23,42,.14)}}
.rt-head{{display:flex;align-items:center;justify-content:space-between;gap:12px;padding:12px 14px;background:#111827;border-bottom:1px solid #263244}}
.rt-title{{display:flex;align-items:center;gap:9px;font-weight:700}}
.rt-dot{{width:9px;height:9px;border-radius:50%;background:#22c55e;box-shadow:0 0 0 4px rgba(34,197,94,.16)}}
.rt-actions{{display:flex;gap:8px;flex-wrap:wrap}}
.rt-actions button{{background:#1f2937;color:#dbeafe;border:1px solid #334155;border-radius:6px;padding:7px 10px}}
.rt-actions button:hover{{background:#334155}}
.rt-help{{padding:9px 14px;color:#93a4ba;background:#0b1220;border-bottom:1px solid #1e293b;font-size:12px}}
.rt-grid{{display:none;gap:8px;padding:10px;min-height:440px;background:#020617}}
.rt-card.open .rt-grid{{display:grid}}
.rt-grid.cols{{grid-template-columns:repeat(var(--rt-cols,1),minmax(280px,1fr));grid-auto-rows:minmax(360px,1fr)}}
.rt-grid.rows{{grid-template-columns:1fr;grid-auto-rows:minmax(260px,1fr)}}
.rt-pane{{display:grid;grid-template-rows:auto 1fr;background:#050b18;border:1px solid #1e293b;border-radius:7px;min-height:260px;overflow:hidden}}
.rt-panebar{{display:flex;align-items:center;gap:8px;padding:8px;background:#0f172a;border-bottom:1px solid #1e293b}}
.rt-panebar select{{min-width:150px;background:#020617;color:#e5e7eb;border:1px solid #334155;border-radius:5px;padding:5px}}
.rt-panebar .rt-meta{{font-size:12px;color:#94a3b8;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}
.rt-panebar button{{margin-left:auto;background:#111827;color:#cbd5e1;border:1px solid #334155;border-radius:5px;padding:4px 7px}}
.rt-term{{margin:0;border-radius:0;background:#020617;color:#d1fae5;max-height:none;height:100%;font:12px ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;line-height:1.45;white-space:pre-wrap}}
.rt-status{{padding:7px 12px;color:#94a3b8;background:#0b1220;border-top:1px solid #1e293b;font-size:12px}}
.dbg-card{{background:#fff;border:1px solid #d8dee4;border-radius:8px;margin:16px 0;overflow:hidden}}
.dbg-head{{display:flex;align-items:center;justify-content:space-between;gap:12px;padding:12px 14px;border-bottom:1px solid #d8dee4;background:#f6f8fa}}
.dbg-title{{font-weight:700}}
.dbg-actions{{display:flex;gap:8px;flex-wrap:wrap;align-items:center}}
.dbg-actions button{{border:1px solid #c9d1d9;border-radius:6px;background:#fff;color:#24292f;padding:6px 9px}}
.dbg-actions button.active{{background:#0969da;border-color:#0969da;color:#fff}}
.dbg-actions input{{padding:6px 8px;min-width:240px}}
.dbg-list{{display:grid;gap:8px;padding:10px;max-height:680px;overflow:auto;background:#f6f8fa}}
.dbg-item{{background:#fff;border:1px solid #d8dee4;border-left:4px solid #8c959f;border-radius:6px;padding:9px}}
.dbg-message{{border-left-color:#0969da}}
.dbg-system{{border-left-color:#8250df}}
.dbg-event{{border-left-color:#bf8700}}
.dbg-side{{border-left-color:#cf222e}}
.dbg-live{{border-left-color:#1a7f37}}
.dbg-last{{border-left-color:#57606a}}
.dbg-meta{{display:flex;gap:6px;flex-wrap:wrap;color:#59636e;font-size:12px;margin-bottom:5px}}
.dbg-body{{white-space:pre-wrap;margin-top:7px;color:#24292f;font:12px ui-monospace,SFMono-Regular,Menlo,Consolas,monospace;max-height:260px;overflow:auto;background:#f6f8fa;border-radius:5px;padding:8px}}
.dbg-json{{white-space:pre-wrap;margin-top:7px;color:#57606a;font:12px ui-monospace,SFMono-Regular,Menlo,Consolas,monospace}}
.dbg-status{{padding:8px 12px;color:#59636e;border-top:1px solid #d8dee4;font-size:12px}}
</style></head><body><div class="app"><aside><h1>Lead Sessions</h1>{team_links}
<p><a href="{refresh_href}">Refresh</a></p>
<h2>New Team</h2><form method="post" action="/new">
<label>Team ID <input name="id" placeholder="optional-id"></label>
<label>Goal <textarea name="goal" rows="5"></textarea></label>
<input type="hidden" name="cwd" value="{selected_cwd}">
<div><strong>Current Directory</strong>{directory_picker}</div>
<label>Existing App Server URL <input name="app_server_url" value="{registered_app_server_url}" placeholder="ws://127.0.0.1:12345"></label>
<details><summary>Advanced Placement (optional override)</summary>
<p class="hint">Normally leave this closed. Lead should infer departments, SSH nodes, Docker containers, rebuilds, and placement from the natural-language goal.</p>
<label>Members <textarea name="members" rows="3" placeholder="verifier:ops@qwenbox"></textarea></label>
<label>Nodes <textarea name="nodes" rows="3" placeholder="qwenbox@ssh-docker=saitou:codex-qwen35-session"></textarea></label>
<label>Discuss rounds <input name="discuss_rounds" value="0"></label>
<label class="check"><input type="checkbox" name="no_keep_alive"> Stop when complete</label>
<input type="hidden" name="dangerously_bypass_present" value="1">
<label class="check"><input type="checkbox" name="dangerously_bypass" checked> Bypass sandbox/approvals</label>
</details>
<button type="submit">Start</button></form>{ui_runs_log}</aside><main>{detail}</main></div>
<div id="team-context-menu" class="context-menu">
<form method="post" action="/delete" onsubmit="return confirm('Delete this team? Running UI-launched team processes will be stopped first.');">
<input type="hidden" name="team" id="delete-team-id">
<button type="submit">Delete Team</button>
</form>
</div>
<script>
const teamMenu = document.getElementById('team-context-menu');
const deleteTeamInput = document.getElementById('delete-team-id');
document.querySelectorAll('.team-wrap').forEach((item) => {{
  item.addEventListener('contextmenu', (event) => {{
    event.preventDefault();
    deleteTeamInput.value = item.dataset.team || '';
    teamMenu.style.left = `${{event.clientX}}px`;
    teamMenu.style.top = `${{event.clientY}}px`;
    teamMenu.classList.add('open');
  }});
}});
document.addEventListener('click', () => teamMenu.classList.remove('open'));
document.addEventListener('keydown', (event) => {{
  if (event.key === 'Escape') {{
    teamMenu.classList.remove('open');
  }}
}});
const rtRoot = document.querySelector('[data-realtime-team]');
if (rtRoot) {{
  const teamId = rtRoot.dataset.realtimeTeam;
  const card = rtRoot;
  const grid = card.querySelector('.rt-grid');
  const status = card.querySelector('.rt-status');
  let snapshot = null;
  let timer = null;
  let panes = [{{ member: 'lead' }}];
  let layout = 'cols';
  function rtSetStatus(text) {{ status.textContent = text; }}
  function rtMembers() {{ return snapshot?.members || []; }}
  function rtMember(name) {{ return rtMembers().find((m) => m.name === name) || rtMembers()[0]; }}
  function rtRenderPane(pane, idx) {{
    const members = rtMembers();
    const selected = pane.member && members.some((m) => m.name === pane.member) ? pane.member : (members[0]?.name || 'lead');
    pane.member = selected;
    const options = members.map((m) => `<option value="${{rtEscAttr(m.name)}}" ${{m.name===selected?'selected':''}}>${{rtEsc(m.name)}} · session ${{rtEsc(m.status)}} · tasks ${{rtEsc(m.task_status)}} · unread ${{m.unread}}/${{m.direct_unread}} · ${{rtEsc(m.node)}}</option>`).join('');
    const m = rtMember(selected);
    const header = m ? `${{m.role}} / ${{m.location}} / tasks ${{m.task_status || '-'}} / unread ${{m.unread}}/${{m.direct_unread}} / cooldown ${{m.cooldown || '-'}} / thread ${{m.thread || '-'}}` : 'waiting for stream';
    const text = m ? rtTerminalText(m) : 'No member stream yet.';
    return `<div class="rt-pane" data-pane="${{idx}}">
      <div class="rt-panebar"><select data-idx="${{idx}}">${{options}}</select><span class="rt-meta">${{rtEsc(header)}}</span><button type="button" data-close="${{idx}}" title="Close pane">x</button></div>
      <pre class="rt-term">${{rtEsc(text)}}</pre>
    </div>`;
  }}
  function rtTerminalText(m) {{
    const parts = [];
    parts.push(`$ member=${{m.name}} role=${{m.role}} session=${{m.status}} tasks=${{m.task_status}} node=${{m.node}} unread=${{m.unread}} direct=${{m.direct_unread}} cooldown=${{m.cooldown || '-'}}`);
    if (m.live && m.live.trim()) parts.push(`\\n# live stream\\n${{m.live}}`);
    else parts.push('\\n# live stream\\n(no active live stream yet)');
    if (m.last && m.last.trim()) parts.push(`\\n# last completed assistant message\\n${{m.last}}`);
    if (m.inbox_tail && m.inbox_tail.trim()) parts.push(`\\n# inbox tail\\n${{m.inbox_tail}}`);
    return parts.join('\\n');
  }}
  function rtRender() {{
    if (!grid) return;
    if (!panes.length) panes = [{{ member: rtMembers()[0]?.name || 'lead' }}];
    grid.classList.toggle('cols', layout === 'cols');
    grid.classList.toggle('rows', layout === 'rows');
    grid.style.setProperty('--rt-cols', Math.max(1, panes.length));
    grid.innerHTML = panes.map(rtRenderPane).join('');
    grid.querySelectorAll('select[data-idx]').forEach((select) => {{
      select.addEventListener('change', () => {{
        panes[Number(select.dataset.idx)].member = select.value;
        rtRender();
      }});
    }});
    grid.querySelectorAll('button[data-close]').forEach((button) => {{
      button.addEventListener('click', () => {{
        if (panes.length > 1) panes.splice(Number(button.dataset.close), 1);
        rtRender();
      }});
    }});
    grid.querySelectorAll('.rt-term').forEach((term) => {{ term.scrollTop = term.scrollHeight; }});
    if (snapshot) rtSetStatus(`updated ${{snapshot.generated_at}} · ${{snapshot.members.length}} members · ${{snapshot.events.length}} recent events`);
  }}
  async function rtPoll() {{
    try {{
      const res = await fetch(`/realtime?team=${{encodeURIComponent(teamId)}}`, {{ cache: 'no-store' }});
      if (!res.ok) throw new Error(`${{res.status}} ${{res.statusText}}`);
      snapshot = await res.json();
      rtRender();
    }} catch (err) {{
      rtSetStatus(`realtime error: ${{err.message || err}}`);
    }}
  }}
  function rtStart() {{
    card.classList.add('open');
    if (!timer) {{
      rtPoll();
      timer = setInterval(rtPoll, 1500);
    }}
  }}
  function rtStop() {{
    card.classList.remove('open');
    if (timer) clearInterval(timer);
    timer = null;
  }}
  function rtEsc(value) {{
    return String(value ?? '').replace(/[&<>"']/g, (ch) => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[ch]));
  }}
  function rtEscAttr(value) {{ return rtEsc(value); }}
  card.querySelector('[data-rt-toggle]').addEventListener('click', () => card.classList.contains('open') ? rtStop() : rtStart());
  card.querySelector('[data-rt-add-h]').addEventListener('click', () => {{ layout='cols'; panes.push({{ member: rtMembers()[panes.length % Math.max(1, rtMembers().length)]?.name || 'lead' }}); rtStart(); rtRender(); }});
  card.querySelector('[data-rt-add-v]').addEventListener('click', () => {{ layout='rows'; panes.push({{ member: rtMembers()[panes.length % Math.max(1, rtMembers().length)]?.name || 'lead' }}); rtStart(); rtRender(); }});
  card.querySelector('[data-rt-refresh]').addEventListener('click', () => {{ rtStart(); rtPoll(); }});
}}
const dbgRoot = document.querySelector('[data-debug-team]');
if (dbgRoot) {{
  const teamId = dbgRoot.dataset.debugTeam;
  const list = dbgRoot.querySelector('.dbg-list');
  const status = dbgRoot.querySelector('.dbg-status');
  const search = dbgRoot.querySelector('[data-dbg-search]');
  const buttons = Array.from(dbgRoot.querySelectorAll('[data-dbg-kind]'));
  let snapshot = null;
  let kind = 'all';
  function dbgEsc(value) {{
    return String(value ?? '').replace(/[&<>"']/g, (ch) => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[ch]));
  }}
  function dbgMatches(item) {{
    if (kind !== 'all' && item.kind !== kind) return false;
    const q = (search?.value || '').trim().toLowerCase();
    if (!q) return true;
    return [item.timestamp, item.kind, item.title, item.actor, item.target, item.body, JSON.stringify(item.meta || {{}})]
      .join('\\n')
      .toLowerCase()
      .includes(q);
  }}
  function dbgRender() {{
    if (!snapshot) return;
    const items = (snapshot.items || []).filter(dbgMatches).slice(-250).reverse();
    list.innerHTML = items.map((item) => {{
      const meta = item.meta ? JSON.stringify(item.meta, null, 2) : '';
      return `<details class="dbg-item dbg-${{dbgEsc(item.kind)}}" open>
        <summary>${{dbgEsc(item.title || item.kind)}}</summary>
        <div class="dbg-meta"><span>${{dbgEsc(item.timestamp)}}</span><span class="pill">${{dbgEsc(item.kind)}}</span><span class="pill">${{dbgEsc(item.actor || '-')}} -> ${{dbgEsc(item.target || '-')}}</span></div>
        <div class="dbg-body">${{dbgEsc(item.body || '')}}</div>
        <details><summary>raw metadata</summary><div class="dbg-json">${{dbgEsc(meta)}}</div></details>
      </details>`;
    }}).join('') || '<p class="hint">No debug timeline entries match the current filter.</p>';
    status.textContent = `updated ${{snapshot.generated_at}} · showing ${{items.length}} / ${{snapshot.items.length}} entries · filter=${{kind}}`;
  }}
  async function dbgPoll() {{
    try {{
      const res = await fetch(`/debug?team=${{encodeURIComponent(teamId)}}`, {{ cache: 'no-store' }});
      if (!res.ok) throw new Error(`${{res.status}} ${{res.statusText}}`);
      snapshot = await res.json();
      dbgRender();
    }} catch (err) {{
      status.textContent = `debug timeline error: ${{err.message || err}}`;
    }}
  }}
  buttons.forEach((button) => {{
    button.addEventListener('click', () => {{
      kind = button.dataset.dbgKind || 'all';
      buttons.forEach((candidate) => candidate.classList.toggle('active', candidate === button));
      dbgRender();
    }});
  }});
  search?.addEventListener('input', dbgRender);
  dbgRoot.querySelector('[data-dbg-refresh]')?.addEventListener('click', dbgPoll);
  dbgPoll();
  setInterval(dbgPoll, 2500);
}}
</script></body></html>"#,
        team_links = team_links,
        refresh_href = selected_id
            .as_ref()
            .map(|team| format!(
                "/?team={}&cwd={}",
                url_encode(team),
                url_encode(&selected_cwd)
            ))
            .unwrap_or_else(|| format!("/?cwd={}", url_encode(&selected_cwd))),
        selected_cwd = html_escape(&selected_cwd),
        registered_app_server_url = html_escape(&registered_app_server_url),
        directory_picker = directory_picker,
        ui_runs_log = ui_runs_log,
        detail = detail,
    ))
}

fn parse_form(raw: &str) -> HashMap<String, String> {
    raw.split('&')
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            Some((url_decode(key).ok()?, url_decode(value).ok()?))
        })
        .collect()
}

fn split_ui_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn render_directory_picker(cwd: &str, selected_team: Option<&str>) -> Result<String> {
    let path = PathBuf::from(cwd);
    let canonical = path.canonicalize().unwrap_or(path);
    let mut entries = Vec::new();
    if let Some(parent) = canonical.parent() {
        entries.push(format!(
            r#"<a href="{href}">../</a>"#,
            href = directory_picker_href(parent, selected_team)
        ));
    }
    if let Ok(read_dir) = fs::read_dir(&canonical) {
        let mut dirs = read_dir
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().map(|ty| ty.is_dir()).unwrap_or(false))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        dirs.sort();
        for dir in dirs.into_iter().take(80) {
            let name = dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_string();
            entries.push(format!(
                r#"<a href="{href}">{name}/</a>"#,
                href = directory_picker_href(&dir, selected_team),
                name = html_escape(&name)
            ));
        }
    }
    Ok(format!(
        r#"<div class="dir-picker"><div class="dir-current">{}</div>{}</div>"#,
        html_escape(&canonical.display().to_string()),
        entries.join("")
    ))
}

fn directory_picker_href(path: &Path, selected_team: Option<&str>) -> String {
    let cwd = url_encode(&path.display().to_string());
    match selected_team {
        Some(team) => format!("/?team={}&cwd={cwd}", url_encode(team)),
        None => format!("/?cwd={cwd}"),
    }
}

fn format_node_location(node: &TeamNode) -> String {
    match node.kind {
        TeamNodeKind::Local => "local machine".to_string(),
        TeamNodeKind::Manual => node.url.clone().unwrap_or_else(|| "manual".to_string()),
        TeamNodeKind::Ssh => format!(
            "ssh:{} cwd={}",
            node.host.as_deref().unwrap_or(""),
            node.cwd.as_deref().unwrap_or("")
        ),
        TeamNodeKind::Docker => format!(
            "docker:{} cwd={}",
            node.container.as_deref().unwrap_or(""),
            node.cwd.as_deref().unwrap_or("")
        ),
        TeamNodeKind::SshDocker => format!(
            "ssh:{} docker:{} cwd={}",
            node.host.as_deref().unwrap_or(""),
            node.container.as_deref().unwrap_or(""),
            node.cwd.as_deref().unwrap_or("")
        ),
    }
}

fn render_message_board(team_dir: &Path, team_id: &str, selected_language: &str) -> Result<String> {
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    let mut messages = Vec::new();
    for event in events
        .into_iter()
        .filter(|event| event.event == "message_sent")
        .rev()
        .take(80)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let from = event
            .data
            .get("from")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let to = event
            .data
            .get("to")
            .map(|value| match value {
                serde_json::Value::Array(values) => values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                serde_json::Value::String(value) => value.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();
        let source = event
            .data
            .get("source")
            .and_then(|value| value.as_str())
            .unwrap_or("mailbox");
        let message = event
            .data
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        messages.push(format!(
            r#"<article class="msg"><div class="msg-meta"><span>{}</span><span class="pill">{} -> {}</span><span class="pill">{}</span></div><div>{}</div></article>"#,
            html_escape(&timestamp_for_ui(&event.timestamp)),
            html_escape(from),
            html_escape(&to),
            html_escape(source),
            html_escape(message),
        ));
    }
    if messages.is_empty() {
        messages.push("<p>No team messages yet.</p>".to_string());
    }
    let selected_language = normalize_translation_language(selected_language);
    let translation = render_translation_panel(team_dir, team_id, &selected_language)?;
    Ok(format!(
        r#"<form method="post" action="/translate" class="translate-form">
<input type="hidden" name="team" value="{team}">
<label>Translate to <select name="language">{options}</select></label>
<button type="submit">Translate</button>
</form>
{translation}
<div class="messages">{messages}</div>"#,
        team = html_escape(team_id),
        options = render_language_options(&selected_language),
        translation = translation,
        messages = messages.join(""),
    ))
}

fn render_lead_chat(team_dir: &Path, team_id: &str) -> Result<String> {
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    let mut chat_items = Vec::new();
    for event in events
        .into_iter()
        .filter(|event| event.event == "message_sent")
        .filter(|event| {
            let from = event
                .data
                .get("from")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let to_user_or_lead = event
                .data
                .get("to")
                .map(|value| match value {
                    serde_json::Value::Array(values) => values
                        .iter()
                        .any(|value| matches!(value.as_str(), Some("user") | Some("lead"))),
                    serde_json::Value::String(value) => value == "user" || value == "lead",
                    _ => false,
                })
                .unwrap_or(false);
            from == "user" || from == "lead" || to_user_or_lead
        })
        .rev()
        .take(30)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let from = event
            .data
            .get("from")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let to = event
            .data
            .get("to")
            .map(|value| match value {
                serde_json::Value::Array(values) => values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                serde_json::Value::String(value) => value.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();
        let message = event
            .data
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        chat_items.push(format!(
            r#"<article class="msg chat-{from_class}"><div class="msg-meta"><span>{time}</span><span class="pill">{from} -> {to}</span></div><div>{message}</div></article>"#,
            from_class = if from == "user" { "user" } else { "lead" },
            time = html_escape(&timestamp_for_ui(&event.timestamp)),
            from = html_escape(from),
            to = html_escape(&to),
            message = html_escape(message),
        ));
    }
    if chat_items.is_empty() {
        chat_items.push("<p>No lead chat yet.</p>".to_string());
    }
    let lead_live = fs::read_to_string(team_dir.join("live_messages").join("lead.md"))
        .ok()
        .filter(|text| !text.trim().is_empty())
        .map(|text| {
            let tail = text
                .lines()
                .rev()
                .take(80)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                r#"<details class="lead-live"><summary>Lead Thread</summary><pre>{}</pre></details>"#,
                html_escape(&tail)
            )
        })
        .unwrap_or_default();
    Ok(format!(
        r#"<form method="post" action="/message" class="lead-chat-form">
<input type="hidden" name="team" value="{team}">
<input type="hidden" name="to" value="lead">
<label>Message to lead <textarea name="message" rows="5" placeholder="追加指示、方針変更、確認したいことを書いてください"></textarea></label>
<button type="submit">Send to Lead</button>
</form>
<div class="messages lead-chat">{items}</div>{lead_live}"#,
        team = html_escape(team_id),
        items = chat_items.join(""),
        lead_live = lead_live,
    ))
}

fn render_translation_panel(team_dir: &Path, team_id: &str, language: &str) -> Result<String> {
    let output = translation_output_path(team_dir, language);
    let status = translation_status_path(team_dir, language);
    let label = translation_language_label(language).unwrap_or(language);
    if output.exists() {
        let translated = fs::read_to_string(&output)?;
        return Ok(format!(
            r#"<details open class="translation"><summary>Translated Team Messages: {}</summary><pre>{}</pre></details>"#,
            html_escape(label),
            html_escape(&translated),
        ));
    }
    if status.exists() {
        let status = fs::read_to_string(&status).unwrap_or_default();
        return Ok(format!(
            r#"<details open class="translation"><summary>Translation Status: {}</summary><pre>{}</pre><p><a href="/?team={}&translation={}">Refresh translation</a></p></details>"#,
            html_escape(label),
            html_escape(&status),
            url_encode(team_id),
            url_encode(language),
        ));
    }
    Ok(String::new())
}

fn render_language_options(selected: &str) -> String {
    translation_languages()
        .iter()
        .map(|(code, label)| {
            format!(
                r#"<option value="{}"{}>{}</option>"#,
                html_escape(code),
                if *code == selected { " selected" } else { "" },
                html_escape(label)
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn translation_languages() -> &'static [(&'static str, &'static str)] {
    &[
        ("ja", "Japanese"),
        ("en", "English"),
        ("ko", "Korean"),
        ("zh", "Chinese"),
        ("es", "Spanish"),
        ("fr", "French"),
        ("de", "German"),
    ]
}

fn normalize_translation_language(language: &str) -> String {
    let language = sanitize_id(language);
    if translation_language_label(&language).is_some() {
        language
    } else {
        "ja".to_string()
    }
}

fn translation_language_label(language: &str) -> Option<&'static str> {
    translation_languages()
        .iter()
        .find(|(code, _)| *code == language)
        .map(|(_, label)| *label)
}

fn translation_dir(team_dir: &Path) -> PathBuf {
    team_dir.join("translations")
}

fn translation_output_path(team_dir: &Path, language: &str) -> PathBuf {
    translation_dir(team_dir).join(format!(
        "messages-{}.md",
        normalize_translation_language(language)
    ))
}

fn translation_status_path(team_dir: &Path, language: &str) -> PathBuf {
    translation_dir(team_dir).join(format!(
        "messages-{}.status",
        normalize_translation_language(language)
    ))
}

fn start_translate_team_messages(team_dir: &Path, language: &str) -> Result<()> {
    let language = normalize_translation_language(language);
    let label = translation_language_label(&language).unwrap_or("Japanese");
    if team_messages_translation_source(team_dir, 120)?
        .trim()
        .is_empty()
    {
        bail!("no team messages to translate");
    }

    let dir = translation_dir(team_dir);
    fs::create_dir_all(&dir)?;
    let output_path = translation_output_path(team_dir, &language);
    let status_path = translation_status_path(team_dir, &language);
    let log_path = dir.join(format!("messages-{language}.log"));
    let _ = fs::remove_file(&output_path);
    write_text_atomic(
        &status_path,
        &format!(
            "queued translation to {label}\nqueued_at={}\nlog={}\n",
            now(),
            log_path.display()
        ),
    )?;
    append_event(
        team_dir,
        "ui_translation_queued",
        serde_json::json!({ "language": language, "label": label }),
    )?;

    let team_dir = team_dir.to_path_buf();
    let language = language.clone();
    std::thread::spawn(move || {
        if let Err(err) = translate_team_messages(&team_dir, &language) {
            let label = translation_language_label(&language).unwrap_or("Japanese");
            let status_path = translation_status_path(&team_dir, &language);
            let log_path = translation_dir(&team_dir).join(format!("messages-{language}.log"));
            let _ = write_text_atomic(
                &status_path,
                &format!(
                    "failed translation to {label}\nfailed_at={}\nerror={:#}\nlog={}\n",
                    now(),
                    err,
                    log_path.display()
                ),
            );
            let _ = append_event(
                &team_dir,
                "ui_translation_failed",
                serde_json::json!({
                    "language": language,
                    "label": label,
                    "error": err.to_string(),
                }),
            );
        }
    });

    Ok(())
}

fn translate_team_messages(team_dir: &Path, language: &str) -> Result<()> {
    let language = normalize_translation_language(language);
    let label = translation_language_label(&language).unwrap_or("Japanese");
    let source = team_messages_translation_source(team_dir, 120)?;
    if source.trim().is_empty() {
        bail!("no team messages to translate");
    }
    let dir = translation_dir(team_dir);
    fs::create_dir_all(&dir)?;
    let output_path = translation_output_path(team_dir, &language);
    let status_path = translation_status_path(team_dir, &language);
    let log_path = dir.join(format!("messages-{language}.log"));
    let config = load_config(team_dir)?;
    let codex_exe = std::env::current_exe().context("resolve current Codex executable")?;
    let prompt = format!(
        r#"Translate the following Codex team message log into {label}.

Purpose:
- The user reads the dashboard in their native language.
- Keep technical terms, commands, paths, IDs, thread IDs, file names, and code literals unchanged unless a short explanation is useful.
- Preserve the message order and speaker/recipient metadata.
- Make the translation natural and easy to skim.
- Do not add new facts or commentary.

Format:
- Markdown.
- Use one bullet per message.
- Start each bullet with timestamp and "from -> to".

Message log:
{source}
"#
    );
    let _ = fs::remove_file(&output_path);
    write_text_atomic(
        &status_path,
        &format!(
            "running translation to {label}\nstarted_at={}\nlog={}\n",
            now(),
            log_path.display()
        ),
    )?;
    append_event(
        team_dir,
        "ui_translation_started",
        serde_json::json!({ "language": language, "label": label }),
    )?;
    let status = run_codex_translation_exec(
        &codex_exe,
        team_dir,
        &config.id,
        &prompt,
        &log_path,
        &output_path,
    )?;
    if status.success() {
        write_text_atomic(
            &status_path,
            &format!(
                "completed translation to {label}\ncompleted_at={}\noutput={}\n",
                now(),
                output_path.display()
            ),
        )?;
        append_event(
            team_dir,
            "ui_translation_completed",
            serde_json::json!({ "language": language, "label": label, "output": output_path }),
        )?;
    } else {
        write_text_atomic(
            &status_path,
            &format!(
                "failed translation to {label}\nfailed_at={}\nstatus={:?}\nlog={}\n",
                now(),
                status.code(),
                log_path.display()
            ),
        )?;
        append_event(
            team_dir,
            "ui_translation_failed",
            serde_json::json!({ "language": language, "label": label, "status": status.code() }),
        )?;
        bail!("translation failed; see {}", log_path.display());
    }
    Ok(())
}

fn team_messages_translation_source(team_dir: &Path, limit: usize) -> Result<String> {
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    let mut lines = Vec::new();
    for event in events
        .into_iter()
        .filter(|event| event.event == "message_sent")
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let from = event
            .data
            .get("from")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let to = event
            .data
            .get("to")
            .map(|value| match value {
                serde_json::Value::Array(values) => values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                serde_json::Value::String(value) => value.clone(),
                _ => String::new(),
            })
            .unwrap_or_default();
        let source = event
            .data
            .get("source")
            .and_then(|value| value.as_str())
            .unwrap_or("mailbox");
        let message = event
            .data
            .get("message")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        lines.push(format!(
            "- [{}] {} -> {} ({source}): {}",
            event.timestamp, from, to, message
        ));
    }
    Ok(lines.join("\n"))
}

fn run_codex_translation_exec(
    codex_exe: &Path,
    cwd: &Path,
    team_id: &str,
    prompt: &str,
    log_path: &Path,
    output_path: &Path,
) -> Result<std::process::ExitStatus> {
    let stdout =
        fs::File::create(log_path).with_context(|| format!("create {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(codex_exe);
    command
        .arg("exec")
        .arg("--skip-git-repo-check")
        .arg("-C")
        .arg(cwd)
        .arg("-o")
        .arg(output_path)
        .env("CODEX_TEAM_ID", team_id)
        .env("CODEX_TEAM_MEMBER", "translator")
        .env("CODEX_TEAM_ROLE", "translator")
        .env("CODEX_TEAM_CLI", codex_exe)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .arg(prompt);
    command
        .spawn()
        .context("spawn Codex translation session")?
        .wait()
        .context("wait for Codex translation session")
}

fn render_realtime_view(team_id: &str, config: &TeamConfig) -> String {
    let first_member = config
        .members
        .iter()
        .find(|member| member.name == "lead")
        .or_else(|| config.members.first())
        .map(|member| member.name.as_str())
        .unwrap_or("lead");
    format!(
        r#"<section class="rt-card" data-realtime-team="{team}" data-default-member="{default_member}">
<div class="rt-head">
  <div class="rt-title"><span class="rt-dot"></span><span>Realtime Team View</span></div>
  <div class="rt-actions">
    <button type="button" data-rt-toggle>Realtime View</button>
    <button type="button" data-rt-add-h>+ Horizontal Split</button>
    <button type="button" data-rt-add-v>+ Vertical Split</button>
    <button type="button" data-rt-refresh>Refresh</button>
  </div>
</div>
<div class="rt-help">Open Realtime View, then use + Horizontal Split for side-by-side panes or + Vertical Split for stacked panes. Each pane has a department selector, so you can watch lead, local departments, SSH departments, and container departments at the same time.</div>
<div class="rt-grid cols"></div>
<div class="rt-status">closed</div>
</section>"#,
        team = html_escape(team_id),
        default_member = html_escape(first_member),
    )
}

fn render_debug_timeline_view(team_id: &str) -> String {
    format!(
        r#"<section class="dbg-card" data-debug-team="{team}">
<div class="dbg-head">
  <div class="dbg-title">Debug Timeline</div>
  <div class="dbg-actions">
    <input type="search" data-dbg-search placeholder="Search messages, events, prompts, paths">
    <button type="button" data-dbg-kind="all" class="active">All</button>
    <button type="button" data-dbg-kind="message">Messages</button>
    <button type="button" data-dbg-kind="system">System</button>
    <button type="button" data-dbg-kind="event">Events</button>
    <button type="button" data-dbg-kind="side">Side-channel</button>
    <button type="button" data-dbg-kind="live">Live</button>
    <button type="button" data-dbg-kind="last">Last</button>
    <button type="button" data-dbg-refresh>Refresh</button>
  </div>
</div>
<div class="hint" style="padding:8px 12px;margin:0">Shows mailbox traffic, system wakeups, runtime events, side-channel replies/context injection, and live/last thread buffers in one timeline.</div>
<div class="dbg-list"><p class="hint">Loading debug timeline...</p></div>
<div class="dbg-status">loading</div>
</section>"#,
        team = html_escape(team_id),
    )
}

fn render_team_realtime_json(team_dir: &Path) -> Result<String> {
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir).unwrap_or_default();
    let mut nodes = load_nodes(team_dir).unwrap_or_default();
    ensure_local_node(&mut nodes);
    let node_by_id = nodes
        .iter()
        .map(|node| (node.id.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    let members = config
        .members
        .iter()
        .map(|member| {
            let mail = mailbox_unread_counts(team_dir, &member.name).unwrap_or_default();
            let cooldown = recent_usage_limit_retry_remaining(team_dir, &member.name)
                .ok()
                .flatten()
                .map(|remaining| format_compact_duration(remaining.as_secs()))
                .unwrap_or_default();
            let node = infer_member_node_for_ui(
                Some(team_dir),
                member,
                member.node.as_deref().unwrap_or("local"),
            );
            let location = node_by_id
                .get(node.as_str())
                .map(format_node_location)
                .unwrap_or_else(|| node.clone());
            let live = fs::read_to_string(
                team_dir
                    .join("live_messages")
                    .join(format!("{}.md", sanitize_id(&member.name))),
            )
            .unwrap_or_default();
            let last = fs::read_to_string(
                team_dir
                    .join("last_messages")
                    .join(format!("{}.md", sanitize_id(&member.name))),
            )
            .unwrap_or_default();
            let inbox_tail = fs::read_to_string(mailbox_path(team_dir, &member.name))
                .unwrap_or_default()
                .lines()
                .rev()
                .take(8)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n");
            UiRealtimeMember {
                name: member.name.clone(),
                role: member.role.clone(),
                status: format!("{:?}", member.status),
                task_status: member_task_status_summary(&tasks, &member.name),
                node,
                location,
                unread: mail.unread,
                direct_unread: mail.direct_unread,
                cooldown,
                thread: member.thread_id.clone().unwrap_or_default(),
                live: tail_chars(&live, 20_000),
                last: tail_chars(&last, 10_000),
                inbox_tail,
            }
        })
        .collect::<Vec<_>>();
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?
        .into_iter()
        .rev()
        .take(80)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(event_record_for_ui)
        .collect::<Vec<_>>();
    let mut messages = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?
        .into_iter()
        .filter(|event| event.event == "message_sent" || event.event == "team_message_ingested")
        .rev()
        .take(60)
        .filter_map(|event| {
            let from = event.data.get("from")?.as_str()?.to_string();
            let to = match event.data.get("to")? {
                serde_json::Value::Array(values) => values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                serde_json::Value::String(value) => value.clone(),
                other => other.to_string(),
            };
            let message = event.data.get("message")?.as_str()?.to_string();
            Some(UiRealtimeMessage {
                timestamp: event.timestamp,
                from,
                to,
                message,
            })
        })
        .collect::<Vec<_>>();
    messages.reverse();
    let snapshot = UiRealtimeSnapshot {
        team: config.id,
        generated_at: now(),
        members,
        events,
        messages,
    };
    serde_json::to_string(&snapshot).context("serialize realtime snapshot")
}

fn render_team_debug_json(team_dir: &Path) -> Result<String> {
    let config = load_config(team_dir)?;
    let timeline = UiDebugTimeline {
        team: config.id,
        generated_at: now(),
        items: collect_ui_debug_timeline(team_dir, 600)?,
    };
    serde_json::to_string(&timeline).context("serialize debug timeline")
}

fn collect_ui_debug_timeline(team_dir: &Path, limit: usize) -> Result<Vec<UiDebugTimelineItem>> {
    let mut items = Vec::new();

    for event in read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))? {
        let kind = if event.event == "message_sent" || event.event == "team_message_ingested" {
            event
                .data
                .get("from")
                .and_then(|value| value.as_str())
                .filter(|from| *from == "system")
                .map(|_| "system")
                .unwrap_or("message")
        } else if event.event.contains("side_channel") {
            "side"
        } else {
            "event"
        };
        let actor = event
            .data
            .get("from")
            .and_then(|value| value.as_str())
            .or_else(|| event.data.get("member").and_then(|value| value.as_str()))
            .unwrap_or("")
            .to_string();
        let target = event
            .data
            .get("to")
            .map(format_json_target)
            .unwrap_or_default();
        let body = event
            .data
            .get("message")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                serde_json::to_string_pretty(&event.data).unwrap_or_else(|_| event.data.to_string())
            });
        let title = if kind == "message" || kind == "system" {
            format!(
                "{} -> {}",
                if actor.is_empty() {
                    "unknown"
                } else {
                    actor.as_str()
                },
                if target.is_empty() {
                    "unknown"
                } else {
                    target.as_str()
                }
            )
        } else {
            event.event.clone()
        };
        items.push(UiDebugTimelineItem {
            timestamp: timestamp_for_ui(&event.timestamp),
            kind: kind.to_string(),
            title,
            actor,
            target,
            body,
            meta: event.data,
        });
    }

    collect_mailbox_debug_items(team_dir, &mut items)?;
    collect_side_channel_debug_items(team_dir, &mut items)?;
    collect_thread_buffer_debug_items(team_dir, "live_messages", "live", &mut items)?;
    collect_thread_buffer_debug_items(team_dir, "last_messages", "last", &mut items)?;

    items.sort_by(|a, b| {
        timestamp_sort_key(&a.timestamp)
            .cmp(&timestamp_sort_key(&b.timestamp))
            .then_with(|| a.kind.cmp(&b.kind))
            .then_with(|| a.title.cmp(&b.title))
    });
    if items.len() > limit {
        items.drain(0..items.len() - limit);
    }
    Ok(items)
}

fn collect_mailbox_debug_items(
    team_dir: &Path,
    items: &mut Vec<UiDebugTimelineItem>,
) -> Result<()> {
    let mailbox_dir = team_dir.join("mailboxes");
    let Ok(entries) = fs::read_dir(&mailbox_dir) else {
        return Ok(());
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let mailbox = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        for msg in read_jsonl::<MailMessage>(&path)? {
            let kind = if msg.from == "system" {
                "system"
            } else {
                "message"
            };
            items.push(UiDebugTimelineItem {
                timestamp: timestamp_for_ui(&msg.timestamp),
                kind: kind.to_string(),
                title: format!("mailbox {} -> {}", msg.from, msg.to),
                actor: msg.from.clone(),
                target: msg.to.clone(),
                body: msg.message.clone(),
                meta: serde_json::json!({
                    "mailbox": mailbox,
                    "read": msg.read,
                    "source": "mailbox_file",
                }),
            });
        }
    }
    Ok(())
}

fn collect_side_channel_debug_items(
    team_dir: &Path,
    items: &mut Vec<UiDebugTimelineItem>,
) -> Result<()> {
    let dir = team_dir.join("side_channel_contexts");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(());
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        for record in read_jsonl::<SideChannelContextRecord>(&path)? {
            items.push(UiDebugTimelineItem {
                timestamp: timestamp_for_ui(&record.created_at),
                kind: "side".to_string(),
                title: format!("side-channel {:?} @{}", record.status, record.member),
                actor: record.member.clone(),
                target: record.recipients.join(","),
                body: format!(
                    "Incoming handled:\n{}\n\nReply sent:\n{}",
                    record.incoming_summary, record.reply
                ),
                meta: serde_json::json!({
                    "id": record.id,
                    "node": record.node,
                    "source_thread": record.source_thread,
                    "side_thread": record.side_thread,
                    "side_turn": record.side_turn,
                    "recipients": record.recipients,
                    "status": record.status,
                    "injected_turns": record.injected_turns,
                    "injected_at": record.injected_at,
                    "acknowledged_at": record.acknowledged_at,
                    "source": "side_channel_context_file",
                }),
            });
        }
    }
    Ok(())
}

fn collect_thread_buffer_debug_items(
    team_dir: &Path,
    dirname: &str,
    kind: &str,
    items: &mut Vec<UiDebugTimelineItem>,
) -> Result<()> {
    let dir = team_dir.join(dirname);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Ok(());
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let body = fs::read_to_string(&path).unwrap_or_default();
        if body.trim().is_empty() {
            continue;
        }
        let member = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_string();
        items.push(UiDebugTimelineItem {
            timestamp: file_modified_timestamp(&path).unwrap_or_else(now),
            kind: kind.to_string(),
            title: format!("{kind} thread buffer @{member}"),
            actor: member.clone(),
            target: String::new(),
            body: tail_chars(&body, 20_000),
            meta: serde_json::json!({
                "path": path.display().to_string(),
                "source": dirname,
            }),
        });
    }
    Ok(())
}

fn format_json_target(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join(","),
        serde_json::Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn timestamp_sort_key(value: &str) -> i64 {
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.timestamp_millis())
        .unwrap_or(0)
}

fn render_events_for_ui(path: &Path) -> Result<String> {
    Ok(read_jsonl::<TeamEventRecord>(path)?
        .into_iter()
        .map(event_record_for_ui)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn event_record_for_ui(event: TeamEventRecord) -> String {
    serde_json::json!({
        "event": event.event,
        "timestamp": timestamp_for_ui(&event.timestamp),
        "data": event.data,
    })
    .to_string()
}

fn file_modified_timestamp(path: &Path) -> Option<String> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    let modified: DateTime<Utc> = modified.into();
    Some(
        modified
            .with_timezone(&tokyo_offset())
            .to_rfc3339_opts(SecondsFormat::Secs, true),
    )
}

fn render_thread_board(
    team_dir: &Path,
    config: &TeamConfig,
    node_by_id: &HashMap<String, TeamNode>,
) -> Result<String> {
    let tasks = load_tasks(team_dir).unwrap_or_default();
    let mut items = Vec::new();
    for member in &config.members {
        let task_status = member_task_status_summary(&tasks, &member.name);
        let node_id = infer_member_node_for_ui(
            Some(team_dir),
            member,
            member.node.as_deref().unwrap_or("local"),
        );
        let location = node_by_id
            .get(node_id.as_str())
            .map(format_node_location)
            .unwrap_or_else(|| node_id.clone());
        let live = fs::read_to_string(
            team_dir
                .join("live_messages")
                .join(format!("{}.md", sanitize_id(&member.name))),
        )
        .unwrap_or_default();
        let last = fs::read_to_string(
            team_dir
                .join("last_messages")
                .join(format!("{}.md", sanitize_id(&member.name))),
        )
        .unwrap_or_default();
        let live = tail_chars(&live, 8000);
        let last = tail_chars(&last, 8000);
        items.push(format!(
            r#"<details><summary>{name} ({role}) - session {status:?} - tasks {tasks} - {location}</summary>
<p><strong>Thread:</strong> <code>{thread}</code></p>
<h4>Live Stream</h4><pre>{live}</pre>
<h4>Last Message</h4><pre>{last}</pre>
</details>"#,
            name = html_escape(&member.name),
            role = html_escape(&member.role),
            status = member.status,
            tasks = html_escape(&task_status),
            location = html_escape(&location),
            thread = html_escape(member.thread_id.as_deref().unwrap_or("")),
            live = html_escape(&live),
            last = html_escape(&last),
        ));
    }
    Ok(format!(r#"<div class="threads">{}</div>"#, items.join("")))
}

fn infer_member_node_for_ui(
    team_dir: Option<&Path>,
    member: &TeamMember,
    default_node: &str,
) -> String {
    if default_node != "local" {
        return default_node.to_string();
    }
    let Some(team_dir) = team_dir else {
        return default_node.to_string();
    };
    let Ok(events) = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")) else {
        return default_node.to_string();
    };
    for event in events.into_iter().rev() {
        if !matches!(
            event.event.as_str(),
            "app_server_member_started"
                | "app_server_member_reactive_started"
                | "app_server_member_completed"
                | "app_server_turn_steered"
        ) {
            continue;
        }
        let event_member = event
            .data
            .get("member")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if event_member != member.name {
            continue;
        }
        if let Some(node) = event.data.get("node").and_then(|value| value.as_str())
            && !node.trim().is_empty()
        {
            return node.to_string();
        }
    }
    default_node.to_string()
}

fn tail_chars(value: &str, max_chars: usize) -> String {
    let count = value.chars().count();
    if count <= max_chars {
        return value.to_string();
    }
    let tail = value
        .chars()
        .skip(count.saturating_sub(max_chars))
        .collect::<String>();
    format!("... trimmed ...\n{tail}")
}

fn form_value(form: &HashMap<String, String>, key: &str) -> Result<String> {
    form.get(key)
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .ok_or_else(|| anyhow!("missing form field `{key}`"))
}

fn url_decode(raw: &str) -> Result<String> {
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'+' => out.push(b' '),
            b'%' if idx + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[idx + 1..idx + 3])?;
                out.push(u8::from_str_radix(hex, 16)?);
                idx += 2;
            }
            byte => out.push(byte),
        }
        idx += 1;
    }
    Ok(String::from_utf8(out)?)
}

fn url_encode(raw: &str) -> String {
    raw.bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            b' ' => vec!['+'],
            _ => format!("%{byte:02X}").chars().collect(),
        })
        .collect()
}

fn html_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn default_ui_cwd(args: &UiArgs) -> String {
    args.default_cwd
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(default_home)
}

fn default_home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "~".to_string())
}

fn expand_home(path: String) -> String {
    if path == "~" {
        return default_home();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return format!("{}/{}", default_home(), rest);
    }
    path
}

#[derive(Clone, Copy)]
enum UiTeamRunStatus {
    Running,
    Stopped,
    Unknown,
}

impl UiTeamRunStatus {
    fn label(self) -> &'static str {
        match self {
            UiTeamRunStatus::Running => "running",
            UiTeamRunStatus::Stopped => "runtime stopped",
            UiTeamRunStatus::Unknown => "unknown",
        }
    }

    fn css_class(self) -> &'static str {
        match self {
            UiTeamRunStatus::Running => "run-running",
            UiTeamRunStatus::Stopped => "run-stopped",
            UiTeamRunStatus::Unknown => "run-unknown",
        }
    }
}

fn team_run_pid_path(team_dir: &Path) -> PathBuf {
    team_dir.join("run.pid")
}

fn write_team_run_pid(team_dir: &Path, pid: u32) -> Result<()> {
    fs::write(team_run_pid_path(team_dir), format!("{pid}\n"))
        .with_context(|| format!("write {}", team_run_pid_path(team_dir).display()))
}

fn team_secretary_bindings_dir(root: &Path) -> PathBuf {
    root.parent()
        .unwrap_or(root)
        .join("team-secretaries")
        .to_path_buf()
}

fn bind_parent_codex_session_to_team(
    root: &Path,
    team_id: &str,
    team_dir: &Path,
    cwd: &Path,
) -> Result<()> {
    let Ok(session_id) = std::env::var("CODEX_THREAD_ID") else {
        return Ok(());
    };
    let session_id = sanitize_id(&session_id);
    if session_id.is_empty() {
        return Ok(());
    }
    let dir = team_secretary_bindings_dir(root);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = dir.join(format!("{session_id}.json"));
    let timestamp = now();
    let created_at = fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<TeamSecretaryBinding>(&raw).ok())
        .map(|binding| binding.created_at)
        .unwrap_or_else(|| timestamp.clone());
    let binding = TeamSecretaryBinding {
        session_id,
        team_id: team_id.to_string(),
        team_dir: team_dir.display().to_string(),
        cwd: cwd.display().to_string(),
        role: "lead_secretary".to_string(),
        created_at,
        updated_at: timestamp,
    };
    write_json_atomic(&path, &binding).with_context(|| format!("write {}", path.display()))?;
    append_event(
        team_dir,
        "lead_secretary_bound",
        serde_json::json!({
            "session_id": binding.session_id,
            "role": binding.role,
            "cwd": binding.cwd,
        }),
    )?;
    Ok(())
}

fn ui_team_pids_dir(root: &Path) -> PathBuf {
    root.join("ui-run-pids")
}

fn ui_team_pid_path(root: &Path, team: &str) -> PathBuf {
    ui_team_pids_dir(root).join(format!("{}.pid", sanitize_id(team)))
}

fn write_ui_team_pid(root: &Path, team: &str, pid: u32) -> Result<()> {
    let dir = ui_team_pids_dir(root);
    fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let path = ui_team_pid_path(root, team);
    fs::write(&path, format!("{pid}\n")).with_context(|| format!("write {}", path.display()))
}

fn remove_ui_team_pid(root: &Path, team: &str) -> Result<()> {
    let path = ui_team_pid_path(root, team);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

fn read_pid_file(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()
}

fn read_ui_team_pid(root: &Path, team: &str) -> Option<u32> {
    read_pid_file(&ui_team_pid_path(root, team))
}

fn read_team_run_pid(team_dir: &Path) -> Option<u32> {
    read_pid_file(&team_run_pid_path(team_dir))
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn process_cmdline(pid: u32) -> Option<String> {
    let path = PathBuf::from(format!("/proc/{pid}/cmdline"));
    let raw = fs::read(&path).ok()?;
    Some(String::from_utf8_lossy(&raw).replace('\0', " "))
}

fn process_looks_like_codex_team(pid: u32) -> bool {
    process_cmdline(pid)
        .map(|cmdline| cmdline.contains("codex") && cmdline.contains("team"))
        .unwrap_or(true)
}

fn process_looks_like_codex_app_server(pid: u32) -> bool {
    process_cmdline(pid)
        .map(|cmdline| cmdline.contains("codex") && cmdline.contains("app-server"))
        .unwrap_or(false)
}

fn collect_descendant_pids(root_pid: u32) -> Vec<u32> {
    let Ok(output) = Command::new("ps")
        .args(["-eo", "pid=,ppid="])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let mut children = HashMap::<u32, Vec<u32>>::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut parts = line.split_whitespace();
        let Some(pid) = parts.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        let Some(ppid) = parts.next().and_then(|value| value.parse::<u32>().ok()) else {
            continue;
        };
        children.entry(ppid).or_default().push(pid);
    }
    let mut out = Vec::new();
    let mut stack = children.remove(&root_pid).unwrap_or_default();
    while let Some(pid) = stack.pop() {
        out.push(pid);
        if let Some(mut nested) = children.remove(&pid) {
            stack.append(&mut nested);
        }
    }
    out
}

fn terminate_pid(pid: u32) {
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status();
}

fn kill_pid(pid: u32) {
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
}

fn stop_process_tree(pid: u32, root_check: fn(u32) -> bool) {
    if !process_alive(pid) || !root_check(pid) {
        return;
    }
    let mut pids = collect_descendant_pids(pid);
    pids.push(pid);
    pids.sort_unstable();
    pids.dedup();
    for child in pids.iter().copied().filter(|child| *child != pid).rev() {
        if process_alive(child) {
            terminate_pid(child);
        }
    }
    terminate_pid(pid);
    for _ in 0..20 {
        if pids.iter().all(|candidate| !process_alive(*candidate)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    for candidate in pids {
        if process_alive(candidate) {
            kill_pid(candidate);
        }
    }
}

fn stop_process(pid: u32) {
    if !process_alive(pid) || !process_looks_like_codex_team(pid) {
        return;
    }
    stop_process_tree(pid, process_looks_like_codex_team);
}

fn stop_ui_team_process(root: &Path, team: &str) -> Result<()> {
    let team_dir = resolve_team_dir(root, Some(team))?;
    let mut pids = Vec::new();
    if let Some(pid) = read_ui_team_pid(root, team) {
        pids.push(pid);
    }
    if let Some(pid) = read_team_run_pid(&team_dir)
        && !pids.contains(&pid)
    {
        pids.push(pid);
    }
    for pid in pids {
        stop_process(pid);
    }
    Ok(())
}

fn stop_team_runtime(root: &Path, args: StopArgs) -> Result<()> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    let mut stopped_pids = Vec::<u32>::new();
    for pid in [
        read_ui_team_pid(root, &config.id),
        read_team_run_pid(&team_dir),
    ]
    .into_iter()
    .flatten()
    {
        if process_alive(pid) {
            stop_process_tree(pid, process_looks_like_codex_team);
            stopped_pids.push(pid);
        }
    }
    if !args.keep_local_app_server {
        if let Some(pid) = stop_registered_app_server_for_team(&team_dir)? {
            stopped_pids.push(pid);
        }
    }
    let mut stopped_nodes = Vec::<String>::new();
    if !args.no_remote_nodes {
        stopped_nodes = stop_remote_node_app_servers(&team_dir)?;
    }
    stopped_pids.extend(stop_local_team_id_processes(&config.id)?);
    let _ = fs::remove_file(team_run_pid_path(&team_dir));
    let _ = remove_ui_team_pid(root, &config.id);
    set_running_members_to_standby_for_pause(&team_dir)?;
    append_event(
        &team_dir,
        "team_runtime_paused",
        serde_json::json!({
            "pids": stopped_pids,
            "remote_nodes": stopped_nodes,
            "keep_local_app_server": args.keep_local_app_server,
            "no_remote_nodes": args.no_remote_nodes,
        }),
    )?;
    println!("Paused team `{}`", config.id);
    println!("State preserved: {}", team_dir.display());
    if stopped_nodes.is_empty() {
        println!("Stopped local runtime/app-server processes.");
    } else {
        println!(
            "Stopped local runtime/app-server processes and node app-servers: {}",
            stopped_nodes.join(", ")
        );
    }
    println!(
        "Resume with: codex team resume --team {} --dangerously-bypass-approvals-and-sandbox",
        config.id
    );
    Ok(())
}

fn stop_local_team_id_processes(team_id: &str) -> Result<Vec<u32>> {
    let output = Command::new("ps")
        .args(["-eww", "-o", "pid=,cmd="])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .context("list local processes")?;
    let current_pid = std::process::id();
    let mut pids = Vec::new();
    let team_container_prefix = format!("codex-team-{team_id}");
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let trimmed = line.trim_start();
        let Some(pid_raw) = trimmed.split_whitespace().next() else {
            continue;
        };
        let Ok(pid) = pid_raw.trim().parse::<u32>() else {
            continue;
        };
        if pid == current_pid || !process_alive(pid) {
            continue;
        }
        let cmdline = trimmed[pid_raw.len()..].trim_start();
        let belongs_to_team = cmdline.contains(&format!("CODEX_TEAM_ID='{}'", team_id))
            || cmdline.contains(&format!("CODEX_TEAM_ID={team_id}"))
            || cmdline.contains(&team_container_prefix)
            || cmdline.contains(&format!("team runtime --team {team_id}"));
        if !belongs_to_team {
            continue;
        }
        let looks_managed = cmdline.contains("ssh ")
            || cmdline.contains("docker exec")
            || cmdline.contains("codex app-server")
            || cmdline.contains("codex team runtime")
            || cmdline.contains("/codex team runtime")
            || cmdline.contains("codex-team");
        if !looks_managed {
            continue;
        }
        terminate_pid(pid);
        pids.push(pid);
    }
    for _ in 0..20 {
        if pids.iter().all(|pid| !process_alive(*pid)) {
            return Ok(pids);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    for pid in &pids {
        if process_alive(*pid) {
            kill_pid(*pid);
        }
    }
    Ok(pids)
}

fn stop_registered_app_server_for_team(team_dir: &Path) -> Result<Option<u32>> {
    let Some(registry) = read_app_server_registry()? else {
        return Ok(None);
    };
    let nodes = load_nodes(team_dir)?;
    let matches_team = nodes
        .iter()
        .any(|node| node.id == "local" && node.url.as_deref() == Some(registry.url.as_str()));
    if !matches_team {
        return Ok(None);
    }
    if process_alive(registry.pid) && process_looks_like_codex_app_server(registry.pid) {
        stop_process_tree(registry.pid, process_looks_like_codex_app_server);
    }
    clear_app_server_registry_if_matches(&registry.url)?;
    set_node_connection(team_dir, "local", TeamNodeStatus::Offline, None)?;
    Ok(Some(registry.pid))
}

fn stop_remote_node_app_servers(team_dir: &Path) -> Result<Vec<String>> {
    let config = load_config(team_dir)?;
    let nodes = load_nodes(team_dir)?;
    let mut stopped = Vec::new();
    for node in nodes {
        if matches!(node.kind, TeamNodeKind::Local | TeamNodeKind::Manual) {
            continue;
        }
        let Some(url) = node.url.as_deref() else {
            continue;
        };
        let Some((_, port)) = parse_ws_host_port(url) else {
            continue;
        };
        let stopped_node = match node.kind {
            TeamNodeKind::Ssh => {
                let Some(host) = node.host.as_deref() else {
                    continue;
                };
                let pattern = format!("[c]odex app-server --listen ws://127.0.0.1:{port}");
                Command::new("ssh")
                    .arg("-o")
                    .arg("BatchMode=yes")
                    .arg(host)
                    .arg(format!("pkill -f {}", shell_quote(&pattern)))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|status| status.success())
                    .unwrap_or(false)
            }
            TeamNodeKind::Docker => {
                let Some(container) = node.container.as_deref() else {
                    continue;
                };
                let pattern = format!("[c]odex app-server --listen ws://0.0.0.0:{port}");
                Command::new("docker")
                    .arg("exec")
                    .arg(container)
                    .arg("pkill")
                    .arg("-f")
                    .arg(pattern)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|status| status.success())
                    .unwrap_or(false)
            }
            TeamNodeKind::SshDocker => {
                let Some(host) = node.host.as_deref() else {
                    continue;
                };
                let Some(container) = node.container.as_deref() else {
                    continue;
                };
                let pattern = format!("[c]odex app-server --listen ws://0.0.0.0:{port}");
                let command = format!(
                    "docker exec {} pkill -f {}",
                    shell_quote(container),
                    shell_quote(&pattern)
                );
                Command::new("ssh")
                    .arg("-o")
                    .arg("BatchMode=yes")
                    .arg(host)
                    .arg(command)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|status| status.success())
                    .unwrap_or(false)
            }
            TeamNodeKind::Local | TeamNodeKind::Manual => false,
        };
        let cleaned_team_processes = cleanup_remote_node_team_processes(&node, &config.id);
        if stopped_node {
            set_node_connection(team_dir, &node.id, TeamNodeStatus::Offline, None)?;
            stopped.push(node.id);
        } else if cleaned_team_processes {
            set_node_connection(team_dir, &node.id, TeamNodeStatus::Offline, None)?;
            stopped.push(node.id);
        }
    }
    Ok(stopped)
}

fn cleanup_remote_node_team_processes(node: &TeamNode, team_id: &str) -> bool {
    match node.kind {
        TeamNodeKind::Ssh => {
            let Some(host) = node.host.as_deref() else {
                return false;
            };
            ssh_shell_success(host, &team_env_cleanup_shell(team_id))
        }
        TeamNodeKind::Docker => {
            let Some(container) = node.container.as_deref() else {
                return false;
            };
            docker_shell_success(
                container,
                &container_team_cleanup_shell(team_id, container, false),
            )
        }
        TeamNodeKind::SshDocker => {
            let Some(host) = node.host.as_deref() else {
                return false;
            };
            let Some(container) = node.container.as_deref() else {
                return false;
            };
            let container_cleanup = ssh_docker_shell_success(
                host,
                container,
                &container_team_cleanup_shell(team_id, container, true),
            );
            let host_cleanup = ssh_shell_success(host, &team_env_cleanup_shell(team_id));
            container_cleanup || host_cleanup
        }
        TeamNodeKind::Local | TeamNodeKind::Manual => false,
    }
}

fn team_env_cleanup_shell(team_id: &str) -> String {
    let quoted_pattern = format!("[C]ODEX_TEAM_ID='{}'", team_id);
    let plain_pattern = format!("[C]ODEX_TEAM_ID={team_id}");
    format!(
        "pkill -TERM -f {} || true; pkill -TERM -f {} || true; sleep 1; pkill -KILL -f {} || true; pkill -KILL -f {} || true",
        shell_quote(&quoted_pattern),
        shell_quote(&plain_pattern),
        shell_quote(&quoted_pattern),
        shell_quote(&plain_pattern),
    )
}

fn container_team_cleanup_shell(
    team_id: &str,
    container: &str,
    include_team_app_server: bool,
) -> String {
    let mut script = team_env_cleanup_shell(team_id);
    let team_container_prefix = format!("codex-team-{team_id}");
    if include_team_app_server || container.starts_with(&team_container_prefix) {
        script.push_str("; pkill -TERM -f '[c]odex app-server' || true; sleep 1; pkill -KILL -f '[c]odex app-server' || true");
    }
    script
}

fn ssh_shell_success(host: &str, command: &str) -> bool {
    Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg(host)
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn docker_shell_success(container: &str, command: &str) -> bool {
    Command::new("docker")
        .arg("exec")
        .arg(container)
        .arg("bash")
        .arg("-lc")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ssh_docker_shell_success(host: &str, container: &str, command: &str) -> bool {
    let command = format!(
        "docker exec {} bash -lc {}",
        shell_quote(container),
        shell_quote(command)
    );
    ssh_shell_success(host, &command)
}

fn set_running_members_to_standby_for_pause(team_dir: &Path) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let mut changed = false;
    for member in &mut config.members {
        if matches!(member.status, MemberStatus::Running | MemberStatus::Online) {
            member.status = MemberStatus::Standby;
            changed = true;
        }
    }
    if changed {
        config.updated_at = now();
        write_json_atomic(&team_dir.join("config.json"), &config)?;
    }
    Ok(())
}

fn ui_team_run_status(root: &Path, team: &TeamConfig) -> UiTeamRunStatus {
    let team_dir = root.join(&team.id);
    let mut saw_pid = false;
    for pid in [
        read_ui_team_pid(root, &team.id),
        read_team_run_pid(&team_dir),
    ]
    .into_iter()
    .flatten()
    {
        saw_pid = true;
        if process_alive(pid) && process_looks_like_codex_team(pid) {
            return UiTeamRunStatus::Running;
        }
    }
    if saw_pid {
        return UiTeamRunStatus::Stopped;
    }
    let Ok(events) = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")) else {
        return UiTeamRunStatus::Unknown;
    };
    for event in events.into_iter().rev().take(20) {
        match event.event.as_str() {
            "team_runtime_paused" => return UiTeamRunStatus::Stopped,
            "app_server_keep_alive_stopped" => return UiTeamRunStatus::Stopped,
            "app_server_keep_alive_idle" => return UiTeamRunStatus::Unknown,
            _ => {}
        }
    }
    UiTeamRunStatus::Unknown
}

fn cleanup_team(root: &Path, args: CleanupArgs) -> Result<()> {
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    if !args.force {
        bail!("refusing to delete `{}` without --force", config.id);
    }
    remove_member_worktrees(&config);
    fs::remove_dir_all(&team_dir)
        .with_context(|| format!("failed to remove {}", team_dir.display()))?;
    println!("Deleted team `{}`", config.id);
    Ok(())
}

fn remove_member_worktrees(config: &TeamConfig) {
    for member in &config.members {
        let Some(path) = member.workspace_path.as_deref() else {
            continue;
        };
        let path = Path::new(path);
        if !path.exists() {
            continue;
        }
        let _ = Command::new("git")
            .arg("-C")
            .arg(path)
            .arg("worktree")
            .arg("remove")
            .arg("--force")
            .arg(path)
            .status();
    }
}

fn parse_member(raw: &str, now: &str) -> Result<TeamMember> {
    let (raw, node) = match raw.rsplit_once('@') {
        Some((left, node)) if !node.trim().is_empty() => (left, Some(sanitize_id(node))),
        _ => (raw, None),
    };
    let (name, role) = match raw.split_once(':') {
        Some((name, role)) => (name, role),
        None => (raw, "worker"),
    };
    let name = sanitize_id(name);
    if name.is_empty() || name == "lead" {
        bail!("invalid member name `{raw}`");
    }
    Ok(TeamMember {
        name,
        role: sanitize_role(role),
        status: MemberStatus::Online,
        joined_at: now.to_string(),
        thread_id: None,
        workspace_path: None,
        node,
    })
}

fn parse_node_spec(raw: &str, now: &str) -> Result<TeamNode> {
    let (left, value) = raw.split_once('=').with_context(|| {
        format!("invalid node spec `{raw}`; expected ID=ws://... or ID@ssh=HOST")
    })?;
    let (id, kind) = match left.split_once('@') {
        Some((id, "ssh")) => (id, TeamNodeKind::Ssh),
        Some((id, "docker")) => (id, TeamNodeKind::Docker),
        Some((id, "ssh-docker" | "ssh_docker")) => (id, TeamNodeKind::SshDocker),
        Some((_, kind)) => bail!("unsupported node kind `{kind}` in `{raw}`"),
        None => (left, TeamNodeKind::Manual),
    };
    let id = sanitize_id(id);
    if id.is_empty() || id == "local" {
        bail!("invalid node id in `{raw}`");
    }
    let (url, host, container) = match kind {
        TeamNodeKind::Manual | TeamNodeKind::Local => (Some(value.to_string()), None, None),
        TeamNodeKind::Ssh => (None, Some(value.to_string()), None),
        TeamNodeKind::Docker => (None, None, Some(value.to_string())),
        TeamNodeKind::SshDocker => {
            let (host, container) = value
                .split_once(':')
                .with_context(|| format!("ssh-docker node `{raw}` needs HOST:CONTAINER"))?;
            (None, Some(host.to_string()), Some(container.to_string()))
        }
    };
    let cwd = if matches!(kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker) {
        Some("/workspace".to_string())
    } else {
        None
    };
    Ok(TeamNode {
        id,
        kind,
        url,
        host,
        container,
        cwd,
        status: TeamNodeStatus::Pending,
        note: String::new(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
    })
}

fn create_task(team_dir: &Path, args: TaskAddArgs) -> Result<TeamTask> {
    let id = allocate_task_id(team_dir)?;
    let created_at = now();
    let depends_on = normalize_task_dependencies(args.depends_on, Some(&id))?;
    validate_task_dependencies_exist(team_dir, &depends_on)?;
    let task = TeamTask {
        id: id.clone(),
        subject: args.subject,
        description: args.description,
        owner: args.owner,
        status: if depends_on.is_empty() {
            TaskStatus::Pending
        } else {
            TaskStatus::Waiting
        },
        depends_on,
        result: None,
        created_at: created_at.clone(),
        updated_at: created_at,
    };
    write_json_atomic(&task_path(team_dir, &id), &task)?;
    Ok(task)
}

fn create_or_reuse_resume_task(
    team_dir: &Path,
    member: &str,
    mission: &str,
) -> Result<(TeamTask, bool)> {
    if let Some(task) = reuse_task_referenced_by_resume_mission(team_dir, member, mission)? {
        return Ok((task, true));
    }
    let subject = format!(
        "Department mission for {member}: {mission}\n\nOperate as one department-level Codex session."
    );
    if let Some(task) = reuse_resume_task(team_dir, member, &subject, mission)? {
        return Ok((task, true));
    }
    let task = create_task(
        team_dir,
        TaskAddArgs {
            subject,
            description: String::new(),
            owner: Some(member.to_string()),
            depends_on: Vec::new(),
        },
    )?;
    Ok((task, false))
}

fn reuse_task_referenced_by_resume_mission(
    team_dir: &Path,
    member: &str,
    mission: &str,
) -> Result<Option<TeamTask>> {
    let referenced_ids = task_ids_referenced_in_text(mission);
    if referenced_ids.is_empty() {
        return Ok(None);
    }

    let mut tasks = load_tasks(team_dir)?;
    for referenced_id in referenced_ids {
        let Some(task) = tasks.iter_mut().find(|task| task.id == referenced_id) else {
            continue;
        };
        if task.owner.as_deref() != Some(member) {
            bail!(
                "resume mission references task {}, but it is owned by {}; reassign or update that task explicitly instead of creating a duplicate mission task",
                task.id,
                task.owner.as_deref().unwrap_or("no owner")
            );
        }
        if matches!(
            task.status,
            TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
        ) {
            bail!(
                "resume mission references task {} which is already {}; reopen it explicitly with `team task set {} --status in_progress --result \"reopened by lead: ...\"` before resuming, or create a clearly different task",
                task.id,
                task.status,
                task.id
            );
        }
        if matches!(
            task.status,
            TaskStatus::Pending | TaskStatus::Blocked | TaskStatus::Review | TaskStatus::Ready
        ) {
            task.status = TaskStatus::InProgress;
        }
        task.result = Some(append_result_note(
            task.result.as_deref(),
            &format!(
                "Resumed referenced task without creating a duplicate mission task. Resume mission: {mission}"
            ),
        ));
        task.updated_at = now();
        let reused = task.clone();
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        append_event(
            team_dir,
            "task_reused_for_resume",
            serde_json::json!({
                "task": reused,
                "member": member,
                "mission": mission,
                "source": "referenced_task",
            }),
        )?;
        return Ok(Some(reused));
    }

    Ok(None)
}

fn task_ids_referenced_in_text(text: &str) -> Vec<String> {
    let tokens = text
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| !token.is_empty())
        .map(|token| token.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let candidate = if token == "task" {
            tokens
                .get(index + 1)
                .filter(|next| next.chars().all(|ch| ch.is_ascii_digit()))
                .cloned()
        } else {
            token
                .strip_prefix("task")
                .filter(|suffix| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
                .map(str::to_string)
        };
        if let Some(id) = candidate
            && seen.insert(id.clone())
        {
            ids.push(id);
        }
    }
    ids
}

fn reuse_resume_task(
    team_dir: &Path,
    member: &str,
    subject: &str,
    mission: &str,
) -> Result<Option<TeamTask>> {
    let mut tasks = load_tasks(team_dir)?;
    let open_owned = tasks
        .iter()
        .filter(|task| {
            task.owner.as_deref() == Some(member)
                && !matches!(
                    task.status,
                    TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
                )
        })
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    if open_owned.is_empty() {
        return Ok(None);
    }

    let normalized_subject = normalize_task_text(subject);
    let selected_id = open_owned
        .iter()
        .find(|id| {
            tasks
                .iter()
                .find(|task| task.id == **id)
                .is_some_and(|task| normalize_task_text(&task.subject) == normalized_subject)
        })
        .cloned()
        .or_else(|| {
            if open_owned.len() == 1 {
                open_owned.first().cloned()
            } else {
                tasks
                    .iter()
                    .filter(|task| open_owned.iter().any(|id| id == &task.id))
                    .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
                    .map(|task| task.id.clone())
            }
        });
    let Some(selected_id) = selected_id else {
        return Ok(None);
    };

    let now = now();
    let mut reused = None;
    for task in &mut tasks {
        if task.id == selected_id {
            if matches!(
                task.status,
                TaskStatus::Pending | TaskStatus::Blocked | TaskStatus::Review
            ) {
                task.status = TaskStatus::InProgress;
            }
            let note =
                format!("Resumed without creating a duplicate task. Resume mission: {mission}");
            task.result = Some(append_result_note(task.result.as_deref(), &note));
            task.updated_at = now.clone();
            reused = Some(task.clone());
            break;
        }
    }
    for task in &tasks {
        write_json_atomic(&task_path(team_dir, &task.id), task)?;
    }
    touch_config(team_dir)?;
    if let Some(task) = reused.as_ref() {
        append_event(
            team_dir,
            "task_reused_for_resume",
            serde_json::json!({
                "task": task,
                "member": member,
                "mission": mission,
            }),
        )?;
    }
    Ok(reused)
}

fn normalize_task_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn append_result_note(existing: Option<&str>, note: &str) -> String {
    match existing
        .map(str::trim)
        .filter(|existing| !existing.is_empty())
    {
        Some(existing) => format!("{existing}\n\n{note}"),
        None => note.to_string(),
    }
}

fn is_soft_dependency_wait(task: &TeamTask) -> bool {
    if task.depends_on.is_empty() {
        return false;
    }
    if task_has_manual_dependency_hold(task) {
        return false;
    }
    if matches!(task.status, TaskStatus::Waiting) {
        return true;
    }
    if !matches!(task.status, TaskStatus::Blocked) {
        return false;
    }
    task.result.as_deref().is_none_or(|result| {
        let normalized = result.trim().to_ascii_lowercase();
        normalized.is_empty()
            || normalized.contains("depend")
            || normalized.contains("blocked on task")
            || normalized.contains("waiting on task")
            || normalized.contains("waiting for task")
            || normalized.contains("deps:")
    })
}

fn task_has_manual_dependency_hold(task: &TeamTask) -> bool {
    let Some(result) = task.result.as_deref() else {
        return false;
    };
    let normalized = result.trim().to_ascii_lowercase();
    [
        "explicit reopen",
        "explicitly reopen",
        "explicit lead reopen",
        "explicitly reopens",
        "ignore ready_to_start",
        "ignore ready-to-start",
        "not execute from ready_to_start",
        "do not execute from ready_to_start",
        "must not execute from ready_to_start",
        "not start from ready_to_start",
        "do not start from ready_to_start",
        "must not start from ready_to_start",
        "lead sync/verification",
        "lead sync and verification",
        "lead sync + verification",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn task_dependencies_completed(task: &TeamTask, tasks: &[TeamTask]) -> bool {
    task.depends_on.iter().all(|dependency| {
        tasks.iter().any(|candidate| {
            candidate.id == *dependency && matches!(candidate.status, TaskStatus::Completed)
        })
    })
}

fn task_has_positive_lead_clearance(task: &TeamTask) -> bool {
    let Some(result) = task.result.as_deref() else {
        return false;
    };
    let normalized = result.to_ascii_lowercase();
    [
        "cleared by lead",
        "lead cleared",
        "lead clearance granted",
        "explicit ready/cleared",
        "explicitly cleared",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn task_requires_contract_input_clearance(
    config: &TeamConfig,
    contract_inputs: &HashMap<String, Vec<ContractDeclaredInput>>,
    task: &TeamTask,
) -> bool {
    if task_has_positive_lead_clearance(task) {
        return false;
    }
    if !contract_inputs.contains_key(task.id.as_str()) {
        return false;
    }
    let Some(owner) = task.owner.as_deref() else {
        return false;
    };
    let Some(member) = config.members.iter().find(|member| member.name == owner) else {
        return false;
    };
    member_node_id(member) != "local"
}

fn task_is_ready(task: &TeamTask, tasks: &[TeamTask]) -> bool {
    matches!(task.status, TaskStatus::Pending | TaskStatus::Ready)
        && task_dependencies_completed(task, tasks)
}

fn auto_promote_dependency_waits(team_dir: &Path) -> Result<Vec<TeamTask>> {
    let mut config = load_config(team_dir)?;
    let mut tasks = load_tasks(team_dir)?;
    let contract_inputs = load_contract_declared_inputs(&load_ownerships(team_dir)?)?;
    let snapshot = tasks.clone();
    let updated_at = now();
    let waiting_ids = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Pending | TaskStatus::Ready))
        .filter(|task| !task.depends_on.is_empty())
        .filter(|task| !task_dependencies_completed(task, &snapshot))
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let ready_ids = tasks
        .iter()
        .filter(|task| is_soft_dependency_wait(task))
        .filter(|task| task_dependencies_completed(task, &snapshot))
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let contract_clearance_hold_ids = tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Waiting | TaskStatus::Blocked
            )
        })
        .filter(|task| task_dependencies_completed(task, &snapshot))
        .filter(|task| task_requires_contract_input_clearance(&config, &contract_inputs, task))
        .map(|task| task.id.clone())
        .collect::<HashSet<_>>();
    let mut promoted = Vec::new();
    let mut held_for_contract_clearance = Vec::new();
    let mut reactivated_members = Vec::new();
    let mut reactivated_tasks = Vec::new();
    let mut tasks_changed = false;
    let mut config_changed = false;
    for task in &mut tasks {
        if waiting_ids.contains(&task.id) {
            task.status = TaskStatus::Waiting;
            task.updated_at = updated_at.clone();
            task.result = Some(append_result_note(
                task.result.as_deref(),
                "Soft-waiting for dependency task(s).",
            ));
            tasks_changed = true;
        }
        if contract_clearance_hold_ids.contains(&task.id) {
            let note = "Dependency gate cleared, but this non-local task has contract-declared inputs. Await explicit lead root-correct verification clearance before READY_TO_START.";
            let already_noted = task
                .result
                .as_deref()
                .is_some_and(|result| result.contains(note));
            let already_waiting = matches!(task.status, TaskStatus::Waiting);
            if !already_waiting || !already_noted {
                task.status = TaskStatus::Waiting;
                task.updated_at = updated_at.clone();
                if !already_noted {
                    task.result = Some(append_result_note(task.result.as_deref(), note));
                }
                held_for_contract_clearance.push(task.clone());
                tasks_changed = true;
            }
        } else if ready_ids.contains(&task.id) {
            task.status = TaskStatus::Ready;
            task.updated_at = updated_at.clone();
            task.result = Some(append_result_note(
                task.result.as_deref(),
                "Dependency gate cleared automatically; task is ready.",
            ));
            promoted.push(task.clone());
            tasks_changed = true;
        }
        if matches!(task.status, TaskStatus::Ready | TaskStatus::Pending)
            && let Some(owner) = task.owner.as_deref()
            && let Some(member) = config
                .members
                .iter_mut()
                .find(|member| member.name == owner)
            && matches!(
                member.status,
                MemberStatus::Standby | MemberStatus::Completed
            )
        {
            member.status = MemberStatus::Online;
            config.updated_at = updated_at.clone();
            config_changed = true;
            reactivated_members.push(owner.to_string());
            reactivated_tasks.push(task.clone());
        }
    }
    if !tasks_changed && !config_changed {
        return Ok(Vec::new());
    }
    if tasks_changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
    }
    if config_changed {
        write_json_atomic(&team_dir.join("config.json"), &config)?;
    }
    touch_config(team_dir)?;
    if !waiting_ids.is_empty() || !reactivated_members.is_empty() {
        append_event(
            team_dir,
            "task_dependency_reconciled",
            serde_json::json!({
                "waiting_tasks": waiting_ids,
                "reactivated_members": reactivated_members,
            }),
        )?;
    }
    for task in &promoted {
        append_event(
            team_dir,
            "task_dependency_unblocked",
            serde_json::json!({ "task": task }),
        )?;
        send_ready_to_start_message(team_dir, task)?;
    }
    for task in &held_for_contract_clearance {
        append_event(
            team_dir,
            "task_contract_input_clearance_required",
            serde_json::json!({ "task": task }),
        )?;
        send_contract_input_clearance_required_message(team_dir, task)?;
    }
    let promoted_ids = promoted.iter().map(|task| &task.id).collect::<HashSet<_>>();
    for task in &reactivated_tasks {
        if !promoted_ids.contains(&task.id) && !contract_clearance_hold_ids.contains(&task.id) {
            send_ready_to_start_message(team_dir, task)?;
        }
    }
    Ok(promoted)
}

fn send_ready_to_start_message(team_dir: &Path, task: &TeamTask) -> Result<()> {
    let config = load_config(team_dir)?;
    let recipients = ready_task_recipients(&config, task);
    if recipients.is_empty() {
        return Ok(());
    }
    let deps = task.depends_on.join(",");
    let message = match task.owner.as_deref() {
        Some(owner) => format!(
            "READY_TO_START: task {} is ready for @{owner}; dependencies completed: {deps}.",
            task.id
        ),
        None => format!(
            "READY_TO_START: unassigned task {} is ready; dependencies completed: {deps}. Members may self-claim it with `team task claim {}` when it is within scope.",
            task.id, task.id
        ),
    };
    send_system_message_to_recipients(team_dir, &recipients, &message)
}

fn send_contract_input_clearance_required_message(team_dir: &Path, task: &TeamTask) -> Result<()> {
    let config = load_config(team_dir)?;
    let recipients = ready_task_recipients(&config, task);
    if recipients.is_empty() {
        return Ok(());
    }
    let owner = task.owner.as_deref().unwrap_or("unassigned");
    let deps = task.depends_on.join(",");
    let message = format!(
        "AWAITING_LEAD_CLEARANCE: task {} dependencies are complete for @{owner} ({deps}), but this non-local task has contract-declared inputs. Lead must sync/root-correct verify declared inputs, predecessor manifests, and guard/bootstrap requirements, then explicitly clear or resume the owner. Do not start from dependency completion alone.",
        task.id
    );
    send_system_message_to_recipients(team_dir, &recipients, &message)
}

fn ready_task_recipients(config: &TeamConfig, task: &TeamTask) -> Vec<String> {
    let mut recipients = Vec::new();
    if let Some(owner) = task.owner.as_deref()
        && config.members.iter().any(|member| member.name == owner)
    {
        recipients.push(owner.to_string());
    }
    if recipients.is_empty() {
        recipients.extend(
            config
                .members
                .iter()
                .filter(|member| member.name != config.lead)
                .map(|member| member.name.clone()),
        );
    }
    if config
        .members
        .iter()
        .any(|member| member.name == config.lead)
        && !recipients.iter().any(|recipient| recipient == &config.lead)
    {
        recipients.push(config.lead.clone());
    }
    recipients
}

fn send_system_message_to_recipients(
    team_dir: &Path,
    recipients: &[String],
    message: &str,
) -> Result<()> {
    let mut seen = HashSet::new();
    let recipients = recipients
        .iter()
        .filter(|recipient| seen.insert((*recipient).clone()))
        .cloned()
        .collect::<Vec<_>>();
    for recipient in &recipients {
        let msg = MailMessage {
            from: "system".to_string(),
            to: recipient.clone(),
            message: message.to_string(),
            timestamp: now(),
            read: false,
        };
        append_jsonl(&mailbox_path(team_dir, &msg.to), &msg)?;
    }
    append_event(
        team_dir,
        "message_sent",
        serde_json::json!({
            "from": "system",
            "to": recipients,
            "message": message,
            "source": "task_ready",
        }),
    )?;
    Ok(())
}

fn claim_ready_task(team_dir: &Path, args: TaskClaimArgs) -> Result<()> {
    auto_promote_dependency_waits(team_dir)?;
    let config = load_config(team_dir)?;
    let owner = args.owner.unwrap_or_else(default_team_member_name);
    ensure_member_exists(&config, &owner)?;

    let mut tasks = load_tasks(team_dir)?;
    let snapshot = tasks.clone();
    let selected_id = match args.id {
        Some(id) => id,
        None => tasks
            .iter()
            .find(|task| task.owner.is_none() && task_is_ready(task, &snapshot))
            .map(|task| task.id.clone())
            .ok_or_else(|| anyhow!("no unassigned ready tasks found"))?,
    };

    let updated_at = now();
    let mut claimed = None;
    for task in &mut tasks {
        if task.id == selected_id {
            if !task_is_ready(task, &snapshot) {
                bail!("task {} is not ready to claim", task.id);
            }
            if let Some(existing_owner) = task.owner.as_deref()
                && existing_owner != owner
            {
                bail!("task {} is already owned by @{existing_owner}", task.id);
            }
            task.owner = Some(owner.clone());
            task.status = TaskStatus::InProgress;
            task.updated_at = updated_at.clone();
            claimed = Some(task.clone());
            break;
        }
    }
    let Some(claimed) = claimed else {
        bail!("task {selected_id} not found");
    };
    for task in &tasks {
        write_json_atomic(&task_path(team_dir, &task.id), task)?;
    }
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "task_claimed",
        serde_json::json!({ "task": claimed, "owner": owner }),
    )?;
    let message = format!(
        "READY_TO_START: @{owner} claimed task {} and moved it to in_progress.",
        claimed.id
    );
    send_team_message_to_dir(team_dir, &owner, &config.lead, &message)?;
    println!("Claimed task {}", claimed.id);
    Ok(())
}

fn update_task(team_dir: &Path, args: TaskSetArgs) -> Result<()> {
    let path = task_path(team_dir, &args.id);
    let mut task: TeamTask = read_json(&path)?;
    let deps_changed = args.clear_depends || !args.depends_on.is_empty();
    if args.clear_owner {
        task.owner = None;
    }
    if let Some(owner) = args.owner {
        task.owner = Some(owner);
    }
    if deps_changed {
        let deps = if args.clear_depends {
            Vec::new()
        } else {
            args.depends_on
        };
        let deps = normalize_task_dependencies(deps, Some(&task.id))?;
        validate_task_dependencies_exist(team_dir, &deps)?;
        let tasks = load_tasks(team_dir)?;
        task.depends_on = deps;
        if !task.depends_on.is_empty()
            && !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
            && !task_dependencies_completed(&task, &tasks)
        {
            task.status = TaskStatus::Waiting;
            task.result = Some(format!(
                "Waiting for dependency task(s): {}.",
                task.depends_on.join(",")
            ));
        }
    }
    let requested_status = args.status;
    if let Some(status) = requested_status {
        task.status = status;
    }
    if let Some(result) = args.result {
        task.result = Some(result);
    }
    if requested_status == Some(TaskStatus::Completed)
        && let Some(issue) = task_completion_blocker(team_dir, &task)?
    {
        task.status = TaskStatus::Blocked;
        task.result = Some(append_result_note(
            task.result.as_deref(),
            &format!("Completion rejected: {issue}"),
        ));
    }
    task.updated_at = now();
    write_json_atomic(&path, &task)?;
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "task_updated",
        serde_json::json!({ "task": task }),
    )?;
    auto_promote_dependency_waits(team_dir)?;
    println!("Updated task {}", args.id);
    Ok(())
}

fn normalize_task_dependencies(
    dependencies: Vec<String>,
    task_id: Option<&str>,
) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for dependency in dependencies {
        let dependency = dependency.trim();
        if dependency.is_empty() {
            continue;
        }
        if task_id.is_some_and(|id| dependency == id) {
            bail!("task {dependency} cannot depend on itself");
        }
        if seen.insert(dependency.to_string()) {
            normalized.push(dependency.to_string());
        }
    }
    Ok(normalized)
}

fn validate_task_dependencies_exist(team_dir: &Path, dependencies: &[String]) -> Result<()> {
    if dependencies.is_empty() {
        return Ok(());
    }
    let tasks = load_tasks(team_dir)?;
    let known = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<HashSet<_>>();
    if let Some(missing) = dependencies
        .iter()
        .find(|dependency| !known.contains(dependency.as_str()))
    {
        bail!("dependency task {missing} not found");
    }
    Ok(())
}

fn claim_ownership(team_dir: &Path, args: OwnershipClaimArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    let owner = args.owner.unwrap_or_else(default_team_member_name);
    ensure_member_exists(&config, &owner)?;
    let path = normalize_ownership_path(&args.path)?;
    let mut ownerships = load_ownerships(team_dir)?;
    let now = now();
    if let Some(existing) = ownerships.iter_mut().find(|entry| entry.path == path) {
        if existing.owner != owner && !args.force {
            bail!(
                "`{}` is already owned by `{}`; ask them or lead for handoff, or pass --force",
                existing.path,
                existing.owner
            );
        }
        let previous_owner = existing.owner.clone();
        existing.owner = owner.clone();
        existing.note = args.note;
        existing.updated_at = now.clone();
        write_ownerships(team_dir, &ownerships)?;
        touch_config(team_dir)?;
        append_event(
            team_dir,
            "ownership_claimed",
            serde_json::json!({
                "path": path,
                "owner": owner,
                "previousOwner": previous_owner,
                "forced": args.force,
            }),
        )?;
        println!("Claimed {path} for {owner}");
        return Ok(());
    }

    ownerships.push(FileOwnership {
        path: path.clone(),
        owner: owner.clone(),
        note: args.note,
        updated_at: now,
    });
    ownerships.sort_by(|a, b| a.path.cmp(&b.path));
    write_ownerships(team_dir, &ownerships)?;
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "ownership_claimed",
        serde_json::json!({ "path": path, "owner": owner, "forced": false }),
    )?;
    println!("Claimed {path} for {owner}");
    Ok(())
}

fn release_ownership(team_dir: &Path, args: OwnershipReleaseArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    let owner = args.owner.unwrap_or_else(default_team_member_name);
    ensure_member_exists(&config, &owner)?;
    let path = normalize_ownership_path(&args.path)?;
    let mut ownerships = load_ownerships(team_dir)?;
    let Some(index) = ownerships.iter().position(|entry| entry.path == path) else {
        bail!("`{path}` is not claimed");
    };
    let existing = &ownerships[index];
    if existing.owner != owner && owner != config.lead && !args.force {
        bail!(
            "`{}` is owned by `{}`; only that owner, lead, or --force can release it",
            existing.path,
            existing.owner
        );
    }
    let released = ownerships.remove(index);
    write_ownerships(team_dir, &ownerships)?;
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "ownership_released",
        serde_json::json!({
            "path": released.path,
            "owner": released.owner,
            "releasedBy": owner,
            "forced": args.force,
        }),
    )?;
    println!("Released {} from {}", released.path, released.owner);
    Ok(())
}

fn add_team_member(team_dir: &Path, args: MemberAddArgs) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let now = now();
    let mut member = parse_member(&args.member, &now)?;
    if let Some(node) = args.node {
        member.node = Some(sanitize_id(&node));
    }
    if let Some(node) = member.node.as_deref()
        && node != "local"
    {
        ensure_node_exists(team_dir, node)?;
    }
    if config
        .members
        .iter()
        .any(|existing| existing.name == member.name)
    {
        bail!(
            "member `{}` already exists in team `{}`",
            member.name,
            config.id
        );
    }
    let mission = if args.mission.trim().is_empty() {
        format!(
            "Department mission for {}: support the team goal where this department's role is useful.\n\nOperate as one department-level Codex session. If the mission is broad or heavy, use available subagent/agent tools, skills, MCP servers, or internal decomposition inside this department.",
            member.name
        )
    } else {
        format!(
            "Department mission for {}: {}\n\nOperate as one department-level Codex session. If the mission is broad or heavy, use available subagent/agent tools, skills, MCP servers, or internal decomposition inside this department.",
            member.name, args.mission
        )
    };
    config.members.push(member.clone());
    config.updated_at = now.clone();
    write_json_atomic(&team_dir.join("config.json"), &config)?;
    let task = create_task(
        team_dir,
        TaskAddArgs {
            subject: mission,
            description: String::new(),
            owner: Some(member.name.clone()),
            depends_on: Vec::new(),
        },
    )?;
    append_event(
        team_dir,
        "member_added",
        serde_json::json!({
            "member": member,
            "task": task,
        }),
    )?;
    println!("Added member {}", task.owner.as_deref().unwrap_or(""));
    Ok(())
}

fn ensure_container_node_departments(team_dir: &Path) -> Result<()> {
    let nodes = load_nodes(team_dir)?;
    let config = load_config(team_dir)?;
    for node in nodes
        .iter()
        .filter(|node| matches!(node.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker))
    {
        if config
            .members
            .iter()
            .any(|member| member.node.as_deref() == Some(node.id.as_str()))
        {
            continue;
        }
        let member_name = unique_member_name(&config, &format!("{}-container", node.id));
        let host_text = node
            .host
            .as_deref()
            .map(|host| format!(" on SSH host `{host}`"))
            .unwrap_or_default();
        let container_text = node
            .container
            .as_deref()
            .unwrap_or("the registered container");
        let cwd_text = node.cwd.as_deref().unwrap_or("/workspace");
        let mission = format!(
            "Run as the container-internal department for node `{node}`{host_text}. You are expected to execute from inside Docker container `{container}` at `{cwd}` through the node app-server, not merely from the host. Take over the main runtime work that this container was created for: install missing tools inside the container, run the sample/application/model/experiment, render or test outputs, debug container-local failures, and produce container-local verification evidence. At the start of your turn, create a concrete runtime workspace such as `{cwd}/runtime_container` and immediately write an initial status/progress artifact there, even before the heavy work finishes. Verify mounts, ports, GPUs, package/tool availability, and run container-local smoke checks before heavy work. Any material command whose exit status matters must leave a command transcript with exact command, cwd, container identity, timestamps when practical, and `rc=`/`exit=`; any long or asynchronous work must be tracked with `team job` or `team wait` instead of being hidden inside an untracked shell or only described in chat. Do not include a live transcript, manifest check log, handoff log, progress file, or helper/finalizer script in a final manifest if you will append to or patch it afterward; either close it permanently before hashing or exclude it and hash a stable final copy. If repairing the manifest changes a script/report/log that is listed in the manifest, regenerate and recheck the manifest again after that file is stable. Immediately before final handoff, rerun manifest verification and report the fresh rc and current manifest/log hashes from disk. Coordinate with the host/SSH department only for image rebuilds, container replacement, mount/port/GPU fixes, or host-side resource issues. Report results and blockers to lead and other departments, and stay available for follow-up container debugging.",
            node = node.id,
            container = container_text,
            cwd = cwd_text,
        );
        add_team_member(
            team_dir,
            MemberAddArgs {
                member: format!("{member_name}:container"),
                node: Some(node.id.clone()),
                mission,
            },
        )?;
        append_event(
            team_dir,
            "container_department_auto_added",
            serde_json::json!({
                "node": node.id,
                "member": member_name,
                "kind": node.kind,
            }),
        )?;
    }
    Ok(())
}

fn unique_member_name(config: &TeamConfig, base: &str) -> String {
    let base = sanitize_id(base);
    if !config
        .members
        .iter()
        .any(|member| member.name == base.as_str())
    {
        return base;
    }
    for index in 2.. {
        let candidate = format!("{base}-{index}");
        if !config.members.iter().any(|member| member.name == candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn standby_team_member(team_dir: &Path, args: MemberStandbyArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    if args.member == config.lead {
        bail!("lead cannot be moved to standby");
    }
    ensure_member_exists(&config, &args.member)?;
    set_member_status(team_dir, &args.member, MemberStatus::Standby)?;
    append_event(
        team_dir,
        "member_standby",
        serde_json::json!({
            "member": args.member,
            "reason": args.reason,
        }),
    )?;
    println!("Moved {} to standby", args.member);
    Ok(())
}

fn resume_team_member(team_dir: &Path, args: MemberResumeArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    ensure_member_exists(&config, &args.member)?;
    set_member_status(team_dir, &args.member, MemberStatus::Online)?;
    let (task, reused_task) = if let Some(mission) = args.mission {
        let (task, reused) = create_or_reuse_resume_task(team_dir, &args.member, &mission)?;
        (Some(task), reused)
    } else {
        (None, false)
    };
    append_event(
        team_dir,
        "member_resumed",
        serde_json::json!({
            "member": args.member,
            "task": task,
            "reused_task": reused_task,
        }),
    )?;
    println!("Resumed {}", args.member);
    Ok(())
}

fn run_node(root: &Path, cli: NodeCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    match cli.subcommand {
        NodeSubcommand::List => list_team_nodes(&team_dir),
        NodeSubcommand::Inspect(args) => inspect_team_nodes(&team_dir, args),
        NodeSubcommand::CreateDocker(args) => create_docker_node(&team_dir, args),
        NodeSubcommand::SyncAssets(args) => sync_node_assets(&team_dir, args),
        NodeSubcommand::SyncPath(args) => sync_node_path(&team_dir, args),
        NodeSubcommand::Add(args) => add_team_node(&team_dir, args),
        NodeSubcommand::Remove(args) => remove_team_node(&team_dir, args),
    }
}

fn list_team_nodes(team_dir: &Path) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    for node in nodes {
        println!(
            "{}  {:?}  {:?}  url={}  host={}  container={}  cwd={}  {}",
            node.id,
            node.kind,
            node.status,
            node.url.unwrap_or_default(),
            node.host.unwrap_or_default(),
            node.container.unwrap_or_default(),
            node.cwd.unwrap_or_default(),
            node.note
        );
    }
    Ok(())
}

fn inspect_team_nodes(team_dir: &Path, args: NodeInspectArgs) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let selected = match args.id.as_deref() {
        Some(id) => {
            let id = sanitize_id(id);
            nodes
                .into_iter()
                .filter(|node| node.id == id)
                .collect::<Vec<_>>()
        }
        None => nodes,
    };
    if selected.is_empty() {
        bail!("node not found");
    }
    for node in selected {
        if !args.raw {
            println!("== {} ({:?}) ==", node.id, node.kind);
        }
        let facts = collect_node_facts(&node)?;
        println!("{}", facts.trim_end());
        if matches!(node.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker)
            && let Some(container) = node.container.as_deref()
        {
            let ports = docker_inspect_value(
                node.host.as_deref(),
                container,
                "{{json .NetworkSettings.Ports}}",
            )
            .unwrap_or_default();
            let mounts = docker_inspect_value(node.host.as_deref(), container, "{{json .Mounts}}")
                .unwrap_or_default();
            println!("docker_ports_json={ports}");
            println!("docker_mounts_json={mounts}");
        }
        if !args.raw {
            println!();
        }
    }
    Ok(())
}

fn create_docker_node(team_dir: &Path, args: NodeCreateDockerArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    let id = sanitize_id(&args.id);
    if id.is_empty() {
        bail!("invalid node id `{}`", args.id);
    }
    let container = args.container.clone().unwrap_or_else(|| {
        format!(
            "codex-team-{}-{}",
            sanitize_id(&config.id),
            sanitize_id(&id)
        )
    });
    let mut mounts = args.mounts.clone();
    if mounts.is_empty() {
        let host_path = if args.host.is_some() {
            format!("/tmp/codex-team-workspaces/{}/{}", config.id, id)
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .display()
                .to_string()
        };
        mounts.push(format!("{host_path}:{}", args.cwd));
    }

    let docker_args = docker_run_args(&args, &container, &mounts);
    let command_text = if let Some(host) = args.host.as_deref() {
        let remote_mount_dirs = mounts
            .iter()
            .filter_map(|mount| mount.split_once(':').map(|(host_path, _)| host_path))
            .filter(|host_path| !host_path.starts_with('/') || !host_path.contains('*'))
            .map(shell_quote)
            .collect::<Vec<_>>();
        let mkdir = if remote_mount_dirs.is_empty() {
            String::new()
        } else {
            format!("mkdir -p {} && ", remote_mount_dirs.join(" "))
        };
        let replace = if args.replace {
            format!(
                "docker rm -f {} >/dev/null 2>&1 || true && ",
                shell_quote(&container)
            )
        } else {
            String::new()
        };
        let remote = format!("{mkdir}{replace}docker {}", docker_args.join(" "));
        run_ssh_command(host, &remote)?
    } else {
        let replace = if args.replace {
            format!(
                "docker rm -f {} >/dev/null 2>&1 || true && ",
                shell_quote(&container)
            )
        } else {
            String::new()
        };
        run_shell_capture(
            &format!("{replace}docker {}", docker_args.join(" ")),
            "run docker container",
        )?
    };

    let kind = if args.host.is_some() {
        TeamNodeKind::SshDocker
    } else {
        TeamNodeKind::Docker
    };
    add_team_node(
        team_dir,
        NodeAddArgs {
            id: id.clone(),
            kind,
            url: None,
            host: args.host,
            container: Some(container.clone()),
            cwd: Some(args.cwd),
            note: args.note,
        },
    )?;
    ensure_container_node_departments(team_dir)?;
    append_event(
        team_dir,
        "docker_node_created",
        serde_json::json!({
            "node": id,
            "container": container,
            "output": command_text,
            "mounts": mounts,
        }),
    )?;
    Ok(())
}

fn docker_run_args(args: &NodeCreateDockerArgs, container: &str, mounts: &[String]) -> Vec<String> {
    let mut docker_args = vec![
        "run".to_string(),
        "-d".to_string(),
        "--name".to_string(),
        shell_quote(container),
    ];
    if args.gpus {
        docker_args.push("--gpus".to_string());
        docker_args.push("all".to_string());
    }
    for mount in mounts {
        docker_args.push("-v".to_string());
        docker_args.push(shell_quote(mount));
    }
    docker_args.push("-w".to_string());
    docker_args.push(shell_quote(&args.cwd));
    for port in &args.ports {
        docker_args.push("-p".to_string());
        docker_args.push(shell_quote(port));
    }
    for env in &args.env {
        docker_args.push("-e".to_string());
        docker_args.push(shell_quote(env));
    }
    docker_args.push(shell_quote(&args.image));
    docker_args.push("bash".to_string());
    docker_args.push("-lc".to_string());
    docker_args.push(shell_quote(&args.command));
    docker_args
}

fn sync_node_assets(team_dir: &Path, args: NodeSyncAssetsArgs) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let node = nodes
        .into_iter()
        .find(|node| node.id == sanitize_id(&args.id))
        .with_context(|| format!("node `{}` not found", args.id))?;
    let (command, existing) = build_asset_sync_command(&node, &args.dest, args.include_auth)?;
    if args.dry_run {
        println!("{command}");
        return Ok(());
    }
    run_shell_command(&command, "sync Codex assets")?;
    append_event(
        team_dir,
        "node_assets_synced",
        serde_json::json!({
            "node": node.id,
            "dest": args.dest,
            "include_auth": args.include_auth,
            "paths": existing,
        }),
    )?;
    println!("Synced Codex assets to node {}", node.id);
    Ok(())
}

fn sync_node_path(team_dir: &Path, args: NodeSyncPathArgs) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let node = nodes
        .into_iter()
        .find(|node| node.id == sanitize_id(&args.id))
        .with_context(|| format!("node `{}` not found", args.id))?;
    let src = args
        .src
        .canonicalize()
        .with_context(|| format!("source path `{}` not found", args.src.display()))?;
    let (command, src_kind) = build_path_sync_command(&node, &src, &args.dest, args.replace)?;
    if args.dry_run {
        println!("{command}");
        return Ok(());
    }
    run_shell_command(&command, "sync team artifact path")?;
    append_event(
        team_dir,
        "node_path_synced",
        serde_json::json!({
            "node": node.id,
            "src": src,
            "dest": args.dest,
            "kind": src_kind,
            "replace": args.replace,
        }),
    )?;
    println!(
        "Synced {} to node {}:{}",
        args.src.display(),
        node.id,
        args.dest
    );
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContractDeclaredInput {
    label: String,
    src: PathBuf,
    dest: String,
    contract_path: PathBuf,
}

fn maybe_sync_contract_declared_inputs(
    team_dir: &Path,
    config: &TeamConfig,
    nodes: &[TeamNode],
    attempts: &mut HashSet<String>,
) -> Result<()> {
    let tasks = load_tasks(team_dir)?;
    let ownerships = load_ownerships(team_dir)?;
    let node_by_id = nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect::<HashMap<_, _>>();
    let member_by_name = config
        .members
        .iter()
        .map(|member| (member.name.as_str(), member))
        .collect::<HashMap<_, _>>();
    let contract_inputs = load_contract_declared_inputs(&ownerships)?;
    if contract_inputs.is_empty() {
        return Ok(());
    }

    for task in tasks.iter().filter(|task| {
        matches!(
            task.status,
            TaskStatus::Ready | TaskStatus::InProgress | TaskStatus::Waiting | TaskStatus::Blocked
        ) && task_dependencies_completed(task, &tasks)
    }) {
        let Some(owner) = task.owner.as_deref() else {
            continue;
        };
        let Some(member) = member_by_name.get(owner) else {
            continue;
        };
        let node_id = member_node_id(member);
        if node_id == "local" {
            continue;
        }
        let Some(node) = node_by_id.get(node_id.as_str()) else {
            continue;
        };
        if matches!(node.kind, TeamNodeKind::Manual) {
            continue;
        }

        let Some(inputs) = contract_inputs.get(task.id.as_str()) else {
            continue;
        };
        for input in inputs {
            let key = format!(
                "{}:{}:{}:{}",
                task.id,
                node.id,
                input.src.display(),
                input.dest
            );
            if !attempts.insert(key.clone()) {
                continue;
            }
            if !input.src.exists() {
                let message = format!(
                    "Contract-declared input sync warning for task {}: `{}` in `{}` points to missing local source `{}` for node `{}` destination `{}`. Keep @{owner} blocked or waiting until lead fixes the authoritative source path or produces the missing artifact.",
                    task.id,
                    input.label,
                    input.contract_path.display(),
                    input.src.display(),
                    node.id,
                    input.dest
                );
                send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
                send_team_message_to_dir(team_dir, "system", owner, &message)?;
                append_event(
                    team_dir,
                    "contract_declared_input_sync_missing_source",
                    serde_json::json!({
                        "task": task.id,
                        "owner": owner,
                        "node": node.id,
                        "label": input.label,
                        "contract": input.contract_path,
                        "src": input.src,
                        "dest": input.dest,
                    }),
                )?;
                continue;
            }

            let src = input
                .src
                .canonicalize()
                .with_context(|| format!("canonicalize {}", input.src.display()))?;
            let (command, src_kind) = build_path_sync_command(node, &src, &input.dest, true)?;
            match run_shell_command(&command, "sync contract-declared team input") {
                Ok(()) => {
                    let message = format!(
                        "Contract-declared input auto-sync for task {}: synced `{}` from `{}` to node `{}` destination `{}` using `{}`. This was declared by `{}`; @{owner} should root-correct verify the manifest before relying on it.",
                        task.id,
                        input.label,
                        src.display(),
                        node.id,
                        input.dest,
                        src_kind,
                        input.contract_path.display()
                    );
                    send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
                    send_team_message_to_dir(team_dir, "system", owner, &message)?;
                    append_event(
                        team_dir,
                        "contract_declared_input_synced",
                        serde_json::json!({
                            "task": task.id,
                            "owner": owner,
                            "node": node.id,
                            "label": input.label,
                            "contract": input.contract_path,
                            "src": src,
                            "dest": input.dest,
                            "kind": src_kind,
                            "replace": true,
                        }),
                    )?;
                }
                Err(err) => {
                    let message = format!(
                        "Contract-declared input sync failed for task {}: `{}` from `{}` to node `{}` destination `{}` failed: {err:#}. Keep @{owner} blocked or waiting until lead/ops repairs the sync and verifies the manifest.",
                        task.id,
                        input.label,
                        src.display(),
                        node.id,
                        input.dest
                    );
                    send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
                    send_team_message_to_dir(team_dir, "system", owner, &message)?;
                    append_event(
                        team_dir,
                        "contract_declared_input_sync_failed",
                        serde_json::json!({
                            "task": task.id,
                            "owner": owner,
                            "node": node.id,
                            "label": input.label,
                            "contract": input.contract_path,
                            "src": src,
                            "dest": input.dest,
                            "error": format!("{err:#}"),
                        }),
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn load_contract_declared_inputs(
    ownerships: &[FileOwnership],
) -> Result<HashMap<String, Vec<ContractDeclaredInput>>> {
    let mut by_task = HashMap::<String, Vec<ContractDeclaredInput>>::new();
    let mut seen_contracts = HashSet::<PathBuf>::new();
    for ownership in ownerships {
        let base = Path::new(&ownership.path);
        let contract_path = if base.is_dir() {
            base.join("runtime_contract.yaml")
        } else if base
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "runtime_contract.yaml")
        {
            base.to_path_buf()
        } else {
            continue;
        };
        if !contract_path.exists() || !seen_contracts.insert(contract_path.clone()) {
            continue;
        }
        let content = fs::read_to_string(&contract_path)
            .with_context(|| format!("read {}", contract_path.display()))?;
        let yaml: serde_yaml::Value = serde_yaml::from_str(&content)
            .with_context(|| format!("parse {}", contract_path.display()))?;
        let Some(task_id) = contract_runtime_task_id(&yaml) else {
            continue;
        };
        let mut inputs = Vec::new();
        collect_contract_declared_inputs(&yaml, &contract_path, &mut inputs);
        if !inputs.is_empty() {
            by_task.entry(task_id).or_default().extend(inputs);
        }
    }
    for inputs in by_task.values_mut() {
        inputs.sort_by(|left, right| {
            left.dest
                .cmp(&right.dest)
                .then_with(|| left.src.cmp(&right.src))
                .then_with(|| left.label.cmp(&right.label))
        });
        inputs.dedup_by(|left, right| left.dest == right.dest && left.src == right.src);
    }
    Ok(by_task)
}

fn contract_runtime_task_id(value: &serde_yaml::Value) -> Option<String> {
    yaml_mapping_get(value, "runtime_task")
        .or_else(|| yaml_mapping_get(value, "runtime_task_id"))
        .or_else(|| yaml_mapping_get(value, "consumer_task"))
        .and_then(yaml_scalar_string)
}

fn collect_contract_declared_inputs(
    value: &serde_yaml::Value,
    contract_path: &Path,
    inputs: &mut Vec<ContractDeclaredInput>,
) {
    let serde_yaml::Value::Mapping(mapping) = value else {
        if let serde_yaml::Value::Sequence(values) = value {
            for item in values {
                collect_contract_declared_inputs(item, contract_path, inputs);
            }
        }
        return;
    };

    let source = [
        "host_path",
        "audit_root_host",
        "validation_root_host",
        "runtime_root_host",
        "provenance_root_host",
        "source_root_host",
        "local_path",
    ]
    .iter()
    .find_map(|key| mapping_get_string(mapping, key).map(|value| ((*key).to_string(), value)));
    let dest = [
        "expected_container_input_root",
        "expected_container_root",
        "container_root",
        "dest",
        "destination",
    ]
    .iter()
    .find_map(|key| mapping_get_string(mapping, key));
    if let (Some((source_key, src)), Some(dest)) = (source, dest)
        && is_probable_local_contract_source(&src)
        && is_probable_node_contract_destination(&dest)
    {
        inputs.push(ContractDeclaredInput {
            label: source_key,
            src: PathBuf::from(src),
            dest,
            contract_path: contract_path.to_path_buf(),
        });
    }

    for child in mapping.values() {
        collect_contract_declared_inputs(child, contract_path, inputs);
    }
}

fn yaml_mapping_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
    let serde_yaml::Value::Mapping(mapping) = value else {
        return None;
    };
    mapping.get(serde_yaml::Value::String(key.to_string()))
}

fn mapping_get_string(mapping: &serde_yaml::Mapping, key: &str) -> Option<String> {
    mapping
        .get(serde_yaml::Value::String(key.to_string()))
        .and_then(yaml_scalar_string)
}

fn yaml_scalar_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        serde_yaml::Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn is_probable_local_contract_source(path: &str) -> bool {
    path.starts_with('/') || path.starts_with("$HOME/") || path.starts_with("~/")
}

fn is_probable_node_contract_destination(path: &str) -> bool {
    path.starts_with('/') || path.starts_with("$HOME/") || path.starts_with("~/")
}

fn sync_codex_assets_to_node(
    node: &TeamNode,
    dest: &str,
    include_auth: bool,
) -> Result<Vec<String>> {
    let (command, existing) = build_asset_sync_command(node, dest, include_auth)?;
    run_shell_command(&command, "sync Codex assets")?;
    Ok(existing.into_iter().map(str::to_string).collect())
}

fn build_path_sync_command(
    node: &TeamNode,
    src: &Path,
    dest: &str,
    replace: bool,
) -> Result<(String, &'static str)> {
    let src_parent = src
        .parent()
        .with_context(|| format!("source path `{}` has no parent", src.display()))?;
    let src_name = src
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| format!("source path `{}` has no UTF-8 file name", src.display()))?;
    let src_kind = if src.is_dir() { "directory" } else { "file" };
    let local_tar = format!(
        "tar -C {} -cf - {}",
        shell_quote_path(src_parent),
        shell_quote(src_name)
    );
    let remote_extract = remote_path_extract_script(src_name, dest, replace);
    let command = match node.kind {
        TeamNodeKind::Local => {
            format!("{local_tar} | bash -lc {}", shell_quote(&remote_extract))
        }
        TeamNodeKind::Ssh => {
            let host = node.host.as_deref().context("ssh node needs host")?;
            format!(
                "{local_tar} | ssh {} {}",
                shell_quote(host),
                shell_quote(&remote_extract)
            )
        }
        TeamNodeKind::Docker => {
            let container = node
                .container
                .as_deref()
                .context("docker node needs container")?;
            format!(
                "{local_tar} | docker exec -i {} bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_extract)
            )
        }
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref().context("ssh-docker node needs host")?;
            let container = node
                .container
                .as_deref()
                .context("ssh-docker node needs container")?;
            let remote_command = format!(
                "docker exec -i {} bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_extract)
            );
            format!(
                "{local_tar} | ssh {} {}",
                shell_quote(host),
                shell_quote(&remote_command)
            )
        }
        TeamNodeKind::Manual => bail!("manual node path sync is not supported"),
    };
    Ok((command, src_kind))
}

fn remote_path_extract_script(src_name: &str, dest: &str, replace: bool) -> String {
    format!(
        r#"set -euo pipefail
{dest_assignment}
src_name={src_name}
replace={replace}
parent="$(dirname "$dest")"
mkdir -p "$parent"
tmp="$(mktemp -d)"
cleanup() {{
  rm -rf "$tmp"
}}
trap cleanup EXIT HUP INT TERM
tar -C "$tmp" -xf -
incoming="$tmp/$src_name"
if [ ! -e "$incoming" ]; then
  echo "sync-path: archive did not contain expected entry $src_name" >&2
  exit 18
fi
if [ -e "$dest" ]; then
  if [ "$replace" != "1" ]; then
    echo "sync-path: destination exists; rerun with --replace: $dest" >&2
    exit 17
  fi
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  backup_dir="$parent/.codex-team-handoff-backups/$stamp"
  mkdir -p "$backup_dir"
  mv "$dest" "$backup_dir/$(basename "$dest")"
fi
mv "$incoming" "$dest"
"#,
        dest_assignment = remote_path_dest_assignment(dest),
        src_name = shell_quote(src_name),
        replace = if replace { "1" } else { "0" },
    )
}

fn maybe_sync_remote_node_assets(
    team_dir: &Path,
    nodes: &[TeamNode],
    node_clients: &HashMap<String, TeamAppServerNodeClient>,
    last_sync: &mut HashMap<String, Instant>,
    interval: Duration,
) -> Result<()> {
    let now_instant = Instant::now();
    for node in nodes {
        if matches!(node.kind, TeamNodeKind::Local | TeamNodeKind::Manual) {
            continue;
        }
        if !node_clients.contains_key(&node.id) {
            continue;
        }
        if last_sync
            .get(&node.id)
            .is_some_and(|last| now_instant.duration_since(*last) < interval)
        {
            continue;
        }
        match sync_codex_assets_to_node(node, "$HOME/.codex", false) {
            Ok(paths) => {
                last_sync.insert(node.id.clone(), now_instant);
                append_event(
                    team_dir,
                    "node_assets_periodic_synced",
                    serde_json::json!({
                        "node": node.id,
                        "paths": paths,
                        "include_auth": false,
                    }),
                )?;
            }
            Err(err) => {
                last_sync.insert(node.id.clone(), now_instant);
                append_event(
                    team_dir,
                    "node_assets_periodic_sync_failed",
                    serde_json::json!({
                        "node": node.id,
                        "error": err.to_string(),
                    }),
                )?;
            }
        }
    }
    Ok(())
}

fn build_asset_sync_command<'a>(
    node: &TeamNode,
    dest: &str,
    include_auth: bool,
) -> Result<(String, Vec<&'a str>)> {
    let codex_home =
        codex_core::config::find_codex_home().context("failed to resolve CODEX_HOME")?;
    let mut includes = vec!["config.toml", "skills", "rules", "memories", ".tmp/plugins"];
    if include_auth {
        includes.push("auth.json");
    }
    let existing = includes
        .into_iter()
        .filter(|path| codex_home.join(path).exists())
        .collect::<Vec<_>>();
    if existing.is_empty() {
        bail!("no syncable Codex assets found in {}", codex_home.display());
    }
    let tar_args = existing
        .iter()
        .map(|path| shell_quote(path))
        .collect::<Vec<_>>()
        .join(" ");
    let local_tar = format!(
        "tar -C {} -cf - {}",
        shell_quote_path(&codex_home),
        tar_args
    );
    let backup_entries = existing
        .iter()
        .map(|path| shell_quote(path))
        .collect::<Vec<_>>()
        .join(" ");
    let remote_extract = format!(
        r#"set -euo pipefail
{dest_assignment}
mkdir -p "$dest"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
backup="$dest/.codex-team-backups/$stamp"
made_backup=0
for p in {backup_entries}; do
  if [ -e "$dest/$p" ]; then
    mkdir -p "$backup/$(dirname "$p")"
    cp -a "$dest/$p" "$backup/$p"
    made_backup=1
  fi
done
tar -C "$dest" -xf -
if [ "$made_backup" = "0" ]; then
  rmdir "$backup" 2>/dev/null || true
fi"#,
        dest_assignment = remote_codex_dest_assignment(dest),
        backup_entries = backup_entries
    );
    let command = match node.kind {
        TeamNodeKind::Local => {
            format!("{local_tar} | bash -lc {}", shell_quote(&remote_extract))
        }
        TeamNodeKind::Ssh => {
            let host = node.host.as_deref().context("ssh node needs host")?;
            format!(
                "{local_tar} | ssh {} {}",
                shell_quote(host),
                shell_quote(&remote_extract)
            )
        }
        TeamNodeKind::Docker => {
            let container = node
                .container
                .as_deref()
                .context("docker node needs container")?;
            format!(
                "{local_tar} | docker exec -i {} bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_extract)
            )
        }
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref().context("ssh-docker node needs host")?;
            let container = node
                .container
                .as_deref()
                .context("ssh-docker node needs container")?;
            let remote_command = format!(
                "docker exec -i {} bash -lc {}",
                shell_quote(container),
                shell_quote(&remote_extract)
            );
            format!(
                "{local_tar} | ssh {} {}",
                shell_quote(host),
                shell_quote(&remote_command)
            )
        }
        TeamNodeKind::Manual => bail!("manual node asset sync is not supported"),
    };
    Ok((command, existing))
}

fn remote_codex_dest_assignment(dest: &str) -> String {
    let trimmed = dest.trim();
    if matches!(trimmed, "$HOME/.codex" | "${HOME}/.codex" | "~/.codex") {
        "dest=\"${HOME:-/root}/.codex\"".to_string()
    } else {
        format!("dest={}", shell_quote(trimmed))
    }
}

fn remote_path_dest_assignment(dest: &str) -> String {
    let trimmed = dest.trim();
    if trimmed == "$HOME" || trimmed == "${HOME}" || trimmed == "~" {
        "dest=\"${HOME:-/root}\"".to_string()
    } else if let Some(rest) = trimmed.strip_prefix("$HOME/") {
        format!("dest=\"${{HOME:-/root}}/{}\"", rest.replace('"', "\\\""))
    } else if let Some(rest) = trimmed.strip_prefix("${HOME}/") {
        format!("dest=\"${{HOME:-/root}}/{}\"", rest.replace('"', "\\\""))
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        format!("dest=\"${{HOME:-/root}}/{}\"", rest.replace('"', "\\\""))
    } else {
        format!("dest={}", shell_quote(trimmed))
    }
}

fn run_job(root: &Path, cli: JobCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    match cli.subcommand {
        JobSubcommand::List(args) => list_jobs(&team_dir, args),
        JobSubcommand::Start(args) => start_team_job(&team_dir, args),
        JobSubcommand::Status(args) => show_job_status(&team_dir, &args.id),
        JobSubcommand::Logs(args) => show_job_logs(&team_dir, args),
        JobSubcommand::Stop(args) => stop_team_job(&team_dir, &args.id),
        JobSubcommand::Artifact(args) => add_job_artifact(&team_dir, args),
    }
}

fn run_wait(root: &Path, cli: WaitCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    match cli.subcommand {
        WaitSubcommand::Add(args) => add_team_wait(&team_dir, args),
        WaitSubcommand::List(args) => {
            print!("{}", format_waits_text_filtered(&team_dir, &args)?);
            Ok(())
        }
        WaitSubcommand::Set(args) => set_team_wait(&team_dir, args),
    }
}

fn list_jobs(team_dir: &Path, args: JobListArgs) -> Result<()> {
    print!("{}", format_jobs_text_filtered(team_dir, &args)?);
    Ok(())
}

fn format_jobs_text_filtered(team_dir: &Path, args: &JobListArgs) -> Result<String> {
    let mut jobs = load_jobs(team_dir)?;
    jobs.retain(|job| {
        if let Some(owner) = args.owner.as_deref()
            && job.owner.as_deref() != Some(owner)
        {
            return false;
        }
        if let Some(task) = args.task.as_deref()
            && job.task_id.as_deref() != Some(task)
        {
            return false;
        }
        if let Some(status) = args.status.as_ref()
            && &job.status != status
        {
            return false;
        }
        true
    });
    jobs.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    if let Some(limit) = args.limit {
        let keep_from = jobs.len().saturating_sub(limit);
        jobs = jobs.split_off(keep_from);
    }
    if jobs.is_empty() {
        return Ok("No jobs.\n".to_string());
    }
    let mut out = String::new();
    for job in jobs {
        out.push_str(&format!(
            "{:<18} {:<10} node={:<16} owner={:<12} task={:<6} pid={} cwd={} command={}\n",
            job.id,
            format!("{:?}", job.status),
            job.node,
            job.owner.as_deref().unwrap_or("-"),
            job.task_id.as_deref().unwrap_or("-"),
            job.pid.unwrap_or_default(),
            job.cwd,
            job.command
        ));
    }
    Ok(out)
}

fn format_waits_text_filtered(team_dir: &Path, args: &WaitListArgs) -> Result<String> {
    let mut waits = load_waits(team_dir)?;
    waits.retain(|wait| {
        if let Some(owner) = args.owner.as_deref()
            && wait.owner.as_deref() != Some(owner)
        {
            return false;
        }
        if let Some(task) = args.task.as_deref()
            && wait.task_id.as_deref() != Some(task)
        {
            return false;
        }
        if let Some(status) = args.status.as_ref()
            && &wait.status != status
        {
            return false;
        }
        true
    });
    waits.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    if let Some(limit) = args.limit {
        let keep_from = waits.len().saturating_sub(limit);
        waits = waits.split_off(keep_from);
    }
    if waits.is_empty() {
        return Ok("No waits.\n".to_string());
    }
    let mut out = String::new();
    for wait in waits {
        let evidence = wait.evidence.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "{:<18} {:<10} owner={:<12} task={:<6} node={:<14} evidence={:<20} title={} condition={} progress={}\n",
            wait.id,
            wait.status,
            wait.owner.as_deref().unwrap_or("-"),
            wait.task_id.as_deref().unwrap_or("-"),
            wait.node.as_deref().unwrap_or("-"),
            evidence,
            wait.title,
            wait.condition,
            wait.progress
        ));
    }
    Ok(out)
}

fn add_team_wait(team_dir: &Path, args: WaitAddArgs) -> Result<()> {
    let config = load_config(team_dir)?;
    let id = allocate_wait_id(team_dir)?;
    let owner = args
        .owner
        .or_else(|| std::env::var("CODEX_TEAM_MEMBER").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "lead".to_string());
    ensure_member_exists(&config, &owner)?;
    let task_id = args.task.filter(|value| !value.trim().is_empty());
    if let Some(task_id) = task_id.as_deref() {
        let tasks = load_tasks(team_dir)?;
        let Some(task) = tasks.iter().find(|task| task.id == task_id) else {
            bail!("task `{task_id}` does not exist");
        };
        if let Some(task_owner) = task.owner.as_deref()
            && task_owner != owner
            && owner != config.lead
        {
            bail!("task `{task_id}` is owned by `{task_owner}`, not `{owner}`");
        }
        set_task_status_if_open(
            team_dir,
            task_id,
            TaskStatus::Waiting,
            Some(&format!("Waiting on `{id}`: {}", args.title)),
        )?;
    }
    if let Some(node_id) = args.node.as_deref() {
        let mut nodes = load_nodes(team_dir)?;
        ensure_local_node(&mut nodes);
        if !nodes.iter().any(|node| node.id == node_id) {
            bail!("node `{node_id}` not found");
        }
    }
    let now = now();
    let wait = TeamWait {
        id: id.clone(),
        title: args.title,
        owner: Some(owner.clone()),
        task_id: task_id.clone(),
        node: args.node.filter(|value| !value.trim().is_empty()),
        condition: args.condition,
        status: args.status,
        progress: args.progress,
        evidence: args.evidence.filter(|value| !value.trim().is_empty()),
        created_at: now.clone(),
        updated_at: now,
    };
    fs::create_dir_all(waits_dir(team_dir))?;
    write_json_atomic(&wait_path(team_dir, &id), &wait)?;
    append_event(
        team_dir,
        "wait_registered",
        serde_json::json!({
            "wait": id,
            "owner": owner,
            "task": task_id,
            "status": wait.status.to_string(),
            "condition": wait.condition.as_str(),
            "evidence": wait.evidence.as_deref(),
        }),
    )?;
    println!("Registered wait {}", wait.id);
    Ok(())
}

fn set_team_wait(team_dir: &Path, args: WaitSetArgs) -> Result<()> {
    let mut wait = load_wait(team_dir, &args.id)?;
    let previous_status = wait.status.clone();
    if let Some(status) = args.status {
        wait.status = status;
    }
    if let Some(progress) = args.progress {
        wait.progress = progress;
    }
    if args.clear_evidence {
        wait.evidence = None;
    }
    if let Some(evidence) = args.evidence {
        wait.evidence = if evidence.trim().is_empty() {
            None
        } else {
            Some(evidence)
        };
    }
    wait.updated_at = now();
    write_json_atomic(&wait_path(team_dir, &wait.id), &wait)?;
    append_event(
        team_dir,
        "wait_updated",
        serde_json::json!({
            "wait": wait.id,
            "previous_status": previous_status.to_string(),
            "status": wait.status.to_string(),
            "owner": wait.owner.as_deref(),
            "task": wait.task_id.as_deref(),
            "evidence": wait.evidence.as_deref(),
        }),
    )?;
    handle_wait_status_change(team_dir, &wait, previous_status)?;
    println!("Updated wait {}", wait.id);
    Ok(())
}

fn handle_wait_status_change(
    team_dir: &Path,
    wait: &TeamWait,
    previous_status: TeamWaitStatus,
) -> Result<()> {
    if wait.status == previous_status {
        return Ok(());
    }
    let Some(task_id) = wait.task_id.as_deref() else {
        return Ok(());
    };
    let config = load_config(team_dir)?;
    let owner = wait.owner.as_deref().unwrap_or(config.lead.as_str());
    let evidence = wait.evidence.as_deref().unwrap_or("-");
    match &wait.status {
        TeamWaitStatus::Completed => {
            set_task_status_if_open(
                team_dir,
                task_id,
                TaskStatus::InProgress,
                Some(&format!(
                    "Wait `{}` completed. Evidence: {evidence}. Owner must inspect the result and publish the final handoff/checklist or next blocker.",
                    wait.id
                )),
            )?;
            resume_wait_owner_after_wait_status_change(team_dir, wait, task_id)?;
        }
        TeamWaitStatus::Failed | TeamWaitStatus::Cancelled | TeamWaitStatus::Blocked => {
            set_task_status_if_open(
                team_dir,
                task_id,
                TaskStatus::Blocked,
                Some(&format!(
                    "Wait `{}` ended as {}. Evidence/progress: {evidence} {}",
                    wait.id, wait.status, wait.progress
                )),
            )?;
            resume_wait_owner_after_wait_status_change(team_dir, wait, task_id)?;
        }
        TeamWaitStatus::Waiting | TeamWaitStatus::Running | TeamWaitStatus::Polling => {}
    }
    if config.members.iter().any(|member| member.name == owner) && owner != config.lead {
        set_member_status(team_dir, owner, MemberStatus::Online)?;
    }
    Ok(())
}

fn resume_wait_owner_after_wait_status_change(
    team_dir: &Path,
    wait: &TeamWait,
    task_id: &str,
) -> Result<()> {
    let config = load_config(team_dir)?;
    let Some(owner) = wait.owner.as_deref() else {
        return Ok(());
    };
    let evidence = wait.evidence.as_deref().unwrap_or("-");
    let language = config.language.unwrap_or_default();
    if owner != config.lead && config.members.iter().any(|member| member.name == owner) {
        set_member_status(team_dir, owner, MemberStatus::Online)?;
        let message = if language.is_ja() {
            format!(
                "WAIT_STATUS: task {task_id} に紐づく wait `{}` が `{}` になりました。condition=`{}` evidence=`{evidence}` progress=`{}`。結果を確認し、final handoff/checklist、次の task、または具体的 blocker を lead/all に送ってください。",
                wait.id, wait.status, wait.condition, wait.progress
            )
        } else {
            format!(
                "WAIT_STATUS: wait `{}` for task {task_id} is now `{}`. condition=`{}` evidence=`{evidence}` progress=`{}`. Inspect the result, then send lead/all the final handoff/checklist, next task, or concrete blocker.",
                wait.id, wait.status, wait.condition, wait.progress
            )
        };
        send_team_message_to_dir(team_dir, "system", owner, &message)?;
    }
    let lead_message = if language.is_ja() {
        format!(
            "WAIT_STATUS: @{owner} の task {task_id} に紐づく wait `{}` が `{}` になりました。condition=`{}` evidence=`{evidence}`。handoff/recovery のため owner を再開しました。",
            wait.id, wait.status, wait.condition
        )
    } else {
        format!(
            "WAIT_STATUS: @{owner}'s wait `{}` for task {task_id} is now `{}`. condition=`{}` evidence=`{evidence}`. Owner was resumed for handoff/recovery.",
            wait.id, wait.status, wait.condition
        )
    };
    send_team_message_to_dir(team_dir, "system", &config.lead, &lead_message)?;
    append_event(
        team_dir,
        "wait_owner_resumed",
        serde_json::json!({
            "wait": wait.id,
            "task": task_id,
            "owner": owner,
            "status": wait.status.to_string(),
            "evidence": wait.evidence.as_deref(),
        }),
    )?;
    Ok(())
}

fn start_team_job(team_dir: &Path, args: JobStartArgs) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let node_id = sanitize_id(&args.node);
    let node = nodes
        .iter()
        .find(|node| node.id == node_id)
        .with_context(|| format!("node `{}` not found", args.node))?
        .clone();
    let id = args
        .id
        .map(|id| sanitize_id(&id))
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| allocate_job_id(team_dir).unwrap_or_else(|_| "job-1".to_string()));
    if job_path(team_dir, &id).exists() {
        bail!("job `{id}` already exists");
    }
    let config = load_config(team_dir)?;
    let owner = args
        .owner
        .or_else(|| std::env::var("CODEX_TEAM_MEMBER").ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "lead".to_string());
    ensure_member_exists(&config, &owner)?;
    let task_id = args.task.filter(|value| !value.trim().is_empty());
    if let Some(task_id) = task_id.as_deref() {
        let tasks = load_tasks(team_dir)?;
        let Some(task) = tasks.iter().find(|task| task.id == task_id) else {
            bail!("task `{task_id}` does not exist");
        };
        if let Some(task_owner) = task.owner.as_deref()
            && task_owner != owner
            && owner != config.lead
        {
            bail!("task `{task_id}` is owned by `{task_owner}`, not `{owner}`");
        }
        set_task_status_if_open(
            team_dir,
            task_id,
            TaskStatus::InProgress,
            Some(&format!("Tracked by job `{id}`.")),
        )?;
    }
    let command = args
        .command
        .iter()
        .map(|part| shell_quote(part))
        .collect::<Vec<_>>()
        .join(" ");
    let cwd = args
        .cwd
        .or_else(|| node.cwd.clone())
        .unwrap_or_else(|| ".".to_string());
    let remote_base = format!("/tmp/codex-team-jobs/{id}");
    let remote_log = format!("{remote_base}/job.log");
    let remote_exit = format!("{remote_base}/exit.code");
    let created_at = now();
    let mut job = TeamJob {
        id: id.clone(),
        node: node.id.clone(),
        command: command.clone(),
        cwd: cwd.clone(),
        owner: Some(owner.clone()),
        task_id: task_id.clone(),
        status: TeamJobStatus::Running,
        pid: None,
        log_path: remote_log.clone(),
        exit_path: remote_exit.clone(),
        exit_code: None,
        note: args.note,
        artifacts: Vec::new(),
        created_at: created_at.clone(),
        updated_at: created_at,
    };
    write_json_atomic(&job_path(team_dir, &id), &job)?;
    append_event(
        team_dir,
        "job_registered_before_remote_start",
        serde_json::json!({
            "job": id,
            "node": node.id,
            "owner": owner.clone(),
            "task": task_id.clone(),
            "log": remote_log,
            "exit": remote_exit,
        }),
    )?;
    let start_script = format!(
        "mkdir -p {base} && cd {cwd} && rm -f {exit_path} && (bash -lc {command} > {log} 2>&1; printf '%s' \"$?\" > {exit_path}) & echo $!",
        base = shell_quote(&remote_base),
        cwd = shell_quote(&cwd),
        exit_path = shell_quote(&remote_exit),
        command = shell_quote(&command),
        log = shell_quote(&remote_log),
    );
    let pid = run_node_command_capture(&node, &start_script)
        .context("start team job")?
        .lines()
        .last()
        .unwrap_or_default()
        .trim()
        .to_string();
    job.pid = if pid.is_empty() { None } else { Some(pid) };
    job.updated_at = now();
    write_json_atomic(&job_path(team_dir, &id), &job)?;
    append_event(
        team_dir,
        "job_started",
        serde_json::json!({
            "job": id,
            "node": node.id,
            "owner": owner,
            "task": task_id,
            "pid": job.pid,
        }),
    )?;
    if job.owner.as_deref() == Some(config.lead.as_str()) && job.task_id.is_none() {
        append_event(
            team_dir,
            "lead_job_without_department_task",
            serde_json::json!({
                "job": job.id,
                "node": job.node,
                "reason": "lead-started job is not tied to a department task",
                "recommendation": "prefer --owner <department> --task <id> so execution evidence is owned and auditable",
            }),
        )?;
    }
    println!(
        "Started job {} on node {} pid={}",
        job.id,
        job.node,
        job.pid.as_deref().unwrap_or("")
    );
    Ok(())
}

fn show_job_status(team_dir: &Path, id: &str) -> Result<()> {
    print!("{}", format_job_status_text(team_dir, id)?);
    Ok(())
}

fn show_job_logs(team_dir: &Path, args: JobLogsArgs) -> Result<()> {
    print!("{}", job_logs_text(team_dir, &args.id, args.tail)?);
    Ok(())
}

fn format_job_status_text(team_dir: &Path, id: &str) -> Result<String> {
    let job = refresh_job_status(team_dir, id)?;
    let mut out = String::new();
    out.push_str(&format!(
        "{} status={:?} node={} pid={} exit={}\n",
        job.id,
        job.status,
        job.node,
        job.pid.as_deref().unwrap_or(""),
        job.exit_code
            .map(|code| code.to_string())
            .unwrap_or_default()
    ));
    out.push_str(&format!("cwd={}\n", job.cwd));
    out.push_str(&format!("log={}\n", job.log_path));
    out.push_str(&format!("command={}\n", job.command));
    if !job.artifacts.is_empty() {
        out.push_str("artifacts:\n");
        for artifact in job.artifacts {
            out.push_str(&format!("  {}  {}\n", artifact.path, artifact.note));
        }
    }
    Ok(out)
}

fn job_logs_text(team_dir: &Path, id: &str, tail: Option<usize>) -> Result<String> {
    let job = load_job(team_dir, id)?;
    let node = load_node_for_job(team_dir, &job)?;
    let script = match tail {
        Some(lines) => format!("tail -n {} {}", lines, shell_quote(&job.log_path)),
        None => format!("cat {}", shell_quote(&job.log_path)),
    };
    run_node_command_capture(&node, &script)
}

fn stop_team_job(team_dir: &Path, id: &str) -> Result<()> {
    let mut job = load_job(team_dir, id)?;
    let node = load_node_for_job(team_dir, &job)?;
    if let Some(pid) = job.pid.as_deref() {
        let script = format!("kill {} >/dev/null 2>&1 || true", shell_quote(pid));
        let _ = run_node_command_capture(&node, &script);
    }
    job.status = TeamJobStatus::Stopped;
    job.updated_at = now();
    write_json_atomic(&job_path(team_dir, &job.id), &job)?;
    append_event(
        team_dir,
        "job_stopped",
        serde_json::json!({ "job": job.id }),
    )?;
    println!("Stopped job {}", job.id);
    Ok(())
}

fn add_job_artifact(team_dir: &Path, args: JobArtifactArgs) -> Result<()> {
    let mut job = load_job(team_dir, &args.id)?;
    job.artifacts.push(TeamArtifact {
        path: args.path,
        note: args.note,
        created_at: now(),
    });
    job.updated_at = now();
    write_json_atomic(&job_path(team_dir, &job.id), &job)?;
    append_event(
        team_dir,
        "job_artifact_added",
        serde_json::json!({ "job": job.id, "artifacts": job.artifacts }),
    )?;
    handle_job_artifact_handoff(team_dir, &job)?;
    println!("Registered artifact for job {}", job.id);
    Ok(())
}

fn handle_job_artifact_handoff(team_dir: &Path, job: &TeamJob) -> Result<()> {
    if !matches!(job.status, TeamJobStatus::Completed) {
        return Ok(());
    }
    let Some(task_id) = job.task_id.as_deref() else {
        return Ok(());
    };
    if !job_owner_matches_task_owner(team_dir, job, task_id)? {
        record_auxiliary_job_status(team_dir, job, task_id)?;
        return Ok(());
    }
    let artifact_summary = if job.artifacts.is_empty() {
        "none".to_string()
    } else {
        job.artifacts
            .iter()
            .map(|artifact| {
                if artifact.note.trim().is_empty() {
                    artifact.path.clone()
                } else {
                    format!("{} ({})", artifact.path, artifact.note)
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let changed = set_task_status_if_open(
        team_dir,
        task_id,
        TaskStatus::InProgress,
        Some(&format!(
            "Job `{}` has registered artifact(s): {artifact_summary}. Owner must inspect them and publish the task's final report/json/manifest/checklist or a concrete blocker before review.",
            job.id
        )),
    )?;
    if changed {
        resume_job_owner_after_job_status_change(team_dir, job, task_id, TaskStatus::InProgress)?;
        append_event(
            team_dir,
            "job_artifact_requires_owner_handoff",
            serde_json::json!({
                "job": job.id,
                "task": task_id,
                "owner": job.owner,
                "artifacts": job.artifacts,
                "recommendation": "artifact registration revived the owner task for formal handoff/checklist or blocker",
            }),
        )?;
    }
    Ok(())
}

fn refresh_job_status(team_dir: &Path, id: &str) -> Result<TeamJob> {
    let mut job = load_job(team_dir, id)?;
    let previous_status = job.status.clone();
    let node = load_node_for_job(team_dir, &job)?;
    let script = format!(
        "if [ -f {exit_path} ]; then cat {exit_path}; elif kill -0 {pid} >/dev/null 2>&1; then echo RUNNING; else echo UNKNOWN; fi",
        exit_path = shell_quote(&job.exit_path),
        pid = shell_quote(job.pid.as_deref().unwrap_or("")),
    );
    let status = run_node_command_capture(&node, &script)
        .unwrap_or_else(|_| "UNKNOWN".to_string())
        .trim()
        .to_string();
    if status == "RUNNING" {
        job.status = TeamJobStatus::Running;
    } else if let Ok(code) = status.parse::<i32>() {
        job.exit_code = Some(code);
        job.status = if code == 0 {
            TeamJobStatus::Completed
        } else {
            TeamJobStatus::Failed
        };
    } else if !matches!(job.status, TeamJobStatus::Stopped) {
        job.status = TeamJobStatus::Unknown;
    }
    job.updated_at = now();
    write_json_atomic(&job_path(team_dir, &job.id), &job)?;
    if job.status != previous_status {
        if !claim_job_status_notification(team_dir, &job.id, &job.status)? {
            return Ok(job);
        }
        append_event(
            team_dir,
            match job.status {
                TeamJobStatus::Completed => "job_completed",
                TeamJobStatus::Failed => "job_failed",
                TeamJobStatus::Stopped => "job_stopped",
                TeamJobStatus::Unknown => "job_unknown",
                TeamJobStatus::Running => "job_running",
            },
            serde_json::json!({
                "job": job.id,
                "node": job.node,
                "owner": job.owner,
                "task": job.task_id,
                "exit_code": job.exit_code,
                "artifacts": job.artifacts,
            }),
        )?;
        if let Some(task_id) = job.task_id.as_deref() {
            if !job_owner_matches_task_owner(team_dir, &job, task_id)? {
                record_auxiliary_job_status(team_dir, &job, task_id)?;
            } else {
                match job.status {
                    TeamJobStatus::Completed => {
                        if job.artifacts.is_empty() {
                            set_task_status_if_open(
                                team_dir,
                                task_id,
                                TaskStatus::InProgress,
                                Some(&format!(
                                    "Job `{}` completed without registered artifacts; owner must continue the task and publish final artifacts/checklist or a blocker before review.",
                                    job.id
                                )),
                            )?;
                            resume_job_owner_after_job_status_change(
                                team_dir,
                                &job,
                                task_id,
                                TaskStatus::InProgress,
                            )?;
                            append_event(
                                team_dir,
                                "job_completed_without_artifacts",
                                serde_json::json!({
                                    "job": job.id,
                                    "task": task_id,
                                    "owner": job.owner,
                                    "recommendation": "if this was a read-only/probe job, do not register fake artifacts; continue the task and publish the final handoff/checklist or a blocker",
                                }),
                            )?;
                        } else {
                            set_task_status_if_open(
                                team_dir,
                                task_id,
                                TaskStatus::InProgress,
                                Some(&format!(
                                    "Job `{}` completed with registered artifacts; owner must inspect them and publish the task's final report/json/manifest/checklist or a blocker before review.",
                                    job.id
                                )),
                            )?;
                            resume_job_owner_after_job_status_change(
                                team_dir,
                                &job,
                                task_id,
                                TaskStatus::InProgress,
                            )?;
                            append_event(
                                team_dir,
                                "job_completed_requires_owner_handoff",
                                serde_json::json!({
                                    "job": job.id,
                                    "task": task_id,
                                    "owner": job.owner,
                                    "artifacts": job.artifacts,
                                    "recommendation": "treat job artifacts as intermediate evidence until the owner publishes a formal final handoff/checklist or blocker",
                                }),
                            )?;
                        }
                    }
                    TeamJobStatus::Failed | TeamJobStatus::Unknown => {
                        set_task_status_if_open(
                            team_dir,
                            task_id,
                            TaskStatus::Blocked,
                            Some(&format!(
                                "Job `{}` ended with status {:?}; inspect {}.",
                                job.id, job.status, job.log_path
                            )),
                        )?;
                        resume_job_owner_after_job_status_change(
                            team_dir,
                            &job,
                            task_id,
                            TaskStatus::Blocked,
                        )?;
                    }
                    TeamJobStatus::Running | TeamJobStatus::Stopped => {}
                }
            }
        }
    }
    Ok(job)
}

fn claim_job_status_notification(
    team_dir: &Path,
    job_id: &str,
    status: &TeamJobStatus,
) -> Result<bool> {
    let status = match status {
        TeamJobStatus::Completed => "completed",
        TeamJobStatus::Failed => "failed",
        TeamJobStatus::Stopped => "stopped",
        TeamJobStatus::Unknown => "unknown",
        TeamJobStatus::Running => return Ok(true),
    };
    let dir = team_dir.join("job_status_notifications");
    fs::create_dir_all(&dir)?;
    let marker = dir.join(format!("{}.{}", sanitize_id(job_id), status));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(mut file) => {
            writeln!(file, "{}", now())?;
            Ok(true)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(err) => Err(err).with_context(|| format!("failed to create {}", marker.display())),
    }
}

fn job_owner_matches_task_owner(team_dir: &Path, job: &TeamJob, task_id: &str) -> Result<bool> {
    let Some(job_owner) = job.owner.as_deref() else {
        return Ok(false);
    };
    let tasks = load_tasks(team_dir)?;
    Ok(tasks
        .iter()
        .find(|task| task.id == task_id)
        .and_then(|task| task.owner.as_deref())
        .is_some_and(|task_owner| task_owner == job_owner))
}

fn record_auxiliary_job_status(team_dir: &Path, job: &TeamJob, task_id: &str) -> Result<()> {
    let task_owner = load_tasks(team_dir)?
        .into_iter()
        .find(|task| task.id == task_id)
        .and_then(|task| task.owner);
    append_event(
        team_dir,
        "auxiliary_job_status_no_task_update",
        serde_json::json!({
            "job": job.id,
            "task": task_id,
            "job_owner": job.owner,
            "task_owner": task_owner,
            "job_status": format!("{:?}", job.status),
            "exit_code": job.exit_code,
            "log_path": job.log_path,
            "recommendation": "job owner differs from task owner, so the task status/result was not modified automatically",
        }),
    )?;
    let config = load_config(team_dir)?;
    let message = format!(
        "AUX_JOB_STATUS: job `{job}` for task {task} ended with status {status:?} exit={exit}, but the job owner ({job_owner}) differs from the task owner ({task_owner}). I did not modify the task status/result automatically. Treat `{log}` as auxiliary evidence; lead/task owner should decide the next handoff, blocker, or clearance.",
        job = &job.id,
        task = task_id,
        status = &job.status,
        exit = job
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "-".to_string()),
        job_owner = job.owner.as_deref().unwrap_or("-"),
        task_owner = task_owner.as_deref().unwrap_or("-"),
        log = &job.log_path,
    );
    send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
    if let Some(owner) = task_owner.as_deref()
        && owner != config.lead
        && config.members.iter().any(|member| member.name == owner)
    {
        send_team_message_to_dir(team_dir, "system", owner, &message)?;
    }
    Ok(())
}

fn resume_job_owner_after_job_status_change(
    team_dir: &Path,
    job: &TeamJob,
    task_id: &str,
    task_status: TaskStatus,
) -> Result<()> {
    let config = load_config(team_dir)?;
    let Some(owner) = job.owner.as_deref() else {
        return Ok(());
    };
    if owner == config.lead {
        return Ok(());
    }
    if !config.members.iter().any(|member| member.name == owner) {
        return Ok(());
    }
    set_member_status(team_dir, owner, MemberStatus::Online)?;
    let status_text = task_status.to_string();
    let language = config.language.unwrap_or_default();
    let followup_guidance = job_status_followup_guidance(job, language);
    let exit = job
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "-".to_string());
    let owner_message = if language.is_ja() {
        format!(
            "JOB_STATUS: あなたの task {task} に紐づく job `{job}` が status {job_status:?} exit={exit} で終了しました。task は現在 `{task_status}` です。`{log}` を確認してください。{followup_guidance}",
            job = &job.id,
            task = task_id,
            job_status = &job.status,
            task_status = &status_text,
            log = &job.log_path,
        )
    } else {
        format!(
            "JOB_STATUS: job `{job}` for your task {task} ended with status {job_status:?} exit={exit}. The task is now `{task_status}`. Inspect `{log}`. {followup_guidance}",
            job = &job.id,
            task = task_id,
            job_status = &job.status,
            task_status = &status_text,
            log = &job.log_path,
        )
    };
    send_team_message_to_dir(team_dir, "system", owner, &owner_message)?;
    let lead_message = if language.is_ja() {
        format!(
            "JOB_STATUS: @{owner} の task {task} に紐づく job `{job}` が status {job_status:?} exit={exit} で終了しました。task は現在 `{task_status}` で、handoff/recovery のため @{owner} を再開しました。",
            job = &job.id,
            task = task_id,
            job_status = &job.status,
            task_status = &status_text,
        )
    } else {
        format!(
            "JOB_STATUS: @{owner}'s job `{job}` for task {task} ended with status {job_status:?} exit={exit}; task is now `{task_status}` and @{owner} was resumed for handoff/recovery.",
            job = &job.id,
            task = task_id,
            job_status = &job.status,
            task_status = &status_text,
        )
    };
    send_team_message_to_dir(team_dir, "system", &config.lead, &lead_message)?;
    append_event(
        team_dir,
        "job_owner_resumed",
        serde_json::json!({
            "job": &job.id,
            "task": task_id,
            "owner": owner,
            "task_status": &status_text,
            "job_status": format!("{:?}", job.status),
            "exit_code": job.exit_code,
        }),
    )?;
    Ok(())
}

fn job_status_followup_guidance(job: &TeamJob, language: TeamPromptLanguage) -> &'static str {
    match job.status {
        TeamJobStatus::Completed if job.artifacts.is_empty() && language.is_ja() => {
            "この job は成果物を登録していません。読み取り専用の調査/検証 job だった場合でも、ここで止まらず owner task を継続し、task の本当の最終 report/json/manifest/checklist を書くか、具体的 blocker を記録してから lead/all に TEAM_COMPLETION_CHECKLIST 付きで通知してください。"
        }
        TeamJobStatus::Completed if job.artifacts.is_empty() => {
            "This job registered no artifacts. If it was only a read-only/probe/verification job, do not register fake artifacts and do not stop here; continue the owner task, write the task's real final report/json/manifest/checklist or mark a concrete blocker, then notify lead/all with TEAM_COMPLETION_CHECKLIST."
        }
        TeamJobStatus::Completed if language.is_ja() => {
            "登録済み job 成果物だけでは task 完了扱いにしません。成果物を確認し、owner task を継続して必要な final report/json/manifest/checklist または具体的 blocker を出し、成果物 path の検証を引用し、review 前に lead/all へ TEAM_COMPLETION_CHECKLIST 付きで通知してください。"
        }
        TeamJobStatus::Completed => {
            "Registered job artifacts are not sufficient by themselves. Inspect them, then continue the owner task and publish the required final report/json/manifest/checklist or a concrete blocker, cite verification for the artifact paths, and notify lead/all with TEAM_COMPLETION_CHECKLIST before review."
        }
        TeamJobStatus::Failed | TeamJobStatus::Unknown if language.is_ja() => {
            "確認が終わるまでは blocker として扱ってください。log path を保持し、失敗原因を診断し、修正して evidence 付きで再実行するか、task を具体的な次アクション付きで blocked にしてください。"
        }
        TeamJobStatus::Failed | TeamJobStatus::Unknown => {
            "Treat this as a blocker until inspected: preserve the log path, diagnose the failure, and either repair/rerun with evidence or mark the task blocked with exact next action."
        }
        TeamJobStatus::Running | TeamJobStatus::Stopped if language.is_ja() => {
            "現在の job 状態と次の checkpoint を lead に報告してください。"
        }
        TeamJobStatus::Running | TeamJobStatus::Stopped => {
            "Report the current job state and next checkpoint to lead."
        }
    }
}

fn add_team_node(team_dir: &Path, args: NodeAddArgs) -> Result<()> {
    let id = sanitize_id(&args.id);
    if id.is_empty() {
        bail!("invalid node id `{}`", args.id);
    }
    if id == "local" && !matches!(args.kind, TeamNodeKind::Local | TeamNodeKind::Manual) {
        bail!("node `local` must use kind local/manual");
    }
    if matches!(args.kind, TeamNodeKind::Manual | TeamNodeKind::Local) && args.url.is_none() {
        bail!("node `{id}` needs --url unless it is managed by the current team run");
    }
    if matches!(args.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker) {
        let container = args
            .container
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .with_context(|| format!("node `{id}` needs --container"))?;
        if matches!(args.kind, TeamNodeKind::SshDocker) && args.host.is_none() {
            bail!("node `{id}` needs --host for ssh-docker");
        }
        if !docker_container_exists(args.host.as_deref(), container) {
            bail!("node `{id}` container `{container}` does not exist or is not inspectable");
        }
    }
    let needs_container_department =
        matches!(args.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker);
    let mut nodes = load_nodes(team_dir)?;
    let now = now();
    if needs_container_department
        && let Some(existing_idx) = nodes.iter().position(|existing| {
            existing.id != id
                && same_container_node_target(
                    existing,
                    &args.kind,
                    args.host.as_deref(),
                    args.container.as_deref(),
                )
        })
    {
        let existing_id = nodes[existing_idx].id.clone();
        let previous_url = nodes[existing_idx].url.clone();
        let previous_cwd = nodes[existing_idx].cwd.clone();
        nodes[existing_idx].kind = args.kind;
        nodes[existing_idx].url = args.url.or(previous_url);
        nodes[existing_idx].host = args.host;
        nodes[existing_idx].container = args.container;
        nodes[existing_idx].cwd = args.cwd.or(previous_cwd);
        nodes[existing_idx].note = args.note;
        nodes[existing_idx].updated_at = now;
        nodes.sort_by(|a, b| a.id.cmp(&b.id));
        write_nodes(team_dir, &nodes)?;
        touch_config(team_dir)?;
        append_event(
            team_dir,
            "node_duplicate_merged",
            serde_json::json!({
                "reported": id,
                "existing": existing_id,
                "reason": "same container target",
            }),
        )?;
        ensure_container_node_departments(team_dir)?;
        println!("Registered node {existing_id}");
        return Ok(());
    }
    let node = TeamNode {
        id: id.clone(),
        kind: args.kind,
        url: args.url,
        host: args.host,
        container: args.container,
        cwd: args.cwd,
        status: TeamNodeStatus::Pending,
        note: args.note,
        created_at: now.clone(),
        updated_at: now,
    };
    match nodes.iter_mut().find(|existing| existing.id == id) {
        Some(existing) => {
            let created_at = existing.created_at.clone();
            *existing = node;
            existing.created_at = created_at;
        }
        None => nodes.push(node),
    }
    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    write_nodes(team_dir, &nodes)?;
    touch_config(team_dir)?;
    append_event(team_dir, "node_added", serde_json::json!({ "node": id }))?;
    if needs_container_department {
        ensure_container_node_departments(team_dir)?;
    }
    println!("Registered node {id}");
    Ok(())
}

fn same_container_node_target(
    node: &TeamNode,
    kind: &TeamNodeKind,
    host: Option<&str>,
    container: Option<&str>,
) -> bool {
    if &node.kind != kind {
        return false;
    }
    let Some(container) = container.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if node.container.as_deref().map(str::trim) != Some(container) {
        return false;
    }
    if matches!(kind, TeamNodeKind::SshDocker) {
        let Some(host) = host.map(str::trim).filter(|value| !value.is_empty()) else {
            return false;
        };
        node.host.as_deref().map(str::trim) == Some(host)
    } else {
        true
    }
}

fn remove_team_node(team_dir: &Path, args: NodeRemoveArgs) -> Result<()> {
    let id = sanitize_id(&args.id);
    if id == "local" {
        bail!("node `local` cannot be removed");
    }
    let config = load_config(team_dir)?;
    if !args.force
        && config
            .members
            .iter()
            .any(|member| member.node.as_deref() == Some(id.as_str()))
    {
        bail!("node `{id}` is assigned to a member; pass --force to remove it");
    }
    let mut nodes = load_nodes(team_dir)?;
    let before = nodes.len();
    nodes.retain(|node| node.id != id);
    if nodes.len() == before {
        bail!("node `{id}` not found");
    }
    write_nodes(team_dir, &nodes)?;
    deactivate_removed_node_members(team_dir, &id)?;
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "node_removed",
        serde_json::json!({ "node": id, "forced": args.force }),
    )?;
    println!("Removed node {id}");
    Ok(())
}

fn deactivate_removed_node_members(team_dir: &Path, node_id: &str) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let now = now();
    let members = config
        .members
        .iter_mut()
        .filter(|member| member.node.as_deref() == Some(node_id))
        .map(|member| {
            member.status = MemberStatus::Standby;
            member.name.clone()
        })
        .collect::<Vec<_>>();
    if members.is_empty() {
        return Ok(());
    }
    config.updated_at = now.clone();
    write_json_atomic(&team_dir.join("config.json"), &config)?;

    let mut tasks = load_tasks(team_dir)?;
    let mut changed_tasks = Vec::new();
    for task in &mut tasks {
        if task
            .owner
            .as_deref()
            .map(|owner| members.iter().any(|member| member == owner))
            .unwrap_or(false)
            && !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
        {
            task.status = TaskStatus::Cancelled;
            task.updated_at = now.clone();
            task.result = Some(format!("Cancelled because node `{node_id}` was removed."));
            changed_tasks.push(task.id.clone());
        }
    }
    for task in &tasks {
        write_json_atomic(&task_path(team_dir, &task.id), task)?;
    }
    append_event(
        team_dir,
        "node_members_deactivated",
        serde_json::json!({
            "node": node_id,
            "members": members,
            "cancelled_tasks": changed_tasks,
        }),
    )?;
    Ok(())
}

fn assign_unowned_tasks_round_robin(team_dir: &Path) -> Result<()> {
    auto_promote_dependency_waits(team_dir)?;
    let config = load_config(team_dir)?;
    let workers: Vec<&TeamMember> = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .collect();
    if workers.is_empty() {
        return Ok(());
    }

    let mut tasks = load_tasks(team_dir)?;
    let snapshot = tasks.clone();
    let mut changed = false;
    let mut worker_idx = 0usize;
    for task in &mut tasks {
        if task.owner.is_none()
            && matches!(task.status, TaskStatus::Pending)
            && task_is_ready(task, &snapshot)
        {
            let member = workers[worker_idx % workers.len()];
            task.owner = Some(member.name.clone());
            task.updated_at = now();
            worker_idx += 1;
            changed = true;
        }
    }

    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        append_event(
            team_dir,
            "tasks_assigned",
            serde_json::json!({ "strategy": "round_robin" }),
        )?;
    }
    Ok(())
}

fn set_member_status(team_dir: &Path, member_name: &str, status: MemberStatus) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let Some(member) = config
        .members
        .iter_mut()
        .find(|member| member.name == member_name)
    else {
        bail!(
            "member `{member_name}` does not exist in team `{}`",
            config.id
        );
    };
    member.status = status;
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)
}

fn normalize_stale_running_members_without_active_turns(
    team_dir: &Path,
    active: &HashMap<String, AppServerMemberRun>,
) -> Result<Vec<String>> {
    let mut config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let now = now();
    let mut normalized = Vec::new();
    for member in &mut config.members {
        if member.role == "lead" || !matches!(member.status, MemberStatus::Running) {
            continue;
        }
        let has_active_turn = active.get(&member.name).is_some_and(|run| !run.completed);
        let has_open_task = tasks
            .iter()
            .any(|task| task.owner.as_deref() == Some(member.name.as_str()) && task_is_open(task));
        if has_active_turn || has_open_task {
            continue;
        }
        member.status = MemberStatus::Standby;
        normalized.push(member.name.clone());
    }
    if normalized.is_empty() {
        return Ok(normalized);
    }
    config.updated_at = now;
    write_json_atomic(&team_dir.join("config.json"), &config)?;
    append_event(
        team_dir,
        "member_status_normalized",
        serde_json::json!({
            "members": normalized,
            "from": "running",
            "to": "standby",
            "reason": "no active app-server turn or open owned task after runtime attach",
        }),
    )?;
    Ok(normalized)
}

fn member_status(team_dir: &Path, member_name: &str) -> Result<Option<MemberStatus>> {
    let config = load_config(team_dir)?;
    Ok(config
        .members
        .iter()
        .find(|member| member.name == member_name)
        .map(|member| member.status.clone()))
}

fn set_member_workspace(team_dir: &Path, member_name: &str, workspace_path: &Path) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let Some(member) = config
        .members
        .iter_mut()
        .find(|member| member.name == member_name)
    else {
        bail!(
            "member `{member_name}` does not exist in team `{}`",
            config.id
        );
    };
    member.workspace_path = Some(workspace_path.display().to_string());
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)
}

fn set_member_thread(team_dir: &Path, member_name: &str, thread_id: &str) -> Result<()> {
    let mut config = load_config(team_dir)?;
    let Some(member) = config
        .members
        .iter_mut()
        .find(|member| member.name == member_name)
    else {
        bail!(
            "member `{member_name}` does not exist in team `{}`",
            config.id
        );
    };
    member.thread_id = Some(thread_id.to_string());
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)
}

fn prepare_member_worktree(
    team_dir: &Path,
    base_cwd: &Path,
    team_id: &str,
    member: &TeamMember,
) -> Result<PathBuf> {
    let worktrees_dir = team_dir.join("worktrees");
    fs::create_dir_all(&worktrees_dir)?;
    let worktree_path = worktrees_dir.join(&member.name);
    if worktree_path.exists() {
        set_member_workspace(team_dir, &member.name, &worktree_path)?;
        return Ok(worktree_path);
    }

    let branch = format!(
        "codex-team/{}/{}",
        sanitize_id(team_id),
        sanitize_id(&member.name)
    );
    let status = Command::new("git")
        .arg("-C")
        .arg(base_cwd)
        .arg("worktree")
        .arg("add")
        .arg("-b")
        .arg(&branch)
        .arg(&worktree_path)
        .arg("HEAD")
        .status()
        .with_context(|| format!("create git worktree for `{}`", member.name))?;
    if !status.success() {
        bail!("failed to create git worktree for `{}`", member.name);
    }
    set_member_workspace(team_dir, &member.name, &worktree_path)?;
    append_event(
        team_dir,
        "member_worktree_created",
        serde_json::json!({
            "member": member.name,
            "branch": branch,
            "path": worktree_path,
        }),
    )?;
    Ok(worktree_path)
}

fn mark_member_tasks(team_dir: &Path, member_name: &str, status: TaskStatus) -> Result<()> {
    auto_promote_dependency_waits(team_dir)?;
    let mut changed = false;
    let mut tasks = load_tasks(team_dir)?;
    let snapshot = tasks.clone();
    for task in &mut tasks {
        if task.owner.as_deref() == Some(member_name)
            && matches!(task.status, TaskStatus::Pending | TaskStatus::Ready)
            && task_dependencies_completed(task, &snapshot)
        {
            task.status = status;
            task.updated_at = now();
            changed = true;
        }
    }
    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
    }
    Ok(())
}

fn set_task_status_if_open(
    team_dir: &Path,
    task_id: &str,
    status: TaskStatus,
    result: Option<&str>,
) -> Result<bool> {
    let mut changed = false;
    let mut completed_tasks = Vec::new();
    let mut rejected_completions = Vec::new();
    let mut tasks = load_tasks(team_dir)?;
    for task in &mut tasks {
        if task.id == task_id
            && !matches!(
                task.status,
                TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
            )
        {
            if status == TaskStatus::Completed
                && let Some(issue) = task_completion_blocker(team_dir, task)?
            {
                task.status = TaskStatus::Blocked;
                task.result = Some(append_result_note(
                    result.or(task.result.as_deref()),
                    &format!("Completion rejected: {issue}"),
                ));
                rejected_completions.push((task.id.clone(), task.owner.clone(), issue));
            } else {
                task.status = status;
                if let Some(result) = result {
                    task.result = Some(result.to_string());
                }
                if status == TaskStatus::Completed {
                    completed_tasks.push(task.clone());
                }
            }
            task.updated_at = now();
            changed = true;
        }
    }
    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        notify_rejected_task_completions(team_dir, &rejected_completions)?;
        notify_completed_task_freezes(team_dir, &completed_tasks)?;
        auto_promote_dependency_waits(team_dir)?;
    }
    Ok(changed)
}

fn complete_member_tasks_if_active(team_dir: &Path, member_name: &str) -> Result<()> {
    let mut changed = false;
    let mut completed_tasks = Vec::new();
    let mut rejected_completions = Vec::new();
    let mut tasks = load_tasks(team_dir)?;
    for task in &mut tasks {
        if task.owner.as_deref() == Some(member_name)
            && matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Ready
                    | TaskStatus::InProgress
                    | TaskStatus::Review
            )
        {
            if let Some(issue) = task_completion_blocker(team_dir, task)? {
                task.status = TaskStatus::Blocked;
                task.result = Some(append_result_note(
                    task.result.as_deref(),
                    &format!("Completion rejected: {issue}"),
                ));
                rejected_completions.push((task.id.clone(), task.owner.clone(), issue));
            } else {
                task.status = TaskStatus::Completed;
                if task.result.is_none() {
                    task.result = Some("Worker exited successfully.".to_string());
                }
                completed_tasks.push(task.clone());
            }
            task.updated_at = now();
            changed = true;
        }
    }
    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        notify_rejected_task_completions(team_dir, &rejected_completions)?;
        notify_completed_task_freezes(team_dir, &completed_tasks)?;
        auto_promote_dependency_waits(team_dir)?;
    }
    Ok(())
}

fn task_completion_missing_required_local_outputs(
    team_dir: &Path,
    task: &TeamTask,
) -> Result<Option<String>> {
    let paths = task_required_local_output_paths(team_dir, task)?;
    if paths.is_empty() {
        return Ok(None);
    }
    let owner_has_completion_checklist_message = task
        .owner
        .as_deref()
        .map(|owner| owner_recent_completion_checklist_message(team_dir, owner))
        .transpose()?
        .unwrap_or(false);
    let mut issues = Vec::new();
    for path in paths {
        if let Some(issue) =
            inspect_local_handoff_path(&path, owner_has_completion_checklist_message)?
        {
            issues.push(format!("{}: {}", path.display(), issue));
        }
    }
    if issues.is_empty() {
        Ok(None)
    } else {
        Ok(Some(format!(
            "required local output package is incomplete ({})",
            issues.join("; ")
        )))
    }
}

fn task_completion_blocker(team_dir: &Path, task: &TeamTask) -> Result<Option<String>> {
    let open_waits = load_waits(team_dir)?
        .into_iter()
        .filter(|wait| wait.task_id.as_deref() == Some(task.id.as_str()))
        .filter(|wait| wait.status.is_open())
        .map(|wait| {
            format!(
                "{} status={} condition={}",
                wait.id, wait.status, wait.condition
            )
        })
        .collect::<Vec<_>>();
    if !open_waits.is_empty() {
        return Ok(Some(format!(
            "task has open wait item(s): {}",
            open_waits.join("; ")
        )));
    }
    task_completion_missing_required_local_outputs(team_dir, task)
}

fn task_required_local_output_paths(team_dir: &Path, task: &TeamTask) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for ownership in load_ownerships(team_dir)?
        .into_iter()
        .filter(|ownership| task.owner.as_deref() == Some(ownership.owner.as_str()))
        .filter(|ownership| ownership_mentions_task(ownership, task))
        .filter(|ownership| ownership_path_is_probably_local(team_dir, &ownership.path))
    {
        paths.push(PathBuf::from(ownership.path));
    }

    for path in extract_probable_local_paths_from_task_text(team_dir, task) {
        paths.push(path);
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn task_required_declared_non_local_output_paths(
    team_dir: &Path,
    task: &TeamTask,
) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for ownership in load_ownerships(team_dir)?
        .into_iter()
        .filter(|ownership| task.owner.as_deref() == Some(ownership.owner.as_str()))
        .filter(|ownership| ownership_mentions_task(ownership, task))
        .filter(|ownership| !ownership_path_is_probably_local(team_dir, &ownership.path))
        .filter(|ownership| path_looks_like_task_handoff_output(&ownership.path))
    {
        paths.push(ownership.path);
    }

    let text = format!(
        "{} {}",
        task.description,
        task.result.as_deref().unwrap_or("")
    );
    for path in extract_absolute_paths_from_text(&text) {
        if !ownership_path_is_probably_local(team_dir, &path)
            && path_looks_like_task_handoff_output(&path)
        {
            paths.push(path);
        }
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn path_looks_like_task_handoff_output(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [
        "/audit/cycle",
        "/method_schema/cycle",
        "/runtime/cycle",
        "/provenance/cycle",
        "schema_handoff_validation",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn extract_probable_local_paths_from_task_text(team_dir: &Path, task: &TeamTask) -> Vec<PathBuf> {
    let text = format!(
        "{} {}",
        task.description,
        task.result.as_deref().unwrap_or("")
    );
    text.split_whitespace()
        .filter_map(clean_embedded_path_token)
        .filter(|raw| ownership_path_is_probably_local(team_dir, raw))
        .map(PathBuf::from)
        .collect()
}

fn clean_embedded_path_token(token: &str) -> Option<&str> {
    let trimmed = token.trim_matches(|ch: char| {
        matches!(
            ch,
            '`' | '\'' | '"' | ',' | '.' | ';' | ':' | ')' | '(' | '[' | ']' | '{' | '}'
        )
    });
    if trimmed.starts_with("/home/") || trimmed.starts_with("/tmp/") {
        Some(trimmed)
    } else {
        None
    }
}

fn extract_absolute_paths_from_text(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(clean_embedded_absolute_path_token)
        .collect()
}

fn clean_embedded_absolute_path_token(token: &str) -> Option<String> {
    let trimmed = token.trim_matches(|ch: char| {
        matches!(
            ch,
            '`' | '\'' | '"' | ',' | '.' | ';' | ':' | ')' | '(' | '[' | ']' | '{' | '}'
        )
    });
    let starts_with_absolute_root = [
        "/home/",
        "/tmp/",
        "/workspace/",
        "/data/",
        "/data2/",
        "/mnt/",
        "/opt/",
        "/root/",
    ]
    .iter()
    .any(|prefix| trimmed.starts_with(prefix));
    if starts_with_absolute_root {
        Some(trimmed.to_string())
    } else {
        None
    }
}

fn notify_rejected_task_completions(
    team_dir: &Path,
    rejected: &[(String, Option<String>, String)],
) -> Result<()> {
    if rejected.is_empty() {
        return Ok(());
    }
    let config = load_config(team_dir)?;
    for (task_id, owner, issue) in rejected {
        let owner_label = owner
            .as_deref()
            .map(|owner| format!("@{owner}"))
            .unwrap_or_else(|| "unassigned owner".to_string());
        let message = format!(
            "Task completion rejected: task {task_id} reported `completed`, but required local output artifacts are not complete. Owner: {owner_label}. Issue: {issue}. The task was kept blocked so downstream dependencies do not start from a missing handoff. Publish the formal package/checklist/manifest or explain the blocker."
        );
        send_team_message_to_dir(team_dir, "system", &config.lead, &message)?;
        if let Some(owner) = owner
            && config.members.iter().any(|member| member.name == *owner)
        {
            send_team_message_to_dir(team_dir, "system", owner, &message)?;
        }
        append_event(
            team_dir,
            "task_completion_rejected_missing_artifacts",
            serde_json::json!({
                "task": task_id,
                "owner": owner,
                "issue": issue,
            }),
        )?;
    }
    Ok(())
}

fn notify_completed_task_freezes(team_dir: &Path, completed: &[TeamTask]) -> Result<()> {
    if completed.is_empty() {
        return Ok(());
    }
    let config = load_config(team_dir)?;
    for task in completed {
        let owner_label = task
            .owner
            .as_deref()
            .map(|owner| format!("@{owner}"))
            .unwrap_or_else(|| "unassigned owner".to_string());
        let subject = task.subject.trim();
        let subject_suffix = if subject.is_empty() {
            String::new()
        } else {
            format!(" `{subject}`")
        };
        let owner_message = format!(
            "TASK_COMPLETION_FREEZE: task {}{} is now completed. Treat declared artifacts, manifests, checklists, and handoff paths as frozen for downstream consumers. Do not mutate completed task artifacts or manifests unless lead explicitly reopens this task. If you discover a correction, send lead a `LEAD_PROPOSAL:` or blocker first with exact paths, old/new hashes, and why the task must be reopened. Preserve stale or superseded handoffs only as failed-attempt provenance, not final evidence.",
            task.id, subject_suffix
        );
        let lead_message = format!(
            "TASK_COMPLETION_FREEZE: task {}{} completed by {owner_label}. Downstream consumers may now rely on the declared handoff. If the owner reports a correction, reopen the task before resyncing, reclearing, or allowing downstream execution; do not allow silent post-completion mutation. Stale handoffs should be preserved only as failed-attempt provenance.",
            task.id, subject_suffix
        );
        send_team_message_to_dir(team_dir, "system", &config.lead, &lead_message)?;
        if let Some(owner) = task.owner.as_deref()
            && owner != config.lead
            && config.members.iter().any(|member| member.name == owner)
        {
            send_team_message_to_dir(team_dir, "system", owner, &owner_message)?;
        }
        append_event(
            team_dir,
            "task_completion_freeze_notified",
            serde_json::json!({
                "task": task.id,
                "owner": task.owner,
            }),
        )?;
    }
    Ok(())
}

fn block_member_tasks_if_active(team_dir: &Path, member_name: &str, reason: &str) -> Result<()> {
    let mut changed = false;
    let mut tasks = load_tasks(team_dir)?;
    for task in &mut tasks {
        if task.owner.as_deref() == Some(member_name)
            && matches!(
                task.status,
                TaskStatus::Pending
                    | TaskStatus::Ready
                    | TaskStatus::InProgress
                    | TaskStatus::Review
            )
        {
            task.status = TaskStatus::Blocked;
            task.updated_at = now();
            task.result = Some(reason.to_string());
            changed = true;
        }
    }
    if changed {
        for task in &tasks {
            write_json_atomic(&task_path(team_dir, &task.id), task)?;
        }
        touch_config(team_dir)?;
        auto_promote_dependency_waits(team_dir)?;
    }
    Ok(())
}

fn team_workers(config: &TeamConfig) -> Vec<TeamMember> {
    config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .cloned()
        .collect()
}

fn send_system_message_to_members(
    team_dir: &Path,
    config: &TeamConfig,
    from: &str,
    members: &[TeamMember],
    message: &str,
) -> Result<()> {
    ensure_member_exists(config, from)?;
    for member in members {
        let msg = MailMessage {
            from: from.to_string(),
            to: member.name.clone(),
            message: message.to_string(),
            timestamp: now(),
            read: false,
        };
        append_jsonl(&mailbox_path(team_dir, &msg.to), &msg)?;
    }
    append_event(
        team_dir,
        "message_sent",
        serde_json::json!({
            "from": from,
            "to": members.iter().map(|member| member.name.clone()).collect::<Vec<_>>(),
            "system": true,
            "message": message,
        }),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_codex_exec(
    codex_exe: &Path,
    cwd: &Path,
    team_id: &str,
    member_name: &str,
    member_role: &str,
    prompt: &str,
    log_path: &Path,
    last_message_path: &Path,
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<std::process::ExitStatus> {
    let stdout =
        fs::File::create(log_path).with_context(|| format!("create {}", log_path.display()))?;
    let stderr = stdout.try_clone()?;
    let mut command = Command::new(codex_exe);
    command
        .arg("exec")
        .arg("-C")
        .arg(cwd)
        .arg("-o")
        .arg(last_message_path)
        .env("CODEX_TEAM_ID", team_id)
        .env("CODEX_TEAM_MEMBER", member_name)
        .env("CODEX_TEAM_ROLE", member_role)
        .env("CODEX_TEAM_CLI", codex_exe)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    if let Some(model) = model {
        command.arg("--model").arg(model);
    }
    if let Some(profile) = profile {
        command.arg("--profile").arg(profile);
    }
    if let Some(sandbox) = sandbox {
        command.arg("--sandbox").arg(sandbox);
    }
    if dangerously_bypass_approvals_and_sandbox {
        command.arg("--dangerously-bypass-approvals-and-sandbox");
    }
    command.arg(prompt);
    command
        .spawn()
        .with_context(|| format!("spawn Codex member `{member_name}`"))?
        .wait()
        .with_context(|| format!("wait for Codex member `{member_name}`"))
}

fn build_discussion_prompt(
    config: &TeamConfig,
    tasks: &[TeamTask],
    member: &TeamMember,
    round: u32,
    total_rounds: u32,
) -> String {
    let assigned_tasks = tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(member.name.as_str()))
        .map(|task| format!("- {} [{}]: {}", task.id, task.status, task.subject))
        .collect::<Vec<_>>()
        .join("\n");
    let member_lines = config
        .members
        .iter()
        .map(|member| format!("- {} ({})", member.name, member.role))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are participating in a Codex agent team discussion round.

Team: {team_id}
Goal: {goal}
Member: {member_name}
Role: {role}
Round: {round}/{total_rounds}

Use the normal Codex runtime exactly as configured for this machine. Your job in this turn is coordination, not implementation.

First read your inbox:
- "$CODEX_TEAM_CLI" team inbox --team "$CODEX_TEAM_ID"

Then send concise messages through the team mailbox:
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" lead "<status, risks, questions, proposed handoff>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" all "<shared assumption, interface contract, blocker, or review concern>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" <member> "<direct question or handoff>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" <member[,member...]> "<same message to a small explicit group>"

Discuss before acting. Surface disagreements, file ownership, interface boundaries, test strategy, blockers, and dependencies. If you can make progress independently, state your plan and any assumptions. Keep this round short and concrete.

Team members:
{member_lines}

Your assigned tasks:
{assigned_tasks}
"#,
        team_id = config.id,
        goal = config.goal,
        member_name = member.name,
        role = member.role,
        round = round,
        total_rounds = total_rounds,
        member_lines = member_lines,
        assigned_tasks = if assigned_tasks.is_empty() {
            "(none)".to_string()
        } else {
            assigned_tasks
        },
    )
}

fn build_worker_prompt(config: &TeamConfig, tasks: &[TeamTask], member: &TeamMember) -> String {
    let assigned_tasks: Vec<&TeamTask> = tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(member.name.as_str()))
        .collect();
    let task_text = assigned_tasks
        .iter()
        .map(|task| {
            let description = if task.description.trim().is_empty() {
                task.subject.as_str()
            } else {
                task.description.as_str()
            };
            format!(
                "- {} [{}]: {}\n  {}",
                task.id, task.status, task.subject, description
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        r#"You are a Codex agent team department.

Team: {team_id}
Goal: {goal}
Department: {member_name}
Role: {role}

Use the normal Codex runtime exactly as configured for this machine. User config, skills, MCP servers, plugins, auth, model settings, and project instructions are available through this Codex session.

Tooling and dependency policy:
- Do not stop at "this image/environment lacks node/python/chromium/rg/etc." when installing the missing tool is reasonable for the task. Install needed libraries, runtimes, CLIs, browsers, test tools, build tools, and package dependencies so the work can be implemented and verified properly.
- In Docker containers, you are often root; use the container package manager directly. On SSH/local hosts, prefer project-local or user-local installs, and use passwordless sudo (`sudo -n`) only when available. Never wait for an interactive sudo password.
- Prefer the environment's best package manager and project conventions: apt/apk/dnf/yum/brew for OS packages, npm/pnpm/yarn for JS, pip/uv/poetry for Python, cargo for Rust, and the repo's lockfiles/scripts when present.
- Use non-interactive, reproducible commands where possible. If work will take time or has an external completion condition, make it observable: use `team job` for PID-backed commands, and use `team wait` for non-PID waits or asynchronous/external dependencies.
- Report significant installs, versions, and any fallback to lead. Only fall back to weaker static checks after a concrete install attempt is impossible or unsafe.

Coordinate through the native team store with these shell commands:
- "$CODEX_TEAM_CLI" team status --team "$CODEX_TEAM_ID"
- "$CODEX_TEAM_CLI" team node --team "$CODEX_TEAM_ID" list
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" list
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" claim [TASK_ID] --owner "$CODEX_TEAM_MEMBER"
- "$CODEX_TEAM_CLI" team ownership --team "$CODEX_TEAM_ID" list
- "$CODEX_TEAM_CLI" team ownership --team "$CODEX_TEAM_ID" claim <PATH> --note "<editing scope>"
- "$CODEX_TEAM_CLI" team ownership --team "$CODEX_TEAM_ID" release <PATH>
- "$CODEX_TEAM_CLI" team inbox --team "$CODEX_TEAM_ID"
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" set <TASK_ID> --status in_progress
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" set <TASK_ID> --status blocked --result "<what you are waiting for>"
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" set <TASK_ID> --status completed --result "<short result>"
- "$CODEX_TEAM_CLI" team job --team "$CODEX_TEAM_ID" start --owner "$CODEX_TEAM_MEMBER" --task <TASK_ID> --node <node-id> --cwd <cwd> -- <command...>
- "$CODEX_TEAM_CLI" team wait --team "$CODEX_TEAM_ID" add "<title>" --owner "$CODEX_TEAM_MEMBER" --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
- "$CODEX_TEAM_CLI" team wait --team "$CODEX_TEAM_ID" list --owner "$CODEX_TEAM_MEMBER"
- "$CODEX_TEAM_CLI" team wait --team "$CODEX_TEAM_ID" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" lead "<message>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" all "<message>"

The message command defaults the sender to CODEX_TEAM_MEMBER, so teammates can DM each other without passing --from. Use `all` for a broadcast.

Start by reading your inbox, the task list, the wait list, and the ownership list. Before editing a file, claim the path with the ownership command. If another department owns the path, do not edit it until that department hands it off or lead explicitly reassigns ownership. Check your inbox again after important task updates and before finishing. Discuss disagreements, blockers, handoffs, and review findings through team messages. Own your department mission end to end. If the work is broad, research-heavy, implementation-heavy, review-heavy, or otherwise benefits from parallel thinking, actively use available subagent/agent tools, skills, MCP servers, and internal decomposition within this department; do not try to carry all substantial work in one main thread when helpers are available. Do not ask the lead to create duplicate peer departments solely for load balancing. Work on tasks assigned to your department. You may also self-claim an unassigned `ready` task with `team task claim` only when it clearly matches your department mission and you can own it end to end; after claiming, message lead with the reason and intended artifacts. When handing a file to another department, send a message and release or ask lead to reassign ownership. If you start work that cannot finish until an observable condition becomes true, register it as `team wait` unless it is already a tracked `team job`. Include the exact completion condition, current request/job/log/checkpoint identifier, owner, task, and expected evidence. Do not mark a task completed while one of its waits is open. If you are blocked waiting for another department, a research gate, credentials, an artifact, lead decision, or any other condition, set your assigned task to `blocked` or register/update a wait, message lead and the relevant department, and finish; do not mark it completed just because your current turn is waiting. If you notice a blocked, pending, or review task whose gate appears cleared, whose prerequisite artifact/handoff has arrived, or whose next owner is ambiguous, do not start owned work for another department; send lead a concise `LEAD_PROPOSAL:` message with the evidence and proposed resume/reassign/review action. If this department is assigned to a non-local node, treat that node as your operational site. If Codex authentication is requested via device code, let the team runtime's direct local browser automation handle the device URL/code; report only if that automation fails and you remain unauthenticated.

Active collaboration protocol:
- Broadcast a short initial plan to `all` when starting nontrivial work, including intended outputs, consumers, and known risks.
- Ask related departments for opinions early, even for small uncertainties that affect design, runtime choices, data/model selection, schema shape, QA criteria, or handoff interpretation. Do not wait until a large failure.
- When you hit an error or weak result, message lead and the relevant consumer/producer department with the exact failure, log/artifact path, your diagnosis, and the next option you propose.
- When you create an artifact, message the departments that should consume or review it. A file that exists but has not been handed off is not complete team work.
- Watch the team state, not only your own files. If you notice a blocked/pending/review task that looks ready because a handoff, artifact, job result, or prerequisite has arrived, send lead a `LEAD_PROPOSAL:` message. Include task id, evidence, and suggested action. This is advisory: lead must approve before you take unassigned work.
- Before finishing, check your inbox once more and answer or acknowledge relevant messages.

Completion checklist:
Before setting an assigned task to `completed` or ending a turn that should complete active work, send a final team message to lead and any consumers, then include this exact marker in your final assistant response:

TEAM_COMPLETION_CHECKLIST:
- artifacts: <paths or "none">
- verification: <commands/results or "not run">
- messages_sent: <lead/all/member messages you sent>
- consumers_notified: <departments or "none">
- blockers_or_limits: <remaining blockers/limits or "none">

If any item is unknown or missing, do not mark the task completed; mark it `blocked` or leave it in progress/standby and ask for help.

Current-run source policy:
- The team mailbox, current tasks, ownership records, and files/artifacts explicitly created for team `{team_id}` are the source of truth for this run.
- Treat pre-existing files, old research notes, stale Docker images, old containers, and old output directories as background context only. They are not authoritative gates or final evidence unless lead explicitly adopts them for this team.
- If a pre-existing artifact conflicts with a current teammate message or the current team goal, do not block on it by default. Ask lead only if adopting that stale artifact would change the current plan; otherwise ignore it and continue from current-run evidence.
- When reusing an old artifact for speed, record provenance and rerun this team's container-local execution and validation before presenting it as evidence.

External dependency and credential policy:
- When choosing or implementing an external model, dataset, package, API, browser, or service, verify the transitive runtime dependencies, not just the top-level repo license.
- If a required artifact is gated, private, returns 401/403, requires manual license acceptance, or requires credentials that the user has not provided for this task, do not silently weaken the result or present partial output as success. Preserve logs, mark the task blocked, and message lead with the exact dependency, URL/repo, status code, and whether a public/local fallback is documented.
- If the goal asks for something open source or publicly runnable, prefer a current model/tool that can complete end-to-end in this environment over a newer one that cannot run without extra authorization. Research must revise the model/tool recommendation if execution proves the first choice is not runnable.

Evidence validation policy:
- Producer departments must generate final hashes and manifests only after all final files are written. A completion checklist, outcome/report JSON, status file, or transcript that is edited after hashing makes the package stale and must be regenerated before handoff.
- If a contract, task, or method package distinguishes train/source data from heldout/test/evaluation-only data, write and hash or timestamp the frozen configuration/plan/thresholds/parameter set before opening, listing, parsing, previewing, loading with PIL/OpenCV/numpy, or otherwise inspecting heldout/test/evaluation-only content. Broad inventory commands such as `find`, `ls -R`, `tree`, `rg`, or shell glob expansion over a parent directory that can reveal heldout/test/evaluation-only path names count as inspection unless the contract explicitly permits that inventory before freeze. Directory existence and package manifest verification are okay only when they do not inspect heldout content or reveal heldout path names beyond the declared manifest. If you accidentally inspect heldout/test/evaluation-only content or path names before the freeze point, stop and report the protocol violation as a failed attempt or FAIL package when the contract defines it; do not continue into evaluation as if it were clean.
- If a contract forbids pre-guard discovery or reads under a protected input root, do not place the only guard policy, method contract, or bootstrap instructions exclusively under that protected root unless the contract declares an exact, hash-bound pre-guard read exception for those files. Prefer a small guard bootstrap seed outside the protected input root, then activate the guarded runner before touching the protected root.
- For guarded or protected-root tasks, never "locate" a seed, contract, input, dataset, or handoff by running broad `find`, `ls`, `tree`, `rg`, glob expansion, or directory inventory over a protected parent such as `/workspace`, `/workspace/inputs`, or `/workspace/data` before the guard/freeze point. Use only exact paths supplied by lead, the task text, `lead_sync_list`, or the external guard seed. If an exact path is missing or ambiguous, block and ask lead for the path instead of discovering it.
- Before a guard/freeze point, an exact file path exception is not permission to run arbitrary probes or readers against that file. Do not run `cat`, `sed`, `head`, `tail`, `awk`, `python -c open(...)`, `--help`, `--version`, smoke tests, import checks, schema probes, Python introspection, or any command variant unless that exact full command appears in `pre_guard_allowed_exact_commands` or the lead's explicit clearance. If you need to understand a guarded file/tool before activation, block and ask lead to update the contract; do not inspect or probe it yourself.
- For guarded tasks with an early fail-closed path, the method package must provide a seed-local schema-valid fail-closed writer or template outside the protected root, plus an exact allowed command for invoking it. Runtime must use that writer/template for pre-guard failures instead of inventing a legacy or best-effort `outcome.json`. If no schema-valid early fail-closed writer/template is available before protected reads are legal, block and ask lead/method to repair the contract before runtime proceeds.
- For any runtime, evaluation, render, build, install, bootstrap, or guarded command that is material to the evidence, write a frozen command transcript before final manifest generation that includes the exact command, cwd, node/container identity when non-local, start/end timestamps when practical, and an explicit shell exit code such as `rc=0` or `exit=23`. A mailbox handoff, event ledger, or summarized outcome alone is not sufficient as clean command-exit evidence. If the command ran as a tracked team job, cite the job id/log path and include the observed exit code in the report/checklist.
- Write hash manifests as one `sha256  path` record per real file. Do not build a single shell variable containing newline-separated paths and pass it as one filename. Prefer `find ... -print0 | sort -z | xargs -0 sha256sum` or an explicit array loop.
- Exclude volatile, self-referential, or still-being-edited files from final producer manifests unless you can guarantee they will not change after hashing. Common volatile examples include the active command transcript, live job log, manifest verification log, handoff/status/progress logs, the manifest file being generated, and any helper/finalizer script that you may edit while repairing or checking the manifest. If you include any such file, write it first, close it, generate the manifest after the last edit, and do not append or patch it after that point. If a manifest repair changes a helper script, regenerate and recheck the manifest again after that script is stable.
- Before announcing a producer handoff as complete, run `sha256sum -c` from the intended manifest root and include the exact command/root plus rc in `TEAM_COMPLETION_CHECKLIST`. If this fails, keep the task in progress or blocked and report the exact mismatch instead of handing off to validation.
- Immediately before the final handoff message, re-read the current manifest files from disk and compute/report the manifest file hashes from those files. Do not rely on hashes remembered from an earlier script run, draft message, previous handoff attempt, or pre-repair state. If the handoff text hash differs from the current on-disk manifest hash, correct the message before sending; a stale handoff hash is still a WARN-worthy provenance defect even when `sha256sum -c` passes.
- When validating manifests, reports, metrics, renders, or schema handoffs, do not assume the working directory. First inspect whether paths inside the manifest are absolute, workspace-relative, package-root-relative, or manifest-directory-relative. Run `sha256sum -c` from the correct base directory, or record multiple attempted bases if the provenance is ambiguous.
- If a manifest check fails because paths are evaluated from the wrong cwd, treat that as a validator methodology issue, not as producer evidence failure. Regenerate the validation report after correcting the cwd/path interpretation, and preserve the failed validator pass in transcript/provenance so audit can see what changed.
- Structured validation ledgers must be polarity-consistent with the final verdict. Before handoff, check that boolean/value/status fields do not contradict themselves; for example, `validator_path_or_tooling_limitation=false` should not be marked `FAIL` unless the ledger explicitly documents inverted semantics. If the structured ledger and final verdict disagree, fix the ledger and regenerate its manifest before audit consumes it.
- Treat explicit negative fields as negative evidence, not success claims. Examples: `blocked_claims`, `downstream_claims_blocked`, `non_claims`, `not_supported`, `not_run`, `blocked_outputs`, and restrictive `claim_boundary` entries usually mean the producer is refusing those claims. More generally, field names containing `blocked`, `non_claim`, `not_supported`, `not_run`, `unsupported`, or `prohibited` are likely negative-polarity fields. Do not fail a package merely because a prohibited claim string appears inside a blocked/non-claim list; fail only if the same claim is also asserted as supported, required evidence is missing, or the polarity is ambiguous after inspection.
- Count generated outputs from their actual recorded locations instead of assumed names such as `predicted` or `gt`. If an eval/render tool writes to `test/rgb`, `test/gt-rgb`, `renders`, or another tool-specific path, record that mapping and preserve the claim boundary instead of falsely reporting missing outputs.
- For optional or tool-version-dependent outputs, use discovery-first inspection before hardcoded probes: list the relevant directories, parse the producer's manifest/schema/outcome files, then decide which optional paths are required for the claim. If a hardcoded optional-path probe fails, preserve that failed probe as audit/validator provenance, rerun with discovery-based paths, and do not classify it as a producer failure unless the claimed required artifact is truly absent.
- Audit/validation departments must distinguish producer evidence failures from validator-script/path bugs. If unsure, message lead plus the producer department with exact paths and the suspected interpretation before finalizing a FAIL verdict.

MCP and context policy:
- Remote/SSH/Docker departments must not assume local MCP servers are reachable just because local config was synced. If an MCP server is unavailable on the remote node, report it as a tooling limit and ask lead/local research to perform MCP-backed reasoning or retrieval locally, then consume the resulting files/messages on the remote node.
- Keep live turn context compact. Do not paste full logs, full papers, long generated files, or huge command output into team messages. Save large evidence to files, register or message paths, and summarize only the decision-relevant facts. This keeps long-running lead/secretary sessions from ballooning.

Docker/container ownership boundary:
- If your department needs Docker as the real execution environment for the main task, your host-side responsibility is to build the image, create or replace a stable long-lived container, mount the relevant workspace, expose needed ports/GPU, and register/report it as a team Docker node. Use `sleep infinity` or an equivalent long-lived command for that team container unless the lead explicitly chooses another lifecycle.
- After the Docker node is registered, stop doing the main application/experiment/model run from the host with `docker run` or `docker exec`. A container-internal department will be added automatically and should own the work inside the container.
- Host-side departments may still rebuild the image, recreate the container, clean stale containers, or provide handoff details when lead asks. Runtime debugging, installs inside the container, sample execution, rendering, and container-local verification belong to the container-internal department.
- If you already created a container manually, immediately report `TEAM_NODE id=<node-id> kind=<docker|ssh-docker> host=<ssh-host-or-> container=<container> cwd=<container-cwd> note=<short_note>` or message lead with the same fields. Do not hide the container as a private side environment.

Assigned tasks:
{task_text}
"#,
        team_id = config.id,
        goal = config.goal,
        member_name = member.name,
        role = member.role,
        task_text = if task_text.is_empty() {
            "(none)".to_string()
        } else {
            task_text
        },
    )
}

fn build_app_server_worker_prompt(
    config: &TeamConfig,
    tasks: &[TeamTask],
    member: &TeamMember,
    codex_exe: &Path,
    nodes: &[TeamNode],
    language: TeamPromptLanguage,
) -> String {
    let mut prompt = build_worker_prompt(config, tasks, member);
    let node_context = build_member_node_context(member, nodes);
    let remote_note = if member_node_id(member) == "local" {
        ""
    } else {
        "\nThis department is assigned to a non-local node. You are already running on that node through its app-server thread; do not SSH into the same node again just to do your normal work. Use `codex-team` directly for team coordination on this node; it talks to the same live team mailbox as local departments. Keep TEAM_MESSAGE lines only as a fallback if `codex-team` is unavailable.\n"
    };
    prompt.push_str(&format!(
        r#"

App-server managed team run:
- Your session is managed as an app-server thread, so CODEX_TEAM_* environment variables may not be present.
{node_context}
- Local-node coordination commands:
  - "{codex}" team status --team "{team_id}"
  - "{codex}" team node --team "{team_id}" list
  - "{codex}" team node --team "{team_id}" inspect [node-id]
  - "{codex}" team node --team "{team_id}" sync-path <node-id> --src <local-path> --dest <node-path> [--replace]
  - "{codex}" team task --team "{team_id}" list
  - "{codex}" team task --team "{team_id}" claim [TASK_ID] --owner "{member}"
  - "{codex}" team job --team "{team_id}" start --owner "{member}" --task <TASK_ID> --node <node-id> --cwd <cwd> -- <command...>
  - "{codex}" team job --team "{team_id}" status <job-id>
  - "{codex}" team job --team "{team_id}" logs <job-id> --tail 80
  - "{codex}" team wait --team "{team_id}" add "<title>" --owner "{member}" --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
  - "{codex}" team wait --team "{team_id}" list --owner "{member}"
  - "{codex}" team wait --team "{team_id}" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
  - "{codex}" team ownership --team "{team_id}" list
  - "{codex}" team ownership --team "{team_id}" claim <PATH> --owner "{member}" --note "<editing scope>"
  - "{codex}" team ownership --team "{team_id}" release <PATH> --owner "{member}"
  - "{codex}" team inbox --team "{team_id}" "{member}"
  - "{codex}" team task --team "{team_id}" set <TASK_ID> --status in_progress
  - "{codex}" team task --team "{team_id}" set <TASK_ID> --status blocked --result "<what you are waiting for>"
  - "{codex}" team task --team "{team_id}" set <TASK_ID> --status completed --result "<short result>"
  - "{codex}" team message --team "{team_id}" --from "{member}" lead "<message>"
  - "{codex}" team message --team "{team_id}" --from "{member}" all "<message>"
  - "{codex}" team message --team "{team_id}" --from "{member}" <member> "<direct question>"
  - "{codex}" team message --team "{team_id}" --from "{member}" <member[,member...]> "<same message to a small explicit group>"
- Non-local node coordination commands. If your department node is not `local`, prefer the `codex-team` helper first. Bootstrap installs it with embedded team/relay defaults, but PATH can differ in app-server shells, so start with:
  - export PATH="$HOME/bin:/usr/local/bin:/root/bin:$PATH"
  - TEAM_CLI="$(command -v codex-team || true)"; if [ -z "$TEAM_CLI" ] && [ -x /root/bin/codex-team ]; then TEAM_CLI=/root/bin/codex-team; fi
  - "$TEAM_CLI" status
  - "$TEAM_CLI" task list
  - "$TEAM_CLI" task claim [TASK_ID] --owner "{member}"
  - "$TEAM_CLI" ownership list
  - "$TEAM_CLI" ownership claim <PATH> --owner "{member}" --note "<editing scope>"
  - "$TEAM_CLI" ownership release <PATH> --owner "{member}"
  - "$TEAM_CLI" inbox "{member}"
  - "$TEAM_CLI" task set <TASK_ID> --status in_progress
  - "$TEAM_CLI" task set <TASK_ID> --status blocked --result "<what you are waiting for>"
  - "$TEAM_CLI" task set <TASK_ID> --status completed --result "<short result>"
  - "$TEAM_CLI" wait add "<title>" --owner "{member}" --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
  - "$TEAM_CLI" wait list --owner "{member}"
  - "$TEAM_CLI" wait set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
  - "$TEAM_CLI" message --from "{member}" lead "<message>"
  - "$TEAM_CLI" message --from "{member}" all "<message>"
  - "$TEAM_CLI" message --from "{member}" <member> "<direct question>"
  - "$TEAM_CLI" message --from "{member}" <member[,member...]> "<same message to a small explicit group>"

When a teammate sends you a message, the orchestrator may steer this active turn with the new message. Treat that as live team discussion and respond or adjust your work if needed. Ask clarifying or review questions back to related departments whenever their judgment could improve the result; do not silently make cross-department decisions.
If your work or an invoked skill creates or uses a Docker container for ongoing team work, do not leave it as an invisible side environment. Ask lead to use `team node create-docker` when possible; otherwise use a stable long-lived container name, mount the relevant workspace, publish any user-facing ports with `-p`, keep the container alive, and send lead the exact container name, host, mount paths, exposed ports, and suggested node kind (`docker` or `ssh-docker`) so lead can register or update the placement. If you cannot run the local team CLI but have enough details, also write one standalone line in this exact format: `TEAM_NODE id=<node-id> kind=<docker|ssh-docker> host=<ssh-host-or-> container=<container> cwd=<container-cwd> note=<short_note>`. The orchestrator will register the node and add a container-internal department automatically. Once the node is registered, the container-internal department owns installs, runtime execution, rendering, tests, and debugging inside that container; host-side departments should stop at image/container creation plus handoff unless lead asks for a rebuild or replacement. Avoid read-write mounting the host's entire `~/.codex` into a root-owned container; use `team node sync-assets`, a dedicated Codex home, copied credentials/config, or the existing bootstrap/auth flow. If lead has already assigned you to a Docker/SSH-Docker node, treat the execution node context above as authoritative.
If you need a local artifact, schema package, config, generated input, report, or source matrix on a remote/Docker node and it is not mounted there, ask lead to hand it off with `team node sync-path <node-id> --src <local-path> --dest <node-path> [--replace]`. Do not silently recreate stale copies on the node, and treat missing handoff files as a blocker until the authoritative artifact is synced.
If your assigned node lacks a normal verification tool, install it before weakening the verification. Example: for a web app, install Node.js/npm or a headless browser when needed to run smoke tests; for Python work, install the project/test dependencies in a venv when appropriate. In containers, root-level installs are acceptable. On SSH/local nodes, use user-local installs or passwordless sudo only.
If you start work that may take time, make it inspectable. Use `team job --owner {member} --task <TASK_ID>` for commands the team CLI can run and inspect. Use `team wait add --owner {member} --task <TASK_ID>` for anything with a completion condition but no reliable team-managed PID. This is generic: do not assume only a fixed set of wait types exists. Include the exact completion condition, current request/log/checkpoint identifier, and expected evidence. Do not hide important background or external work in an untracked shell process or an unregistered wait.
If you start a tracked job or wait yourself, send `all` or the relevant departments the id, target node if any, exact intent, completion condition, and expected log/artifact/evidence paths. When it completes or fails, update the job/wait, hand off the result, and include it in `TEAM_COMPLETION_CHECKLIST`.
If this session runs on a remote/SSH/Docker node where both the local team CLI path and `codex-team` are unavailable, communicate by writing a standalone line in this exact format:
TEAM_MESSAGE to=<lead|all|member|member[,member...]>: <message>
If `codex-team task set ...` is unavailable, also write one standalone line in this exact format:
TEAM_TASK id=<TASK_ID> status=<pending|waiting|ready|in_progress|blocked|review|completed|failed|cancelled> result=<short result or blocker>
If `codex-team wait ...` is unavailable, register or update a wait with one standalone line in this exact format:
TEAM_WAIT title=<short_no_spaces> task=<TASK_ID> status=<waiting|running|polling|blocked|completed|failed|cancelled> | condition=<exact completion condition> | progress=<request id, URL, log path, checkpoint, or current state> | evidence=<path-or-url-or-empty>
The orchestrator will copy TEAM_MESSAGE lines into the local team mailbox, TEAM_TASK lines into the task table, and TEAM_WAIT lines into the wait table while your response streams, and again when your turn completes.
{remote_note}
"#,
        codex = codex_exe.display(),
        team_id = config.id,
        member = member.name,
        node_context = node_context,
        remote_note = remote_note,
    ));
    localize_team_prompt(prompt, language)
}

fn localize_team_prompt(prompt: String, language: TeamPromptLanguage) -> String {
    if !language.is_ja() {
        return prompt;
    }
    format!(
        "重要: この team は `--language ja` で実行されています。以下の運用指示・コマンド・ポリシーを日本語で解釈し、team message、handoff、status、debug log に残る自然文は原則として日本語で書いてください。コマンド名、識別子、path、JSON/YAML key、status enum、TEAM_COMPLETION_CHECKLIST など機械可読な語は変更せず、そのまま使ってください。\n\n{prompt}"
    )
}

fn build_member_node_context(member: &TeamMember, nodes: &[TeamNode]) -> String {
    let node_id = member_node_id(member);
    let Some(node) = nodes.iter().find(|node| node.id == node_id) else {
        return format!(
            "- Execution node: {node_id}. Node metadata was not available in this prompt.\n"
        );
    };
    let mut context = format!(
        r#"- Execution node:
  - id: {id}
  - kind: {kind:?}
  - host: {host}
  - container: {container}
  - cwd on node: {cwd}
  - note: {note}
"#,
        id = node.id,
        kind = node.kind,
        host = node.host.as_deref().unwrap_or(""),
        container = node.container.as_deref().unwrap_or(""),
        cwd = node.cwd.as_deref().unwrap_or("."),
        note = node.note,
    );
    match node.kind {
        TeamNodeKind::Local => {
            context.push_str("  - site meaning: this thread runs on the local machine.\n");
        }
        TeamNodeKind::Ssh => {
            context.push_str(
                "  - site meaning: this thread already runs on the SSH host through a remote app-server. Do not SSH into the same host for ordinary work.\n",
            );
        }
        TeamNodeKind::Docker => {
            context.push_str(
                "  - site meaning: this thread already runs inside the Docker container through its app-server. Do not docker exec into the same container for ordinary work.\n",
            );
            context.push_str(&docker_runtime_prompt_context(None, node));
        }
        TeamNodeKind::SshDocker => {
            context.push_str(
                "  - site meaning: this thread already runs inside a Docker container on the SSH host through its app-server. Do not SSH/docker exec into the same site for ordinary work.\n",
            );
            context.push_str(&docker_runtime_prompt_context(node.host.as_deref(), node));
        }
        TeamNodeKind::Manual => {
            context.push_str(
                "  - site meaning: this thread runs on a manually registered app-server node.\n",
            );
        }
    }
    context
}

fn docker_runtime_prompt_context(host: Option<&str>, node: &TeamNode) -> String {
    let Some(container) = node.container.as_deref() else {
        return String::new();
    };
    let inspect = |template: &str| docker_inspect_value(host, container, template).ok();
    let network = inspect("{{.HostConfig.NetworkMode}}").unwrap_or_default();
    let ports = inspect("{{json .NetworkSettings.Ports}}").unwrap_or_default();
    let mounts = inspect("{{json .Mounts}}").unwrap_or_default();
    let image = inspect("{{.Config.Image}}").unwrap_or_default();
    format!(
        r#"  - docker image: {image}
  - docker network mode: {network}
  - docker published/container ports: {ports}
  - docker mounts: {mounts}
"#
    )
}

fn build_reactive_steer_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    language: TeamPromptLanguage,
) -> String {
    let message_lines = messages
        .iter()
        .map(|message| {
            format!(
                "- [{}] {} -> {}: {}",
                message.timestamp, message.from, message.to, message.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    if language.is_ja() {
        format!(
            r#"Reactive team message update for {member} ({role}).

あなたの turn がまだ実行中の間に、新しい teammate message が届きました:
{message_lines}

すぐに考慮してください。plan が変わるなら現在の作業を調整してください。編集中の file に影響するなら、続行前に team ownership list を確認してください。reply、handoff、ownership change、clarification が必要なら、続行前に簡潔な team message を送ってください。自然文は日本語で書いてください。
"#,
            member = member.name,
            role = member.role,
            message_lines = message_lines,
        )
    } else {
        format!(
            r#"Reactive team message update for {member} ({role}).

New teammate message(s) arrived while your turn is still running:
{message_lines}

Consider this immediately. If it changes your plan, adjust your current work. If it affects a file you are editing, check the team ownership list before continuing. If a reply, handoff, ownership change, or clarification is needed, send a concise team message before continuing.
"#,
            member = member.name,
            role = member.role,
            message_lines = message_lines,
        )
    }
}

fn build_app_server_lead_prompt(
    config: &TeamConfig,
    tasks: &[TeamTask],
    member: &TeamMember,
    codex_exe: &Path,
    language: TeamPromptLanguage,
) -> String {
    let task_text = tasks
        .iter()
        .map(|task| {
            format!(
                "- {} [{}] owner={} subject={}",
                task.id,
                task.status,
                task.owner.as_deref().unwrap_or(""),
                task.subject
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let member_lines = config
        .members
        .iter()
        .map(|member| format!("- {} ({})", member.name, member.role))
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        r#"You are the live lead for a Codex app-server agent team.

Team: {team_id}
Goal: {goal}
Member: {member_name}
Role: {role}

You are a real app-server thread. Your job is orchestration, not implementation. Read current team state and your inbox, then send concise coordination only when useful.

Coordinate toward the user's current task, not toward an implicit endless improvement loop. Create, resume, reassign, or stand down departments based on current tasks, mailboxes, artifacts, and blockers. If a task description says "after runtime", "after validation", "after handoff", or names an upstream task, set that upstream task in `--depends-on`; do not start downstream validation/review before its real handoff exists. Before clearing a non-local runtime/validation department, inspect the contract and task text for every named predecessor package, prior review/audit note, validation report, source matrix, config, or generated input; sync those artifacts to the node and root-correct verify their manifests, not only the immediate method package or producer package. If you notice you created the wrong dependency list or cleared a task before required predecessor artifacts were synced, immediately fix it with `team task set <TASK_ID> --depends-on ... --status waiting|blocked --result "<corrected gate>"`, sync/verify the missing artifacts, and message the affected departments to standby until the handoff lands. Only start an automatic improvement/research loop when the active user instruction or an explicit domain skill requires that behavior.

Commands:
- "{codex}" team status --team "{team_id}"
- "{codex}" team node --team "{team_id}" list
- "{codex}" team node --team "{team_id}" inspect [node-id]
- "{codex}" team node --team "{team_id}" add <node-id> --kind manual --url ws://127.0.0.1:<forwarded-port> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh --host <ssh-host> --cwd <remote-cwd> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" add <node-id> --kind docker --container <container> --cwd <container-cwd> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh-docker --host <ssh-host> --container <container> --cwd <container-cwd> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" create-docker <node-id> [--host <ssh-host>] --image <image> --mount <host:container> --port <host:container> --gpus --replace
- "{codex}" team node --team "{team_id}" sync-assets <node-id> [--include-auth]
- "{codex}" team node --team "{team_id}" sync-path <node-id> --src <local-path> --dest <node-path> [--replace]
- "{codex}" team node --team "{team_id}" remove <node-id> --force
- "{codex}" team job --team "{team_id}" start --owner lead --task <TASK_ID> --node <node-id> --cwd <cwd> -- <command...>
- "{codex}" team job --team "{team_id}" status <job-id>
- "{codex}" team job --team "{team_id}" logs <job-id> --tail 80
- "{codex}" team job --team "{team_id}" artifact <job-id> <path> --note "<what it is>"
- "{codex}" team wait --team "{team_id}" add "<title>" --owner <department> --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>" [--node <node-id>] [--evidence <path-or-url>]
- "{codex}" team wait --team "{team_id}" list [--owner <department>] [--task <TASK_ID>]
- "{codex}" team wait --team "{team_id}" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
- "{codex}" team task --team "{team_id}" list
- "{codex}" team task --team "{team_id}" set <TASK_ID> --status <status> --depends-on <DEP_TASK_ID> [--depends-on <DEP_TASK_ID>...] --result "<why>"
- "{codex}" team ownership --team "{team_id}" list
- "{codex}" team ownership --team "{team_id}" claim <PATH> --owner <member> --note "<scope or handoff>"
- "{codex}" team ownership --team "{team_id}" release <PATH> --owner lead --force
- "{codex}" team member --team "{team_id}" list
- "{codex}" team member --team "{team_id}" add <name:role> --node <node-id> --mission "<why this department is needed>"
- "{codex}" team member --team "{team_id}" standby <member> --reason "<why active work is no longer needed>"
- "{codex}" team member --team "{team_id}" resume <member> --mission "<new active mission>"
- "{codex}" team inbox --team "{team_id}" lead
- "{codex}" team message --team "{team_id}" --from lead all "<coordination, priority, or decision>"
- "{codex}" team message --team "{team_id}" --from lead <member> "<direct instruction or clarification>"
- "{codex}" team message --team "{team_id}" --from lead <member[,member...]> "<same decision or clarification to a small explicit group>"

At the beginning, assign obvious file or directory ownership when the goal implies shared edits. Name the primary owner and handoff order instead of letting departments edit the same file at the same time. Use ownership claims for these decisions, and message the relevant departments.

Current-run source policy: team mailbox messages, current tasks, ownerships, and artifacts explicitly created for team `{team_id}` are authoritative. Pre-existing files, stale research notes, old Docker images, old containers, and old output directories are background context only. Do not let stale artifacts from a prior team override the current deep_thinker/research handoff or block execution unless you explicitly adopt them for this team after checking provenance. If you reuse an old image or artifact for speed, require a fresh container node and fresh container-local execution/validation for this team before accepting final evidence.

External dependency and credential policy: for tasks involving public/open-source models, datasets, packages, APIs, browsers, services, or other external artifacts, require the responsible department to verify transitive runtime accessibility before accepting the choice. A top-level open-source license is not enough if a required checkpoint, submodel, dataset, browser binary, package, or service is gated/private or returns 401/403 in this environment. If a run hits unprovided credentials, manual license acceptance, or a gated dependency, treat that as a real blocker: preserve exact logs and config paths, keep QA blocked, and resume research/ops to either find a documented public/local fallback or choose another current runnable option. Do not mark the overall goal complete with partial artifacts, stale outputs, or an image that cannot run end to end.

Evidence validation policy: when a validator or audit department reports manifest/render/schema failures, distinguish actual producer evidence failure from validation-script assumptions. Require the department to inspect whether manifest entries are absolute, workspace-relative, package-root-relative, manifest-directory-relative, stale, malformed, or self-referential before finalizing a FAIL. If a failed check is due to the wrong cwd or assumed output path, have the validator preserve that failed pass as provenance, rerun with the correct base/path mapping, and then hand off the corrected verdict. If an optional or tool-version-dependent path is missing, require discovery-first inspection of the directory tree plus producer manifest/schema/outcome files before deciding whether it was required evidence; preserve hardcoded optional-path probe failures as validator/audit provenance, not producer failures, unless the claimed artifact is truly absent. If a producer manifest is stale, malformed, generated before final writes, contains newline-expanded pseudo-paths, or includes volatile logs such as active transcripts/job logs that changed after hashing, route the package back to the producer to regenerate hashes/manifests before validation/audit proceeds. Also treat stale manifest hashes written only in a handoff message as a provenance defect: before accepting a final handoff, require the producer to re-read current on-disk manifest files, report those current manifest-file hashes, and explain any mismatch between handoff text and current files. If a structured validation ledger contradicts its final verdict, such as an absence/false value marked FAIL without documented inverted semantics, route it back to the validator to fix the ledger and regenerate the manifest. Treat negative evidence fields such as `blocked_claims`, `downstream_claims_blocked`, `non_claims`, `not_supported`, `not_run`, `blocked_outputs`, and restrictive `claim_boundary` entries as refusals/limits unless the artifact also asserts the same claim as supported; more generally, keys containing `blocked`, `non_claim`, `not_supported`, `not_run`, `unsupported`, or `prohibited` are likely negative-polarity fields. Do not fail merely because a prohibited claim string appears in a blocked/non-claim list. Do not let audit consume a FAIL that is actually a validator cwd/path or polarity bug, and do not let it consume a PASS based on stale producer manifests.
If a method/runtime contract forbids pre-guard discovery or reads under a protected input root, require a guard bootstrap path that is outside that protected root, or a precise hash-bound pre-guard read exception for the exact method/guard files. Do not clear runtime just because the method package was synced if the runtime cannot legally read it before the guard is active. When clearing guarded runtime, give the executor exact bootstrap paths and explicitly forbid using `find`, `ls`, `tree`, `rg`, glob expansion, or parent-directory inventory over protected roots such as `/workspace`, `/workspace/inputs`, or `/workspace/data` to locate them; if an exact path is missing, the executor must block and ask lead. Also explicitly forbid `cat`, `sed`, `head`, `tail`, `awk`, `python -c open(...)`, `--help`, `--version`, smoke tests, import checks, schema probes, Python introspection, or any other pre-guard file reader/probe/command variant unless the exact full command is listed in `pre_guard_allowed_exact_commands`; an exact file path exception alone is not enough to run a reader or probe command. Before clearing guarded runtime, verify that a seed-local schema-valid early fail-closed writer/template is available outside protected roots and that its invocation is an exact allowed command; if fail-closed evidence would require reading the protected method schema before the guard is active, keep runtime blocked and route the contract back to method/schema.
For runtime, bootstrap, evaluation, render, build, install, or guarded commands whose exit status is material to a claim, require an artifact-level command transcript with the exact command, cwd, node/container identity when non-local, and explicit shell exit code (`rc=...` or `exit=...`) before accepting the handoff as clean. Event-level ledgers, mailbox summaries, or outcome JSON may support the claim, but they are not a clean substitute for direct command-exit evidence. If the department uses `team job`, require the final report/checklist to cite the job id/log path and observed exit code.

MCP and context policy: remote/SSH/Docker departments may not have working access to local MCP servers even when config and skills are synced. If a remote department reports MCP transport failures, do not treat that as a worker failure; route MCP-backed research/reasoning/tool calls to a local department or the lead, save the result into current-run artifacts, then hand those artifacts to the remote executor. Keep lead turns and team messages compact: never paste full logs, papers, generated files, or huge command output into mailbox messages. Require paths, hashes, short summaries, and registered artifacts instead.

You also own placement. The normal user flow is natural language plus the bypass/sandbox choice only; do not expect the user to hand-write members, nodes, Docker flags, mounts, or ports. If the user request mentions SSH, a remote machine, Docker, a container, or environment-specific development/testing, inspect the node list and create or update the needed node before adding/resuming a department there. Use `team node inspect` before assigning nontrivial work to learn OS, tools, Docker, GPU, ports, mounts, and Codex availability. The team runner will bootstrap Codex, `codex-team`, and app-server on SSH/Docker nodes when a department is assigned to them. If auth is needed, the runtime captures the Codex device URL/code from remote login output and drives the local dedicated Codex Teams auth-browser profile; prefer waiting for that automation over asking the user to perform low-level placement/auth steps. Prefer adding or resuming a department on the right node over asking the user to provide low-level placement details.

Docker/container policy: this applies even when Docker is introduced by a skill, a department plan, or implementation needs rather than by the user's initial wording. Do not assign a department to a Docker node merely because the user asked to build a Docker image; first create or discover the real container on the correct host. Prefer `team node create-docker` for team-managed containers because it creates a long-lived container with stable naming, workspace mounts, optional ports/GPU, and node registration in one step. If a host/ops department already owns container creation, do not race it by creating a second container yourself; tell that department to use `team node create-docker` or report one real container, then choose exactly one active Docker node and remove stale duplicates. Docker and ssh-docker nodes automatically get a container-internal department if no member is already assigned there, so as soon as a container is created and registered, at least one container-internal session should join the team and coordinate like local/SSH departments.

Hard Docker ownership boundary: for main task execution, the host/SSH department may build the image and create/register/replace the long-lived container, but it must stop there and hand off. The container-internal department owns package installs inside the container, sample/model/application execution, rendering, tests, debugging, and final container-local verification. Do not accept a final result for a Docker-based task unless a Docker or ssh-docker node was registered and a container-internal department actually started and participated after container creation. If a host department continues the main run with `docker run`/`docker exec` after the container should have become a node, redirect it to create/register the node and resume the container department instead.

If CUDA, base image, driver, library, port, or mount choices turn out wrong, you are responsible for rebuilding/replacing the container and keeping the team node valid; the user should not need to provide new flags. Reusing the same stable container name is acceptable: update the node if cwd/mount/port/context changed, then resume or message the existing container department rather than creating duplicate departments. If a department or skill creates a container manually that should host ongoing team work, create it with a stable name, mount the relevant workspace (for example `-v "$PWD:/workspace" -w /workspace`), publish any user-facing service ports with `-p host_port:container_port`, and keep it alive long enough for app-server bootstrap. Avoid read-write mounting the host's entire `~/.codex` into a root-owned container; use `team node sync-assets`, a dedicated Codex home, copied credentials/config, or the existing bootstrap/auth flow so host config ownership is not changed. Then register it as a node with `team node add --kind docker --container <name> --cwd /workspace` for local Docker, or `--kind ssh-docker --host <ssh-host> --container <name> --cwd /workspace` for Docker on an SSH host. If a department can report but cannot run the local team CLI, tell it to emit `TEAM_NODE id=<node-id> kind=<docker|ssh-docker> host=<ssh-host-or-> container=<container> cwd=<container-cwd> note=<short_note>` on its own line; the orchestrator will register that node and add the container department. For SSH-host Docker, run Docker creation/removal on that SSH host, then register the resulting `ssh-docker` node. If a container is rebuilt or replaced, update/remove the old node and add the new container node before assigning departments.

Remote/container artifact handoff policy: when a non-local department needs a local artifact, schema package, report, source matrix, config, or generated input that is not mounted on its node, do not ask the user to copy it manually and do not let the remote/container department recreate stale copies. Use `team node sync-path <node-id> --src <local-path> --dest <node-path> [--replace]` to package the authoritative local artifact into the node workspace, then notify the consumer department with the exact destination path and expected hashes/manifests. Before clearing a remote/container runtime or validation task, inspect the task text, latest method contract, and previous audit/validation recommendation for all required predecessor artifacts, not just the immediate producer package. Sync and root-correct verify every required prior audit, validation report, source matrix, config, generated input, and method package that the contract names; if any is missing, keep the task waiting/blocked and resume lead/ops to sync it before runtime starts. Treat missing handoff files as a blocker until the sync happens.

Tooling policy: lead should expect departments to install missing task tools instead of downgrading work quality. If `team node inspect` or a department report shows missing Node.js, Python tooling, browsers, build tools, CUDA libraries, package managers, or test utilities, instruct the responsible department to install what is needed on its own node and verify with the best practical checks. In Docker containers, root installs are acceptable. On SSH/local nodes, use project-local or user-local installs first, and passwordless sudo (`sudo -n`) only when available. Do not require user intervention for ordinary package installs. Ask for a fallback only when install is impossible, unsafe, or requires an interactive password.

For any long-running or externally-completed work, make the completion condition explicit. Use `team job start/status/logs/artifact` for PID-backed commands that the team CLI can run and inspect. Use `team wait add/list/set` for anything with a completion condition but no reliable team-managed PID, including tool/API polling, service-side processing, human/account/credential gates, external queues, remote workflows owned by another process, or any other waitable dependency. Do not hardcode the category: if a task cannot continue until some observable condition becomes true, register a wait with owner, task, condition, progress/request/log identifiers, and final evidence. A task with an open wait is not complete; when the wait is completed/failed/blocked, resume the owner to inspect the result and publish the real handoff, next action, or blocker.

Collaboration policy: departments should over-communicate compared with a solo Codex session. Require each nontrivial department to broadcast an initial plan, ask producer/consumer departments for judgment on uncertain choices, report failures with exact logs and proposed next actions, and hand off artifacts to the departments that must consume or review them. Departments have different natural speeds; do not equate slower output with failure, and do not push for low-quality premature artifacts just to satisfy a heartbeat. For slow or quiet work, require status evidence, current subtask, running tool/job/MCP details, risks, and the next checkpoint. Departments are also allowed to act as observers: if they see a blocked/pending/review task that appears ready or misassigned, they should send lead a `LEAD_PROPOSAL:` with evidence instead of silently waiting or starting unassigned work. Treat proposals as advisory signals; validate them against current tasks, ownerships, mailboxes, jobs, and artifacts before resuming or reassigning anyone. A completed task without a `TEAM_COMPLETION_CHECKLIST` in the department's final response is not a clean completion; resume that department with a concrete mission to send missing messages, evidence, verification, and handoff paths instead of doing its work yourself. If a department ends too quickly after a substantial mission, treat that as suspicious until its checklist and mailbox messages prove real work or a valid blocker.

Idle outreach policy: keep-alive may periodically send messages from standby/completed departments to active or blocked departments asking if help is needed. Treat useful replies as a signal to resume the helper with a concrete mission or route the question to the right owner. Standby/completed departments may also send `LEAD_PROPOSAL:` if they notice a cleared blocker, duplicate task, missing owner, or ready review gate. Do not turn outreach into busywork; if nobody needs help and no proposal is useful, no action is required.

During keep-alive, keep placement dynamic just like departments: add nodes when new SSH/Docker work appears, add or resume departments on those nodes when useful, and remove nodes only when no active department needs them. Be conservative with removal: standby departments may still answer questions, so remove a node only after its departments are standby/completed and no follow-up is likely. Prefer standby for departments; use node removal for stale containers, recreated containers, or unreachable placement candidates.

If a department reports that it is blocked on a gate or handoff, that is not completion. Leave or move it to standby/blocked. When the required handoff arrives, explicitly resume that department with a concrete mission instead of assuming the old completed turn will continue automatically. If another department notices that the handoff has arrived and proposes a resume, verify the evidence and then act or explain why not.

During the run, add a new department only when the existing departments cannot reasonably cover a distinct ownership domain. When teammate messages arrive later, the orchestrator may either steer this active turn or start a new lead turn in this same thread. Reply with decisions, unblockers, ownership changes, placement changes, department changes, or handoffs. Keep each lead turn short and finish when no coordination is needed.

Team members:
{member_lines}

Current tasks:
{task_text}
"#,
        team_id = config.id,
        goal = config.goal,
        member_name = member.name,
        role = member.role,
        codex = codex_exe.display(),
        member_lines = member_lines,
        task_text = if task_text.is_empty() {
            "(none)".to_string()
        } else {
            task_text
        },
    );
    localize_team_prompt(prompt, language)
}

fn build_reactive_lead_turn_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    codex_exe: &Path,
    team_id: &str,
    language: TeamPromptLanguage,
) -> String {
    let message_lines = messages
        .iter()
        .map(|message| {
            format!(
                "- [{}] {} -> {}: {}",
                message.timestamp, message.from, message.to, message.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        r#"Reactive lead update for {member} ({role}).

New message(s) arrived for lead while the lead turn was idle:
{message_lines}

Use the team CLI if you need context:
- "{codex}" team status --team "{team_id}"
- "{codex}" team node --team "{team_id}" list
- "{codex}" team node --team "{team_id}" inspect [node-id]
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh --host <ssh-host> --cwd <remote-cwd>
- "{codex}" team node --team "{team_id}" add <node-id> --kind docker --container <container> --cwd <container-cwd>
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh-docker --host <ssh-host> --container <container> --cwd <container-cwd>
- "{codex}" team node --team "{team_id}" create-docker <node-id> [--host <ssh-host>] --image <image> --mount <host:container> --port <host:container> --gpus --replace
- "{codex}" team node --team "{team_id}" sync-assets <node-id> [--include-auth]
- "{codex}" team node --team "{team_id}" sync-path <node-id> --src <local-path> --dest <node-path> [--replace]
- "{codex}" team node --team "{team_id}" remove <node-id> --force
- "{codex}" team job --team "{team_id}" start --owner lead --task <TASK_ID> --node <node-id> --cwd <cwd> -- <command...>
- "{codex}" team job --team "{team_id}" status <job-id>
- "{codex}" team job --team "{team_id}" logs <job-id> --tail 80
- "{codex}" team wait --team "{team_id}" add "<title>" --owner <department> --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
- "{codex}" team wait --team "{team_id}" list [--owner <department>] [--task <TASK_ID>]
- "{codex}" team wait --team "{team_id}" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
- "{codex}" team task --team "{team_id}" list
- "{codex}" team task --team "{team_id}" set <TASK_ID> --status <status> --depends-on <DEP_TASK_ID> [--depends-on <DEP_TASK_ID>...] --result "<why>"
- "{codex}" team ownership --team "{team_id}" list
- "{codex}" team member --team "{team_id}" list
- "{codex}" team member --team "{team_id}" add <name:role> --node <node-id> --mission "<why this department is needed>"
- "{codex}" team member --team "{team_id}" standby <member> --reason "<why active work is no longer needed>"
- "{codex}" team inbox --team "{team_id}" lead

Respond as lead only if coordination, prioritization, clarification, ownership reassignment, placement add/remove, department add/standby/resume, job/wait tracking, tooling setup, a handoff, or a `LEAD_PROPOSAL:` is useful. Current-run mailbox messages and team-owned artifacts are authoritative; stale files/images/containers from earlier teams should not override the current plan unless you deliberately adopt them with provenance and require fresh validation. If a message reveals SSH/Docker/container work, inspect/create/update the placement node and assign/resume a department there. If a teammate sends `LEAD_PROPOSAL:`, treat it as advisory: inspect the cited task, dependency, artifact, job, wait, mailbox, and ownership state, then either resume/reassign/merge/cancel with a concrete instruction or reply why the proposal is premature. If a blocked department's gate has cleared, resume it with a concrete next mission instead of treating its earlier waiting turn as completed work. If a run hits a gated/private/401/403 external dependency or unprovided credential, preserve the evidence, keep QA blocked, and resume research/ops to find a documented public fallback or choose another current runnable option; do not accept partial output as completion. If a skill or department created/recreated a Docker container, register or update the Docker node immediately and let the auto-added container department take over work inside that container. Keep exactly one active Docker node for a given purpose; if lead and a department both created containers, choose the intended active node, standby the duplicate container department, and remove the stale node so its tasks are cancelled. If the host/SSH department is about to continue the main task through `docker run`/`docker exec`, stop it at image/container creation and redirect runtime execution, installs, rendering, and verification to the container-internal department. If a teammate reports a missing normal tool or weakens verification because something is unavailable, tell that department to install the needed dependency on its node when reasonable; Docker root installs and passwordless sudo/user-local installs are allowed. If a teammate starts long-running work, use `team job --owner <department> --task <task-id>` when there is a trackable command PID, or `team wait add --owner <department> --task <task-id>` when completion depends on an external/non-PID condition; require an exact condition and later evidence. If a department completed without a TEAM_COMPLETION_CHECKLIST or without notifying consumers, resume it and demand the missing handoff/evidence; avoid substituting your own direct `team job` unless no department can reasonably own the work. Keep this short and concrete.
"#,
        member = member.name,
        role = member.role,
        codex = codex_exe.display(),
        team_id = team_id,
        message_lines = message_lines,
    );
    localize_team_prompt(prompt, language)
}

fn build_reactive_member_turn_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    codex_exe: &Path,
    team_id: &str,
    standby: bool,
    language: TeamPromptLanguage,
) -> String {
    let message_lines = messages
        .iter()
        .map(|message| {
            format!(
                "- [{}] {} -> {}: {}",
                message.timestamp, message.from, message.to, message.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mode = if standby {
        "You are currently in standby: answer questions, clarify prior work, and help with handoffs, but do not take new implementation work unless lead explicitly resumes you."
    } else {
        "Your main task turn has completed, but the team still needs a short follow-up answer or handoff."
    };

    let prompt = format!(
        r#"Reactive department follow-up for {member} ({role}).

{mode}

New teammate message(s):
{message_lines}

Use the team CLI if you need context:
- "{codex}" team status --team "{team_id}"
- "{codex}" team node --team "{team_id}" inspect [node-id]
- "{codex}" team task --team "{team_id}" list
- "{codex}" team task --team "{team_id}" claim [TASK_ID] --owner "{member}"
- "{codex}" team wait --team "{team_id}" list --owner "{member}"
- "{codex}" team wait --team "{team_id}" add "<title>" --owner "{member}" --task <TASK_ID> --condition "<exact completion condition>" --progress "<request id, URL, log path, checkpoint, or current state>"
- "{codex}" team wait --team "{team_id}" set <WAIT_ID> --status <waiting|running|polling|blocked|completed|failed|cancelled> --progress "<current state>" [--evidence <path-or-url>]
- "{codex}" team ownership --team "{team_id}" list
- "{codex}" team inbox --team "{team_id}" "{member}"
- "{codex}" team message --team "{team_id}" --from "{member}" lead "<answer, blocker, or handoff>"
- "{codex}" team message --team "{team_id}" --from "{member}" all "<short update when useful>"
- "{codex}" team message --team "{team_id}" --from "{member}" <member[,member...]> "<same answer or question to a small explicit group>"

If the follow-up asks for work that needs missing normal tools, install them when reasonable before weakening implementation or verification. Docker root installs, user-local installs, and passwordless sudo (`sudo -n`) are allowed; do not wait for an interactive sudo password.

If the follow-up exposes an uncertainty, missing input, weak result, long wait, or cross-department decision, ask the relevant department for judgment instead of answering only to lead. If work cannot continue until an observable condition becomes true, register/update a `team wait` with the exact condition, current progress/request/log/checkpoint, task, and evidence path when available. If the follow-up completes active work, include TEAM_COMPLETION_CHECKLIST in your final response with artifacts, verification, messages_sent, consumers_notified, and blockers_or_limits.
If this is an idle outreach message and you need help, reply with the exact blocker/question and what kind of help would unblock you. If you do not need help, no reply is required.

If lead or the task table exposes an unassigned `ready` task that clearly matches your department mission, you may claim it with `team task claim`, then message lead with the reason and intended artifacts. Otherwise do not start unrelated work from a follow-up.

Respond only if useful. Send concise team messages for answers, handoffs, installed tooling, verification results, blockers, or a self-claim notice, then finish.
"#,
        member = member.name,
        role = member.role,
        mode = mode,
        message_lines = message_lines,
        codex = codex_exe.display(),
        team_id = team_id,
    );
    localize_team_prompt(prompt, language)
}

fn build_app_server_lead_final_prompt(
    config: &TeamConfig,
    team_dir: &Path,
    language: TeamPromptLanguage,
) -> Result<String> {
    let tasks = load_tasks(team_dir)?;
    let task_text = tasks
        .iter()
        .map(|task| {
            format!(
                "- {} [{}] owner={} subject={} result={}",
                task.id,
                task.status,
                task.owner.as_deref().unwrap_or(""),
                task.subject,
                task.result.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let prompt = format!(
        r#"Produce the final lead synthesis for this Codex agent team.

Team: {team_id}
Goal: {goal}
Team state directory: {team_dir}

Summarize:
- what each member did
- task status and important messages
- changed files or outputs
- checks or review results
- unresolved risks and recommended next action

Current tasks:
{task_text}
"#,
        team_id = config.id,
        goal = config.goal,
        team_dir = team_dir.display(),
        task_text = if task_text.is_empty() {
            "(none)".to_string()
        } else {
            task_text
        },
    );
    Ok(localize_team_prompt(prompt, language))
}

fn build_lead_synthesis_prompt(team_dir: &Path) -> Result<String> {
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let task_text = tasks
        .iter()
        .map(|task| {
            format!(
                "- {} [{}] owner={} subject={} result={}",
                task.id,
                task.status,
                task.owner.as_deref().unwrap_or(""),
                task.subject,
                task.result.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let members = config
        .members
        .iter()
        .map(|member| {
            format!(
                "- {} ({}) status={:?} workspace={}",
                member.name,
                member.role,
                member.status,
                member.workspace_path.as_deref().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    Ok(format!(
        r#"You are the lead Codex agent for a local agent team.

Team: {team_id}
Goal: {goal}
Team state directory: {team_dir}

Use the normal Codex runtime exactly as configured for this machine. User config, skills, MCP servers, plugins, auth, model settings, and project instructions are available through this Codex session.

Read the team state, worker logs, final messages, and any worktree diffs. Produce a concise final synthesis for the user:
- what each member did
- task status and important results
- changed files or relevant diffs if any
- tests or checks run
- unresolved issues and recommended next action

Do not merge worktrees automatically unless the user explicitly requested auto-merge. If worktrees exist, summarize the branches and how to inspect or merge them.

Members:
{members}

Tasks:
{task_text}
"#,
        team_id = config.id,
        goal = config.goal,
        team_dir = team_dir.display(),
        members = members,
        task_text = if task_text.is_empty() {
            "(none)".to_string()
        } else {
            task_text
        },
    ))
}

fn load_team_summaries(root: &Path) -> Result<Vec<TeamConfig>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut teams = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir()
            && let Ok(config) = load_config(&entry.path())
        {
            teams.push(config);
        }
    }
    Ok(teams)
}

fn resolve_team_dir(root: &Path, team: Option<&str>) -> Result<PathBuf> {
    match team {
        Some(team) => {
            let dir = root.join(sanitize_id(team));
            if !dir.join("config.json").exists() {
                bail!("team `{team}` does not exist");
            }
            Ok(dir)
        }
        None => {
            let teams = load_team_summaries(root)?;
            let latest = teams
                .into_iter()
                .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
                .ok_or_else(|| anyhow!("no teams found; run `codex team start` first"))?;
            Ok(root.join(latest.id))
        }
    }
}

fn load_config(team_dir: &Path) -> Result<TeamConfig> {
    read_json(&team_dir.join("config.json"))
        .with_context(|| format!("failed to read {}", team_dir.join("config.json").display()))
}

fn touch_config(team_dir: &Path) -> Result<()> {
    let mut config = load_config(team_dir)?;
    config.updated_at = now();
    write_json_atomic(&team_dir.join("config.json"), &config)
}

fn load_tasks(team_dir: &Path) -> Result<Vec<TeamTask>> {
    let task_dir = team_dir.join("tasks");
    if !task_dir.exists() {
        return Ok(Vec::new());
    }
    let mut tasks = Vec::new();
    for entry in fs::read_dir(task_dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
        {
            tasks.push(read_json::<TeamTask>(&entry.path())?);
        }
    }
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(tasks)
}

fn jobs_dir(team_dir: &Path) -> PathBuf {
    team_dir.join("jobs")
}

fn job_path(team_dir: &Path, job_id: &str) -> PathBuf {
    jobs_dir(team_dir).join(format!("{}.json", sanitize_id(job_id)))
}

fn waits_dir(team_dir: &Path) -> PathBuf {
    team_dir.join("waits")
}

fn wait_path(team_dir: &Path, wait_id: &str) -> PathBuf {
    waits_dir(team_dir).join(format!("{}.json", sanitize_id(wait_id)))
}

fn load_job(team_dir: &Path, job_id: &str) -> Result<TeamJob> {
    read_json(&job_path(team_dir, job_id)).with_context(|| format!("failed to read job `{job_id}`"))
}

fn load_jobs(team_dir: &Path) -> Result<Vec<TeamJob>> {
    let dir = jobs_dir(team_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut jobs = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
        {
            jobs.push(read_json::<TeamJob>(&entry.path())?);
        }
    }
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(jobs)
}

fn load_wait(team_dir: &Path, wait_id: &str) -> Result<TeamWait> {
    read_json(&wait_path(team_dir, wait_id))
        .with_context(|| format!("failed to read wait `{wait_id}`"))
}

fn load_waits(team_dir: &Path) -> Result<Vec<TeamWait>> {
    let dir = waits_dir(team_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut waits = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("json")
        {
            waits.push(read_json::<TeamWait>(&entry.path())?);
        }
    }
    waits.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(waits)
}

fn allocate_job_id(team_dir: &Path) -> Result<String> {
    fs::create_dir_all(jobs_dir(team_dir))?;
    let mut high = 0_u64;
    for job in load_jobs(team_dir)? {
        if let Some(number) = job.id.strip_prefix("job-")
            && let Ok(number) = number.parse::<u64>()
        {
            high = high.max(number);
        }
    }
    Ok(format!("job-{}", high + 1))
}

fn allocate_wait_id(team_dir: &Path) -> Result<String> {
    fs::create_dir_all(waits_dir(team_dir))?;
    let mut high = 0_u64;
    for wait in load_waits(team_dir)? {
        if let Some(number) = wait.id.strip_prefix("wait-")
            && let Ok(number) = number.parse::<u64>()
        {
            high = high.max(number);
        }
    }
    Ok(format!("wait-{}", high + 1))
}

fn load_node_for_job(team_dir: &Path, job: &TeamJob) -> Result<TeamNode> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    nodes
        .into_iter()
        .find(|node| node.id == job.node)
        .with_context(|| format!("node `{}` for job `{}` not found", job.node, job.id))
}

fn nodes_path(team_dir: &Path) -> PathBuf {
    team_dir.join("nodes.json")
}

fn load_nodes(team_dir: &Path) -> Result<Vec<TeamNode>> {
    let path = nodes_path(team_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_json(&path).with_context(|| format!("failed to read {}", path.display()))
}

fn write_nodes(team_dir: &Path, nodes: &[TeamNode]) -> Result<()> {
    write_json_atomic(&nodes_path(team_dir), nodes)
}

fn set_node_connection(
    team_dir: &Path,
    node_id: &str,
    status: TeamNodeStatus,
    url: Option<String>,
) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    let Some(node) = nodes.iter_mut().find(|node| node.id == node_id) else {
        append_event(
            team_dir,
            "node_status_update_skipped",
            serde_json::json!({
                "node": node_id,
                "status": status,
                "reason": "node not registered",
            }),
        )?;
        return Ok(());
    };
    node.status = status;
    if let Some(url) = url {
        node.url = Some(url);
    }
    node.updated_at = now();
    write_nodes(team_dir, &nodes)?;
    Ok(())
}

fn ensure_local_node(nodes: &mut Vec<TeamNode>) {
    if nodes.iter().any(|node| node.id == "local") {
        return;
    }
    let now = now();
    nodes.push(TeamNode {
        id: "local".to_string(),
        kind: TeamNodeKind::Local,
        url: None,
        host: None,
        container: None,
        cwd: None,
        status: TeamNodeStatus::Online,
        note: "Current machine; URL is resolved from --app-server-url, registry, or UI-managed app-server at run time.".to_string(),
        created_at: now.clone(),
        updated_at: now,
    });
}

fn ensure_node_exists(team_dir: &Path, node_id: &str) -> Result<()> {
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    if nodes.iter().any(|node| node.id == node_id) {
        Ok(())
    } else {
        bail!(
            "node `{node_id}` is not registered; run `codex team node add {node_id} --url ws://...` first"
        )
    }
}

fn load_ownerships(team_dir: &Path) -> Result<Vec<FileOwnership>> {
    let path = ownerships_path(team_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_json(&path).with_context(|| format!("failed to read {}", path.display()))
}

fn write_ownerships(team_dir: &Path, ownerships: &[FileOwnership]) -> Result<()> {
    write_json_atomic(&ownerships_path(team_dir), ownerships)
}

fn allocate_task_id(team_dir: &Path) -> Result<String> {
    let task_dir = team_dir.join("tasks");
    fs::create_dir_all(&task_dir)?;
    let mut high = 0_u64;
    for entry in fs::read_dir(&task_dir)? {
        let entry = entry?;
        if let Some(stem) = entry.path().file_stem().and_then(|stem| stem.to_str())
            && let Ok(n) = stem.parse::<u64>()
        {
            high = high.max(n);
        }
    }
    Ok((high + 1).to_string())
}

fn print_ownership(ownership: &FileOwnership) {
    let note = if ownership.note.trim().is_empty() {
        String::new()
    } else {
        format!("  {}", ownership.note)
    };
    println!(
        "  {:<24} {}{}",
        format!("@{}", ownership.owner),
        ownership.path,
        note
    );
}

fn print_task(task: &TeamTask) {
    let owner = task
        .owner
        .as_ref()
        .map(|owner| format!("@{owner}"))
        .unwrap_or_default();
    let deps = if task.depends_on.is_empty() {
        String::new()
    } else {
        format!(" deps:{}", task.depends_on.join(","))
    };
    println!(
        "  {:>3} {:<11} {:<16} {}{}",
        task.id, task.status, owner, task.subject, deps
    );
}

fn normalize_ownership_path(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        bail!("ownership path cannot be empty");
    }
    if trimmed.contains('\n') {
        bail!("ownership path cannot contain newlines");
    }
    let normalized = trimmed.trim_start_matches("./").replace('\\', "/");
    if normalized == "." || normalized.starts_with("../") || normalized == ".." {
        bail!("ownership path must stay inside the workspace");
    }
    Ok(normalized)
}

fn ensure_member_exists(config: &TeamConfig, name: &str) -> Result<()> {
    if config.members.iter().any(|member| member.name == name) {
        Ok(())
    } else {
        bail!("member `{name}` does not exist in team `{}`", config.id)
    }
}

fn resolve_message_recipients(config: &TeamConfig, from: &str, to: &str) -> Result<Vec<String>> {
    let mut raw_recipients = Vec::new();
    for recipient in to.split(',') {
        let trimmed = recipient.trim();
        if trimmed.is_empty() {
            bail!("message recipient list contains an empty recipient");
        }
        raw_recipients.push(trimmed.to_string());
    }
    if raw_recipients.is_empty() {
        bail!("message recipient cannot be empty");
    }

    let all_count = raw_recipients
        .iter()
        .filter(|recipient| recipient.as_str() == "all" || recipient.as_str() == "@all")
        .count();
    if all_count > 0 && raw_recipients.len() > 1 {
        bail!("recipient `all` cannot be combined with explicit recipients");
    }

    if raw_recipients.len() == 1
        && (raw_recipients[0].as_str() == "all" || raw_recipients[0].as_str() == "@all")
    {
        let recipients = config
            .members
            .iter()
            .filter(|member| member.name != from)
            .map(|member| member.name.clone())
            .collect::<Vec<_>>();
        if recipients.is_empty() {
            bail!("team `{}` has no recipients for broadcast", config.id);
        }
        return Ok(recipients);
    }

    let mut recipients = Vec::new();
    let mut seen = HashSet::new();
    for recipient in raw_recipients {
        let recipient = sanitize_id(&recipient);
        if recipient.is_empty() {
            bail!("message recipient list contains an empty recipient");
        }
        if recipient != "user" {
            ensure_member_exists(config, &recipient)?;
        }
        if seen.insert(recipient.clone()) {
            recipients.push(recipient);
        }
    }
    Ok(recipients)
}

fn default_team_member_name() -> String {
    std::env::var("CODEX_TEAM_MEMBER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "lead".to_string())
}

fn mailbox_path(team_dir: &Path, member: &str) -> PathBuf {
    team_dir
        .join("mailboxes")
        .join(format!("{}.jsonl", sanitize_id(member)))
}

fn task_path(team_dir: &Path, task_id: &str) -> PathBuf {
    team_dir
        .join("tasks")
        .join(format!("{}.json", sanitize_id(task_id)))
}

fn ownerships_path(team_dir: &Path) -> PathBuf {
    team_dir.join("ownerships.json")
}

fn append_event(team_dir: &Path, event: &str, data: serde_json::Value) -> Result<()> {
    let config = load_config(team_dir)?;
    let entry = Event {
        event,
        timestamp: now(),
        team: &config.id,
        data,
    };
    append_jsonl(&team_dir.join("events.jsonl"), &entry)
}

fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

fn write_jsonl_atomic<T: Serialize>(path: &Path, values: &[T]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("jsonl.tmp");
    let mut out = String::new();
    for value in values {
        out.push_str(&serde_json::to_string(value)?);
        out.push('\n');
    }
    fs::write(&tmp, out)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn read_jsonl<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path)?;
    let mut values = Vec::new();
    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value = serde_json::from_str(line)
            .with_context(|| format!("failed to parse {} line {}", path.display(), idx + 1))?;
        values.push(value);
    }
    Ok(values)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path)?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn write_json_atomic<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("json")
    ));
    let json = serde_json::to_string_pretty(value)?;
    fs::write(&tmp, format!("{json}\n"))?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn write_text_atomic(path: &Path, value: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("txt")
    ));
    fs::write(&tmp, value)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn append_text(path: &Path, value: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(value.as_bytes())?;
    Ok(())
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn sanitize_role(value: &str) -> String {
    let role = value.trim();
    if role.is_empty() {
        "worker".to_string()
    } else {
        role.to_string()
    }
}

fn tokyo_offset() -> FixedOffset {
    FixedOffset::east_opt(9 * 60 * 60).expect("valid Tokyo UTC offset")
}

fn tokyo_now() -> DateTime<FixedOffset> {
    Utc::now().with_timezone(&tokyo_offset())
}

fn timestamp_for_ui(value: &str) -> String {
    DateTime::parse_from_rfc3339(value)
        .map(|timestamp| {
            timestamp
                .with_timezone(&tokyo_offset())
                .to_rfc3339_opts(SecondsFormat::Secs, true)
        })
        .unwrap_or_else(|_| value.to_string())
}

fn now() -> String {
    tokyo_now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn write_test_config(team_dir: &Path) {
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-task-test".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "engineering".to_string(),
                    role: "engineering".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "quality".to_string(),
                    role: "quality".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
    }

    fn write_test_task(
        team_dir: &Path,
        id: &str,
        owner: Option<&str>,
        status: TaskStatus,
        depends_on: Vec<&str>,
        result: Option<&str>,
    ) {
        let now = now();
        write_json_atomic(
            &task_path(team_dir, id),
            &TeamTask {
                id: id.to_string(),
                subject: format!("task {id}"),
                description: String::new(),
                owner: owner.map(str::to_string),
                status,
                depends_on: depends_on.into_iter().map(str::to_string).collect(),
                result: result.map(str::to_string),
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");
    }

    fn write_test_job(
        team_dir: &Path,
        id: &str,
        owner: Option<&str>,
        task_id: Option<&str>,
        status: TeamJobStatus,
        created_at: &str,
    ) {
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_json_atomic(
            &job_path(team_dir, id),
            &TeamJob {
                id: id.to_string(),
                node: "local".to_string(),
                command: "true".to_string(),
                cwd: team_dir.display().to_string(),
                owner: owner.map(str::to_string),
                task_id: task_id.map(str::to_string),
                status,
                pid: None,
                log_path: team_dir.join(format!("{id}.log")).display().to_string(),
                exit_path: team_dir.join(format!("{id}.exit")).display().to_string(),
                exit_code: None,
                note: String::new(),
                artifacts: Vec::new(),
                created_at: created_at.to_string(),
                updated_at: created_at.to_string(),
            },
        )
        .expect("write job");
    }

    fn write_test_wait(
        team_dir: &Path,
        id: &str,
        owner: Option<&str>,
        task_id: Option<&str>,
        status: TeamWaitStatus,
    ) {
        fs::create_dir_all(team_dir.join("waits")).expect("waits dir");
        let now = now();
        write_json_atomic(
            &wait_path(team_dir, id),
            &TeamWait {
                id: id.to_string(),
                title: format!("wait {id}"),
                owner: owner.map(str::to_string),
                task_id: task_id.map(str::to_string),
                node: None,
                condition: "condition becomes true".to_string(),
                status,
                progress: "still waiting".to_string(),
                evidence: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write wait");
    }

    #[test]
    fn completed_job_artifact_revives_blocked_owner_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mut config = load_config(team_dir).expect("config");
        config.language = Some(TeamPromptLanguage::Ja);
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            Vec::new(),
            Some("Job completed before artifact registration."),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        write_test_job(
            team_dir,
            "job-1",
            Some("engineering"),
            Some("2"),
            TeamJobStatus::Completed,
            &old,
        );

        add_job_artifact(
            team_dir,
            JobArtifactArgs {
                id: "job-1".to_string(),
                path: "/tmp/report.txt".to_string(),
                note: "remote evidence".to_string(),
            },
        )
        .expect("add artifact");

        let task = read_json::<TeamTask>(&task_path(team_dir, "2")).expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("/tmp/report.txt"))
        );
        let config = load_config(team_dir).expect("config");
        let engineering = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(engineering.status, MemberStatus::Online);
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox");
        assert!(engineering_messages.iter().any(|message| {
            message.message.contains("JOB_STATUS")
                && message.message.contains("確認してください")
                && message.message.contains("TEAM_COMPLETION_CHECKLIST")
        }));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "job_artifact_requires_owner_handoff")
        );
    }

    #[test]
    fn open_wait_blocks_task_completion() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "7",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        write_test_wait(
            team_dir,
            "wait-1",
            Some("engineering"),
            Some("7"),
            TeamWaitStatus::Polling,
        );

        let changed = set_task_status_if_open(team_dir, "7", TaskStatus::Completed, Some("done"))
            .expect("set task");

        assert!(changed);
        let task = read_json::<TeamTask>(&task_path(team_dir, "7")).expect("task");
        assert_eq!(task.status, TaskStatus::Blocked);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("open wait item"))
        );
    }

    #[test]
    fn wait_completion_resumes_owner_for_handoff() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "8",
            Some("engineering"),
            TaskStatus::Waiting,
            Vec::new(),
            Some("Waiting on wait-1"),
        );
        write_test_wait(
            team_dir,
            "wait-1",
            Some("engineering"),
            Some("8"),
            TeamWaitStatus::Polling,
        );

        set_team_wait(
            team_dir,
            WaitSetArgs {
                id: "wait-1".to_string(),
                status: Some(TeamWaitStatus::Completed),
                progress: Some("finished".to_string()),
                evidence: Some("/tmp/result.json".to_string()),
                clear_evidence: false,
            },
        )
        .expect("set wait");

        let task = read_json::<TeamTask>(&task_path(team_dir, "8")).expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        let config = load_config(team_dir).expect("config");
        let engineering = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(engineering.status, MemberStatus::Online);
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("messages");
        assert!(messages.iter().any(|message| {
            message.message.contains("WAIT_STATUS") && message.message.contains("wait-1")
        }));
    }

    #[test]
    fn task_watchdog_revives_blocked_task_with_existing_completed_job_artifact() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            Vec::new(),
            Some("Waiting for artifact handoff."),
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "2")).expect("task");
        task.updated_at = old.clone();
        write_json_atomic(&task_path(team_dir, "2"), &task).expect("write task");
        write_test_job(
            team_dir,
            "job-1",
            Some("engineering"),
            Some("2"),
            TeamJobStatus::Completed,
            &old,
        );
        let mut job = load_job(team_dir, "job-1").expect("job");
        job.artifacts.push(TeamArtifact {
            path: "/tmp/report.txt".to_string(),
            note: "remote evidence".to_string(),
            created_at: old,
        });
        write_json_atomic(&job_path(team_dir, "job-1"), &job).expect("write job");

        let config = load_config(team_dir).expect("config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let task = read_json::<TeamTask>(&task_path(team_dir, "2")).expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        let config = load_config(team_dir).expect("config");
        let engineering = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(engineering.status, MemberStatus::Online);
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "task_watchdog_completed_artifact_revival"
                && event.data.get("job").and_then(|value| value.as_str()) == Some("job-1")
        }));
    }

    #[test]
    fn task_status_counts_separate_completed_open_cancelled_and_failed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::Completed,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "3",
            Some("quality"),
            TaskStatus::Cancelled,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "4",
            Some("quality"),
            TaskStatus::Failed,
            Vec::new(),
            None,
        );

        let tasks = load_tasks(team_dir).expect("tasks");

        assert_eq!(
            format_task_status_counts(&tasks),
            "1 completed, 1 open, 1 cancelled, 1 failed, 4 total"
        );
    }

    #[test]
    fn failed_tasks_are_not_open_for_member_state() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::Failed,
            Vec::new(),
            None,
        );
        let tasks = load_tasks(team_dir).expect("tasks");

        assert!(!task_is_open(&tasks[0]));
        assert_eq!(
            member_task_status_summary(&tasks, "engineering"),
            "no_open_tasks"
        );
    }

    #[test]
    fn status_text_includes_node_last_seen_timestamp() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_nodes(
            team_dir,
            &[TeamNode {
                id: "remote".to_string(),
                kind: TeamNodeKind::Ssh,
                url: Some("ws://127.0.0.1:9999".to_string()),
                host: Some("remote-host".to_string()),
                container: None,
                cwd: Some("/work".to_string()),
                status: TeamNodeStatus::Online,
                note: String::new(),
                created_at: "2026-05-01T00:00:00Z".to_string(),
                updated_at: "2026-05-08T06:41:31Z".to_string(),
            }],
        )
        .expect("write nodes");

        let status = format_status_text(team_dir).expect("status");

        assert!(status.contains(
            "remote Ssh Online url=ws://127.0.0.1:9999 last_seen=2026-05-08T06:41:31Z age="
        ));
        assert!(status.contains(" stale"));
    }

    #[test]
    fn status_text_includes_member_unread_mail_counts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "user".to_string(),
                to: "lead".to_string(),
                message: "continue the loop".to_string(),
                timestamp: now(),
                read: false,
            },
        )
        .expect("unread message");
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "system".to_string(),
                to: "lead".to_string(),
                message: "old tick".to_string(),
                timestamp: now(),
                read: true,
            },
        )
        .expect("read message");

        let status = format_status_text(team_dir).expect("status");

        assert!(status.contains(
            "lead (lead) session=Online tasks=no_open_tasks node=local unread=1 direct=1"
        ));
        assert!(status.contains(
            "engineering (engineering) session=Standby tasks=no_open_tasks node=local unread=0 direct=0"
        ));
    }

    #[test]
    fn compact_duration_formats_status_ages() {
        assert_eq!(format_compact_duration(42), "42s");
        assert_eq!(format_compact_duration(125), "2m5s");
        assert_eq!(format_compact_duration(3_900), "1h5m");
        assert_eq!(format_compact_duration(176_400), "2d1h");
    }

    #[test]
    fn status_text_includes_usage_limit_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let retry_at = (Local::now() + chrono::Duration::hours(2))
            .format("%B %-d, %Y %-I:%M %p")
            .to_string();
        append_event(
            team_dir,
            "app_server_member_usage_limited",
            serde_json::json!({
                "member": "lead",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": format!("You've hit your usage limit. Visit settings or try again at {retry_at}."),
                "retry_after_sec": 2700,
            }),
        )
        .expect("usage event");

        let status = format_status_text(team_dir).expect("status");

        assert!(status.contains("Cooldowns:\n"));
        assert!(status.contains("lead usage_limit retry_in="));
    }

    #[test]
    fn message_cli_allows_reserved_system_sender() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);

        send_message(
            root,
            MessageArgs {
                selector: TeamSelector {
                    team: Some("team-task-test".to_string()),
                },
                from: Some("system".to_string()),
                to: "lead".to_string(),
                message: "gate correction".to_string(),
            },
        )
        .expect("send system message");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(&team_dir, "lead")).expect("lead mailbox");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from, "system");
        assert_eq!(messages[0].message, "gate correction");
    }

    #[test]
    fn message_cli_allows_comma_separated_recipients() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);

        send_message(
            root,
            MessageArgs {
                selector: TeamSelector {
                    team: Some("team-task-test".to_string()),
                },
                from: Some("lead".to_string()),
                to: "engineering, quality,engineering".to_string(),
                message: "please coordinate".to_string(),
            },
        )
        .expect("send message");

        let engineering =
            read_jsonl::<MailMessage>(&mailbox_path(&team_dir, "engineering")).expect("mailbox");
        let quality =
            read_jsonl::<MailMessage>(&mailbox_path(&team_dir, "quality")).expect("mailbox");
        assert_eq!(engineering.len(), 1);
        assert_eq!(quality.len(), 1);
        assert_eq!(engineering[0].message, "please coordinate");
        assert_eq!(quality[0].message, "please coordinate");

        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        let event = events
            .iter()
            .find(|event| event.event == "message_sent")
            .expect("message_sent event");
        assert_eq!(
            event.data.get("to").expect("to"),
            &serde_json::json!(["engineering", "quality"])
        );
    }

    #[test]
    fn message_cli_rejects_all_mixed_with_explicit_recipient() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);

        let err = send_message(
            root,
            MessageArgs {
                selector: TeamSelector {
                    team: Some("team-task-test".to_string()),
                },
                from: Some("lead".to_string()),
                to: "all,quality".to_string(),
                message: "ambiguous".to_string(),
            },
        )
        .expect_err("mixed all should fail");

        assert!(
            err.to_string()
                .contains("recipient `all` cannot be combined")
        );
    }

    #[test]
    fn job_list_filters_by_owner_task_status_and_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_job(
            team_dir,
            "job-old",
            Some("engineering"),
            Some("34"),
            TeamJobStatus::Completed,
            "2026-05-08T08:00:00Z",
        );
        write_test_job(
            team_dir,
            "job-active",
            Some("engineering"),
            Some("34"),
            TeamJobStatus::Running,
            "2026-05-08T08:10:00Z",
        );
        write_test_job(
            team_dir,
            "job-other-task",
            Some("engineering"),
            Some("35"),
            TeamJobStatus::Running,
            "2026-05-08T08:20:00Z",
        );
        write_test_job(
            team_dir,
            "job-other-owner",
            Some("quality"),
            Some("34"),
            TeamJobStatus::Running,
            "2026-05-08T08:30:00Z",
        );

        let output = format_jobs_text_filtered(
            team_dir,
            &JobListArgs {
                owner: Some("engineering".to_string()),
                task: Some("34".to_string()),
                status: Some(TeamJobStatus::Running),
                limit: Some(1),
            },
        )
        .expect("format jobs");

        assert!(output.contains("job-active"));
        assert!(!output.contains("job-old"));
        assert!(!output.contains("job-other-task"));
        assert!(!output.contains("job-other-owner"));
    }

    #[test]
    fn remote_bootstrap_installs_codex_team_wrapper_on_path() {
        let script = remote_app_server_bootstrap_script(
            "team-test",
            "http://127.0.0.1:12345",
            "ws://127.0.0.1:23456",
        );

        assert!(script.contains("$HOME/bin/.codex-team-real"));
        assert!(script.contains("export CODEX_TEAM_ID="));
        assert!(script.contains("CODEX_TEAM_ID:-"));
        assert!(script.contains("team-test"));
        assert!(script.contains("export CODEX_TEAM_RELAY_URL="));
        assert!(script.contains("CODEX_TEAM_RELAY_URL:-"));
        assert!(script.contains("http://127.0.0.1:12345"));
        assert!(
            script.contains("install -m 0755 \"$helper_real\" /usr/local/bin/.codex-team-real")
        );
        assert!(
            script.contains("install -m 0755 \"$HOME/bin/codex-team\" /usr/local/bin/codex-team")
        );
        assert!(script.contains("export PATH=\"$HOME/bin:/usr/local/bin:/root/bin:$PATH\""));
        assert!(script.contains("CODEX_TEAM_HELPER_TIMEOUT:-30s"));
    }

    #[test]
    fn codex_device_auth_parser_extracts_url_and_code() {
        let log =
            "Open https://auth.openai.com/codex/device and enter this one-time code: ABCD-EFGHI";
        let parsed = parse_codex_device_auth_from_log(log)
            .expect("parse")
            .expect("auth");
        assert_eq!(parsed.0, "https://auth.openai.com/codex/device");
        assert_eq!(parsed.1, "ABCDEFGHI");

        let default_url = parse_codex_device_auth_from_log("device code is ZYXW98765")
            .expect("parse")
            .expect("auth");
        assert_eq!(default_url.0, "https://auth.openai.com/codex/device");
        assert_eq!(default_url.1, "ZYXW98765");
    }

    #[test]
    fn auth_browser_helpers_parse_endpoint_and_code() {
        let log = "noise\nDevTools listening on ws://127.0.0.1:35833/devtools/browser/abc\n";
        let ws = parse_auth_browser_ws_from_log(log)
            .expect("parse")
            .expect("ws");
        assert_eq!(ws, "ws://127.0.0.1:35833/devtools/browser/abc".to_string());
        assert_eq!(
            cdp_http_from_ws(&ws).expect("http"),
            "http://127.0.0.1:35833".to_string()
        );
        assert_eq!(
            normalize_codex_device_code("abcd-efghi").expect("code"),
            "ABCDEFGHI".to_string()
        );
    }

    #[test]
    fn dependency_soft_wait_auto_promotes_to_ready_and_notifies_owner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            vec!["1"],
            None,
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert_eq!(promoted.len(), 1);
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Ready);
        assert_eq!(task.owner.as_deref(), Some("engineering"));
        assert_eq!(task.depends_on, vec!["1".to_string()]);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("Dependency gate cleared automatically"))
        );
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].message.contains("READY_TO_START: task 2"));
    }

    #[test]
    fn task_add_records_created_before_auto_unblocked() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let team_dir = root.join("team-task-test");
        write_test_config(&team_dir);
        write_test_task(
            &team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );

        run_task(
            root,
            TaskCli {
                selector: TeamSelector {
                    team: Some("team-task-test".to_string()),
                },
                subcommand: TaskSubcommand::Add(TaskAddArgs {
                    subject: "dependent task".to_string(),
                    description: String::new(),
                    owner: Some("engineering".to_string()),
                    depends_on: vec!["1".to_string()],
                }),
            },
        )
        .expect("task add");

        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        let task_created_index = events
            .iter()
            .position(|event| event.event == "task_created")
            .expect("task_created event");
        let task_unblocked_index = events
            .iter()
            .position(|event| event.event == "task_dependency_unblocked")
            .expect("task_dependency_unblocked event");
        assert!(
            task_created_index < task_unblocked_index,
            "task_created should be recorded before dependency unblocking"
        );
    }

    #[test]
    fn pending_dependency_task_soft_waits_until_dependencies_complete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            None,
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Pending,
            vec!["1"],
            None,
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Waiting);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("Soft-waiting"))
        );
    }

    #[test]
    fn task_set_replaces_dependencies_and_waits_when_gate_incomplete() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            None,
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );

        update_task(
            team_dir,
            TaskSetArgs {
                id: "2".to_string(),
                status: None,
                owner: None,
                clear_owner: false,
                depends_on: vec!["1".to_string()],
                clear_depends: false,
                result: None,
            },
        )
        .expect("update dependency");

        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.depends_on, vec!["1".to_string()]);
        assert_eq!(task.status, TaskStatus::Waiting);
        assert!(
            task.result
                .as_deref()
                .is_some_and(|result| result.contains("Waiting for dependency task(s): 1"))
        );
    }

    #[test]
    fn task_add_rejects_self_dependency() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);

        let err = create_task(
            team_dir,
            TaskAddArgs {
                subject: "bad task".to_string(),
                description: String::new(),
                owner: Some("engineering".to_string()),
                depends_on: vec!["1".to_string()],
            },
        )
        .expect_err("self dependency should be rejected");

        assert!(err.to_string().contains("task 1 cannot depend on itself"));
        assert!(!task_path(team_dir, "1").exists());
    }

    #[test]
    fn task_add_rejects_unknown_dependency() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(team_dir, "1", None, TaskStatus::Completed, Vec::new(), None);

        let err = create_task(
            team_dir,
            TaskAddArgs {
                subject: "bad future dependency".to_string(),
                description: String::new(),
                owner: Some("engineering".to_string()),
                depends_on: vec!["3".to_string()],
            },
        )
        .expect_err("unknown dependency should be rejected");

        assert!(err.to_string().contains("dependency task 3 not found"));
        assert!(!task_path(team_dir, "2").exists());
    }

    #[test]
    fn dependency_auto_promote_preserves_hard_block_reason() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            vec!["1"],
            Some("Waiting for user decision."),
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert_eq!(promoted.len(), 0);
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Blocked);
        assert_eq!(task.result.as_deref(), Some("Waiting for user decision."));
    }

    #[test]
    fn dependency_auto_promote_preserves_manual_reopen_wait() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "51",
            Some("method_schema"),
            TaskStatus::Completed,
            Vec::new(),
            Some("method handoff complete"),
        );
        write_test_task(
            team_dir,
            "52",
            Some("runtime_cycle2"),
            TaskStatus::Waiting,
            vec!["51"],
            Some(
                "Await lead sync/verification of input and method package, then explicit reopen. Runtime must not execute from READY_TO_START alone.",
            ),
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "52").expect("task 52");
        assert_eq!(task.status, TaskStatus::Waiting);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("explicit reopen") && !result.contains("Dependency gate cleared")
        }));
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "runtime_cycle2")).expect("mailbox");
        assert!(messages.is_empty());
    }

    #[test]
    fn dependency_auto_promote_holds_remote_contract_inputs_for_lead_clearance() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mut config = load_config(team_dir).expect("load config");
        let engineering = config
            .members
            .iter_mut()
            .find(|member| member.name == "engineering")
            .expect("engineering member");
        engineering.status = MemberStatus::Online;
        engineering.node = Some("remote-runtime".to_string());
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        let contract_dir = team_dir.join("method_schema").join("cycle23");
        fs::create_dir_all(&contract_dir).expect("contract dir");
        fs::write(
            contract_dir.join("runtime_contract.yaml"),
            format!(
                r#"runtime_task: 2
method_package:
  host_path: {}
  expected_container_input_root: /workspace/inputs/method_schema/cycle23
"#,
                contract_dir.display()
            ),
        )
        .expect("contract");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: contract_dir.display().to_string(),
                owner: "method_schema".to_string(),
                note: "Task1 Cycle23 contract".to_string(),
                updated_at: now(),
            }],
        )
        .expect("ownerships");
        write_test_task(
            team_dir,
            "1",
            Some("lead"),
            TaskStatus::Completed,
            Vec::new(),
            Some("done"),
        );
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Waiting,
            vec!["1"],
            Some("Waiting for dependency task(s): 1"),
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "2").expect("task 2");
        assert_eq!(task.status, TaskStatus::Waiting);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("contract-declared inputs")
                && result.contains("explicit lead root-correct verification clearance")
                && !result.contains("Dependency gate cleared automatically")
        }));
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox");
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(engineering_messages.len(), 1);
        assert_eq!(lead_messages.len(), 1);
        assert!(
            engineering_messages[0]
                .message
                .contains("AWAITING_LEAD_CLEARANCE: task 2")
        );
        assert!(!engineering_messages[0].message.contains("READY_TO_START"));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.event == "task_contract_input_clearance_required"
                        && event.data.get("task").is_some()
                })
                .count(),
            1
        );

        let promoted_again = auto_promote_dependency_waits(team_dir).expect("auto promote again");
        assert!(promoted_again.is_empty());
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox after second pass");
        let lead_messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead"))
            .expect("lead mailbox after second pass");
        assert_eq!(engineering_messages.len(), 1);
        assert_eq!(lead_messages.len(), 1);
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event == "task_contract_input_clearance_required")
                .count(),
            1
        );
    }

    #[test]
    fn claim_ready_task_self_assigns_first_unowned_ready_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(team_dir, "1", None, TaskStatus::Pending, Vec::new(), None);

        claim_ready_task(
            team_dir,
            TaskClaimArgs {
                id: None,
                owner: Some("quality".to_string()),
            },
        )
        .expect("claim ready task");

        let tasks = load_tasks(team_dir).expect("load tasks");
        let task = tasks.iter().find(|task| task.id == "1").expect("task 1");
        assert_eq!(task.owner.as_deref(), Some("quality"));
        assert_eq!(task.status, TaskStatus::InProgress);
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(lead_messages.len(), 1);
        assert!(lead_messages[0].message.contains("@quality claimed task 1"));
    }

    #[test]
    fn round_robin_does_not_assign_unowned_ready_tasks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            None,
            TaskStatus::Ready,
            Vec::new(),
            Some("ready for claim"),
        );

        assign_unowned_tasks_round_robin(team_dir).expect("assign tasks");

        let task = read_json::<TeamTask>(&task_path(team_dir, "1")).expect("task");
        assert_eq!(task.owner, None);
        assert_eq!(task.status, TaskStatus::Ready);
    }

    #[test]
    fn round_robin_still_assigns_unowned_pending_tasks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(team_dir, "1", None, TaskStatus::Pending, Vec::new(), None);

        assign_unowned_tasks_round_robin(team_dir).expect("assign tasks");

        let task = read_json::<TeamTask>(&task_path(team_dir, "1")).expect("task");
        assert_eq!(task.owner.as_deref(), Some("engineering"));
        assert_eq!(task.status, TaskStatus::Pending);
    }

    #[test]
    fn owned_pending_task_reactivates_completed_member() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mut config = load_config(team_dir).expect("load config");
        let member = config
            .members
            .iter_mut()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        member.status = MemberStatus::Completed;
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::Pending,
            Vec::new(),
            None,
        );

        let promoted = auto_promote_dependency_waits(team_dir).expect("auto promote");

        assert!(promoted.is_empty());
        let config = load_config(team_dir).expect("reload config");
        let member = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(member.status, MemberStatus::Online);
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].message.contains("READY_TO_START: task 1"));
    }

    #[test]
    fn inactive_member_mailbox_poll_does_not_advance_seen_count() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        send_team_message_to_dir(team_dir, "lead", "engineering", "START_NOW: task 1")
            .expect("send message");
        let config = load_config(team_dir).expect("load config");
        let member = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        let mut mailbox_counts = HashMap::new();

        let inactive_messages =
            collect_new_active_mailbox_messages(team_dir, member, false, &mut mailbox_counts)
                .expect("inactive poll");

        assert!(inactive_messages.is_none());
        assert!(mailbox_counts.is_empty());

        let active_messages =
            collect_new_active_mailbox_messages(team_dir, member, true, &mut mailbox_counts)
                .expect("active poll")
                .expect("pending delivery");

        assert_eq!(active_messages.messages.len(), 1);
        assert!(mailbox_counts.is_empty());
        assert!(active_messages.messages[0].message.contains("START_NOW"));
    }

    #[test]
    fn mailbox_counts_resume_from_first_unread_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mailbox = mailbox_path(team_dir, "engineering");
        write_jsonl_atomic(
            &mailbox,
            &[
                MailMessage {
                    from: "lead".to_string(),
                    to: "engineering".to_string(),
                    timestamp: now(),
                    message: "already handled".to_string(),
                    read: true,
                },
                MailMessage {
                    from: "lead".to_string(),
                    to: "engineering".to_string(),
                    timestamp: now(),
                    message: "resume this after restart".to_string(),
                    read: false,
                },
            ],
        )
        .expect("write mailbox");
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let config = load_config(team_dir).expect("load config");
        let tasks = load_tasks(team_dir).expect("tasks");

        let counts = current_mailbox_counts(team_dir, &config.members, &tasks).expect("counts");

        assert_eq!(counts.get("engineering"), Some(&1));
    }

    #[test]
    fn mailbox_counts_skip_old_worker_mail_when_no_open_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mailbox = mailbox_path(team_dir, "engineering");
        write_jsonl_atomic(
            &mailbox,
            &[
                MailMessage {
                    from: "lead".to_string(),
                    to: "engineering".to_string(),
                    timestamp: now(),
                    message: "old handoff".to_string(),
                    read: false,
                },
                MailMessage {
                    from: "system".to_string(),
                    to: "engineering".to_string(),
                    timestamp: now(),
                    message: "old heartbeat".to_string(),
                    read: false,
                },
            ],
        )
        .expect("write mailbox");
        let config = load_config(team_dir).expect("load config");
        let tasks = load_tasks(team_dir).expect("tasks");

        let counts = current_mailbox_counts(team_dir, &config.members, &tasks).expect("counts");

        assert_eq!(counts.get("engineering"), Some(&2));
        assert_eq!(counts.get("lead"), Some(&0));
    }

    #[test]
    fn next_action_signals_include_owned_final_audit_recommendation() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let audit_dir = team_dir.join("audit");
        fs::create_dir_all(&audit_dir).expect("audit dir");
        fs::write(
            audit_dir.join("cycle_final_audit.md"),
            "Verdict: PASS_WITH_WARNINGS\n\nRecommended next action: fix the runtime dataset layout and rerun the smoke test.\n",
        )
        .expect("write audit");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: audit_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "audit artifacts".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");

        let signals =
            collect_recent_next_action_signals(team_dir, 4).expect("collect next actions");

        assert_eq!(signals.len(), 1);
        assert!(signals[0].contains("cycle_final_audit.md"));
        assert!(signals[0].contains("fix the runtime dataset layout"));
    }

    #[test]
    fn remote_codex_dest_assignment_expands_home_on_remote() {
        let assignment = remote_codex_dest_assignment("$HOME/.codex");

        assert_eq!(assignment, "dest=\"${HOME:-/root}/.codex\"");
        assert!(!assignment.contains("'$HOME/.codex'"));
    }

    #[test]
    fn remote_codex_dest_assignment_quotes_literal_paths() {
        let assignment = remote_codex_dest_assignment("/root/.codex");

        assert_eq!(assignment, "dest='/root/.codex'");
    }

    #[test]
    fn remote_path_dest_assignment_expands_home_on_remote() {
        let assignment = remote_path_dest_assignment("$HOME/project/schema");

        assert_eq!(assignment, "dest=\"${HOME:-/root}/project/schema\"");
        assert!(!assignment.contains("'$HOME/project/schema'"));
    }

    #[test]
    fn path_sync_command_uses_node_destination_and_replace_guard() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("method_schema");
        fs::create_dir_all(&src).expect("src dir");
        fs::write(src.join("schema.json"), "{}").expect("src file");
        let node = TeamNode {
            id: "runtime".to_string(),
            kind: TeamNodeKind::SshDocker,
            status: TeamNodeStatus::Online,
            url: None,
            host: Some("saitou".to_string()),
            container: Some("runtime-container".to_string()),
            cwd: Some("/workspace".to_string()),
            note: String::new(),
            created_at: now(),
            updated_at: now(),
        };

        let (command, src_kind) =
            build_path_sync_command(&node, &src, "/workspace/inputs/method_schema", true)
                .expect("build command");

        assert_eq!(src_kind, "directory");
        assert!(command.contains("ssh 'saitou'"));
        assert!(command.contains("docker exec -i"));
        assert!(command.contains("runtime-container"));
        assert!(command.contains("/workspace/inputs/method_schema"));
        assert!(command.contains("replace=1"));
        assert!(command.contains(".codex-team-handoff-backups"));
    }

    #[test]
    fn runtime_contract_inputs_extract_host_to_container_pairs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let contract_dir = team_dir.join("method_schema").join("cycle20");
        let audit_dir = team_dir.join("audit").join("cycle19");
        fs::create_dir_all(&contract_dir).expect("contract dir");
        fs::create_dir_all(&audit_dir).expect("audit dir");
        fs::write(audit_dir.join("sha256_manifest.txt"), "").expect("manifest");
        fs::write(
            contract_dir.join("runtime_contract.yaml"),
            format!(
                r#"runtime_task: 95
method_package:
  host_path: {}
  expected_container_input_root: /workspace/inputs/method_schema/cycle20
authoritative_predecessor:
  audit_root_host: {}
  expected_container_root: /workspace/inputs/audit/cycle19
"#,
                contract_dir.display(),
                audit_dir.display()
            ),
        )
        .expect("contract");

        let map = load_contract_declared_inputs(&[FileOwnership {
            path: contract_dir.display().to_string(),
            owner: "method_schema".to_string(),
            note: "Task94 Cycle20 contract".to_string(),
            updated_at: now(),
        }])
        .expect("load inputs");
        let inputs = map.get("95").expect("runtime task inputs");

        assert!(inputs.iter().any(|input| {
            input.src == contract_dir
                && input.dest == "/workspace/inputs/method_schema/cycle20"
                && input.label == "host_path"
        }));
        assert!(inputs.iter().any(|input| {
            input.src == audit_dir
                && input.dest == "/workspace/inputs/audit/cycle19"
                && input.label == "audit_root_host"
        }));
    }

    #[test]
    fn contract_input_sync_warns_when_declared_source_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let contract_dir = team_dir.join("method_schema").join("cycle20");
        fs::create_dir_all(&contract_dir).expect("contract dir");
        let missing_audit = team_dir.join("audit").join("cycle19");
        fs::write(
            contract_dir.join("runtime_contract.yaml"),
            format!(
                r#"runtime_task: 95
authoritative_predecessor:
  audit_root_host: {}
  expected_container_root: /workspace/inputs/audit/cycle19
"#,
                missing_audit.display()
            ),
        )
        .expect("contract");
        let now_stamp = now();
        let config = TeamConfig {
            version: 1,
            id: "team-contract-sync".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now_stamp.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "runtime".to_string(),
                    role: "container".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now_stamp.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: Some("remote-runtime".to_string()),
                },
            ],
            language: None,
            created_at: now_stamp.clone(),
            updated_at: now_stamp.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "95"),
            &TeamTask {
                id: "95".to_string(),
                subject: "runtime gate".to_string(),
                description: String::new(),
                owner: Some("runtime".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now_stamp.clone(),
                updated_at: now_stamp,
            },
        )
        .expect("write task");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: contract_dir.display().to_string(),
                owner: "method_schema".to_string(),
                note: "Task94 Cycle20 contract".to_string(),
                updated_at: now(),
            }],
        )
        .expect("ownerships");
        let nodes = vec![TeamNode {
            id: "remote-runtime".to_string(),
            kind: TeamNodeKind::SshDocker,
            url: None,
            host: Some("saitou".to_string()),
            container: Some("runtime-container".to_string()),
            cwd: Some("/workspace".to_string()),
            status: TeamNodeStatus::Online,
            note: String::new(),
            created_at: now(),
            updated_at: now(),
        }];
        let mut attempts = HashSet::new();

        maybe_sync_contract_declared_inputs(team_dir, &config, &nodes, &mut attempts)
            .expect("sync warning");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let runtime_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "runtime")).expect("runtime inbox");
        assert_eq!(lead_messages.len(), 1);
        assert_eq!(runtime_messages.len(), 1);
        assert!(
            lead_messages[0]
                .message
                .contains("Contract-declared input sync warning")
        );
        assert!(lead_messages[0].message.contains("missing local source"));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "contract_declared_input_sync_missing_source"
                && event.data.get("task").and_then(|value| value.as_str()) == Some("95")
        }));
    }

    #[test]
    fn member_turn_has_completion_checklist_requires_all_fields() {
        let mut buffers = HashMap::new();
        buffers.insert(
            "ops".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: /tmp/out
- verification: smoke ok
- messages_sent: lead and audit
- consumers_notified: audit
- blockers_or_limits: none"#
                .to_string(),
        );

        assert!(member_turn_has_completion_checklist(&buffers, "ops"));

        buffers.insert(
            "ops".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: /tmp/out
- verification: smoke ok"#
                .to_string(),
        );

        assert!(!member_turn_has_completion_checklist(&buffers, "ops"));
    }

    #[test]
    fn active_task_completion_rejects_empty_artifacts_field() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "84",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut buffers = HashMap::new();
        buffers.insert(
            "engineering".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: none
- verification: checked status only
- messages_sent: lead
- consumers_notified: lead
- blockers_or_limits: none"#
                .to_string(),
        );

        let issue = member_turn_active_task_completion_issue(team_dir, &buffers, "engineering")
            .expect("completion issue")
            .expect("empty artifacts should be rejected");

        assert!(issue.contains("artifacts is empty"));
    }

    #[test]
    fn active_task_completion_requires_declared_remote_handoff_path_in_checklist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let now = now();
        write_json_atomic(
            &task_path(team_dir, "84"),
            &TeamTask {
                id: "84".to_string(),
                subject: "remote validation".to_string(),
                description: "Produce report/json/checklist under /workspace/runtime/schema_handoff_validation_cycle17.".to_string(),
                owner: Some("engineering".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");
        let mut buffers = HashMap::new();
        buffers.insert(
            "engineering".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: /workspace/runtime/other_validation
- verification: sha256sum -c rc=0
- messages_sent: lead and audit
- consumers_notified: audit
- blockers_or_limits: none"#
                .to_string(),
        );

        let issue = member_turn_active_task_completion_issue(team_dir, &buffers, "engineering")
            .expect("completion issue")
            .expect("missing required path should be rejected");

        assert!(issue.contains("schema_handoff_validation_cycle17"));
    }

    #[test]
    fn active_task_completion_accepts_declared_remote_handoff_path_in_checklist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let now = now();
        write_json_atomic(
            &task_path(team_dir, "84"),
            &TeamTask {
                id: "84".to_string(),
                subject: "remote validation".to_string(),
                description: "Produce report/json/checklist under /workspace/runtime/schema_handoff_validation_cycle17.".to_string(),
                owner: Some("engineering".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");
        let mut buffers = HashMap::new();
        buffers.insert(
            "engineering".to_string(),
            r#"TEAM_COMPLETION_CHECKLIST:
- artifacts: /workspace/runtime/schema_handoff_validation_cycle17/{report.md,validation.json,TEAM_COMPLETION_CHECKLIST.md,sha256_manifest.txt}
- verification: cd /workspace/runtime/schema_handoff_validation_cycle17 && sha256sum -c sha256_manifest.txt rc=0
- messages_sent: lead and audit
- consumers_notified: audit
- blockers_or_limits: bounded validation only"#
                .to_string(),
        );

        let issue = member_turn_active_task_completion_issue(team_dir, &buffers, "engineering")
            .expect("completion issue");

        assert_eq!(issue, None);
    }

    #[test]
    fn new_app_server_turn_resets_stale_assistant_buffer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_test_config(tmp.path());
        let config = load_config(tmp.path()).expect("config");
        let member = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering")
            .clone();
        let mut run = AppServerMemberRun {
            member,
            node_id: "local".to_string(),
            cwd: tmp.path().to_path_buf(),
            thread_id: "thread-1".to_string(),
            turn_id: "turn-old".to_string(),
            completed: true,
            failed: false,
            standby_after_turn: false,
            team_message_scan_offset: 42,
            last_activity_at: Instant::now(),
            last_activity_kind: "turn_completed".to_string(),
            last_stale_notice_at: None,
            retry_not_before: None,
            side_context_ids: Vec::new(),
        };
        let mut buffers = HashMap::from([(
            "engineering".to_string(),
            "old blocked output that must not affect the next turn".to_string(),
        )]);

        assert!(reset_member_turn_buffer_if_new(
            &mut run,
            &mut buffers,
            "engineering",
            "turn-new"
        ));

        assert_eq!(buffers.get("engineering").map(String::as_str), Some(""));
        assert_eq!(run.team_message_scan_offset, 0);
    }

    #[test]
    fn new_app_server_turn_rotates_live_message_to_last_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let live_path = team_dir.join("live_messages").join("engineering.md");
        let last_path = team_dir.join("last_messages").join("engineering.md");
        write_text_atomic(&live_path, "old live output\nold checklist\n").expect("write live");

        reset_member_live_message_for_new_turn(team_dir, "engineering", "turn-new")
            .expect("reset live");

        let live = fs::read_to_string(&live_path).expect("live");
        let last = fs::read_to_string(&last_path).expect("last");
        assert!(live.contains("turn-new"));
        assert!(!live.contains("old checklist"));
        assert_eq!(last, "old live output\nold checklist\n");
    }

    #[test]
    fn prompts_require_current_manifest_hashes_in_handoffs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_test_config(tmp.path());
        let config = load_config(tmp.path()).expect("config");
        let worker = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("worker");
        let lead = config
            .members
            .iter()
            .find(|member| member.name == "lead")
            .expect("lead");

        let worker_prompt = build_worker_prompt(&config, &[], worker);
        assert!(worker_prompt.contains("team task --team \"$CODEX_TEAM_ID\" claim"));
        assert!(worker_prompt.contains("self-claim an unassigned `ready` task"));
        assert!(worker_prompt.contains("re-read the current manifest files from disk"));
        assert!(worker_prompt.contains("stale handoff hash"));
        assert!(
            worker_prompt.contains("write and hash or timestamp the frozen configuration/plan")
        );
        assert!(worker_prompt.contains("heldout/test/evaluation-only"));
        assert!(worker_prompt.contains("Broad inventory commands"));
        assert!(worker_prompt.contains("reveal heldout/test/evaluation-only path names"));
        assert!(worker_prompt.contains("guard bootstrap seed outside the protected input root"));
        assert!(worker_prompt.contains("never \"locate\" a seed"));
        assert!(worker_prompt.contains("ask lead for the path instead of discovering it"));
        assert!(worker_prompt.contains("an exact file path exception is not permission"));
        assert!(worker_prompt.contains("Do not run `cat`, `sed`, `head`, `tail`"));
        assert!(worker_prompt.contains("python -c open(...)"));
        assert!(worker_prompt.contains("pre_guard_allowed_exact_commands"));
        assert!(worker_prompt.contains("seed-local schema-valid fail-closed writer or template"));
        assert!(worker_prompt.contains("legacy or best-effort `outcome.json`"));
        assert!(worker_prompt.contains("frozen command transcript"));
        assert!(worker_prompt.contains("explicit shell exit code"));
        assert!(
            worker_prompt.contains("event ledger, or summarized outcome alone is not sufficient")
        );

        let lead_prompt = build_app_server_lead_prompt(
            &config,
            &[],
            lead,
            Path::new("codex"),
            TeamPromptLanguage::En,
        );
        assert!(lead_prompt.contains("stale manifest hashes written only in a handoff message"));
        assert!(lead_prompt.contains("current manifest-file hashes"));
        assert!(lead_prompt.contains("every named predecessor package"));
        assert!(lead_prompt.contains("not only the immediate method package"));
        assert!(lead_prompt.contains("root-correct verify every required prior audit"));
        assert!(lead_prompt.contains("cannot legally read it before the guard is active"));
        assert!(lead_prompt.contains("give the executor exact bootstrap paths"));
        assert!(lead_prompt.contains("parent-directory inventory over protected roots"));
        assert!(lead_prompt.contains("exact file path exception alone is not enough"));
        assert!(lead_prompt.contains("file reader/probe/command variant"));
        assert!(lead_prompt.contains("`cat`, `sed`, `head`, `tail`"));
        assert!(lead_prompt.contains("seed-local schema-valid early fail-closed writer/template"));
        assert!(lead_prompt.contains("keep runtime blocked and route the contract back"));
        assert!(lead_prompt.contains("artifact-level command transcript"));
        assert!(lead_prompt.contains("Event-level ledgers, mailbox summaries, or outcome JSON"));
        assert!(lead_prompt.contains("job id/log path and observed exit code"));
    }

    #[test]
    fn team_signal_ingest_streams_complete_lines_without_losing_partial_tail() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "1",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );

        let member = TeamMember {
            name: "engineering".to_string(),
            role: "engineering".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: Some("thread".to_string()),
            workspace_path: None,
            node: None,
        };
        let mut active = HashMap::from([(
            "engineering".to_string(),
            AppServerMemberRun {
                member,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "test".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        )]);
        let mut buffers = HashMap::from([(
            "engineering".to_string(),
            "TEAM_MESSAGE to=lead: hel".to_string(),
        )]);

        ingest_team_signal_lines(team_dir, "engineering", &mut active, &buffers, false)
            .expect("partial ingest");
        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(messages.is_empty());

        buffers.insert(
            "engineering".to_string(),
            "TEAM_MESSAGE to=lead: hello\nTEAM_TASK id=1 status=blocked result=waiting for data\n"
                .to_string(),
        );
        ingest_team_signal_lines(team_dir, "engineering", &mut active, &buffers, false)
            .expect("complete ingest");
        ingest_team_signal_lines(team_dir, "engineering", &mut active, &buffers, true)
            .expect("final ingest");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message, "hello");
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "1")
            .expect("task");
        assert_eq!(task.status, TaskStatus::Blocked);
        assert_eq!(task.result.as_deref(), Some("waiting for data"));
    }

    #[test]
    fn team_signal_ingest_allows_comma_separated_message_recipients() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);

        let member = TeamMember {
            name: "engineering".to_string(),
            role: "engineering".to_string(),
            status: MemberStatus::Running,
            joined_at: now(),
            thread_id: Some("thread".to_string()),
            workspace_path: None,
            node: None,
        };
        let mut active = HashMap::from([(
            "engineering".to_string(),
            AppServerMemberRun {
                member,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "test".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        )]);
        let buffers = HashMap::from([(
            "engineering".to_string(),
            "TEAM_MESSAGE to=lead, quality: corrected verification passed\n".to_string(),
        )]);

        ingest_team_signal_lines(team_dir, "engineering", &mut active, &buffers, false)
            .expect("ingest");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let quality_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "quality")).expect("quality mailbox");
        assert_eq!(lead_messages.len(), 1);
        assert_eq!(quality_messages.len(), 1);
        assert_eq!(lead_messages[0].message, "corrected verification passed");
        assert_eq!(quality_messages[0].message, "corrected verification passed");
    }

    #[test]
    fn completed_tracked_job_with_artifacts_keeps_task_in_progress_and_resumes_owner() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "30",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        set_member_status(team_dir, "engineering", MemberStatus::Failed).expect("failed status");
        let exit_path = team_dir.join("job.exit");
        fs::write(&exit_path, "0").expect("exit code");
        let job = TeamJob {
            id: "job-1".to_string(),
            node: "local".to_string(),
            command: "true".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: Some("30".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: exit_path.display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: vec![TeamArtifact {
                path: "runtime/outcome.json".to_string(),
                note: "final handoff".to_string(),
                created_at: now(),
            }],
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "job-1"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "job-1").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Completed);
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "30")
            .expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(task.result.as_deref().is_some_and(|result| {
            result.contains("completed with registered artifacts")
                && result.contains("final report/json/manifest/checklist")
        }));
        let config = load_config(team_dir).expect("config");
        let member = config
            .members
            .iter()
            .find(|member| member.name == "engineering")
            .expect("engineering");
        assert_eq!(member.status, MemberStatus::Online);
        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(owner_messages.iter().any(|message| {
            message
                .message
                .contains("JOB_STATUS: job `job-1` for your task 30")
                && message
                    .message
                    .contains("Registered job artifacts are not sufficient")
                && message.message.contains("TEAM_COMPLETION_CHECKLIST")
        }));
        assert!(lead_messages.iter().any(|message| {
            message
                .message
                .contains("@engineering's job `job-1` for task 30")
                && message.message.contains("task is now `in_progress`")
        }));
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.get("event").and_then(|value| value.as_str())
                == Some("job_completed_requires_owner_handoff")
        }));
    }

    #[test]
    fn completed_job_without_artifacts_keeps_task_in_progress() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "34",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let exit_path = team_dir.join("job.exit");
        fs::write(&exit_path, "0").expect("exit code");
        let job = TeamJob {
            id: "job-no-artifacts".to_string(),
            node: "local".to_string(),
            command: "true".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: Some("34".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: exit_path.display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "job-no-artifacts"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "job-no-artifacts").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Completed);
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "34")
            .expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(
            task.result.as_deref().is_some_and(|result| {
                result.contains("completed without registered artifacts")
            })
        );
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert!(owner_messages.iter().any(|message| {
            message.message.contains("do not register fake artifacts")
                && message
                    .message
                    .contains("write the task's real final report/json/manifest/checklist")
        }));
        assert!(lead_messages.iter().any(|message| {
            message
                .message
                .contains("task 34 ended with status Completed")
        }));
    }

    #[test]
    fn completed_job_status_notification_is_idempotent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "34",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let exit_path = team_dir.join("job.exit");
        fs::write(&exit_path, "0").expect("exit code");
        let mut job = TeamJob {
            id: "job-repeat".to_string(),
            node: "local".to_string(),
            command: "true".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("engineering".to_string()),
            task_id: Some("34".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: exit_path.display().to_string(),
            exit_code: None,
            note: String::new(),
            artifacts: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "job-repeat"), &job).expect("write job");

        refresh_job_status(team_dir, "job-repeat").expect("first refresh");

        // Simulate a concurrent refresher that loaded the old Running state before
        // another refresher persisted the terminal status and sent notifications.
        job.status = TeamJobStatus::Running;
        job.exit_code = None;
        write_json_atomic(&job_path(team_dir, "job-repeat"), &job).expect("rewrite stale job");

        refresh_job_status(team_dir, "job-repeat").expect("second refresh");

        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        let completed_without_artifacts_count = events
            .iter()
            .filter(|event| {
                event.get("event").and_then(|value| value.as_str())
                    == Some("job_completed_without_artifacts")
            })
            .count();
        assert_eq!(completed_without_artifacts_count, 1);

        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        let owner_status_count = owner_messages
            .iter()
            .filter(|message| {
                message
                    .message
                    .contains("JOB_STATUS: job `job-repeat` for your task 34")
            })
            .count();
        assert_eq!(owner_status_count, 1);

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let lead_status_count = lead_messages
            .iter()
            .filter(|message| {
                message
                    .message
                    .contains("@engineering's job `job-repeat` for task 34")
            })
            .count();
        assert_eq!(lead_status_count, 1);
    }

    #[test]
    fn auxiliary_job_does_not_overwrite_task_status_or_result() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_task(
            team_dir,
            "79",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("runtime is producing final package"),
        );
        let exit_path = team_dir.join("job.exit");
        fs::write(&exit_path, "0").expect("exit code");
        let job = TeamJob {
            id: "lead-verify".to_string(),
            node: "local".to_string(),
            command: "true".to_string(),
            cwd: team_dir.display().to_string(),
            owner: Some("lead".to_string()),
            task_id: Some("79".to_string()),
            status: TeamJobStatus::Running,
            pid: Some("999999".to_string()),
            log_path: team_dir.join("job.log").display().to_string(),
            exit_path: exit_path.display().to_string(),
            exit_code: None,
            note: "read-only verification".to_string(),
            artifacts: Vec::new(),
            created_at: now(),
            updated_at: now(),
        };
        write_json_atomic(&job_path(team_dir, "lead-verify"), &job).expect("write job");

        let refreshed = refresh_job_status(team_dir, "lead-verify").expect("refresh job");

        assert_eq!(refreshed.status, TeamJobStatus::Completed);
        let task = load_tasks(team_dir)
            .expect("tasks")
            .into_iter()
            .find(|task| task.id == "79")
            .expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(
            task.result.as_deref(),
            Some("runtime is producing final package")
        );
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.get("event").and_then(|value| value.as_str())
                == Some("auxiliary_job_status_no_task_update")
        }));
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let owner_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering")).expect("mailbox");
        assert!(
            lead_messages
                .iter()
                .any(|message| message.message.contains("AUX_JOB_STATUS"))
        );
        assert!(
            owner_messages
                .iter()
                .any(|message| message.message.contains("AUX_JOB_STATUS"))
        );
    }

    #[test]
    fn idle_outreach_sends_from_free_department_to_active_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-test".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "helper".to_string(),
                    role: "review".to_string(),
                    status: MemberStatus::Completed,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "target".to_string(),
                    role: "worker".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "1"),
            &TeamTask {
                id: "1".to_string(),
                subject: "active work".to_string(),
                description: String::new(),
                owner: Some("target".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");

        let mut last = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0;
        maybe_send_idle_department_outreach(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("outreach");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "target")).expect("target mailbox");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].from, "helper");
        assert!(messages[0].message.contains("Periodic idle outreach"));
    }

    #[test]
    fn idle_wakeup_is_batched_and_rotates_idle_departments() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let mut members = vec![TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Online,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        }];
        for name in ["research", "capture", "method", "audit"] {
            members.push(TeamMember {
                name: name.to_string(),
                role: "worker".to_string(),
                status: MemberStatus::Completed,
                joined_at: now.clone(),
                thread_id: None,
                workspace_path: None,
                node: None,
            });
        }
        let config = TeamConfig {
            version: 1,
            id: "team-idle".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members,
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        let mut idle_since = HashMap::new();
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0_usize;
        let old = Instant::now() - Duration::from_secs(601);
        for name in ["research", "capture", "method", "audit"] {
            idle_since.insert(name.to_string(), old);
        }

        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("first wakeup batch");

        assert_eq!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research"))
                .expect("research")
                .len(),
            1
        );
        assert_eq!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "capture"))
                .expect("capture")
                .len(),
            1
        );
        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "method"))
                .expect("method")
                .is_empty()
        );
        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "audit"))
                .expect("audit")
                .is_empty()
        );

        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("suppressed immediate batch");
        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "method"))
                .expect("method")
                .is_empty()
        );

        last_batch = Instant::now() - Duration::from_secs(601);
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("second wakeup batch");

        assert_eq!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "method"))
                .expect("method")
                .len(),
            1
        );
        assert_eq!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "audit"))
                .expect("audit")
                .len(),
            1
        );
    }

    #[test]
    fn heartbeat_skips_department_recently_woken_by_idle_wakeup() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-idle-heartbeat".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "research".to_string(),
                    role: "research".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        let mut idle_since = HashMap::from([(
            "research".to_string(),
            Instant::now() - Duration::from_secs(601),
        )]);
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0_usize;
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("idle wakeup");

        let mut heartbeats = HashMap::new();
        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &HashMap::new(),
            &mut heartbeats,
            &last_wakeup,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("heartbeat");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research")).expect("mailbox");
        assert_eq!(messages.len(), 1);
        assert!(messages[0].message.contains("Department idle wakeup"));
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        let skipped_count = events
            .iter()
            .filter(|event| {
                event.get("event").and_then(|value| value.as_str())
                    == Some("department_heartbeat_skipped")
                    && event
                        .get("data")
                        .and_then(|data| data.get("reason"))
                        .and_then(|value| value.as_str())
                        == Some("recent_idle_wakeup")
            })
            .count();
        assert_eq!(skipped_count, 1);

        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &HashMap::new(),
            &mut heartbeats,
            &last_wakeup,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("second heartbeat");
        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        let skipped_count = events
            .iter()
            .filter(|event| {
                event.get("event").and_then(|value| value.as_str())
                    == Some("department_heartbeat_skipped")
                    && event
                        .get("data")
                        .and_then(|data| data.get("reason"))
                        .and_then(|value| value.as_str())
                        == Some("recent_idle_wakeup")
            })
            .count();
        assert_eq!(skipped_count, 1);
    }

    #[test]
    fn heartbeat_suppresses_empty_standby_department_during_usage_limit_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let research = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-heartbeat-cooldown".to_string(),
            goal: "continuous research loop".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), research],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        let mut active = HashMap::new();
        active.insert(
            "lead".to_string(),
            AppServerMemberRun {
                member: lead,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: true,
                failed: false,
                standby_after_turn: false,
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "usage_limited".to_string(),
                last_stale_notice_at: None,
                retry_not_before: Some(Instant::now() + Duration::from_secs(300)),
                side_context_ids: Vec::new(),
            },
        );

        let mut heartbeats = HashMap::new();
        maybe_send_department_heartbeats(
            team_dir,
            &config,
            &active,
            &mut heartbeats,
            &HashMap::new(),
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("heartbeat");

        let research_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research")).expect("mailbox");
        assert!(research_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "department_heartbeat_skipped"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("usage_limit_cooldown")
        }));
    }

    #[test]
    fn idle_wakeup_suppresses_empty_standby_department_during_usage_limit_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let research = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-idle-cooldown".to_string(),
            goal: "continuous research loop".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), research],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        let mut active = HashMap::new();
        active.insert(
            "lead".to_string(),
            AppServerMemberRun {
                member: lead,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: true,
                failed: false,
                standby_after_turn: false,
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "usage_limited".to_string(),
                last_stale_notice_at: None,
                retry_not_before: Some(Instant::now() + Duration::from_secs(300)),
                side_context_ids: Vec::new(),
            },
        );
        let mut idle_since = HashMap::from([(
            "research".to_string(),
            Instant::now() - Duration::from_secs(601),
        )]);
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        let mut cursor = 0_usize;

        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &active,
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("idle wakeup");
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &active,
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("second idle wakeup");

        let research_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research")).expect("mailbox");
        assert!(research_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        let skipped_count = events
            .iter()
            .filter(|event| {
                event.event == "department_idle_wakeup_skipped"
                    && event.data.get("reason").and_then(|value| value.as_str())
                        == Some("usage_limit_cooldown")
            })
            .count();
        assert_eq!(skipped_count, 1);
    }

    #[test]
    fn runtime_attach_normalizes_stale_running_member_without_open_tasks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-stale-running".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "validator".to_string(),
                    role: "container".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        let normalized =
            normalize_stale_running_members_without_active_turns(team_dir, &HashMap::new())
                .expect("normalize");

        assert_eq!(normalized, vec!["validator".to_string()]);
        assert_eq!(
            member_status(team_dir, "validator").expect("status"),
            Some(MemberStatus::Standby)
        );
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "member_status_normalized"
                && event.data.get("reason").and_then(|value| value.as_str())
                    == Some("no active app-server turn or open owned task after runtime attach")
        }));
    }

    #[test]
    fn runtime_attach_keeps_running_member_with_open_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-running-task".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "validator".to_string(),
                    role: "container".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "1",
            Some("validator"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );

        let normalized =
            normalize_stale_running_members_without_active_turns(team_dir, &HashMap::new())
                .expect("normalize");

        assert!(normalized.is_empty());
        assert_eq!(
            member_status(team_dir, "validator").expect("status"),
            Some(MemberStatus::Running)
        );
    }

    #[test]
    fn idle_wakeup_cooldown_is_restored_from_recent_events_after_restart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-idle-restart".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "research".to_string(),
                    role: "research".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "runtime".to_string(),
                    role: "container".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        append_event(
            team_dir,
            "department_idle_wakeup_sent",
            serde_json::json!({
                "member": "research",
                "role": "research",
                "node": "local",
                "idle_for_sec": 900,
                "owned_open_tasks": 0,
            }),
        )
        .expect("recent idle wakeup event");

        let mut idle_since = HashMap::from([
            (
                "research".to_string(),
                Instant::now() - Duration::from_secs(601),
            ),
            (
                "runtime".to_string(),
                Instant::now() - Duration::from_secs(601),
            ),
        ]);
        let mut last_wakeup = HashMap::new();
        let mut last_batch = Instant::now() - Duration::from_secs(601);
        seed_department_idle_wakeup_cooldowns(
            team_dir,
            &mut last_wakeup,
            &mut last_batch,
            Duration::from_secs(600),
        )
        .expect("seed cooldowns");
        let mut cursor = 0_usize;
        maybe_send_department_idle_wakeups(
            team_dir,
            &config,
            &HashMap::new(),
            &mut idle_since,
            &mut last_wakeup,
            &mut last_batch,
            &mut cursor,
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("suppressed restart batch");

        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "research"))
                .expect("research mailbox")
                .is_empty()
        );
        assert!(
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "runtime"))
                .expect("runtime mailbox")
                .is_empty()
        );
    }

    #[test]
    fn mark_mailbox_messages_read_updates_unread_tail() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-read".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![TeamMember {
                name: "lead".to_string(),
                role: "lead".to_string(),
                status: MemberStatus::Online,
                joined_at: now.clone(),
                thread_id: None,
                workspace_path: None,
                node: None,
            }],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "user".to_string(),
                to: "lead".to_string(),
                message: "old".to_string(),
                timestamp: now.clone(),
                read: false,
            },
        )
        .expect("append old");
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "user".to_string(),
                to: "lead".to_string(),
                message: "new".to_string(),
                timestamp: now,
                read: false,
            },
        )
        .expect("append new");

        mark_mailbox_messages_read(team_dir, "lead", 1).expect("mark read");

        let messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("read mailbox");
        assert!(!messages[0].read);
        assert!(messages[1].read);
    }

    #[test]
    fn acknowledge_mailbox_delivery_marks_only_delivered_messages() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let now = now();
        let path = mailbox_path(team_dir, "worker");
        for message in ["old", "delivered", "arrived_later"] {
            append_jsonl(
                &path,
                &MailMessage {
                    from: "lead".to_string(),
                    to: "worker".to_string(),
                    message: message.to_string(),
                    timestamp: now.clone(),
                    read: false,
                },
            )
            .expect("append message");
        }
        let mut counts = HashMap::from([("worker".to_string(), 1)]);

        acknowledge_mailbox_delivery(team_dir, &mut counts, "worker", 1, 1).expect("ack delivered");

        let messages = read_jsonl::<MailMessage>(&path).expect("read mailbox");
        assert!(!messages[0].read);
        assert!(messages[1].read);
        assert!(!messages[2].read);
        assert_eq!(counts.get("worker"), Some(&2));
    }

    #[test]
    fn side_channel_context_reinjects_until_acknowledged() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-side-context".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "worker".to_string(),
                    role: "worker".to_string(),
                    status: MemberStatus::Running,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        append_jsonl(
            &side_channel_context_path(team_dir, "worker"),
            &SideChannelContextRecord {
                id: "ctx-1".to_string(),
                member: "worker".to_string(),
                node: "local".to_string(),
                source_thread: "thread-main".to_string(),
                side_thread: "thread-side".to_string(),
                side_turn: "turn-side".to_string(),
                recipients: vec!["reviewer".to_string()],
                incoming_summary: "reviewer asked a question".to_string(),
                reply: "Quick side-channel reply from @worker while my main turn continues:\n\nI will check it.".to_string(),
                created_at: now,
                status: SideChannelContextStatus::Pending,
                injected_turns: Vec::new(),
                injected_at: None,
                acknowledged_at: None,
            },
        )
        .expect("append context");

        let (prompt, ids) = append_side_channel_context_prompt(
            team_dir,
            "worker",
            "turn-main-1",
            "base prompt".to_string(),
            TeamPromptLanguage::En,
        )
        .expect("append context prompt");
        assert_eq!(ids, vec!["ctx-1".to_string()]);
        assert!(prompt.contains("Pending side-channel context"));
        assert!(prompt.contains("machine-readable artifacts/manifests"));
        assert!(prompt.contains("Do not hand off, complete a task, or rely on stale artifacts"));
        mark_side_channel_contexts_injected(team_dir, "worker", &ids, "turn-main-1")
            .expect("mark injected");

        let (_, same_turn_ids) = append_side_channel_context_prompt(
            team_dir,
            "worker",
            "turn-main-1",
            "base prompt".to_string(),
            TeamPromptLanguage::En,
        )
        .expect("same turn prompt");
        assert!(same_turn_ids.is_empty());

        let (_, next_turn_ids) = append_side_channel_context_prompt(
            team_dir,
            "worker",
            "turn-main-2",
            "base prompt".to_string(),
            TeamPromptLanguage::En,
        )
        .expect("next turn prompt");
        assert_eq!(next_turn_ids, vec!["ctx-1".to_string()]);

        acknowledge_side_channel_contexts(team_dir, "worker", &ids, "turn-main-1").expect("ack");
        let (_, after_ack_ids) = append_side_channel_context_prompt(
            team_dir,
            "worker",
            "turn-main-3",
            "base prompt".to_string(),
            TeamPromptLanguage::En,
        )
        .expect("after ack prompt");
        assert!(after_ack_ids.is_empty());
    }

    #[test]
    fn ui_debug_timeline_collects_messages_events_side_context_and_buffers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let timestamp = now();
        append_event(
            team_dir,
            "message_sent",
            serde_json::json!({
                "from": "system",
                "to": "engineering",
                "message": "Automated lead status check",
                "source": "idle_wakeup",
            }),
        )
        .expect("append event");
        append_jsonl(
            &mailbox_path(team_dir, "lead"),
            &MailMessage {
                from: "engineering".to_string(),
                to: "lead".to_string(),
                message: "Need review".to_string(),
                timestamp: timestamp.clone(),
                read: false,
            },
        )
        .expect("append mailbox");
        append_jsonl(
            &side_channel_context_path(team_dir, "engineering"),
            &SideChannelContextRecord {
                id: "ctx-ui".to_string(),
                member: "engineering".to_string(),
                node: "local".to_string(),
                source_thread: "thread-main".to_string(),
                side_thread: "thread-side".to_string(),
                side_turn: "turn-side".to_string(),
                recipients: vec!["quality".to_string()],
                incoming_summary: "quality asked for status".to_string(),
                reply: "Runtime evidence is pending.".to_string(),
                created_at: timestamp,
                status: SideChannelContextStatus::Pending,
                injected_turns: Vec::new(),
                injected_at: None,
                acknowledged_at: None,
            },
        )
        .expect("append side context");
        fs::create_dir_all(team_dir.join("live_messages")).expect("live dir");
        fs::write(
            team_dir.join("live_messages").join("engineering.md"),
            "currently building",
        )
        .expect("write live");

        let timeline = collect_ui_debug_timeline(team_dir, 100).expect("timeline");
        assert!(
            timeline
                .iter()
                .any(|item| item.kind == "system"
                    && item.body.contains("Automated lead status check"))
        );
        assert!(
            timeline
                .iter()
                .any(|item| item.kind == "message" && item.body == "Need review")
        );
        assert!(
            timeline
                .iter()
                .any(|item| item.kind == "side"
                    && item.body.contains("Runtime evidence is pending."))
        );
        assert!(
            timeline
                .iter()
                .any(|item| item.kind == "live" && item.body.contains("currently building"))
        );

        let json = render_team_debug_json(team_dir).expect("debug json");
        assert!(json.contains("team-task-test"));
        assert!(json.contains("ctx-ui"));
    }

    #[test]
    fn ui_timestamps_are_rendered_in_tokyo_offset() {
        assert_eq!(
            timestamp_for_ui("2026-05-10T00:00:00Z"),
            "2026-05-10T09:00:00+09:00"
        );
        assert!(now().ends_with("+09:00"));
    }

    #[test]
    fn task_watchdog_warns_about_unattended_active_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        let now = now();
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let config = TeamConfig {
            version: 1,
            id: "team-watch".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "worker".to_string(),
                    role: "worker".to_string(),
                    status: MemberStatus::Completed,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "42"),
            &TeamTask {
                id: "42".to_string(),
                subject: "needs work".to_string(),
                description: String::new(),
                owner: Some("worker".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: old.clone(),
                updated_at: old,
            },
        )
        .expect("write task");

        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let worker_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "worker")).expect("worker mailbox");
        assert_eq!(lead_messages.len(), 1);
        assert_eq!(worker_messages.len(), 1);
        assert!(lead_messages[0].message.contains("Task watchdog"));
        let config = load_config(team_dir).expect("reload config");
        let worker = config
            .members
            .iter()
            .find(|member| member.name == "worker")
            .expect("worker");
        assert_eq!(worker.status, MemberStatus::Online);
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(events.iter().any(|event| {
            event.event == "task_watchdog_attention"
                && event
                    .data
                    .get("owner_reactivated")
                    .and_then(|value| value.as_bool())
                    == Some(true)
        }));
    }

    #[test]
    fn task_watchdog_ignores_tasks_waiting_on_incomplete_dependencies() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        fs::create_dir_all(team_dir.join("jobs")).expect("jobs dir");
        write_test_config(team_dir);
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        write_test_task(
            team_dir,
            "1",
            None,
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "1")).expect("read task");
        task.updated_at = old.clone();
        write_json_atomic(&task_path(team_dir, "1"), &task).expect("write parent task");
        write_test_task(
            team_dir,
            "2",
            Some("engineering"),
            TaskStatus::Blocked,
            vec!["1"],
            Some("Waiting for task 1 handoff."),
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "2")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "2"), &task).expect("write waiting task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let worker_messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
            .expect("worker mailbox");
        assert!(lead_messages.is_empty());
        assert!(worker_messages.is_empty());
    }

    #[test]
    fn review_handoff_watchdog_warns_when_task_artifact_directory_is_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("audit").join("cycle8");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task44 Cycle 8 final audit package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "44",
            Some("quality"),
            TaskStatus::Review,
            Vec::new(),
            Some("semantic verification passed; final package pending"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "44")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "44"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let quality_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "quality")).expect("quality mailbox");
        assert!(lead_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message
                    .message
                    .contains("directory exists but contains no files")
        }));
        assert!(quality_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("TEAM_COMPLETION_CHECKLIST")
        }));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "review_handoff_artifact_attention")
        );
    }

    #[test]
    fn review_handoff_watchdog_accepts_complete_local_artifact_package() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("audit").join("cycle8");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("cycle8_audit_report.md"), "# report\n").expect("report");
        fs::write(artifact_dir.join("cycle8_audit.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "- artifacts: ok\n",
        )
        .expect("checklist");
        let manifest = Command::new("sha256sum")
            .args([
                "cycle8_audit_report.md",
                "cycle8_audit.json",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("sha256_manifest.txt"), manifest.stdout).expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task44 Cycle 8 final audit package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "44",
            Some("quality"),
            TaskStatus::Review,
            Vec::new(),
            Some("final package present"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "44")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "44"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let quality_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "quality")).expect("quality mailbox");
        assert!(
            !lead_messages
                .iter()
                .any(|message| message.message.contains("Review handoff watchdog"))
        );
        assert!(
            !quality_messages
                .iter()
                .any(|message| message.message.contains("Review handoff watchdog"))
        );
    }

    #[test]
    fn completion_blocker_accepts_named_manifest_and_checklist_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("evaluation");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(
            artifact_dir.join("claim_evidence_matrix.md"),
            "# claim matrix\n",
        )
        .expect("matrix");
        fs::write(artifact_dir.join("evaluation_status.md"), "# status\n").expect("status");
        fs::write(artifact_dir.join("evaluation_plan.yaml"), "version: 1\n").expect("yaml");
        let manifest = Command::new("sha256sum")
            .args([
                "claim_evidence_matrix.md",
                "evaluation_status.md",
                "evaluation_plan.yaml",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(
            artifact_dir.join("evaluation_manifest.sha256"),
            manifest.stdout,
        )
        .expect("manifest");
        send_team_message_to_dir(
            team_dir,
            "quality",
            "lead",
            "Final handoff\n\nTEAM_COMPLETION_CHECKLIST:\n- artifacts: evaluation\n- verification: sha256sum -c evaluation_manifest.sha256 rc=0",
        )
        .expect("message");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task44 evaluation handoff".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "44",
            Some("quality"),
            TaskStatus::InProgress,
            Vec::new(),
            Some("handoff complete"),
        );
        let task = read_json::<TeamTask>(&task_path(team_dir, "44")).expect("task");

        let issue = task_completion_missing_required_local_outputs(team_dir, &task)
            .expect("completion blocker");

        assert_eq!(issue, None);
    }

    #[test]
    fn review_handoff_watchdog_ignores_stale_empty_placeholder_when_complete_package_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let placeholder_dir = team_dir.join("audit").join("cycle10_placeholder");
        let artifact_dir = team_dir.join("audit").join("cycle10_final");
        fs::create_dir_all(&placeholder_dir).expect("placeholder dir");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("cycle10_audit_report.md"), "# report\n").expect("report");
        fs::write(artifact_dir.join("cycle10_audit.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "- artifacts: ok\n",
        )
        .expect("checklist");
        let manifest = Command::new("sha256sum")
            .args([
                "cycle10_audit_report.md",
                "cycle10_audit.json",
                "TEAM_COMPLETION_CHECKLIST.md",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("sha256_manifest.txt"), manifest.stdout).expect("manifest");
        write_ownerships(
            team_dir,
            &[
                FileOwnership {
                    path: placeholder_dir.display().to_string(),
                    owner: "quality".to_string(),
                    note: "Task50 initial placeholder package root".to_string(),
                    updated_at: now(),
                },
                FileOwnership {
                    path: artifact_dir.display().to_string(),
                    owner: "quality".to_string(),
                    note: "Task50 final audit package".to_string(),
                    updated_at: now(),
                },
            ],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "50",
            Some("quality"),
            TaskStatus::Review,
            Vec::new(),
            Some("final package present"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "50")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "50"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let quality_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "quality")).expect("quality mailbox");
        assert!(
            !lead_messages
                .iter()
                .any(|message| message.message.contains("Review handoff watchdog"))
        );
        assert!(
            !quality_messages
                .iter()
                .any(|message| message.message.contains("Review handoff watchdog"))
        );
    }

    #[test]
    fn review_handoff_watchdog_warns_when_manifest_fails() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("audit").join("cycle8");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("cycle8_audit_report.md"), "# report\n").expect("report");
        fs::write(artifact_dir.join("cycle8_audit.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "- artifacts: ok\n",
        )
        .expect("checklist");
        fs::write(
            artifact_dir.join("sha256_manifest.txt"),
            "0000000000000000000000000000000000000000000000000000000000000000  cycle8_audit_report.md\n",
        )
        .expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "quality".to_string(),
                note: "Task44 Cycle 8 final audit package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "44",
            Some("quality"),
            TaskStatus::Review,
            Vec::new(),
            Some("final package present"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "44")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "44"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(lead_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("failed sha256 verification")
        }));
    }

    #[test]
    fn review_handoff_watchdog_warns_when_manifest_contains_volatile_transcript() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let artifact_dir = team_dir.join("runtime").join("cycle12");
        fs::create_dir_all(&artifact_dir).expect("artifact dir");
        fs::write(artifact_dir.join("report.md"), "# report\n").expect("report");
        fs::write(artifact_dir.join("outcome.json"), "{}\n").expect("json");
        fs::write(
            artifact_dir.join("TEAM_COMPLETION_CHECKLIST.md"),
            "- artifacts: ok\n",
        )
        .expect("checklist");
        fs::write(
            artifact_dir.join("command_transcript.log"),
            "still being appended\n",
        )
        .expect("transcript");
        let manifest = Command::new("sha256sum")
            .args([
                "report.md",
                "outcome.json",
                "TEAM_COMPLETION_CHECKLIST.md",
                "command_transcript.log",
            ])
            .current_dir(&artifact_dir)
            .output()
            .expect("sha256sum");
        assert!(manifest.status.success());
        fs::write(artifact_dir.join("sha256_manifest.txt"), manifest.stdout).expect("manifest");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: artifact_dir.display().to_string(),
                owner: "engineering".to_string(),
                note: "Task60 Cycle 12 final runtime package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "60",
            Some("engineering"),
            TaskStatus::Review,
            Vec::new(),
            Some("final package present"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "60")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "60"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox");
        assert!(lead_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("volatile entry")
                && message.message.contains("command_transcript.log")
        }));
        assert!(engineering_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("exclude active transcripts")
        }));
    }

    #[test]
    fn review_handoff_watchdog_warns_for_remote_only_artifact_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: "/__codex_remote__/workspace/runtime/cycle12_gate".to_string(),
                owner: "engineering".to_string(),
                note: "Task60 container runtime final package".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");
        write_test_task(
            team_dir,
            "60",
            Some("engineering"),
            TaskStatus::Review,
            Vec::new(),
            Some("container package ready for review"),
        );
        let old = (Utc::now() - chrono::Duration::seconds(180))
            .to_rfc3339_opts(SecondsFormat::Secs, true);
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "60")).expect("read task");
        task.updated_at = old;
        write_json_atomic(&task_path(team_dir, "60"), &task).expect("write task");

        let config = load_config(team_dir).expect("load config");
        let mut last = Instant::now() - Duration::from_secs(61);
        let mut warned = HashSet::new();
        maybe_warn_unattended_tasks(
            team_dir,
            &config,
            &HashMap::new(),
            &mut last,
            &mut warned,
            Duration::from_secs(60),
            TeamPromptLanguage::En,
        )
        .expect("watchdog");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let engineering_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
                .expect("engineering mailbox");
        assert!(lead_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("not locally inspectable")
                && message
                    .message
                    .contains("node-side manifest/checklist verification")
        }));
        assert!(engineering_messages.iter().any(|message| {
            message.message.contains("Review handoff watchdog")
                && message.message.contains("not locally inspectable")
        }));
    }

    #[test]
    fn completed_task_with_missing_declared_local_package_does_not_unblock_dependency() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let missing_dir = team_dir.join("method_schema").join("cycle9_contract");
        write_test_task(
            team_dir,
            "46",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut producer = read_json::<TeamTask>(&task_path(team_dir, "46")).expect("producer");
        producer.subject = "Cycle 9 method/schema".to_string();
        producer.description = format!(
            "Produce the contract package at {} before runtime starts.",
            missing_dir.display()
        );
        write_json_atomic(&task_path(team_dir, "46"), &producer).expect("write producer");
        write_test_task(
            team_dir,
            "47",
            Some("quality"),
            TaskStatus::Waiting,
            vec!["46"],
            Some("Soft-waiting for dependency task(s)."),
        );

        let changed = set_task_status_if_open(team_dir, "46", TaskStatus::Completed, Some("Done."))
            .expect("set task");
        assert!(changed);

        let producer = read_json::<TeamTask>(&task_path(team_dir, "46")).expect("producer");
        let consumer = read_json::<TeamTask>(&task_path(team_dir, "47")).expect("consumer");
        assert_eq!(producer.status, TaskStatus::Blocked);
        assert!(
            producer
                .result
                .as_deref()
                .unwrap_or_default()
                .contains("Completion rejected")
        );
        assert_eq!(consumer.status, TaskStatus::Waiting);
        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(lead_messages.iter().any(|message| {
            message
                .message
                .contains("Task completion rejected: task 46")
        }));
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "task_completion_rejected_missing_artifacts")
        );
    }

    #[test]
    fn completed_task_sends_freeze_notice_to_owner_and_lead_once() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "86",
            Some("engineering"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut task = read_json::<TeamTask>(&task_path(team_dir, "86")).expect("task");
        task.subject = "Cycle 18 method package".to_string();
        write_json_atomic(&task_path(team_dir, "86"), &task).expect("write task");

        let changed = set_task_status_if_open(
            team_dir,
            "86",
            TaskStatus::Completed,
            Some("final handoff manifest hash abc123"),
        )
        .expect("set completed");
        assert!(changed);
        let changed_again =
            set_task_status_if_open(team_dir, "86", TaskStatus::Completed, Some("duplicate"))
                .expect("set completed again");
        assert!(!changed_again);

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let owner_messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, "engineering"))
            .expect("owner mailbox");
        let lead_freezes = lead_messages
            .iter()
            .filter(|message| {
                message.message.contains("TASK_COMPLETION_FREEZE")
                    && message.message.contains("task 86")
                    && message.message.contains("completed by @engineering")
                    && message.message.contains("reopen the task before")
                    && message.message.contains("silent post-completion mutation")
            })
            .count();
        let owner_freezes = owner_messages
            .iter()
            .filter(|message| {
                message.message.contains("TASK_COMPLETION_FREEZE")
                    && message.message.contains("Cycle 18 method package")
                    && message
                        .message
                        .contains("Do not mutate completed task artifacts")
                    && message.message.contains("LEAD_PROPOSAL")
                    && message.message.contains("old/new hashes")
                    && message.message.contains("failed-attempt provenance")
            })
            .count();
        assert_eq!(lead_freezes, 1);
        assert_eq!(owner_freezes, 1);

        let events =
            read_jsonl::<serde_json::Value>(&team_dir.join("events.jsonl")).expect("events");
        let freeze_events = events
            .iter()
            .filter(|event| {
                event.get("event").and_then(|value| value.as_str())
                    == Some("task_completion_freeze_notified")
                    && event
                        .get("data")
                        .and_then(|data| data.get("task"))
                        .and_then(|task| task.as_str())
                        == Some("86")
            })
            .count();
        assert_eq!(freeze_events, 1);
    }

    #[test]
    fn member_resume_reuses_existing_open_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let now = now();
        let config = TeamConfig {
            version: 1,
            id: "team-resume".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "engineering".to_string(),
                    role: "engineering".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "3"),
            &TeamTask {
                id: "3".to_string(),
                subject: "Department mission for engineering: Implement app.\n\nOperate as one department-level Codex session.".to_string(),
                description: String::new(),
                owner: Some("engineering".to_string()),
                status: TaskStatus::Blocked,
                depends_on: Vec::new(),
                result: Some("Waiting for schema handoff.".to_string()),
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");

        resume_team_member(
            team_dir,
            MemberResumeArgs {
                member: "engineering".to_string(),
                mission: Some("Implement app after schema handoff.".to_string()),
            },
        )
        .expect("resume");

        let tasks = load_tasks(team_dir).expect("load tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "3");
        assert_eq!(tasks[0].status, TaskStatus::InProgress);
        assert!(
            tasks[0]
                .result
                .as_deref()
                .unwrap_or_default()
                .contains("Resumed without creating a duplicate task")
        );
        let events =
            read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("read events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "task_reused_for_resume")
        );
        assert!(events.iter().any(|event| {
            event.event == "member_resumed"
                && event
                    .data
                    .get("reused_task")
                    .and_then(|value| value.as_bool())
                    == Some(true)
        }));
    }

    #[test]
    fn member_resume_with_multiple_open_tasks_still_does_not_create_duplicate() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        let now = now();
        let older =
            (Utc::now() - chrono::Duration::seconds(60)).to_rfc3339_opts(SecondsFormat::Secs, true);
        let config = TeamConfig {
            version: 1,
            id: "team-resume-multi".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "quality".to_string(),
                    role: "quality".to_string(),
                    status: MemberStatus::Standby,
                    joined_at: now.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        for (id, updated_at) in [("4", older.clone()), ("6", now.clone())] {
            write_json_atomic(
                &task_path(team_dir, id),
                &TeamTask {
                    id: id.to_string(),
                    subject: format!("quality task {id}"),
                    description: String::new(),
                    owner: Some("quality".to_string()),
                    status: TaskStatus::Blocked,
                    depends_on: Vec::new(),
                    result: None,
                    created_at: updated_at.clone(),
                    updated_at,
                },
            )
            .expect("write task");
        }

        resume_team_member(
            team_dir,
            MemberResumeArgs {
                member: "quality".to_string(),
                mission: Some("Run QA after engineering handoff.".to_string()),
            },
        )
        .expect("resume");

        let tasks = load_tasks(team_dir).expect("load tasks");
        assert_eq!(tasks.len(), 2);
        let latest = tasks.iter().find(|task| task.id == "6").expect("task 6");
        assert_eq!(latest.status, TaskStatus::InProgress);
        assert!(
            latest
                .result
                .as_deref()
                .unwrap_or_default()
                .contains("Resumed without creating a duplicate task")
        );
    }

    #[test]
    fn member_resume_reuses_task_id_named_in_mission() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "131",
            Some("engineering"),
            TaskStatus::Blocked,
            Vec::new(),
            Some("Waiting for explicit lead clearance."),
        );

        resume_team_member(
            team_dir,
            MemberResumeArgs {
                member: "engineering".to_string(),
                mission: Some(
                    "Task131 CLEARANCE: execute exactly the cleared command.".to_string(),
                ),
            },
        )
        .expect("resume referenced task");

        let tasks = load_tasks(team_dir).expect("load tasks");
        assert_eq!(tasks.len(), 1);
        let task = tasks.iter().find(|task| task.id == "131").expect("task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert!(
            task.result
                .as_deref()
                .unwrap_or_default()
                .contains("Resumed referenced task without creating a duplicate")
        );
    }

    #[test]
    fn member_resume_refuses_to_duplicate_completed_referenced_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        write_test_task(
            team_dir,
            "131",
            Some("engineering"),
            TaskStatus::Completed,
            Vec::new(),
            Some("Frozen handoff."),
        );

        let err = resume_team_member(
            team_dir,
            MemberResumeArgs {
                member: "engineering".to_string(),
                mission: Some("Task131 CLEARANCE: run the repair job.".to_string()),
            },
        )
        .expect_err("completed referenced task should require explicit reopen");

        assert!(err.to_string().contains("already completed"));
        let tasks = load_tasks(team_dir).expect("load tasks");
        assert_eq!(tasks.len(), 1);
        assert!(!task_path(team_dir, "132").exists());
    }

    #[test]
    fn worker_output_waiting_for_lead_clearance_counts_as_blocked() {
        let mut buffers = HashMap::new();
        buffers.insert(
            "runtime".to_string(),
            "Task131 remains blocked until explicit lead clearance with exact command.".to_string(),
        );

        assert!(member_turn_reports_blocked(&buffers, "runtime"));
    }

    #[test]
    fn lead_autonomy_tick_reports_open_tasks_even_with_active_turns() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let worker = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-tick".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), worker.clone()],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "1"),
            &TeamTask {
                id: "1".to_string(),
                subject: "research cycle".to_string(),
                description: String::new(),
                owner: Some("research".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");
        send_team_message_to_dir(
            team_dir,
            "research",
            "lead",
            "LEAD_PROPOSAL: task 1 appears ready for review; resume audit with the current evidence.",
        )
        .expect("send proposal");
        send_team_message_to_dir(
            team_dir,
            "@system",
            "lead",
            "Lead autonomy tick instructions mention `LEAD_PROPOSAL:` and task 1, but this is not a teammate proposal.",
        )
        .expect("send system instruction");

        let mut active = HashMap::new();
        active.insert(
            "research".to_string(),
            AppServerMemberRun {
                member: worker,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "turn_started".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );
        let mut last_tick = Instant::now() - Duration::from_secs(181);
        maybe_send_lead_autonomy_tick(
            team_dir,
            &config,
            &active,
            &mut last_tick,
            Duration::from_secs(180),
            TeamPromptLanguage::En,
        )
        .expect("lead tick");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(lead_messages.len(), 3);
        let tick = lead_messages.last().expect("tick message");
        assert!(tick.message.contains("Lead autonomy tick"));
        assert!(tick.message.contains("research cycle"));
        assert!(tick.message.contains("Recent LEAD_PROPOSAL signals"));
        assert!(tick.message.contains("task 1 appears ready for review"));
        assert!(!tick.message.contains("this is not a teammate proposal"));

        let task_proposals =
            collect_recent_lead_proposals_for_task(team_dir, "lead", "1", 5).expect("proposals");
        assert_eq!(task_proposals.len(), 1);
        assert!(task_proposals[0].contains("@research"));
    }

    #[test]
    fn lead_autonomy_tick_surfaces_next_action_when_goal_continues() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let audit_dir = team_dir.join("audit");
        fs::create_dir_all(&audit_dir).expect("audit dir");
        fs::write(
            audit_dir.join("cycle0_audit.md"),
            "Verdict: PASS_WITH_WARNINGS\n\nRecommended next action: launch cycle1 with a public articulated-object sequence and frozen evaluation gates.\n",
        )
        .expect("audit");
        let created = now();
        let config = TeamConfig {
            version: 1,
            id: "team-continuation".to_string(),
            goal: "Keep iterating: research -> docker -> runtime -> audit -> next cycle until blocker.".to_string(),
            lead: "lead".to_string(),
            members: vec![TeamMember {
                name: "lead".to_string(),
                role: "lead".to_string(),
                status: MemberStatus::Running,
                joined_at: created.clone(),
                thread_id: None,
                workspace_path: None,
                node: None,
            }],
            language: None,
            created_at: created.clone(),
            updated_at: created,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_ownerships(
            team_dir,
            &[FileOwnership {
                path: audit_dir.display().to_string(),
                owner: "audit".to_string(),
                note: "cycle0 final audit".to_string(),
                updated_at: now(),
            }],
        )
        .expect("write ownerships");

        let active = HashMap::new();
        let mut last_tick = Instant::now() - Duration::from_secs(181);
        maybe_send_lead_autonomy_tick(
            team_dir,
            &config,
            &active,
            &mut last_tick,
            Duration::from_secs(180),
            TeamPromptLanguage::En,
        )
        .expect("lead tick");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let tick = lead_messages.last().expect("tick message");
        assert!(tick.message.contains("Recent artifact next-action signals"));
        assert!(tick.message.contains("public articulated-object sequence"));
        assert!(
            tick.message
                .contains("explicitly requests continuation/iteration")
        );
        assert!(tick.message.contains("Open tasks:\n- none"));
    }

    #[test]
    fn lead_proposal_collection_ignores_resolved_old_signals() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let created = now();
        let config = TeamConfig {
            version: 1,
            id: "team-proposals".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![
                TeamMember {
                    name: "lead".to_string(),
                    role: "lead".to_string(),
                    status: MemberStatus::Online,
                    joined_at: created.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
                TeamMember {
                    name: "runtime".to_string(),
                    role: "runtime".to_string(),
                    status: MemberStatus::Online,
                    joined_at: created.clone(),
                    thread_id: None,
                    workspace_path: None,
                    node: None,
                },
            ],
            language: None,
            created_at: created.clone(),
            updated_at: created,
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");

        send_team_message_to_dir(
            team_dir,
            "runtime",
            "lead",
            "LEAD_PROPOSAL: task 10 is ready; resume runtime after sync.",
        )
        .expect("send old proposal");
        send_team_message_to_dir(
            team_dir,
            "lead",
            "runtime",
            "Earlier LEAD_PROPOSAL items are accepted/addressed by the current dependency graph; no separate action needed.",
        )
        .expect("send resolution");
        std::thread::sleep(Duration::from_secs(1));
        send_team_message_to_dir(
            team_dir,
            "runtime",
            "lead",
            "LEAD_PROPOSAL: task 11 is ready; resume validator after runtime handoff.",
        )
        .expect("send new proposal");

        let proposals =
            collect_recent_lead_proposals(team_dir, "lead", 5).expect("collect proposals");
        assert_eq!(proposals.len(), 1);
        assert!(proposals[0].contains("task 11"));
        assert!(!proposals[0].contains("task 10"));
    }

    #[test]
    fn lead_autonomy_tick_is_suppressed_during_usage_limit_cooldown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Standby,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let worker = TeamMember {
            name: "research".to_string(),
            role: "research".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-cooldown".to_string(),
            goal: "continuous research loop".to_string(),
            lead: "lead".to_string(),
            members: vec![lead.clone(), worker],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_test_task(
            team_dir,
            "1",
            Some("research"),
            TaskStatus::InProgress,
            Vec::new(),
            None,
        );
        let mut active = HashMap::new();
        active.insert(
            "lead".to_string(),
            AppServerMemberRun {
                member: lead,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: true,
                failed: false,
                standby_after_turn: false,
                team_message_scan_offset: 0,
                last_activity_at: Instant::now(),
                last_activity_kind: "usage_limited".to_string(),
                last_stale_notice_at: None,
                retry_not_before: Some(Instant::now() + Duration::from_secs(300)),
                side_context_ids: Vec::new(),
            },
        );
        let mut last_tick = Instant::now() - Duration::from_secs(181);

        maybe_send_lead_autonomy_tick(
            team_dir,
            &config,
            &active,
            &mut last_tick,
            Duration::from_secs(180),
            TeamPromptLanguage::En,
        )
        .expect("lead tick");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert!(lead_messages.is_empty());
        let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).expect("events");
        assert!(
            events
                .iter()
                .any(|event| event.event == "lead_autonomy_tick_suppressed")
        );
    }

    #[test]
    fn usage_limit_cooldown_is_restored_from_recent_event_after_restart() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let retry_at = (Local::now() + chrono::Duration::hours(2))
            .format("%B %-d, %Y %-I:%M %p")
            .to_string();
        append_event(
            team_dir,
            "app_server_member_usage_limited",
            serde_json::json!({
                "member": "lead",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": format!("You've hit your usage limit. Visit settings or try again at {retry_at}."),
                "retry_after_sec": 2700,
            }),
        )
        .expect("usage event");

        let remaining = recent_usage_limit_retry_remaining_with_auth(team_dir, "lead", None)
            .expect("restore cooldown");

        assert!(remaining.is_some_and(|remaining| remaining > Duration::from_secs(90 * 60)));
    }

    #[test]
    fn usage_limit_cooldown_is_ignored_after_auth_refresh() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let event_time = Utc::now() - chrono::Duration::hours(1);
        let event = Event {
            event: "app_server_member_usage_limited",
            timestamp: event_time.to_rfc3339_opts(SecondsFormat::Secs, true),
            team: "team-task-test",
            data: serde_json::json!({
                "member": "lead",
                "node": "local",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": "You've hit your usage limit. Visit settings or try again at May 12th, 2026 7:43 AM.",
                "retry_after_sec": 7200,
            }),
        };
        append_jsonl(&team_dir.join("events.jsonl"), &event).expect("usage event");
        let auth_json = team_dir.join("auth.json");
        fs::write(&auth_json, "{}").expect("auth refresh");

        let remaining =
            recent_usage_limit_retry_remaining_with_auth(team_dir, "lead", Some(&auth_json))
                .expect("cooldown check");

        assert!(remaining.is_none());
    }

    #[test]
    fn usage_limit_cooldown_is_ignored_after_node_device_auth_refresh() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        write_test_config(team_dir);
        let mut config = load_config(team_dir).expect("config");
        config.members.push(TeamMember {
            name: "docker_build".to_string(),
            role: "ops".to_string(),
            status: MemberStatus::Standby,
            joined_at: now(),
            thread_id: None,
            workspace_path: None,
            node: Some("saitou".to_string()),
        });
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        let event_time = Utc::now() - chrono::Duration::hours(1);
        let event = Event {
            event: "app_server_member_usage_limited",
            timestamp: event_time.to_rfc3339_opts(SecondsFormat::Secs, true),
            team: "team-task-test",
            data: serde_json::json!({
                "member": "docker_build",
                "node": "saitou",
                "thread": "thread",
                "turn": "turn",
                "status": "Failed",
                "error": "You've hit your usage limit. Visit settings or try again at May 12th, 2026 7:43 AM.",
                "retry_after_sec": 7200,
            }),
        };
        append_jsonl(&team_dir.join("events.jsonl"), &event).expect("usage event");
        append_event(
            team_dir,
            "node_direct_device_auth_completed",
            serde_json::json!({
                "node": "saitou",
                "url": "https://auth.openai.com/codex/device",
                "log": "/tmp/node-saitou.log",
            }),
        )
        .expect("auth event");

        let remaining =
            recent_usage_limit_retry_remaining_with_auth(team_dir, "docker_build", None)
                .expect("cooldown check");

        assert!(remaining.is_none());
    }

    #[test]
    fn usage_limit_cooldown_uses_retry_at_time_when_present() {
        let error = "You've hit your usage limit. Visit settings or try again at 4:43 PM.";
        let now_secs = 16 * 60 * 60 + 16 * 60;

        let cooldown = usage_limit_cooldown_from_error(error, &[now_secs]);

        assert_eq!(cooldown, Duration::from_secs(27 * 60));
    }

    #[test]
    fn usage_limit_cooldown_uses_full_retry_datetime_when_present() {
        let error = "You've hit your usage limit. Visit https://chatgpt.com/codex/settings/usage to purchase more credits or try again at May 12th, 2026 7:43 AM.";
        let now = Local
            .with_ymd_and_hms(2026, 5, 9, 4, 23, 0)
            .single()
            .expect("local datetime");

        let cooldown = usage_limit_cooldown_from_error_at(error, now, &[4 * 60 * 60 + 23 * 60]);

        assert_eq!(
            cooldown,
            Duration::from_secs((3 * 24 * 60 * 60) + (3 * 60 * 60) + (20 * 60))
        );
    }

    #[test]
    fn usage_limit_cooldown_rolls_retry_time_to_next_day() {
        let error = "You've hit your usage limit. Please try again at 7:43 AM.";
        let now_secs = 23 * 60 * 60 + 50 * 60;

        let cooldown = usage_limit_cooldown_from_error(error, &[now_secs]);

        assert_eq!(cooldown, Duration::from_secs(7 * 60 * 60 + 53 * 60));
    }

    #[test]
    fn usage_limit_cooldown_chooses_shortest_wall_clock_candidate() {
        let error = "You've hit your usage limit. Please try again at 7:43 AM.";
        let jst_now_secs = 16 * 60 * 60 + 21 * 60;
        let utc_now_secs = 7 * 60 * 60 + 21 * 60;

        let cooldown = usage_limit_cooldown_from_error(error, &[jst_now_secs, utc_now_secs]);

        assert_eq!(cooldown, Duration::from_secs(22 * 60));
    }

    #[test]
    fn usage_limit_cooldown_short_backs_off_when_retry_minute_just_passed() {
        let error = "You've hit your usage limit. Please try again at 7:43 AM.";
        let utc_now_secs = 7 * 60 * 60 + 43 * 60 + 3;

        let cooldown = usage_limit_cooldown_from_error(error, &[utc_now_secs]);

        assert_eq!(cooldown, Duration::from_secs(5 * 60));
    }

    #[test]
    fn usage_limit_cooldown_falls_back_without_retry_time() {
        let error = "You've hit your usage limit. Visit settings to purchase more credits.";

        let cooldown = usage_limit_cooldown_from_error(error, &[0]);

        assert_eq!(cooldown, Duration::from_secs(45 * 60));
    }

    #[test]
    fn stale_active_turn_warns_lead_and_member() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let team_dir = tmp.path();
        fs::create_dir_all(team_dir.join("tasks")).expect("tasks dir");
        fs::create_dir_all(team_dir.join("mailboxes")).expect("mailboxes dir");
        let now = now();
        let lead = TeamMember {
            name: "lead".to_string(),
            role: "lead".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let worker = TeamMember {
            name: "capture".to_string(),
            role: "capture".to_string(),
            status: MemberStatus::Running,
            joined_at: now.clone(),
            thread_id: None,
            workspace_path: None,
            node: None,
        };
        let config = TeamConfig {
            version: 1,
            id: "team-stale".to_string(),
            goal: "test".to_string(),
            lead: "lead".to_string(),
            members: vec![lead, worker.clone()],
            language: None,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        write_json_atomic(&team_dir.join("config.json"), &config).expect("write config");
        write_json_atomic(
            &task_path(team_dir, "3"),
            &TeamTask {
                id: "3".to_string(),
                subject: "capture plan".to_string(),
                description: String::new(),
                owner: Some("capture".to_string()),
                status: TaskStatus::InProgress,
                depends_on: Vec::new(),
                result: None,
                created_at: now.clone(),
                updated_at: now,
            },
        )
        .expect("write task");

        let mut active = HashMap::new();
        active.insert(
            "capture".to_string(),
            AppServerMemberRun {
                member: worker,
                node_id: "local".to_string(),
                cwd: team_dir.to_path_buf(),
                thread_id: "thread".to_string(),
                turn_id: "turn".to_string(),
                completed: false,
                failed: false,
                standby_after_turn: false,
                team_message_scan_offset: 0,
                last_activity_at: Instant::now() - Duration::from_secs(900),
                last_activity_kind: "turn_started".to_string(),
                last_stale_notice_at: None,
                retry_not_before: None,
                side_context_ids: Vec::new(),
            },
        );

        let mut last_check = Instant::now() - Duration::from_secs(31);
        maybe_warn_stale_active_turns(
            team_dir,
            &config,
            &mut active,
            &mut last_check,
            Duration::from_secs(30),
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("stale warning");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        let worker_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "capture")).expect("worker mailbox");
        assert_eq!(lead_messages.len(), 1);
        assert_eq!(worker_messages.len(), 1);
        assert!(
            lead_messages[0]
                .message
                .contains("Stale active turn attention")
        );
        assert_eq!(worker_messages[0].from, "lead");
        assert!(
            worker_messages[0]
                .message
                .contains("Automated lead status check")
        );

        active
            .get_mut("capture")
            .expect("active capture")
            .last_stale_notice_at = Some(Instant::now() - Duration::from_secs(601));
        let mut last_check = Instant::now() - Duration::from_secs(31);
        maybe_warn_stale_active_turns(
            team_dir,
            &config,
            &mut active,
            &mut last_check,
            Duration::from_secs(30),
            Duration::from_secs(600),
            TeamPromptLanguage::En,
        )
        .expect("repeated stale warning");

        let lead_messages =
            read_jsonl::<MailMessage>(&mailbox_path(team_dir, "lead")).expect("lead mailbox");
        assert_eq!(lead_messages.len(), 2);
        assert!(lead_messages[1].message.contains("Escalation:"));
        assert!(
            lead_messages[1]
                .message
                .contains("cancel/reassign/recover the task")
        );
    }
}
