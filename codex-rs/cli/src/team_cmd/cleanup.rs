#[derive(Clone, Copy)]
enum UiTeamRunStatus {
    Running,
    Stop,
    Exiting,
    Unknown,
}

impl UiTeamRunStatus {
    fn label(self) -> &'static str {
        match self {
            UiTeamRunStatus::Running => "running",
            UiTeamRunStatus::Stop => "stop(idle)",
            UiTeamRunStatus::Exiting => "exiting",
            UiTeamRunStatus::Unknown => "unknown",
        }
    }

    fn css_class(self) -> &'static str {
        match self {
            UiTeamRunStatus::Running => "run-running",
            UiTeamRunStatus::Stop => "run-stop",
            UiTeamRunStatus::Exiting => "run-stopped",
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
    if args.all {
        let mut teams = load_team_summaries(root)?;
        teams.sort_by(|a, b| a.id.cmp(&b.id));
        let mut stopped = 0_usize;
        for config in teams {
            let team_dir = root.join(&config.id);
            if matches!(
                team_run_status_for_dir(&team_dir, &config.id),
                UiTeamRunStatus::Running | UiTeamRunStatus::Stop
            ) {
                stop_one_team_runtime(
                    root,
                    &team_dir,
                    &config,
                    args.keep_local_app_server,
                    args.no_remote_nodes,
                )?;
                stopped += 1;
            }
        }
        println!("Paused {stopped} live team runtime(s).");
        return Ok(());
    }
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    stop_one_team_runtime(
        root,
        &team_dir,
        &config,
        args.keep_local_app_server,
        args.no_remote_nodes,
    )
}

fn stop_one_team_runtime(
    root: &Path,
    team_dir: &Path,
    config: &TeamConfig,
    keep_local_app_server: bool,
    no_remote_nodes: bool,
) -> Result<()> {
    let mut stopped_pids = Vec::<u32>::new();
    for pid in [
        read_ui_team_pid(root, &config.id),
        read_team_run_pid(team_dir),
    ]
    .into_iter()
    .flatten()
    {
        if process_alive(pid) {
            stop_process_tree(pid, process_looks_like_codex_team);
            stopped_pids.push(pid);
        }
    }
    if !keep_local_app_server {
        if let Some(pid) = stop_registered_app_server_for_team(team_dir)? {
            stopped_pids.push(pid);
        }
    }
    let mut stopped_nodes = Vec::<String>::new();
    if !no_remote_nodes {
        stopped_nodes = stop_remote_node_app_servers(team_dir)?;
    }
    stopped_pids.extend(stop_local_team_id_processes(&config.id)?);
    let _ = fs::remove_file(team_run_pid_path(team_dir));
    let _ = remove_ui_team_pid(root, &config.id);
    set_running_members_to_standby_for_pause(team_dir)?;
    append_event(
        team_dir,
        "team_runtime_paused",
        serde_json::json!({
            "pids": stopped_pids,
            "remote_nodes": stopped_nodes,
            "keep_local_app_server": keep_local_app_server,
            "no_remote_nodes": no_remote_nodes,
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

fn cleanup_node_app_servers_before_spawn(team_dir: &Path, node: &TeamNode, team_id: &str) {
    if matches!(node.kind, TeamNodeKind::Local | TeamNodeKind::Manual) {
        return;
    }
    let cleaned = cleanup_remote_node_team_processes_scoped(node, team_id);
    if cleaned {
        let _ = append_event(
            team_dir,
            "node_app_server_pre_spawn_cleanup",
            serde_json::json!({
                "node": node.id,
                "kind": format!("{:?}", node.kind),
                "reason": "removed stale same-team remote/container app-server processes before spawning a fresh node runtime",
            }),
        );
    }
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

fn cleanup_remote_node_team_processes_scoped(node: &TeamNode, team_id: &str) -> bool {
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
                &container_team_cleanup_shell(team_id, container, false),
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
    let managed_container_prefixes = [
        format!("codex-team-{team_id}"),
        format!("team-{team_id}-"),
        format!("{team_id}-"),
    ];
    if include_team_app_server
        || managed_container_prefixes
            .iter()
            .any(|prefix| container.starts_with(prefix))
    {
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
    team_run_status_for_dir(&team_dir, &team.id)
}

fn team_run_status_for_dir(team_dir: &Path, team_id: &str) -> UiTeamRunStatus {
    let mut saw_pid = false;
    let tasks = load_tasks(team_dir).unwrap_or_default();
    let waits = load_waits(team_dir).unwrap_or_default();
    let open_work = open_task_count(&tasks) > 0 || open_wait_count(&waits) > 0;
    for pid in [
        team_dir
            .parent()
            .and_then(|root| read_ui_team_pid(root, team_id)),
        read_team_run_pid(&team_dir),
    ]
    .into_iter()
    .flatten()
    {
        saw_pid = true;
        if process_alive(pid) && process_looks_like_codex_team(pid) {
            return if open_work {
                UiTeamRunStatus::Running
            } else {
                UiTeamRunStatus::Stop
            };
        }
    }
    if saw_pid {
        return UiTeamRunStatus::Exiting;
    }
    let Ok(events) = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")) else {
        return UiTeamRunStatus::Unknown;
    };
    for event in events.into_iter().rev().take(20) {
        match event.event.as_str() {
            "team_runtime_paused" => return UiTeamRunStatus::Exiting,
            "app_server_keep_alive_stopped" => return UiTeamRunStatus::Exiting,
            "app_server_keep_alive_idle" => return UiTeamRunStatus::Unknown,
            _ => {}
        }
    }
    UiTeamRunStatus::Unknown
}

fn team_keep_alive_idle_age_secs(team_dir: &Path) -> Result<Option<u64>> {
    let events = read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl"))?;
    for event in events.into_iter().rev() {
        match event.event.as_str() {
            "app_server_keep_alive_idle" => {
                let timestamp = DateTime::parse_from_rfc3339(&event.timestamp)
                    .with_context(|| format!("parse event timestamp {}", event.timestamp))?;
                let elapsed = Utc::now()
                    .signed_duration_since(timestamp.with_timezone(&Utc))
                    .num_seconds()
                    .max(0) as u64;
                return Ok(Some(elapsed));
            }
            "app_server_keep_alive_stopped" | "team_runtime_paused" => return Ok(None),
            _ => {}
        }
    }
    Ok(None)
}

fn cleanup_team(root: &Path, args: CleanupArgs) -> Result<()> {
    if args.exiting {
        let mut teams = load_team_summaries(root)?;
        teams.sort_by(|a, b| a.id.cmp(&b.id));
        let mut deleted = 0_usize;
        for config in teams {
            let team_dir = root.join(&config.id);
            if !matches!(
                team_run_status_for_dir(&team_dir, &config.id),
                UiTeamRunStatus::Exiting
            ) {
                continue;
            }
            cleanup_one_team(root, &team_dir, &config, &args)?;
            deleted += 1;
        }
        if args.dry_run {
            println!("Would delete {deleted} exiting team workspace(s).");
        } else {
            println!("Deleted {deleted} exiting team workspace(s).");
        }
        return Ok(());
    }
    let team_dir = resolve_team_dir(root, args.selector.team.as_deref())?;
    let config = load_config(&team_dir)?;
    cleanup_one_team(root, &team_dir, &config, &args)
}

fn cleanup_one_team(
    root: &Path,
    team_dir: &Path,
    config: &TeamConfig,
    args: &CleanupArgs,
) -> Result<()> {
    if !args.force && !args.dry_run {
        bail!("refusing to delete `{}` without --force", config.id);
    }
    if args.dry_run {
        println!("Would delete team `{}`", config.id);
        println!("  local state: {}", team_dir.display());
    }
    let runtime_status = team_run_status_for_dir(team_dir, &config.id);
    if matches!(
        runtime_status,
        UiTeamRunStatus::Running | UiTeamRunStatus::Stop
    ) {
        if args.dry_run {
            println!("  would pause live runtime before cleanup");
        } else {
            stop_one_team_runtime(
                root,
                team_dir,
                config,
                false,
                !(args.remote_state || args.containers),
            )?;
        }
    }
    if args.remote_state || args.containers {
        cleanup_remote_team_resources(team_dir, config, args)?;
    }
    if args.dry_run {
        return Ok(());
    }
    remove_member_worktrees(&config);
    fs::remove_dir_all(&team_dir)
        .with_context(|| format!("failed to remove {}", team_dir.display()))?;
    let _ = remove_ui_team_pid(root, &config.id);
    println!("Deleted team `{}`", config.id);
    Ok(())
}

fn cleanup_remote_team_resources(
    team_dir: &Path,
    config: &TeamConfig,
    args: &CleanupArgs,
) -> Result<()> {
    let nodes = load_nodes(team_dir)?;
    let mut failures = Vec::<String>::new();
    for node in nodes {
        if matches!(node.kind, TeamNodeKind::Local | TeamNodeKind::Manual) {
            continue;
        }
        let mut node_actions = Vec::<String>::new();
        if args.remote_state {
            match cleanup_remote_team_state_for_node(&node, &config.id, args.dry_run) {
                Ok(action) => node_actions.push(action),
                Err(err) => failures.push(format!("{} remote-state: {err:#}", node.id)),
            }
        }
        if args.containers && matches!(node.kind, TeamNodeKind::Docker | TeamNodeKind::SshDocker) {
            match remove_team_container_for_node(&node, args.dry_run) {
                Ok(action) => node_actions.push(action),
                Err(err) => failures.push(format!("{} container: {err:#}", node.id)),
            }
        }
        if !node_actions.is_empty() {
            println!("{}: {}", node.id, node_actions.join("; "));
        }
    }
    if !failures.is_empty() {
        for failure in &failures {
            eprintln!("remote cleanup failed: {failure}");
        }
        if !args.ignore_remote_errors {
            bail!(
                "remote cleanup failed for {} operation(s); rerun with --ignore-remote-errors to delete local state anyway",
                failures.len()
            );
        }
    }
    if !args.dry_run {
        append_event(
            team_dir,
            "team_remote_cleanup_completed",
            serde_json::json!({
                "remote_state": args.remote_state,
                "containers": args.containers,
                "failures": failures,
                "ignore_remote_errors": args.ignore_remote_errors,
            }),
        )?;
    }
    Ok(())
}

fn cleanup_remote_team_state_for_node(
    node: &TeamNode,
    team_id: &str,
    dry_run: bool,
) -> Result<String> {
    let script = remote_team_state_cleanup_shell(team_id);
    match node.kind {
        TeamNodeKind::Ssh => {
            let host = node.host.as_deref().context("ssh node needs host")?;
            if dry_run {
                return Ok(format!("would delete remote state on ssh:{host}"));
            }
            run_ssh_command(host, &script)?;
            Ok(format!("deleted remote state on ssh:{host}"))
        }
        TeamNodeKind::Docker => {
            let container = node
                .container
                .as_deref()
                .context("docker node needs container")?;
            if dry_run {
                return Ok(format!("would delete remote state in docker:{container}"));
            }
            run_shell_capture(
                &format!(
                    "docker exec {} bash -lc {}",
                    shell_quote(container),
                    shell_quote(&script)
                ),
                "delete docker node team state",
            )?;
            Ok(format!("deleted remote state in docker:{container}"))
        }
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref().context("ssh-docker node needs host")?;
            let container = node
                .container
                .as_deref()
                .context("ssh-docker node needs container")?;
            if dry_run {
                return Ok(format!(
                    "would delete remote state on ssh:{host} and ssh-docker:{host}:{container}"
                ));
            }
            run_ssh_command(host, &script)?;
            run_ssh_command(
                host,
                &format!(
                    "docker exec {} bash -lc {}",
                    shell_quote(container),
                    shell_quote(&script)
                ),
            )?;
            Ok(format!(
                "deleted remote state on ssh:{host} and ssh-docker:{host}:{container}"
            ))
        }
        TeamNodeKind::Local | TeamNodeKind::Manual => Ok("skipped local/manual node".to_string()),
    }
}

fn remote_team_state_cleanup_shell(team_id: &str) -> String {
    format!(
        r#"set -euo pipefail
team_id={team_id}
if [ -z "$team_id" ] || [ "$team_id" = "." ] || [ "$team_id" = "/" ]; then
  echo "refusing unsafe team id: $team_id" >&2
  exit 64
fi
bases="${{CODEX_HOME:-}} ${{HOME:-/root}}/.codex"
for base in $bases; do
  [ -n "$base" ] || continue
  case "$base" in "/"|"/root"|"/home"|"/tmp") continue ;; esac
  target="$base/teams/$team_id"
  case "$target" in
    */teams/"$team_id") ;;
    *) echo "refusing unsafe target: $target" >&2; exit 65 ;;
  esac
  if [ -d "$target" ]; then
    rm -rf -- "$target"
    echo "deleted $target"
  fi
done
"#,
        team_id = shell_quote(team_id),
    )
}

fn remove_team_container_for_node(node: &TeamNode, dry_run: bool) -> Result<String> {
    match node.kind {
        TeamNodeKind::Docker => {
            let container = node
                .container
                .as_deref()
                .context("docker node needs container")?;
            if dry_run {
                return Ok(format!("would remove docker container {container}"));
            }
            run_shell_capture(
                &format!("docker rm -f {}", shell_quote(container)),
                "remove docker team container",
            )?;
            Ok(format!("removed docker container {container}"))
        }
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref().context("ssh-docker node needs host")?;
            let container = node
                .container
                .as_deref()
                .context("ssh-docker node needs container")?;
            if dry_run {
                return Ok(format!(
                    "would remove docker container {container} on ssh:{host}"
                ));
            }
            run_ssh_command(host, &format!("docker rm -f {}", shell_quote(container)))?;
            Ok(format!(
                "removed docker container {container} on ssh:{host}"
            ))
        }
        TeamNodeKind::Local | TeamNodeKind::Manual | TeamNodeKind::Ssh => {
            Ok("skipped non-container node".to_string())
        }
    }
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
