use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use anyhow::bail;
use chrono::SecondsFormat;
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
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::TurnSteerParams;
use codex_app_server_protocol::TurnSteerResponse;
use codex_app_server_protocol::UserInput as AppServerUserInput;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

const CODEX_TEAM_HELPER_URL: &str =
    "https://raw.githubusercontent.com/yukimaru77/codex-team-tools/main/bin/codex-team";

#[derive(Debug, Parser)]
#[command(bin_name = "codex team")]
pub struct TeamCli {
    #[command(subcommand)]
    subcommand: TeamSubcommand,
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

    /// Manage shared team tasks.
    Task(TaskCli),

    /// Manage shared file ownership claims.
    Ownership(OwnershipCli),

    /// Manage team departments.
    Member(MemberCli),

    /// Manage local, SSH, Docker, and remote app-server nodes.
    Node(NodeCli),

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

    /// Do not keep the app-server team alive after tasks complete.
    #[arg(long, default_value_t = false)]
    no_keep_alive: bool,

    /// Connect to an existing app-server websocket instead of starting one.
    #[arg(long)]
    app_server_url: Option<String>,

    /// Ignore the registered default app-server and start a private one.
    #[arg(long, default_value_t = false)]
    no_app_server_registry: bool,
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

    /// List tasks.
    List,

    /// Update task owner, status, or result.
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

    /// Result or summary for the task.
    #[arg(long)]
    result: Option<String>,
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

    /// Recipient member name.
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

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TeamConfig {
    version: u32,
    id: String,
    goal: String,
    lead: String,
    members: Vec<TeamMember>,
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, clap::ValueEnum)]
#[clap(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
enum TaskStatus {
    Pending,
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
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Review => "review",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct MailMessage {
    from: String,
    to: String,
    message: String,
    timestamp: String,
    read: bool,
}

#[derive(Debug, Serialize)]
struct Event<'a> {
    event: &'a str,
    timestamp: String,
    team: &'a str,
    data: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct AppServerRegistry {
    url: String,
    pid: u32,
    updated_at: String,
}

impl TeamCli {
    pub async fn run(self) -> Result<()> {
        let codex_home =
            codex_core::config::find_codex_home().context("failed to resolve CODEX_HOME")?;
        let root = codex_home.join("teams");

        match self.subcommand {
            TeamSubcommand::Start(args) => {
                let (team_id, team_dir) = create_team(&root, args)?;
                println!("Created team `{team_id}`");
                println!("State: {}", team_dir.display());
                Ok(())
            }
            TeamSubcommand::Run(args) => {
                if args.app_server {
                    run_team_app_server(&root, args).await
                } else {
                    run_team(&root, args)
                }
            }
            TeamSubcommand::List => list_teams(&root),
            TeamSubcommand::Status(selector) => {
                let team_dir = resolve_team_dir(&root, selector.team.as_deref())?;
                print_status(&team_dir)
            }
            TeamSubcommand::Discuss(args) => discuss_team(&root, args),
            TeamSubcommand::Task(cli) => run_task(&root, cli),
            TeamSubcommand::Ownership(cli) => run_ownership(&root, cli),
            TeamSubcommand::Member(cli) => run_member(&root, cli),
            TeamSubcommand::Node(cli) => run_node(&root, cli),
            TeamSubcommand::Message(args) => send_message(&root, args),
            TeamSubcommand::Inbox(args) => read_inbox(&root, args),
            TeamSubcommand::Logs(args) => read_logs(&root, args),
            TeamSubcommand::Monitor(args) => start_tmux_monitor(&root, args),
            TeamSubcommand::Ui(args) => start_team_ui(&root, args),
            TeamSubcommand::Cleanup(args) => cleanup_team(&root, args),
        }
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
    Ok(Some(url.to_string()))
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
        None => format!("team-{}", Utc::now().format("%Y%m%d%H%M%S")),
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
        nodes: infer_placement_candidates(goal),
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
    let mut departments = design
        .departments
        .into_iter()
        .filter(|department| !department.name.trim().is_empty())
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
                "Department mission for {}: {}\n\nOperate as one department-level Codex session. If the mission is broad or heavy, use available subagent/agent tools, skills, MCP servers, or internal decomposition inside this department instead of asking the lead to create duplicate peer departments for load balancing.",
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
    model: Option<&str>,
    profile: Option<&str>,
    sandbox: Option<&str>,
    dangerously_bypass_approvals_and_sandbox: bool,
) -> Result<LeadDepartmentDesign> {
    let output =
        tempfile::NamedTempFile::new().context("create lead department design temp file")?;
    let output_path = output.path().to_path_buf();
    let prompt = build_lead_department_design_prompt(goal, placement_candidates);
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
) -> String {
    let candidates = if placement_candidates.is_empty() {
        "(none; use local unless the user explicitly identifies a reachable SSH host or Docker container later)".to_string()
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
    format!(
        r#"You are the lead agent directly listening to the user's request. The user is effectively the president/CEO. Your job is to create departments, not balanced worker headcount.

User goal:
{goal}

Placement candidates inferred from the user request or explicit CLI flags:
{candidates}

Design a small department structure for this goal:
- Create 2 to 5 departments.
- Each department is one peer Codex session with a clear ownership domain.
- Do not create duplicate departments just to balance workload.
- If a department's work is heavy, that department should use available subagent/agent tools, skills, MCP servers, or its own internal decomposition.
- Prefer domain ownership such as product, engineering, design, quality, research, docs, ops, security, data, etc.
- Use lowercase ASCII identifiers for department names and roles.
- Decide placement as part of the department design. Use `"node": "local"` or omit `node` for local departments. Use a non-local node only when the user's request clearly calls for that site, such as SSH work, Docker/container work, GPU/remote environment work, or environment-specific validation.
- You may include extra node candidates in `nodes` when the user mentioned a reachable SSH host or Docker container that is not already in the placement candidates.
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
            .map(|container| format!("{id}@docker={}", container.trim())),
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref()?.trim();
            let container = node.container.as_deref()?.trim();
            if host.is_empty() || container.is_empty() {
                None
            } else {
                Some(format!("{id}@ssh-docker={host}:{container}"))
            }
        }
    }
}

fn node_spec_id(raw: &str) -> String {
    let left = raw.split_once('=').map(|(left, _)| left).unwrap_or(raw);
    let id = left.split_once('@').map(|(id, _)| id).unwrap_or(left);
    sanitize_id(id)
}

fn infer_placement_candidates(goal: &str) -> Vec<LeadNodeDesign> {
    let mut nodes = Vec::new();
    let words = placement_words(goal);
    for (idx, word) in words.iter().enumerate() {
        let lower = word.to_ascii_lowercase();
        if matches!(lower.as_str(), "ssh" | "ssh先" | "ssh-host" | "remote-ssh") {
            if words
                .get(idx + 1)
                .map(|word| {
                    matches!(
                        word.to_ascii_lowercase().as_str(),
                        "docker" | "container" | "containers" | "コンテナ" | "dockerコンテナ"
                    )
                })
                .unwrap_or(false)
            {
                continue;
            }
            if let Some(host) = next_placement_value(&words, idx + 1) {
                push_unique_lead_node(
                    &mut nodes,
                    LeadNodeDesign {
                        id: sanitize_id(&host),
                        kind: TeamNodeKind::Ssh,
                        host: Some(host),
                        container: None,
                        cwd: None,
                        note: "Inferred from SSH wording in the user request.".to_string(),
                    },
                );
            }
        }
        if matches!(
            lower.as_str(),
            "docker" | "container" | "containers" | "コンテナ" | "dockerコンテナ"
        ) {
            if words
                .get(idx.saturating_sub(1))
                .filter(|_| idx > 0)
                .map(|word| {
                    matches!(
                        word.to_ascii_lowercase().as_str(),
                        "ssh" | "ssh先" | "ssh-host" | "remote-ssh"
                    )
                })
                .unwrap_or(false)
            {
                continue;
            }
            let container =
                next_placement_value(&words, idx + 1).unwrap_or_else(|| "codex-team".to_string());
            push_unique_lead_node(
                &mut nodes,
                LeadNodeDesign {
                    id: sanitize_id(&container),
                    kind: TeamNodeKind::Docker,
                    host: None,
                    container: Some(container),
                    cwd: None,
                    note: "Inferred from Docker/container wording in the user request.".to_string(),
                },
            );
        }
    }
    nodes
}

fn placement_words(goal: &str) -> Vec<String> {
    goal.split(|ch: char| {
        ch.is_whitespace()
            || matches!(
                ch,
                '"' | '\''
                    | '`'
                    | ','
                    | '.'
                    | ':'
                    | ';'
                    | '('
                    | ')'
                    | '['
                    | ']'
                    | '{'
                    | '}'
                    | '、'
                    | '。'
                    | '「'
                    | '」'
                    | '（'
                    | '）'
            )
    })
    .filter(|part| !part.trim().is_empty())
    .map(|part| part.trim().to_string())
    .collect()
}

fn next_placement_value(words: &[String], start: usize) -> Option<String> {
    for word in words.iter().skip(start).take(3) {
        let lower = word.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "で" | "に"
                | "を"
                | "の"
                | "使う"
                | "使用"
                | "利用"
                | "host"
                | "container"
                | "node"
                | "nodes"
                | "department"
                | "departments"
                | "lead"
                | "local"
                | "remote"
                | "docker"
                | "ssh"
                | "smoke"
                | "test"
                | "works"
                | "work"
                | "working"
                | "current"
                | "directly"
                | "inside"
                | "run"
                | "running"
                | "must"
                | "should"
                | "send"
                | "message"
                | "task"
                | "from"
                | "to"
                | "on"
                | "in"
                | "at"
                | "コンテナ"
                | "部署"
                | "ローカル"
                | "リモート"
        ) {
            continue;
        }
        if lower.starts_with("--") || lower == "ssh" || lower == "docker" {
            continue;
        }
        let value = word
            .trim_matches(|ch: char| {
                matches!(
                    ch,
                    '"' | '\'' | '`' | ',' | '.' | ':' | ';' | '、' | '。' | '「' | '」'
                )
            })
            .to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn push_unique_lead_node(nodes: &mut Vec<LeadNodeDesign>, node: LeadNodeDesign) {
    let id = sanitize_id(&node.id);
    if id.is_empty() || nodes.iter().any(|existing| sanitize_id(&existing.id) == id) {
        return;
    }
    nodes.push(LeadNodeDesign { id, ..node });
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
    let inferred_nodes = infer_placement_candidates(&args.start.goal);
    merge_lead_node_designs(&mut args.start, &inferred_nodes);
    apply_natural_language_defaults(&mut args.start);
    let (team_id, team_dir) = create_team(root, args.start)?;
    println!("Created team `{team_id}`");
    println!("State: {}", team_dir.display());

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
    let inferred_nodes = infer_placement_candidates(&args.start.goal);
    merge_lead_node_designs(&mut args.start, &inferred_nodes);
    let use_lead_department_design = should_use_lead_department_design(&args.start);
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
        apply_natural_language_defaults(&mut args.start);
        None
    };

    let (team_id, team_dir) = create_team(root, args.start)?;
    println!("Created app-server team `{team_id}`");
    println!("State: {}", team_dir.display());
    if let Some(design) = lead_department_design.as_ref() {
        merge_lead_node_metadata(&team_dir, &design.nodes)?;
        append_event(
            &team_dir,
            "lead_department_design",
            serde_json::json!({ "nodes": &design.nodes, "departments": &design.departments }),
        )?;
    }

    assign_unowned_tasks_round_robin(&team_dir)?;
    let mut config = load_config(&team_dir)?;
    let tasks = load_tasks(&team_dir)?;
    let workers = team_workers(&config);
    if workers.is_empty() {
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
            let prompt = build_app_server_lead_prompt(&config, &tasks, lead_member, &codex_exe);
            println!(
                "--- app-server lead thread: {} ({}) ---",
                lead_member.name, lead_member.role
            );
            println!("{prompt}");
        }
        for member in &workers {
            let mut dry_nodes = load_nodes(&team_dir)?;
            ensure_local_node(&mut dry_nodes);
            let prompt =
                build_app_server_worker_prompt(&config, &tasks, member, &codex_exe, &dry_nodes);
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
    let mut nodes = load_nodes(&team_dir)?;
    ensure_local_node(&mut nodes);
    let mut needed_node_ids = vec!["local".to_string()];
    for member in &workers {
        let assigned = tasks
            .iter()
            .any(|task| task.owner.as_deref() == Some(member.name.as_str()));
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
                ephemeral: Some(true),
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
        let assigned = tasks
            .iter()
            .any(|task| task.owner.as_deref() == Some(member.name.as_str()));
        if !assigned {
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
                    ephemeral: Some(true),
                    ..ThreadStartParams::default()
                },
            })
            .await
            .map_err(|err| anyhow!(err))?;
        set_member_thread(&team_dir, &member.name, &thread.thread.id)?;
        set_member_workspace(&team_dir, &member.name, &worker_cwd)?;

        let prompt = build_app_server_worker_prompt(&config, &tasks, member, &codex_exe, &nodes);
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
            },
        );
        started_workers += 1;
    }

    if started_workers == 0 {
        bail!("no workers had assigned tasks");
    }

    let lead_prompt = build_app_server_lead_prompt(&config, &tasks, &lead_member, &codex_exe);
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

    let mut mailbox_counts = current_mailbox_counts(&team_dir, &config.members)?;
    let poll_interval = Duration::from_millis(args.reactive_poll_ms.max(250));
    let mut keep_alive_idle_reported = false;

    loop {
        let has_running_turn = active.values().any(|run| !run.completed);
        let has_unstarted_member = has_unstarted_app_server_members(&team_dir, &active)?;
        if !has_running_turn && !has_unstarted_member {
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
                    &thread_to_member,
                    &mut assistant_buffers,
                ).await?;
                nodes = load_nodes(&team_dir)?;
                ensure_local_node(&mut nodes);
                sync_removed_app_server_nodes(
                    &mut node_clients,
                    &mut node_processes,
                    &nodes,
                    &team_dir,
                    &active,
                ).await?;
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
                ).await?;
                steer_new_team_messages(
                    &mut node_clients,
                    &team_dir,
                    &config.members,
                    &mut active,
                    &mut mailbox_counts,
                    &cwd,
                    args.model.clone(),
                    approval_policy,
                    args.dangerously_bypass_approvals_and_sandbox,
                    &codex_exe,
                ).await?;
            }
        }
    }

    if !args.no_synthesis
        && let Some(lead_run) = active.get(&lead_member.name)
        && lead_run.completed
    {
        let prompt = build_app_server_lead_final_prompt(&config, &team_dir)?;
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
    ensure_member_exists(&config, &from)?;
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
    let config = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    let completed = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Completed))
        .count();
    let mut out = String::new();
    out.push_str(&format!("Team: {}\n", config.id));
    out.push_str(&format!("Goal: {}\n", config.goal));
    out.push_str(&format!("Members: {}\n", config.members.len()));
    for member in &config.members {
        out.push_str(&format!(
            "  {} ({}) {:?} node={}\n",
            member.name,
            member.role,
            member.status,
            member.node.as_deref().unwrap_or("local")
        ));
    }
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    out.push_str(&format!("Nodes: {}\n", nodes.len()));
    for node in nodes {
        out.push_str(&format!(
            "  {} {:?} {:?} url={}\n",
            node.id,
            node.kind,
            node.status,
            node.url.as_deref().unwrap_or("")
        ));
    }
    out.push_str(&format!("Tasks: {completed}/{} completed\n", tasks.len()));
    out.push_str(&format_tasks_text(team_dir)?);
    let ownerships = format_ownerships_text(team_dir)?;
    if !ownerships.trim().is_empty() && !ownerships.starts_with("No ownership") {
        out.push_str(&format!("Ownerships:\n{ownerships}"));
    }
    Ok(out)
}

fn format_tasks_text(team_dir: &Path) -> Result<String> {
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
        "in_progress" => Ok(TaskStatus::InProgress),
        "blocked" => Ok(TaskStatus::Blocked),
        "review" => Ok(TaskStatus::Review),
        "completed" => Ok(TaskStatus::Completed),
        "failed" => Ok(TaskStatus::Failed),
        "cancelled" | "canceled" => Ok(TaskStatus::Cancelled),
        other => bail!("unsupported task status `{other}`"),
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
        return Ok((url, None));
    }
    match &node.kind {
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
    }
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
    let remote_relay_port = relay_port;
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
        try_authorize_codex_device_from_log(&log_path, &mut auth_attempted)?;
        std::thread::sleep(Duration::from_millis(200));
    }
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
        try_authorize_codex_device_from_log(&log_path, &mut auth_attempted)?;
        std::thread::sleep(Duration::from_millis(200));
    }
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
        format!("http://127.0.0.1:{relay_port}")
    } else {
        let gateway = gateway.trim();
        if gateway.is_empty() {
            bail!("ssh-docker node `{}` has no docker gateway", node.id);
        }
        format!("http://{gateway}:{relay_port}")
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
        relay_port,
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
        .arg(format!("{relay_port}:127.0.0.1:{relay_port}"))
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
        try_authorize_codex_device_from_log(&log_path, &mut auth_attempted)?;
        std::thread::sleep(Duration::from_millis(200));
    }
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
if ! command -v curl >/dev/null 2>&1 || ! command -v tar >/dev/null 2>&1 || ! command -v bash >/dev/null 2>&1; then
  if [ -n "$install_prefix" ] || [ "$(id -u)" = "0" ]; then
    if command -v apt-get >/dev/null 2>&1; then
      $install_prefix apt-get update -y
      $install_prefix apt-get install -y curl tar ca-certificates bash
    elif command -v apk >/dev/null 2>&1; then
      $install_prefix apk add --no-cache curl tar ca-certificates bash
    elif command -v dnf >/dev/null 2>&1; then
      $install_prefix dnf install -y curl tar ca-certificates bash
    elif command -v yum >/dev/null 2>&1; then
      $install_prefix yum install -y curl tar ca-certificates bash
    fi
  fi
fi
if [ -z "${{HOME:-}}" ]; then
  export HOME=/root
fi
if command -v codex >/dev/null 2>&1; then
  CODEX_BIN="$(command -v codex)"
elif [ -x "$HOME/.codex/bin/codex" ]; then
  CODEX_BIN="$HOME/.codex/bin/codex"
elif [ -x "$HOME/.local/bin/codex" ]; then
  CODEX_BIN="$HOME/.local/bin/codex"
elif [ -x "$HOME/bin/codex" ]; then
  CODEX_BIN="$HOME/bin/codex"
else
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
curl -fsSL {helper_url} -o "$HOME/bin/codex-team"
chmod 0755 "$HOME/bin/codex-team"
cd "$HOME"
export PATH="$HOME/bin:$PATH"
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

fn try_authorize_codex_device_from_log(log_path: &Path, attempted: &mut bool) -> Result<()> {
    if *attempted || !log_path.exists() {
        return Ok(());
    }
    let log = fs::read_to_string(log_path).unwrap_or_default();
    if !log.contains("auth.openai.com") && !log.contains("device") {
        return Ok(());
    }
    let health = Command::new("curl")
        .arg("-fsS")
        .arg("--max-time")
        .arg("2")
        .arg("http://127.0.0.1:3334/health")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !matches!(health, Ok(status) if status.success()) {
        return Ok(());
    }
    *attempted = true;
    let body = serde_json::json!({ "message": log }).to_string();
    let output = Command::new("curl")
        .arg("-sS")
        .arg("-X")
        .arg("POST")
        .arg("http://127.0.0.1:3334/authorize")
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("--max-time")
        .arg("600")
        .arg("-d")
        .arg(body)
        .output()
        .context("call codex-auth-server for remote device auth")?;
    append_text(
        log_path,
        &format!(
            "\n[codex-team auth-server status={}] {}\n{}\n",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )?;
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
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
}

struct TeamAppServerNodeClient {
    client: RemoteAppServerClient,
    request_counter: i64,
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
) -> Result<()> {
    let Some(run) = active.get_mut(member_name) else {
        bail!("member `{member_name}` has no app-server thread");
    };
    let node_client = node_clients
        .get_mut(&run.node_id)
        .with_context(|| format!("app-server client missing for node `{}`", run.node_id))?;
    let turn_cwd = run.cwd.clone();
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
    set_member_status(team_dir, member_name, MemberStatus::Running)?;
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
    Ok(())
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
                    bail!("app-server node `{node_id}` disconnected");
                }
                Err(_) => break,
            };
            handle_app_server_event(
                &mut node_client.client,
                &node_id,
                event,
                team_dir,
                active,
                thread_to_member,
                assistant_buffers,
            )
            .await?;
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
    thread_to_member: &HashMap<String, String>,
    assistant_buffers: &mut HashMap<String, String>,
) -> Result<()> {
    match event {
        AppServerEvent::ServerNotification(ServerNotification::AgentMessageDelta(delta)) => {
            if let Some(member) = thread_to_member.get(&thread_key(node_id, &delta.thread_id)) {
                assistant_buffers
                    .entry(member.clone())
                    .or_default()
                    .push_str(&delta.delta);
                append_text(
                    &team_dir
                        .join("live_messages")
                        .join(format!("{}.md", sanitize_id(member))),
                    &delta.delta,
                )?;
            }
        }
        AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(completed)) => {
            handle_app_server_turn_completed(
                team_dir,
                active,
                thread_to_member,
                assistant_buffers,
                node_id,
                completed,
            )?;
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
    match completed.turn.status {
        TurnStatus::Completed => {
            if run.member.role == "lead" {
                set_member_status(team_dir, member_name, MemberStatus::Online)?;
            } else if run.standby_after_turn
                || member_status(team_dir, member_name)? == Some(MemberStatus::Standby)
            {
                set_member_status(team_dir, member_name, MemberStatus::Standby)?;
                complete_member_tasks_if_active(team_dir, member_name)?;
                run.standby_after_turn = false;
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
        }
        _ => {
            run.failed = true;
            set_member_status(team_dir, member_name, MemberStatus::Failed)?;
            append_event(
                team_dir,
                "app_server_member_failed",
                serde_json::json!({
                    "member": member_name,
                    "node": node_id,
                    "thread": completed.thread_id,
                    "turn": completed.turn.id,
                    "status": format!("{:?}", completed.turn.status),
                    "error": completed.turn.error.map(|err| err.message),
                }),
            )?;
        }
    }
    ingest_team_message_lines(team_dir, member_name, active, assistant_buffers)?;
    Ok(())
}

fn ingest_team_message_lines(
    team_dir: &Path,
    member_name: &str,
    active: &mut HashMap<String, AppServerMemberRun>,
    assistant_buffers: &HashMap<String, String>,
) -> Result<()> {
    let Some(run) = active.get_mut(member_name) else {
        return Ok(());
    };
    let Some(buffer) = assistant_buffers.get(member_name) else {
        return Ok(());
    };
    let offset = run.team_message_scan_offset.min(buffer.len());
    let new_text = &buffer[offset..];
    run.team_message_scan_offset = buffer.len();
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
    Ok(())
}

fn parse_team_message_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    let marker = line.find("TEAM_MESSAGE ")?;
    let rest = &line[marker + "TEAM_MESSAGE ".len()..];
    let rest = rest.strip_prefix("to=")?;
    let (to, message) = rest.split_once(':')?;
    let to = sanitize_id(to.trim());
    let message = message.trim();
    if to.is_empty() || message.is_empty() {
        return None;
    }
    Some((to, message.to_string()))
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
) -> Result<()> {
    let latest = load_config(team_dir)?;
    let tasks = load_tasks(team_dir)?;
    for member in latest.members.iter().filter(|member| member.role != "lead") {
        if active.contains_key(&member.name) {
            continue;
        }
        if !matches!(member.status, MemberStatus::Online | MemberStatus::Running) {
            continue;
        }
        let has_active_task = tasks.iter().any(|task| {
            task.owner.as_deref() == Some(member.name.as_str())
                && !matches!(
                    task.status,
                    TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
                )
        });
        if !has_active_task {
            continue;
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
            let (url, process) = resolve_or_spawn_node_app_server(team_dir, &node, relay_port)?;
            if let Some(process) = process {
                node_processes.push(process);
            }
            let connected_client = connect_team_app_server(&url).await?;
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
                    ephemeral: Some(true),
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
            .or_insert(read_jsonl::<MailMessage>(&mailbox_path(team_dir, &member.name))?.len());
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
    let connected = node_clients.keys().cloned().collect::<Vec<_>>();
    for node_id in connected {
        if node_id == "local" || known.contains(&node_id) {
            continue;
        }
        let active_member = active
            .values()
            .any(|run| run.node_id == node_id && !run.completed);
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
                        && !matches!(
                            task.status,
                            TaskStatus::Completed | TaskStatus::Cancelled | TaskStatus::Failed
                        )
                })
        }))
}

fn current_mailbox_counts(
    team_dir: &Path,
    members: &[TeamMember],
) -> Result<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    for member in members {
        let count = read_jsonl::<MailMessage>(&mailbox_path(team_dir, &member.name))?.len();
        counts.insert(member.name.clone(), count);
    }
    Ok(counts)
}

async fn steer_new_team_messages(
    node_clients: &mut HashMap<String, TeamAppServerNodeClient>,
    team_dir: &Path,
    members: &[TeamMember],
    active: &mut HashMap<String, AppServerMemberRun>,
    mailbox_counts: &mut HashMap<String, usize>,
    cwd: &Path,
    model: Option<String>,
    approval_policy: Option<AskForApproval>,
    dangerously_bypass_approvals_and_sandbox: bool,
    codex_exe: &Path,
) -> Result<()> {
    let mut by_recipient = HashMap::<String, Vec<MailMessage>>::new();
    for member in members {
        let messages = read_jsonl::<MailMessage>(&mailbox_path(team_dir, &member.name))?;
        let seen = mailbox_counts
            .get(&member.name)
            .copied()
            .unwrap_or_default()
            .min(messages.len());
        mailbox_counts.insert(member.name.clone(), messages.len());
        if active.contains_key(&member.name) && !matches!(member.status, MemberStatus::Offline) {
            for message in messages.into_iter().skip(seen) {
                by_recipient
                    .entry(member.name.clone())
                    .or_default()
                    .push(message);
            }
        }
    }

    for (member_name, messages) in by_recipient {
        let Some(run) = active.get(&member_name) else {
            continue;
        };
        if run.completed {
            if run.member.role == "lead" {
                let config = load_config(team_dir)?;
                let prompt =
                    build_reactive_lead_turn_prompt(&run.member, &messages, codex_exe, &config.id);
                start_app_server_member_turn(
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
                );
                start_app_server_member_turn(
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
            }
            continue;
        }
        let steer_text = build_reactive_steer_prompt(&run.member, &messages);
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
            .request_typed(ClientRequest::TurnSteer {
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
                let response: TurnSteerResponse = response;
                append_event(
                    team_dir,
                    "app_server_turn_steered",
                    serde_json::json!({
                        "member": member_name,
                        "node": run.node_id,
                        "thread": run.thread_id.clone(),
                        "turn": response.turn_id,
                        "messages": messages.len(),
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
                        "messages": messages.len(),
                        "error": err.to_string(),
                    }),
                )?;
            }
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
    let completed = tasks
        .iter()
        .filter(|task| matches!(task.status, TaskStatus::Completed))
        .count();
    println!("Team: {}", config.id);
    println!("Goal: {}", config.goal);
    println!("Members: {}", config.members.len());
    for member in &config.members {
        println!(
            "  {} ({}) {:?} node={}",
            member.name,
            member.role,
            member.status,
            member.node.as_deref().unwrap_or("local")
        );
    }
    let mut nodes = load_nodes(team_dir)?;
    ensure_local_node(&mut nodes);
    println!("Nodes: {}", nodes.len());
    for node in nodes {
        println!(
            "  {} {:?} {:?} url={}",
            node.id,
            node.kind,
            node.status,
            node.url.as_deref().unwrap_or("")
        );
    }
    println!("Tasks: {completed}/{} completed", tasks.len());
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
    match cli.subcommand {
        TaskSubcommand::Add(args) => {
            let task = create_task(&team_dir, args)?;
            touch_config(&team_dir)?;
            append_event(
                &team_dir,
                "task_created",
                serde_json::json!({ "task": task }),
            )?;
            println!("Created task {}", task.id);
            Ok(())
        }
        TaskSubcommand::List => {
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
    let from = args.from.unwrap_or_else(default_team_member_name);
    ensure_member_exists(&config, &from)?;
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
    let listener =
        TcpListener::bind(&args.listen).with_context(|| format!("bind {}", args.listen))?;
    let url = format!("http://{}", args.listen);
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

fn ensure_team_ui_app_server(root: &Path) -> Result<Option<Child>> {
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
            let html = render_team_ui(root, args, selected.as_deref(), selected_cwd.as_deref())?;
            write_http_response(stream, "200 OK", "text/html; charset=utf-8", &html)?;
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
                    from: Some("lead".to_string()),
                    to,
                    message,
                },
            )?;
            redirect_team_ui(stream, Some(&team))?;
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
            let registered_app_server_url = read_registered_app_server_url().unwrap_or(None);
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("team")
                .arg("swarm")
                .arg(goal)
                .arg("--app-server")
                .arg("--dangerously-bypass-approvals-and-sandbox")
                .arg("--cd")
                .arg(cwd)
                .stdin(Stdio::null());
            if let Some(app_server_url) = app_server_url {
                if registered_app_server_url.as_deref() != Some(app_server_url.as_str()) {
                    command.arg("--app-server-url").arg(app_server_url);
                }
            } else {
                command.arg("--no-app-server-registry");
            }
            let log_path = root.join("ui-runs.log");
            let log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .with_context(|| format!("open {}", log_path.display()))?;
            let stderr = log.try_clone()?;
            command.stdout(Stdio::from(log)).stderr(Stdio::from(stderr));
            command.spawn().context("spawn team run from UI")?;
            redirect_team_ui(stream, None)?;
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
    let selected_events = selected_dir
        .as_ref()
        .and_then(|dir| fs::read_to_string(dir.join("events.jsonl")).ok())
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
    let team_links = teams
        .iter()
        .map(|team| {
            let active = selected_id.as_deref() == Some(team.id.as_str());
            format!(
                r#"<a class="team {active}" href="/?team={id}"><strong>{id}</strong><span>{goal}</span><small>{updated}</small></a>"#,
                active = if active { "active" } else { "" },
                id = html_escape(&team.id),
                goal = html_escape(&team.goal),
                updated = html_escape(&team.updated_at),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let detail = if let Some(config) = selected_config {
        let members = config
            .members
            .iter()
            .map(|member| {
                format!(
                    "<tr><td>{}</td><td>{}</td><td>{:?}</td><td>{}</td></tr>",
                    html_escape(&member.name),
                    html_escape(&member.role),
                    member.status,
                    html_escape(member.thread_id.as_deref().unwrap_or(""))
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
        format!(
            r#"<section><h2>{id}</h2><p>{goal}</p>
<form method="post" action="/message">
<input type="hidden" name="team" value="{id}">
<label>To <input name="to" value="lead"></label>
<label>Message <textarea name="message" rows="4"></textarea></label>
<button type="submit">Send</button>
</form>
<h3>Members</h3><table><tr><th>Name</th><th>Role</th><th>Status</th><th>Thread</th></tr>{members}</table>
<h3>Tasks</h3><table><tr><th>ID</th><th>Status</th><th>Owner</th><th>Subject</th></tr>{tasks}</table>
<h3>Events</h3><pre>{events}</pre></section>"#,
            id = html_escape(&config.id),
            goal = html_escape(&config.goal),
            members = members,
            tasks = tasks,
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
.team{{display:block;padding:10px;border-radius:6px;color:inherit;text-decoration:none;border:1px solid transparent;margin-bottom:8px}}
.team.active{{background:#eaf2ff;border-color:#8bb8ff}}
.team span,.team small{{display:block;color:#59636e;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
form{{display:grid;gap:10px;margin:12px 0;padding:12px;background:#fff;border:1px solid #d8dee4;border-radius:6px}}
label{{display:grid;gap:4px}} input,textarea{{font:inherit;padding:8px;border:1px solid #c9d1d9;border-radius:4px}} button{{width:max-content;padding:8px 12px}}
.dir-picker{{background:#fff;border:1px solid #d8dee4;border-radius:6px;padding:10px;margin:10px 0;max-height:260px;overflow:auto}}
.dir-picker a{{display:block;padding:5px 0;color:#0969da;text-decoration:none;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
.dir-current{{font-weight:600;word-break:break-all}}
table{{width:100%;border-collapse:collapse;background:#fff}} th,td{{padding:8px;border:1px solid #d8dee4;text-align:left;vertical-align:top}}
pre{{background:#111827;color:#d1d5db;padding:12px;border-radius:6px;overflow:auto;max-height:360px}}
</style></head><body><div class="app"><aside><h1>Lead Sessions</h1>{team_links}
<p><a href="{refresh_href}">Refresh</a></p>
<h2>New Team</h2><form method="post" action="/new">
<label>Goal <textarea name="goal" rows="5"></textarea></label>
<input type="hidden" name="cwd" value="{selected_cwd}">
<div><strong>Current Directory</strong>{directory_picker}</div>
<label>Existing App Server URL <input name="app_server_url" value="{registered_app_server_url}" placeholder="ws://127.0.0.1:12345"></label>
<button type="submit">Start</button></form></aside><main>{detail}</main></div></body></html>"#,
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
    Ok(TeamNode {
        id,
        kind,
        url,
        host,
        container,
        cwd: None,
        status: TeamNodeStatus::Pending,
        note: String::new(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
    })
}

fn create_task(team_dir: &Path, args: TaskAddArgs) -> Result<TeamTask> {
    let id = allocate_task_id(team_dir)?;
    let now = now();
    let task = TeamTask {
        id: id.clone(),
        subject: args.subject,
        description: args.description,
        owner: args.owner,
        status: if args.depends_on.is_empty() {
            TaskStatus::Pending
        } else {
            TaskStatus::Blocked
        },
        depends_on: args.depends_on,
        result: None,
        created_at: now.clone(),
        updated_at: now,
    };
    write_json_atomic(&task_path(team_dir, &id), &task)?;
    Ok(task)
}

fn update_task(team_dir: &Path, args: TaskSetArgs) -> Result<()> {
    let path = task_path(team_dir, &args.id);
    let mut task: TeamTask = read_json(&path)?;
    if args.clear_owner {
        task.owner = None;
    }
    if let Some(owner) = args.owner {
        task.owner = Some(owner);
    }
    if let Some(status) = args.status {
        task.status = status;
    }
    if let Some(result) = args.result {
        task.result = Some(result);
    }
    task.updated_at = now();
    write_json_atomic(&path, &task)?;
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "task_updated",
        serde_json::json!({ "task": task }),
    )?;
    println!("Updated task {}", args.id);
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
    let task = if let Some(mission) = args.mission {
        Some(create_task(
            team_dir,
            TaskAddArgs {
                subject: format!(
                    "Department mission for {}: {}\n\nOperate as one department-level Codex session.",
                    args.member, mission
                ),
                description: String::new(),
                owner: Some(args.member.clone()),
                depends_on: Vec::new(),
            },
        )?)
    } else {
        None
    };
    append_event(
        team_dir,
        "member_resumed",
        serde_json::json!({
            "member": args.member,
            "task": task,
        }),
    )?;
    println!("Resumed {}", args.member);
    Ok(())
}

fn run_node(root: &Path, cli: NodeCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    match cli.subcommand {
        NodeSubcommand::List => list_team_nodes(&team_dir),
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
    let mut nodes = load_nodes(team_dir)?;
    let now = now();
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
    println!("Registered node {id}");
    Ok(())
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
    touch_config(team_dir)?;
    append_event(
        team_dir,
        "node_removed",
        serde_json::json!({ "node": id, "forced": args.force }),
    )?;
    println!("Removed node {id}");
    Ok(())
}

fn assign_unowned_tasks_round_robin(team_dir: &Path) -> Result<()> {
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
    let mut changed = false;
    let mut worker_idx = 0usize;
    for task in &mut tasks {
        if task.owner.is_none() {
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
    let mut changed = false;
    let mut tasks = load_tasks(team_dir)?;
    for task in &mut tasks {
        if task.owner.as_deref() == Some(member_name)
            && matches!(task.status, TaskStatus::Pending | TaskStatus::Blocked)
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

fn complete_member_tasks_if_active(team_dir: &Path, member_name: &str) -> Result<()> {
    let mut changed = false;
    let mut tasks = load_tasks(team_dir)?;
    for task in &mut tasks {
        if task.owner.as_deref() == Some(member_name)
            && matches!(task.status, TaskStatus::InProgress | TaskStatus::Review)
        {
            task.status = TaskStatus::Completed;
            task.updated_at = now();
            if task.result.is_none() {
                task.result = Some("Worker exited successfully.".to_string());
            }
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

Coordinate through the native team store with these shell commands:
- "$CODEX_TEAM_CLI" team status --team "$CODEX_TEAM_ID"
- "$CODEX_TEAM_CLI" team node --team "$CODEX_TEAM_ID" list
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" list
- "$CODEX_TEAM_CLI" team ownership --team "$CODEX_TEAM_ID" list
- "$CODEX_TEAM_CLI" team ownership --team "$CODEX_TEAM_ID" claim <PATH> --note "<editing scope>"
- "$CODEX_TEAM_CLI" team ownership --team "$CODEX_TEAM_ID" release <PATH>
- "$CODEX_TEAM_CLI" team inbox --team "$CODEX_TEAM_ID"
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" set <TASK_ID> --status in_progress
- "$CODEX_TEAM_CLI" team task --team "$CODEX_TEAM_ID" set <TASK_ID> --status completed --result "<short result>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" lead "<message>"
- "$CODEX_TEAM_CLI" team message --team "$CODEX_TEAM_ID" all "<message>"

The message command defaults the sender to CODEX_TEAM_MEMBER, so teammates can DM each other without passing --from. Use `all` for a broadcast.

Start by reading your inbox and the ownership list. Before editing a file, claim the path with the ownership command. If another department owns the path, do not edit it until that department hands it off or lead explicitly reassigns ownership. Check your inbox again after important task updates and before finishing. Discuss disagreements, blockers, handoffs, and review findings through team messages. Own your department mission end to end. If the work is broad or heavy, use available subagent/agent tools, skills, MCP servers, or internal decomposition within this department. Do not ask the lead to create duplicate peer departments solely for load balancing. Work only on tasks assigned to your department unless the lead explicitly redirects you. When handing a file to another department, send a message and release or ask lead to reassign ownership. When finished, update task status and send a final message to lead. If this department is assigned to a non-local node, treat that node as your operational site. If Codex authentication is requested via device code, follow the local `codex-auth` skill by passing the device URL and code to the codex-auth-server.

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
  - "{codex}" team task --team "{team_id}" list
  - "{codex}" team ownership --team "{team_id}" list
  - "{codex}" team ownership --team "{team_id}" claim <PATH> --owner "{member}" --note "<editing scope>"
  - "{codex}" team ownership --team "{team_id}" release <PATH> --owner "{member}"
  - "{codex}" team inbox --team "{team_id}" "{member}"
  - "{codex}" team task --team "{team_id}" set <TASK_ID> --status in_progress
  - "{codex}" team task --team "{team_id}" set <TASK_ID> --status completed --result "<short result>"
  - "{codex}" team message --team "{team_id}" --from "{member}" lead "<message>"
  - "{codex}" team message --team "{team_id}" --from "{member}" all "<message>"
  - "{codex}" team message --team "{team_id}" --from "{member}" <member> "<direct question>"
- Non-local node coordination commands. If your department node is not `local`, prefer these first and run them directly in your current shell:
  - codex-team status
  - codex-team task list
  - codex-team ownership list
  - codex-team ownership claim <PATH> --owner "{member}" --note "<editing scope>"
  - codex-team ownership release <PATH> --owner "{member}"
  - codex-team inbox "{member}"
  - codex-team task set <TASK_ID> --status in_progress
  - codex-team task set <TASK_ID> --status completed --result "<short result>"
  - codex-team message --from "{member}" lead "<message>"
  - codex-team message --from "{member}" all "<message>"
  - codex-team message --from "{member}" <member> "<direct question>"

When a teammate sends you a message, the orchestrator may steer this active turn with the new message. Treat that as live team discussion and respond or adjust your work if needed.
If your work or an invoked skill creates or uses a Docker container for ongoing team work, do not leave it as an invisible side environment. Use a stable container name, mount the relevant workspace, publish any user-facing ports with `-p`, and send lead the exact container name, host, mount paths, exposed ports, and suggested node kind (`docker` or `ssh-docker`) so lead can register or update the placement. Avoid read-write mounting the host's entire `~/.codex` into a root-owned container; use a dedicated Codex home, copied credentials/config, or the existing bootstrap/auth flow. If lead has already assigned you to a Docker/SSH-Docker node, treat the execution node context above as authoritative.
If this session runs on a remote/SSH/Docker node where both the local team CLI path and `codex-team` are unavailable, communicate by writing a standalone line in this exact format:
TEAM_MESSAGE to=<lead|all|member>: <message>
The orchestrator will copy those lines into the local team mailbox after your turn completes.
{remote_note}
"#,
        codex = codex_exe.display(),
        team_id = config.id,
        member = member.name,
        node_context = node_context,
        remote_note = remote_note,
    ));
    prompt
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

fn build_reactive_steer_prompt(member: &TeamMember, messages: &[MailMessage]) -> String {
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

fn build_app_server_lead_prompt(
    config: &TeamConfig,
    tasks: &[TeamTask],
    member: &TeamMember,
    codex_exe: &Path,
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

    format!(
        r#"You are the live lead for a Codex app-server agent team.

Team: {team_id}
Goal: {goal}
Member: {member_name}
Role: {role}

You are a real app-server thread. Your job is orchestration, not implementation. Read current team state and your inbox, then send concise coordination only when useful.

Commands:
- "{codex}" team status --team "{team_id}"
- "{codex}" team node --team "{team_id}" list
- "{codex}" team node --team "{team_id}" add <node-id> --kind manual --url ws://127.0.0.1:<forwarded-port> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh --host <ssh-host> --cwd <remote-cwd> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" add <node-id> --kind docker --container <container> --cwd <container-cwd> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh-docker --host <ssh-host> --container <container> --cwd <container-cwd> --note "<site/purpose>"
- "{codex}" team node --team "{team_id}" remove <node-id> --force
- "{codex}" team task --team "{team_id}" list
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

At the beginning, assign obvious file or directory ownership when the goal implies shared edits. Name the primary owner and handoff order instead of letting departments edit the same file at the same time. Use ownership claims for these decisions, and message the relevant departments.

You also own placement. If the user request mentions SSH, a remote machine, Docker, a container, or environment-specific development/testing, inspect the node list and create or update the needed node before adding/resuming a department there. The team runner will bootstrap Codex, `codex-team`, and app-server on SSH/Docker nodes when a department is assigned to them. If passwordless sudo is available on a remote site, needed base libraries may be installed automatically by bootstrap. If auth is needed, use the `codex-auth` flow: capture the Codex device URL/code from the remote login output and pass it to the local codex-auth-server. Prefer adding or resuming a department on the right node over asking the user to hand-write `--node` flags.

Docker/container policy: this applies even when Docker is introduced by a skill, a department plan, or implementation needs rather than by the user's initial wording. If a department or skill creates a container that should host ongoing team work, create it with a stable name, mount the relevant workspace (for example `-v "$PWD:/workspace" -w /workspace`), publish any user-facing service ports with `-p host_port:container_port`, and keep it alive long enough for app-server bootstrap. Avoid read-write mounting the host's entire `~/.codex` into a root-owned container; use a dedicated Codex home, copied credentials/config, or the existing bootstrap/auth flow so host config ownership is not changed. Then register it as a node with `team node add --kind docker --container <name> --cwd /workspace` for local Docker, or `--kind ssh-docker --host <ssh-host> --container <name> --cwd /workspace` for Docker on an SSH host. For SSH-host Docker, run Docker creation/removal on that SSH host, then register the resulting `ssh-docker` node. If a container is rebuilt or replaced, update/remove the old node and add the new container node before assigning departments.

During keep-alive, keep placement dynamic just like departments: add nodes when new SSH/Docker work appears, add or resume departments on those nodes when useful, and remove nodes only when no active department needs them. Be conservative with removal: standby departments may still answer questions, so remove a node only after its departments are standby/completed and no follow-up is likely. Prefer standby for departments; use node removal for stale containers, recreated containers, or unreachable placement candidates.

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
    )
}

fn build_reactive_lead_turn_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    codex_exe: &Path,
    team_id: &str,
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

    format!(
        r#"Reactive lead update for {member} ({role}).

New message(s) arrived for lead while the lead turn was idle:
{message_lines}

Use the team CLI if you need context:
- "{codex}" team status --team "{team_id}"
- "{codex}" team node --team "{team_id}" list
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh --host <ssh-host> --cwd <remote-cwd>
- "{codex}" team node --team "{team_id}" add <node-id> --kind docker --container <container> --cwd <container-cwd>
- "{codex}" team node --team "{team_id}" add <node-id> --kind ssh-docker --host <ssh-host> --container <container> --cwd <container-cwd>
- "{codex}" team node --team "{team_id}" remove <node-id> --force
- "{codex}" team ownership --team "{team_id}" list
- "{codex}" team member --team "{team_id}" list
- "{codex}" team member --team "{team_id}" add <name:role> --node <node-id> --mission "<why this department is needed>"
- "{codex}" team member --team "{team_id}" standby <member> --reason "<why active work is no longer needed>"
- "{codex}" team inbox --team "{team_id}" lead

Respond as lead only if coordination, prioritization, clarification, ownership reassignment, placement add/remove, department add/standby/resume, or a handoff is useful. If a message reveals SSH/Docker/container work, create or update the placement node and assign/resume a department there. If a skill or department created/recreated a Docker container, register or update the Docker node immediately and tell the department it is now running in that placement. Keep this short and concrete.
"#,
        member = member.name,
        role = member.role,
        codex = codex_exe.display(),
        team_id = team_id,
        message_lines = message_lines,
    )
}

fn build_reactive_member_turn_prompt(
    member: &TeamMember,
    messages: &[MailMessage],
    codex_exe: &Path,
    team_id: &str,
    standby: bool,
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

    format!(
        r#"Reactive department follow-up for {member} ({role}).

{mode}

New teammate message(s):
{message_lines}

Use the team CLI if you need context:
- "{codex}" team status --team "{team_id}"
- "{codex}" team ownership --team "{team_id}" list
- "{codex}" team inbox --team "{team_id}" "{member}"

Respond only if useful. Send concise team messages for answers, handoffs, or blockers, then finish.
"#,
        member = member.name,
        role = member.role,
        mode = mode,
        message_lines = message_lines,
        codex = codex_exe.display(),
        team_id = team_id,
    )
}

fn build_app_server_lead_final_prompt(config: &TeamConfig, team_dir: &Path) -> Result<String> {
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

    Ok(format!(
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
    ))
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
    if to == "all" || to == "@all" {
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

    ensure_member_exists(config, to)?;
    Ok(vec![to.to_string()])
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

fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
