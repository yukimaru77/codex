fn print_status(team_dir: &Path) -> Result<()> {
    print!("{}", format_status_text(team_dir)?);
    Ok(())
}

fn run_task(root: &Path, cli: TaskCli) -> Result<()> {
    let team_dir = resolve_team_dir(root, cli.selector.team.as_deref())?;
    let _lock = lock_team_state(&team_dir)?;
    match cli.subcommand {
        TaskSubcommand::Add(args) => {
            let (task, reused) = create_or_reuse_similar_open_task(&team_dir, args)?;
            if reused {
                append_event(
                    &team_dir,
                    "task_add_reused_similar_open_task",
                    serde_json::json!({ "task": task }),
                )?;
                println!("Reused task {}", task.id);
            } else {
                append_event(
                    &team_dir,
                    "task_created",
                    serde_json::json!({ "task": task }),
                )?;
                auto_promote_dependency_waits(&team_dir)?;
                touch_config(&team_dir)?;
                println!("Created task {}", task.id);
            }
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
            let selected_task = request.query.get("task").cloned();
            let selected_translation = request.query.get("translation").cloned();
            let html = render_team_ui(
                root,
                args,
                selected.as_deref(),
                selected_cwd.as_deref(),
                selected_task.as_deref(),
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
        ("POST", "/resume") => {
            let form = parse_form(&request.body);
            let team = form_value(&form, "team")?;
            let team_dir = resolve_team_dir(root, Some(&team))?;
            let config = load_config(&team_dir)?;
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg("team")
                .arg("resume")
                .arg("--team")
                .arg(&config.id)
                .arg("--dangerously-bypass-approvals-and-sandbox");
            if let Some(language) = config.language {
                command.arg("--language").arg(language.cli_value());
            }
            command.stdin(Stdio::null());
            let log_path = root.join("ui-runs.log");
            let log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .with_context(|| format!("open {}", log_path.display()))?;
            let stderr = log.try_clone()?;
            command.stdout(Stdio::from(log)).stderr(Stdio::from(stderr));
            let child = command.spawn().context("spawn team resume from UI")?;
            write_ui_team_pid(root, &config.id, child.id())?;
            redirect_team_ui(stream, Some(&config.id))?;
        }
        ("POST", "/stop") => {
            let form = parse_form(&request.body);
            let team = form_value(&form, "team")?;
            stop_team_runtime(
                root,
                StopArgs {
                    selector: TeamSelector {
                        team: Some(team.clone()),
                    },
                    all: false,
                    keep_local_app_server: false,
                    no_remote_nodes: false,
                },
            )?;
            redirect_team_ui(stream, Some(&team))?;
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
                    exiting: false,
                    remote_state: false,
                    containers: false,
                    ignore_remote_errors: false,
                    dry_run: false,
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

fn render_token_usage_panel(team_dir: &Path) -> String {
    let records =
        read_jsonl::<TeamTokenUsageRecord>(&team_token_usage_path(team_dir)).unwrap_or_default();
    if records.is_empty() {
        return r#"<section class="usage-panel"><h3>Token Usage</h3><p class="hint">No token usage records yet. New app-server turns will populate this panel.</p></section>"#
            .to_string();
    }

    let mut usage_updates = HashMap::<String, TeamTokenUsageRecord>::new();
    for record in records {
        // App-server may emit the same cumulative usage update more than once.
        // Deduplicate exact cumulative positions, then sum `last` across model calls.
        let key = format!(
            "{}|{}|{}|{}",
            record.node, record.thread, record.turn, record.total.total_tokens
        );
        usage_updates.insert(key, record);
    }

    let mut total = TeamTokenUsageBreakdown::default();
    let mut by_category = HashMap::<String, TeamTokenUsageBreakdown>::new();
    let mut by_member = HashMap::<String, TeamTokenUsageBreakdown>::new();
    let mut by_category_member = HashMap::<String, TeamTokenUsageBreakdown>::new();
    let mut by_node = HashMap::<String, TeamTokenUsageBreakdown>::new();
    let mut updates = usage_updates.into_values().collect::<Vec<_>>();
    updates.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    for record in &updates {
        total.add_assign(record.last);
        by_category
            .entry(record.category.clone())
            .or_default()
            .add_assign(record.last);
        by_member
            .entry(format!("{} ({})", record.member, record.role))
            .or_default()
            .add_assign(record.last);
        by_category_member
            .entry(format!(
                "{} / {} ({})",
                record.category, record.member, record.role
            ))
            .or_default()
            .add_assign(record.last);
        by_node
            .entry(record.node.clone())
            .or_default()
            .add_assign(record.last);
    }

    let category_rows = render_token_usage_rows(by_category, total.total_tokens, 12);
    let member_rows = render_token_usage_rows(by_member, total.total_tokens, 12);
    let category_member_rows =
        render_token_usage_rows(by_category_member, total.total_tokens, 16);
    let node_rows = render_token_usage_rows(by_node, total.total_tokens, 8);
    let hotspot_rows = render_token_usage_hotspot_rows(&updates, 12);
    let side_context_rows = render_side_channel_context_pressure_rows(team_dir);
    let recent_rows = updates
        .iter()
        .take(20)
        .map(|record| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&timestamp_for_ui(&record.timestamp)),
                html_escape(&record.category),
                html_escape(&record.member),
                html_escape(&record.node),
                html_escape(&record.turn),
                html_escape(&format_tokens(record.last.total_tokens)),
                html_escape(&format_tokens(record.last.input_tokens)),
                html_escape(&format_tokens(record.last.cached_input_tokens)),
                html_escape(&format_tokens(record.last.uncached_input_tokens())),
                html_escape(&format_tokens(record.last.output_tokens)),
            )
        })
        .collect::<Vec<_>>()
        .join("");

    format!(
        r#"<section class="usage-panel"><h3>Token Usage</h3>
<div class="usage-summary">
  <div><strong>{total_tokens}</strong><span>Total</span></div>
  <div><strong>{input_tokens}</strong><span>Input</span></div>
  <div><strong>{cached_input_tokens}</strong><span>Cached Input</span></div>
  <div><strong>{uncached_input_tokens}</strong><span>Uncached Input</span></div>
  <div><strong>{output_tokens}</strong><span>Output</span></div>
  <div><strong>{reasoning_output_tokens}</strong><span>Reasoning</span></div>
</div>
<div class="usage-grid">
  <details open><summary>By Feature</summary><table><tr><th>Feature</th><th>Share</th><th>Total</th><th>Input</th><th>Cached</th><th>Uncached</th><th>Output</th><th>Reasoning</th></tr>{category_rows}</table></details>
  <details><summary>By Member</summary><table><tr><th>Member</th><th>Share</th><th>Total</th><th>Input</th><th>Cached</th><th>Uncached</th><th>Output</th><th>Reasoning</th></tr>{member_rows}</table></details>
  <details><summary>By Feature x Member</summary><table><tr><th>Feature / Member</th><th>Share</th><th>Total</th><th>Input</th><th>Cached</th><th>Uncached</th><th>Output</th><th>Reasoning</th></tr>{category_member_rows}</table></details>
  <details><summary>By Node</summary><table><tr><th>Node</th><th>Share</th><th>Total</th><th>Input</th><th>Cached</th><th>Uncached</th><th>Output</th><th>Reasoning</th></tr>{node_rows}</table></details>
</div>
<details open><summary>Token Bottlenecks</summary><table><tr><th>Time</th><th>Feature</th><th>Member</th><th>Node</th><th>Turn</th><th>Last Total</th><th>Uncached</th><th>Context</th></tr>{hotspot_rows}</table></details>
{side_context_rows}
<details><summary>Recent Usage Updates</summary><table><tr><th>Time</th><th>Feature</th><th>Member</th><th>Node</th><th>Turn</th><th>Total</th><th>Input</th><th>Cached</th><th>Uncached</th><th>Output</th></tr>{recent_rows}</table></details>
<p class="hint">Aggregation deduplicates repeated cumulative app-server notifications, then sums each model-call `last` usage by the active Teams feature category. Bottlenecks are sorted by single model-call cost. Uncached input and context share are the quickest signals for prompt bloat that is not being served from cache.</p>
</section>"#,
        total_tokens = html_escape(&format_tokens(total.total_tokens)),
        input_tokens = html_escape(&format_tokens(total.input_tokens)),
        cached_input_tokens = html_escape(&format_tokens(total.cached_input_tokens)),
        uncached_input_tokens = html_escape(&format_tokens(total.uncached_input_tokens())),
        output_tokens = html_escape(&format_tokens(total.output_tokens)),
        reasoning_output_tokens = html_escape(&format_tokens(total.reasoning_output_tokens)),
        category_rows = category_rows,
        member_rows = member_rows,
        category_member_rows = category_member_rows,
        node_rows = node_rows,
        hotspot_rows = hotspot_rows,
        side_context_rows = side_context_rows,
        recent_rows = recent_rows,
    )
}

fn render_token_usage_rows(
    values: HashMap<String, TeamTokenUsageBreakdown>,
    grand_total: i64,
    limit: usize,
) -> String {
    let mut rows = values.into_iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| b.1.total_tokens.cmp(&a.1.total_tokens).then(a.0.cmp(&b.0)));
    rows.into_iter()
        .take(limit)
        .map(|(label, usage)| {
            let share = token_usage_share_cell(usage.total_tokens, grand_total);
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&label),
                share,
                html_escape(&format_tokens(usage.total_tokens)),
                html_escape(&format_tokens(usage.input_tokens)),
                html_escape(&format_tokens(usage.cached_input_tokens)),
                html_escape(&format_tokens(usage.uncached_input_tokens())),
                html_escape(&format_tokens(usage.output_tokens)),
                html_escape(&format_tokens(usage.reasoning_output_tokens)),
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn token_usage_share_cell(value: i64, grand_total: i64) -> String {
    if grand_total <= 0 || value <= 0 {
        return r#"<div class="usage-share"><span style="width:0%"></span><em>0.0%</em></div>"#
            .to_string();
    }
    let pct = (value as f64 / grand_total as f64 * 100.0).clamp(0.0, 100.0);
    format!(
        r#"<div class="usage-share"><span style="width:{pct:.1}%"></span><em>{pct:.1}%</em></div>"#
    )
}

fn render_token_usage_hotspot_rows(records: &[TeamTokenUsageRecord], limit: usize) -> String {
    let mut rows = records.iter().collect::<Vec<_>>();
    rows.sort_by(|a, b| {
        b.last
            .total_tokens
            .cmp(&a.last.total_tokens)
            .then(
                b.last
                    .uncached_input_tokens()
                    .cmp(&a.last.uncached_input_tokens()),
            )
            .then(b.timestamp.cmp(&a.timestamp))
    });
    rows.into_iter()
        .take(limit)
        .map(|record| {
            let context = record
                .model_context_window
                .map(|window| token_context_share_cell(record.last.total_tokens, window))
                .unwrap_or_else(|| "-".to_string());
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&timestamp_for_ui(&record.timestamp)),
                html_escape(&record.category),
                html_escape(&record.member),
                html_escape(&record.node),
                html_escape(&record.turn),
                html_escape(&format_tokens(record.last.total_tokens)),
                html_escape(&format_tokens(record.last.uncached_input_tokens())),
                context,
            )
        })
        .collect::<Vec<_>>()
        .join("")
}

fn token_context_share_cell(value: i64, context_window: i64) -> String {
    if value <= 0 || context_window <= 0 {
        return "-".to_string();
    }
    let pct = (value as f64 / context_window as f64 * 100.0).clamp(0.0, 999.0);
    let class = if pct >= 80.0 {
        "hot"
    } else if pct >= 50.0 {
        "warn"
    } else {
        "ok"
    };
    format!(r#"<span class="usage-context {class}">{pct:.1}%</span>"#)
}

fn render_side_channel_context_pressure_rows(team_dir: &Path) -> String {
    let side_dir = team_dir.join("side_channel_contexts");
    let Ok(entries) = std::fs::read_dir(&side_dir) else {
        return String::new();
    };
    let mut rows = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let member = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("unknown")
            .to_string();
        let contexts = read_jsonl::<SideChannelContextRecord>(&path).unwrap_or_default();
        if contexts.is_empty() {
            continue;
        }
        let total = contexts.len();
        let pending = contexts
            .iter()
            .filter(|context| context.status == SideChannelContextStatus::Pending)
            .count();
        let injected = contexts
            .iter()
            .filter(|context| context.status == SideChannelContextStatus::Injected)
            .count();
        let acknowledged = contexts
            .iter()
            .filter(|context| context.status == SideChannelContextStatus::Acknowledged)
            .count();
        rows.push((member, total, pending, injected, acknowledged));
    }
    if rows.is_empty() {
        return String::new();
    }
    rows.sort_by(|a, b| b.2.cmp(&a.2).then(b.1.cmp(&a.1)).then(a.0.cmp(&b.0)));
    let body = rows
        .into_iter()
        .take(12)
        .map(|(member, total, pending, injected, acknowledged)| {
            let pressure = if pending >= MAX_SIDE_CHANNEL_CONTEXTS_PER_PROMPT {
                format!(r#"<span class="pill warn">capped at {}</span>"#, MAX_SIDE_CHANNEL_CONTEXTS_PER_PROMPT)
            } else {
                r#"<span class="pill">ok</span>"#.to_string()
            };
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                html_escape(&member),
                html_escape(&total.to_string()),
                html_escape(&pending.to_string()),
                html_escape(&injected.to_string()),
                html_escape(&acknowledged.to_string()),
                pressure,
                html_escape(&format!("side_channel_contexts/{}.jsonl", sanitize_id(&member))),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        r#"<details><summary>Side-channel Context Pressure</summary><table><tr><th>Member</th><th>Total</th><th>Pending</th><th>Injected</th><th>Acknowledged</th><th>Prompt</th><th>Source</th></tr>{body}</table><p class="hint">Pending side-channel context is now capped before prompt reinjection. High pending counts are still shown here because they explain historical token spikes and may require a human audit of older commitments.</p></details>"#
    )
}

fn render_team_ui(
    root: &Path,
    args: &UiArgs,
    selected: Option<&str>,
    selected_cwd: Option<&str>,
    selected_task: Option<&str>,
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
    let selected_jobs = selected_dir
        .as_ref()
        .and_then(|dir| load_jobs(dir).ok())
        .unwrap_or_default();
    let selected_waits = selected_dir
        .as_ref()
        .and_then(|dir| load_waits(dir).ok())
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
                let status = format_node_display_status(&node.status, stale);
                format!(
                    "<tr><td>{}</td><td>{:?}</td><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                    html_escape(&node.id),
                    node.kind,
                    html_escape(&status),
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
        let run_status = selected_dir
            .as_ref()
            .map(|dir| team_run_status_for_dir(dir, &config.id))
            .unwrap_or(UiTeamRunStatus::Unknown);
        let run_controls = render_team_runtime_controls(&config.id, run_status);
        let kanban_boards = render_team_kanban_boards(
            selected_dir.as_deref(),
            &config,
            &selected_tasks,
            &selected_jobs,
            &selected_waits,
            &selected_nodes,
            selected_task,
            &selected_cwd,
        );
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
        let token_usage_panel = selected_dir
            .as_ref()
            .map(|dir| render_token_usage_panel(dir))
            .unwrap_or_default();
        let realtime_view = render_realtime_view(&config.id, &config);
        let agent_flow_console = render_agent_flow_console_view(&config.id);
        let debug_timeline = render_debug_timeline_view(&config.id);
        format!(
            r#"<section><h2>{id}</h2><p>{goal}</p>
{run_controls}
{kanban_boards}
<h3>Lead Chat</h3>{lead_chat}
{realtime_view}
{agent_flow_console}
{debug_timeline}
{token_usage_panel}
<h3>Members</h3><table><tr><th>Name</th><th>Role</th><th>Session</th><th>Tasks</th><th>Node</th><th>Location</th><th>Unread/Direct</th><th>Cooldown</th><th>Thread</th></tr>{members}</table>
<h3>Nodes</h3><table><tr><th>ID</th><th>Kind</th><th>Status</th><th>URL</th><th>Last Seen</th><th>Age</th><th>Health</th><th>Host</th><th>Container</th><th>CWD</th></tr>{nodes}</table>
<h3>Tasks</h3><table><tr><th>ID</th><th>Status</th><th>Owner</th><th>Subject</th></tr>{tasks}</table>
<h3>Team Messages</h3>{message_board}
<h3>Thread Contents</h3>{thread_board}
<h3>Events</h3><pre>{events}</pre></section>"#,
            id = html_escape(&config.id),
            goal = html_escape(&config.goal),
            run_controls = run_controls,
            kanban_boards = kanban_boards,
            lead_chat = lead_chat,
            realtime_view = realtime_view,
            agent_flow_console = agent_flow_console,
            debug_timeline = debug_timeline,
            token_usage_panel = token_usage_panel,
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
.run-waiting{{color:#4d3d00;background:#fff4b8;border-color:#d4a72c}}
.run-stop{{color:#7d4e00;background:#fff8c5;border-color:#d4a72c}}
.run-stopped{{color:#82071e;background:#ffebe9;border-color:#ff8182}}
.run-unknown{{color:#59636e;background:#f6f8fa}}
.context-menu{{display:none;position:fixed;z-index:100;background:#fff;border:1px solid #d8dee4;border-radius:6px;box-shadow:0 8px 24px rgba(27,31,36,.16);padding:6px}}
.context-menu.open{{display:block}}
.context-menu form{{margin:0;padding:0;border:0;background:transparent}}
.context-menu button{{width:170px;text-align:left;background:transparent;border:0;border-radius:4px;padding:8px 10px;color:#82071e}}
.context-menu button:hover{{background:#ffebe9}}
form{{display:grid;gap:10px;margin:12px 0;padding:12px;background:#fff;border:1px solid #d8dee4;border-radius:6px}}
label{{display:grid;gap:4px}} input,textarea{{font:inherit;padding:8px;border:1px solid #c9d1d9;border-radius:4px}} button{{width:max-content;padding:8px 12px}}
.runtime-card{{display:flex;align-items:center;gap:10px;flex-wrap:wrap;background:#fff;border:1px solid #d8dee4;border-radius:6px;padding:10px;margin:12px 0}}
.runtime-card .hint{{flex-basis:100%;margin:0;color:#59636e}}
.inline-form{{display:inline-grid;margin:0;padding:0;border:0;background:transparent}}
.dir-picker{{background:#fff;border:1px solid #d8dee4;border-radius:6px;padding:10px;margin:10px 0;max-height:260px;overflow:auto}}
.dir-picker a{{display:block;padding:5px 0;color:#0969da;text-decoration:none;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
.dir-current{{font-weight:600;word-break:break-all}}
table{{width:100%;border-collapse:collapse;background:#fff}} th,td{{padding:8px;border:1px solid #d8dee4;text-align:left;vertical-align:top}}
pre{{background:#111827;color:#d1d5db;padding:12px;border-radius:6px;overflow:auto;max-height:360px}}
.kanban-shell{{margin:16px 0 20px}}
.kanban-topbar{{display:flex;justify-content:space-between;align-items:center;gap:16px;margin:4px 0 12px}}
.kanban-topbar h3{{margin:0;font-size:24px}}
.kanban-topbar p,.board-title p,.panel-title p{{margin:4px 0 0;color:#59636e}}
.kanban-actions{{display:flex;align-items:center;gap:8px;flex-wrap:wrap}}
.kanban-actions button,.primary-action,.board-back{{border:1px solid #d8dee4;border-radius:6px;background:#fff;color:#24292f;padding:8px 12px;text-decoration:none}}
.primary-action{{background:#2563eb;color:#fff;border-color:#2563eb}}
.kbd{{display:inline-block;padding:4px 8px;background:#f6f8fa;border:1px solid #d8dee4;border-radius:6px;color:#59636e}}
.kanban-board{{background:#fff;border:1px solid #d8dee4;border-radius:8px;padding:16px;margin:12px 0;overflow:auto}}
.board-title{{display:flex;justify-content:space-between;align-items:start;gap:12px;margin-bottom:14px}}
.board-title h4{{font-size:20px;margin:0}}
.kanban-grid{{display:grid;grid-template-columns:repeat(4,minmax(240px,1fr));gap:12px;min-width:980px}}
.kanban-col{{display:grid;grid-template-rows:auto 1fr auto;gap:10px;border:1px solid #d8dee4;border-radius:8px;background:#fbfcfe;padding:12px;min-height:360px}}
.kanban-col.jobs{{min-height:280px}}
.kanban-col-head{{display:flex;justify-content:space-between;gap:10px}}
.kanban-col-head strong{{font-size:16px}}
.kanban-col-head span{{display:block;margin-top:4px;color:#59636e;font-size:12px}}
.kanban-col-head em{{font-style:normal;background:#eef2f7;border-radius:8px;padding:4px 9px;align-self:start}}
.kanban-cards{{display:grid;align-content:start;gap:10px}}
.kanban-card{{display:grid;gap:8px;background:#fff;border:1px solid #d8dee4;border-radius:8px;padding:12px;color:#24292f;text-decoration:none;min-height:118px;box-shadow:0 1px 2px rgba(27,31,36,.04)}}
.kanban-card.selected{{outline:2px solid #2563eb;outline-offset:1px}}
.kanban-card strong{{font-size:15px;line-height:1.35}}
.card-line{{display:flex;justify-content:space-between;gap:8px;align-items:center;color:#59636e}}
.assignee{{display:grid;grid-template-columns:auto auto 1fr;align-items:center;gap:7px;color:#59636e;font-size:13px}}
.assignee small{{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}}
.avatar{{display:inline-grid;place-items:center;width:24px;height:24px;border-radius:50%;background:#dbeafe;color:#0f3d85;font-weight:700;font-size:12px}}
.status-badge{{display:inline-block;border-radius:7px;padding:3px 8px;font-size:11px;font-weight:700;text-transform:uppercase;white-space:nowrap}}
.status-badge.todo{{background:#eaf2ff;color:#0969da}}
.status-badge.active{{background:#dafbe1;color:#116329}}
.status-badge.review{{background:#fff1d6;color:#9a6700}}
.status-badge.blocked{{background:#ffebe9;color:#a40e26}}
.status-badge.completed{{background:#dafbe1;color:#1a7f37}}
.status-badge.muted{{background:#f6f8fa;color:#57606a}}
.kanban-note{{margin:0;color:#59636e;font-size:12px;line-height:1.35}}
.kanban-empty{{margin:8px 0;color:#8c959f;font-size:12px}}
.kanban-add{{align-self:end;color:#0969da;text-decoration:none;font-weight:600}}
.task-hero{{display:grid;grid-template-columns:auto 1fr minmax(180px,260px);gap:18px;align-items:center;border:1px solid #d8dee4;border-radius:8px;padding:14px;margin-bottom:14px;background:#fff}}
.task-square{{display:grid;place-items:center;width:72px;height:72px;border-radius:8px;background:#dcfce7;color:#047857;font-weight:800;font-size:18px}}
.task-hero h4{{margin:7px 0 4px;font-size:18px}}
.task-hero p{{margin:0;color:#59636e}}
.hero-owner{{display:grid;gap:6px;border-left:1px solid #d8dee4;padding-left:16px}}
.hero-owner span,.hero-owner small{{color:#59636e}}
.hero-owner strong{{display:flex;align-items:center;gap:8px}}
.member-board{{display:grid;grid-template-columns:minmax(300px,.85fr) minmax(320px,1fr) minmax(360px,1.25fr);gap:14px;margin:12px 0}}
.member-panel{{background:#fff;border:1px solid #d8dee4;border-radius:8px;padding:16px;overflow:auto}}
.panel-title{{display:grid;gap:4px;margin-bottom:12px}}
.panel-title h4{{margin:0;font-size:18px}}
.member-tabs{{display:flex;gap:8px;flex-wrap:wrap;margin-top:8px}}
.member-tab{{border:1px solid #d8dee4;border-radius:6px;padding:6px 10px;background:#fff}}
.member-tab.active{{border-color:#2563eb;background:#eff6ff;color:#1d4ed8}}
.journal-panel details{{border-top:1px solid #d8dee4;padding:8px 0}}
.journal-panel summary{{cursor:pointer;font-weight:700;color:#24292f}}
.journal-summary{{display:flex;gap:8px;flex-wrap:wrap;align-items:center;margin:8px 0}}
.journal-summary span:not(.status-badge){{border:1px solid #d8dee4;border-radius:6px;padding:3px 7px;background:#f6f8fa;color:#57606a;font-size:12px}}
.journal-list{{margin:8px 0 0;padding-left:18px;display:grid;gap:5px}}
.journal-list li{{line-height:1.35}}
.journal-panel pre{{white-space:pre-wrap;max-height:220px;overflow:auto;background:#f6f8fa;border:1px solid #d8dee4;border-radius:6px;padding:10px}}
tr.selected{{background:#f6f8ff}}
@media (max-width: 1100px){{.kanban-grid{{grid-template-columns:repeat(2,minmax(240px,1fr));min-width:0}}.member-board{{grid-template-columns:1fr}}.task-hero{{grid-template-columns:1fr}}.hero-owner{{border-left:0;padding-left:0;border-top:1px solid #d8dee4;padding-top:12px}}}}
.usage-panel{{margin:14px 0}}
.usage-summary{{display:grid;grid-template-columns:repeat(auto-fit,minmax(130px,1fr));gap:8px;margin:8px 0 12px}}
.usage-summary div{{background:#fff;border:1px solid #d8dee4;border-radius:6px;padding:10px}}
.usage-summary strong{{display:block;font-size:20px;line-height:1.2}}
.usage-summary span{{display:block;color:#59636e;font-size:12px;margin-top:3px}}
.usage-grid{{display:grid;grid-template-columns:repeat(auto-fit,minmax(280px,1fr));gap:10px}}
.usage-grid details,.usage-panel details{{background:#fff;border:1px solid #d8dee4;border-radius:6px;padding:10px;margin:8px 0}}
.usage-panel summary{{cursor:pointer;font-weight:600}}
.usage-share{{min-width:120px;position:relative;height:18px;background:#eef2f7;border-radius:4px;overflow:hidden}}
.usage-share span{{position:absolute;inset:0 auto 0 0;background:#9ec5fe}}
.usage-share em{{position:relative;z-index:1;display:block;text-align:right;padding-right:5px;font-style:normal;font-size:12px;line-height:18px;color:#24292f}}
.usage-context{{display:inline-block;border:1px solid #d8dee4;border-radius:999px;padding:1px 7px;background:#f6f8fa;color:#39424e;font-size:12px}}
.usage-context.warn{{background:#fff8c5;border-color:#d4a72c;color:#7d4e00}}
.usage-context.hot{{background:#ffebe9;border-color:#ff8182;color:#82071e}}
.usage-context.ok{{background:#dafbe1;border-color:#4ac26b;color:#116329}}
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
.af-card{{background:#fff;border:1px solid #d8dee4;border-radius:10px;margin:16px 0;overflow:hidden;box-shadow:0 12px 32px rgba(27,31,36,.08)}}
.af-head{{display:flex;align-items:center;justify-content:space-between;gap:12px;padding:14px 16px;border-bottom:1px solid #d8dee4;background:#fbfcfe}}
.af-title{{display:flex;align-items:center;gap:10px;font-weight:800;font-size:16px}}
.af-mark{{display:inline-grid;place-items:center;width:28px;height:28px;border-radius:8px;background:#eaf2ff;color:#0969da}}
.af-actions{{display:flex;gap:8px;align-items:center;flex-wrap:wrap}}
.af-actions input,.af-actions select{{padding:7px 9px;border:1px solid #c9d1d9;border-radius:6px;background:#fff}}
.af-actions button{{border:1px solid #c9d1d9;border-radius:6px;background:#fff;color:#24292f;padding:7px 10px}}
.af-layout{{display:grid;grid-template-columns:220px 1fr 260px;min-height:520px}}
.af-side{{border-right:1px solid #d8dee4;background:#f6f8fa;padding:14px;display:grid;align-content:start;gap:12px}}
.af-side h4,.af-detail h4{{margin:0;font-size:14px}}
.af-filter{{display:grid;gap:7px}}
.af-filter label{{display:flex;gap:8px;align-items:center;color:#39424e}}
.af-flow-wrap{{position:relative;overflow:auto;background:linear-gradient(#fff,#fff),linear-gradient(90deg,rgba(99,102,241,.06),rgba(20,184,166,.05));background-blend-mode:normal;padding:14px}}
.af-toolbar{{display:flex;justify-content:space-between;align-items:center;gap:10px;margin-bottom:12px;color:#59636e;font-size:12px}}
.af-stage{{display:grid;gap:0;min-width:860px;position:relative}}
.af-row{{display:grid;position:relative;min-height:68px;border-bottom:1px solid #eef2f7}}
.af-row.header{{position:sticky;top:0;z-index:4;background:rgba(255,255,255,.96);border-bottom:1px solid #d8dee4;min-height:54px}}
.af-time{{padding:12px 10px;color:#59636e;font:12px ui-monospace,SFMono-Regular,Menlo,monospace;border-right:1px dashed #d8dee4}}
.af-lane-head{{display:grid;place-items:center;padding:9px 8px;border-left:1px dashed rgba(37,99,235,.22)}}
.af-lane-pill{{display:flex;align-items:center;gap:7px;max-width:160px;border:1px solid #c7d2fe;border-radius:7px;background:#eef2ff;color:#3730a3;padding:7px 10px;font-weight:700;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
.af-lane-dot{{width:8px;height:8px;border-radius:50%;background:#6366f1;flex:0 0 auto}}
.af-cell{{border-left:1px dashed rgba(37,99,235,.16)}}
.af-link{{align-self:center;height:0;border-top:2px solid #8c959f;margin:0 22px;z-index:1;position:relative}}
.af-link::after{{content:"";position:absolute;right:-1px;top:-5px;border-left:7px solid #8c959f;border-top:4px solid transparent;border-bottom:4px solid transparent}}
.af-link.reverse::after{{left:-1px;right:auto;border-left:0;border-right:7px solid #8c959f}}
.af-chip{{align-self:center;justify-self:center;max-width:170px;border:1px solid #d8dee4;border-radius:8px;background:#fff;padding:7px 9px;z-index:2;box-shadow:0 2px 8px rgba(27,31,36,.08);cursor:pointer}}
.af-chip strong{{display:block;font-size:12px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
.af-chip span{{display:block;margin-top:3px;color:#59636e;font-size:11px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
.af-chip.message{{border-color:#bfdbfe;background:#eff6ff}}
.af-chip.system{{border-color:#ddd6fe;background:#f5f3ff}}
.af-chip.event{{border-color:#fde68a;background:#fffbeb}}
.af-chip.side{{border-color:#fecaca;background:#fff1f2}}
.af-chip.live{{border-color:#bbf7d0;background:#f0fdf4}}
.af-chip.last{{border-color:#e5e7eb;background:#f9fafb}}
.af-chip.selected{{outline:3px solid #2563eb;outline-offset:2px}}
.af-self{{align-self:center;justify-self:center;width:54px;height:22px;border:2px solid #8c959f;border-left:0;border-radius:0 14px 14px 0;z-index:1}}
.af-detail{{border-left:1px solid #d8dee4;background:#fff;padding:14px;display:grid;align-content:start;gap:12px}}
.af-detail dl{{display:grid;grid-template-columns:70px 1fr;gap:8px;margin:0}}
.af-detail dt{{color:#59636e}}
.af-detail dd{{margin:0;word-break:break-word}}
.af-preview{{white-space:pre-wrap;background:#f6f8fa;border:1px solid #d8dee4;border-radius:6px;padding:10px;max-height:260px;overflow:auto;font:12px ui-monospace,SFMono-Regular,Menlo,monospace}}
.af-status{{padding:8px 14px;color:#59636e;border-top:1px solid #d8dee4;font-size:12px}}
@media (max-width: 1200px){{.af-layout{{grid-template-columns:1fr}}.af-side,.af-detail{{border:0;border-top:1px solid #d8dee4}}}}
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
const afRoot = document.querySelector('[data-agent-flow-team]');
if (afRoot) {{
  const teamId = afRoot.dataset.agentFlowTeam;
  const stage = afRoot.querySelector('.af-stage');
  const status = afRoot.querySelector('.af-status');
  const summary = afRoot.querySelector('[data-af-summary]');
  const search = afRoot.querySelector('[data-af-search]');
  const windowSelect = afRoot.querySelector('[data-af-window]');
  const autoscrollButton = afRoot.querySelector('[data-af-autoscroll]');
  const kindChecks = Array.from(afRoot.querySelectorAll('[data-af-kind]'));
  let snapshot = null;
  let selectedId = null;
  let autoscroll = true;
  function afEsc(value) {{
    return String(value ?? '').replace(/[&<>"']/g, (ch) => ({{'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}}[ch]));
  }}
  function afSplitTargets(value) {{
    return String(value || '').split(',').map((part) => part.trim()).filter(Boolean);
  }}
  function afActor(item) {{
    const actor = String(item.actor || '').trim();
    if (actor) return actor;
    if (item.kind === 'system') return 'system';
    if (item.kind === 'event') return 'system';
    return 'unknown';
  }}
  function afPrimaryTarget(item) {{
    const targets = afSplitTargets(item.target);
    if (targets.length) return targets[0];
    return afActor(item);
  }}
  function afEnabledKinds() {{
    return new Set(kindChecks.filter((box) => box.checked).map((box) => box.dataset.afKind));
  }}
  function afMatches(item) {{
    if (!afEnabledKinds().has(item.kind)) return false;
    const q = (search?.value || '').trim().toLowerCase();
    if (!q) return true;
    return [item.timestamp, item.kind, item.title, item.actor, item.target, item.body, JSON.stringify(item.meta || {{}})]
      .join('\\n').toLowerCase().includes(q);
  }}
  function afLaneOrder(items) {{
    const seen = new Set();
    const lanes = [];
    const add = (name) => {{
      const clean = String(name || '').trim();
      if (!clean || seen.has(clean)) return;
      seen.add(clean);
      lanes.push(clean);
    }};
    ['user', 'system', 'lead'].forEach(add);
    for (const item of items) {{
      add(afActor(item));
      afSplitTargets(item.target).forEach(add);
    }}
    return lanes.slice(0, 12);
  }}
  function afKindLabel(kind) {{
    return ({{ message: 'message', system: 'system', event: 'event', side: 'side', live: 'live', last: 'last' }})[kind] || kind || 'event';
  }}
  function afTime(value) {{
    const text = String(value || '');
    const match = text.match(/T?(\\d\\d:\\d\\d:\\d\\d)/);
    return match ? match[1] : text.slice(11, 19) || text;
  }}
  function afShort(value, limit = 80) {{
    const text = String(value || '').replace(/\\s+/g, ' ').trim();
    return text.length > limit ? `${{text.slice(0, limit - 1)}}…` : text;
  }}
  function afRow(item, idx, lanes) {{
    const actor = afActor(item);
    const target = afPrimaryTarget(item);
    const actorIdx = Math.max(0, lanes.indexOf(actor));
    const targetIdx = Math.max(0, lanes.indexOf(target));
    const minIdx = Math.min(actorIdx, targetIdx);
    const maxIdx = Math.max(actorIdx, targetIdx);
    const id = item._afId;
    const cells = lanes.map(() => '<div class="af-cell"></div>').join('');
    const link = actorIdx === targetIdx
      ? `<div class="af-self" style="grid-column:${{actorIdx + 2}}"></div>`
      : `<div class="af-link ${{targetIdx < actorIdx ? 'reverse' : ''}}" style="grid-column:${{minIdx + 2}} / ${{maxIdx + 3}}"></div>`;
    const chip = `<button type="button" class="af-chip ${{afEsc(item.kind)}} ${{selectedId === id ? 'selected' : ''}}" data-af-id="${{id}}" style="grid-column:${{actorIdx + 2}}">
      <strong>${{afEsc(item.title || afKindLabel(item.kind))}}</strong>
      <span>${{afEsc(afShort(item.body || item.kind, 96))}}</span>
    </button>`;
    return `<div class="af-row" style="grid-template-columns:92px repeat(${{lanes.length}}, minmax(140px, 1fr));">
      <div class="af-time">${{afEsc(afTime(item.timestamp))}}</div>${{cells}}${{link}}${{chip}}
    </div>`;
  }}
  function afSelect(item) {{
    selectedId = item?._afId || null;
    afRoot.querySelector('[data-af-detail-kind]').textContent = item ? afKindLabel(item.kind) : '-';
    afRoot.querySelector('[data-af-detail-actor]').textContent = item?.actor || afActor(item || {{}}) || '-';
    afRoot.querySelector('[data-af-detail-target]').textContent = item?.target || '-';
    afRoot.querySelector('[data-af-detail-time]').textContent = item?.timestamp || '-';
    afRoot.querySelector('[data-af-detail-title]').textContent = item?.title || '-';
    afRoot.querySelector('[data-af-detail-body]').textContent = item?.body || 'Select an event chip to inspect the content.';
    afRender();
  }}
  function afRender() {{
    if (!snapshot) return;
    const limit = Number(windowSelect?.value || 160);
    const items = (snapshot.items || [])
      .map((item, idx) => Object.assign({{ _afId: `${{item.timestamp}}-${{idx}}` }}, item))
      .filter(afMatches)
      .slice(-limit);
    const lanes = afLaneOrder(items);
    const header = `<div class="af-row header" style="grid-template-columns:92px repeat(${{lanes.length}}, minmax(140px, 1fr));">
      <div class="af-time">Time</div>
      ${{lanes.map((lane, idx) => `<div class="af-lane-head"><span class="af-lane-pill"><i class="af-lane-dot" style="background:hsl(${{(idx * 47) % 360}} 70% 52%)"></i>${{afEsc(lane)}}</span></div>`).join('')}}
    </div>`;
    stage.innerHTML = header + (items.map((item, idx) => afRow(item, idx, lanes)).join('') || '<p class="hint">No flow events match the current filters.</p>');
    stage.querySelectorAll('[data-af-id]').forEach((button) => {{
      button.addEventListener('click', () => {{
        const item = items.find((candidate) => candidate._afId === button.dataset.afId);
        afSelect(item);
      }});
    }});
    if (summary) summary.textContent = `${{items.length}} events · ${{lanes.length}} lanes · generated ${{snapshot.generated_at}}`;
    status.textContent = `updated ${{snapshot.generated_at}} · filter window=${{limit}}`;
    if (autoscroll) {{
      const wrap = afRoot.querySelector('.af-flow-wrap');
      wrap.scrollTop = wrap.scrollHeight;
    }}
  }}
  async function afPoll() {{
    try {{
      const res = await fetch(`/debug?team=${{encodeURIComponent(teamId)}}`, {{ cache: 'no-store' }});
      if (!res.ok) throw new Error(`${{res.status}} ${{res.statusText}}`);
      snapshot = await res.json();
      afRender();
    }} catch (err) {{
      status.textContent = `agent flow error: ${{err.message || err}}`;
    }}
  }}
  kindChecks.forEach((box) => box.addEventListener('change', afRender));
  search?.addEventListener('input', afRender);
  windowSelect?.addEventListener('change', afRender);
  autoscrollButton?.addEventListener('click', () => {{
    autoscroll = !autoscroll;
    autoscrollButton.textContent = `Auto-scroll: ${{autoscroll ? 'on' : 'off'}}`;
  }});
  afRoot.querySelector('[data-af-refresh]')?.addEventListener('click', afPoll);
  afPoll();
  setInterval(afPoll, 2500);
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

fn render_team_kanban_boards(
    team_dir: Option<&Path>,
    config: &TeamConfig,
    tasks: &[TeamTask],
    jobs: &[TeamJob],
    waits: &[TeamWait],
    nodes: &[TeamNode],
    selected_task: Option<&str>,
    selected_cwd: &str,
) -> String {
    let selected_task_id = selected_task
        .map(sanitize_id)
        .filter(|id| tasks.iter().any(|task| task.id == *id))
        .or_else(|| {
            tasks
                .iter()
                .find(|task| task_is_open(task))
                .or_else(|| tasks.first())
                .map(|task| task.id.clone())
        });
    let selected_task = selected_task_id
        .as_ref()
        .and_then(|id| tasks.iter().find(|task| task.id == *id));
    let selected_member = selected_task
        .and_then(|task| task.owner.as_deref())
        .or_else(|| {
            config
                .members
                .iter()
                .find(|member| member.role != "lead")
                .map(|m| m.name.as_str())
        })
        .unwrap_or("lead");
    let task_board =
        render_tasks_kanban_board(config, tasks, selected_task_id.as_deref(), selected_cwd);
    let job_board = render_jobs_kanban_board(config, selected_task, jobs, waits);
    let member_board =
        render_member_assignment_board(config, tasks, nodes, selected_member, team_dir);
    format!(
        r#"<section class="kanban-shell">
<div class="kanban-topbar">
  <div><h3>Work Boards</h3><p>Tasks, task-scoped jobs/waits, and member assignment for the selected team.</p></div>
  <div class="kanban-actions"><span class="kbd">⌘ K</span><button type="button">Filter</button><button type="button">Group</button></div>
</div>
{task_board}
{job_board}
{member_board}
</section>"#,
        task_board = task_board,
        job_board = job_board,
        member_board = member_board,
    )
}

fn render_tasks_kanban_board(
    config: &TeamConfig,
    tasks: &[TeamTask],
    selected_task_id: Option<&str>,
    selected_cwd: &str,
) -> String {
    let columns: [(&str, &str, &[TaskStatus]); 4] = [
        (
            "Todo",
            "Queued or waiting work",
            &[TaskStatus::Pending, TaskStatus::Ready, TaskStatus::Waiting],
        ),
        (
            "In Progress",
            "Active execution and review",
            &[TaskStatus::InProgress, TaskStatus::Review],
        ),
        (
            "Blocked",
            "Needs unblock or repair",
            &[TaskStatus::Blocked, TaskStatus::Failed],
        ),
        (
            "Completed",
            "Accepted or cancelled work",
            &[TaskStatus::Completed, TaskStatus::Cancelled],
        ),
    ];
    let columns = columns
        .iter()
        .map(|(title, subtitle, statuses)| {
            let cards = tasks
                .iter()
                .filter(|task| statuses.contains(&task.status))
                .map(|task| render_task_kanban_card(config, task, selected_task_id, selected_cwd))
                .collect::<Vec<_>>();
            format!(
                r##"<div class="kanban-col"><div class="kanban-col-head"><div><strong>{title}</strong><span>{subtitle}</span></div><em>{count}</em></div><div class="kanban-cards">{cards}</div><a class="kanban-add" href="#lead-chat">+ Add task via lead</a></div>"##,
                title = html_escape(title),
                subtitle = html_escape(subtitle),
                count = cards.len(),
                cards = if cards.is_empty() {
                    r#"<p class="kanban-empty">No items</p>"#.to_string()
                } else {
                    cards.join("")
                },
            )
        })
        .collect::<Vec<_>>()
        .join("");
    format!(
        r##"<section class="kanban-board"><div class="board-title"><div><h4>Tasks Board</h4><p>Project / Tasks</p></div><a class="primary-action" href="#lead-chat">+ New Task</a></div><div class="kanban-grid">{columns}</div></section>"##
    )
}

fn render_task_kanban_card(
    config: &TeamConfig,
    task: &TeamTask,
    selected_task_id: Option<&str>,
    selected_cwd: &str,
) -> String {
    let owner = task.owner.as_deref().unwrap_or("unassigned");
    let role = config
        .members
        .iter()
        .find(|member| member.name == owner)
        .map(|member| member.role.as_str())
        .unwrap_or("");
    let selected_class = if selected_task_id == Some(task.id.as_str()) {
        " selected"
    } else {
        ""
    };
    let href = format!(
        "/?team={}&cwd={}&task={}",
        url_encode(&config.id),
        url_encode(selected_cwd),
        url_encode(&task.id)
    );
    let result = task
        .result
        .as_deref()
        .filter(|result| !result.trim().is_empty())
        .map(|result| {
            format!(
                r#"<p class="kanban-note">{}</p>"#,
                html_escape(&compact_one_line(result, 180))
            )
        })
        .unwrap_or_default();
    format!(
        r#"<a class="kanban-card{selected_class}" href="{href}">
<div class="card-line"><code>T{task_id}</code><span class="status-badge {status_class}">{status}</span></div>
<strong>{subject}</strong>
<div class="assignee"><span class="avatar">{initial}</span><span>{owner}</span><small>{role}</small></div>
{result}
</a>"#,
        selected_class = selected_class,
        href = html_escape(&href),
        task_id = html_escape(&task.id),
        status_class = task_status_css(task.status),
        status = html_escape(&task.status.to_string()),
        subject = html_escape(&compact_one_line(&task.subject, 160)),
        initial = html_escape(&avatar_initial(owner)),
        owner = html_escape(owner),
        role = html_escape(role),
        result = result,
    )
}

fn render_jobs_kanban_board(
    config: &TeamConfig,
    selected_task: Option<&TeamTask>,
    jobs: &[TeamJob],
    waits: &[TeamWait],
) -> String {
    let Some(task) = selected_task else {
        return r#"<section class="kanban-board"><div class="board-title"><div><h4>Jobs Board</h4><p>Select a task to inspect jobs and waits.</p></div></div></section>"#.to_string();
    };
    let task_jobs = jobs
        .iter()
        .filter(|job| job.task_id.as_deref() == Some(task.id.as_str()))
        .collect::<Vec<_>>();
    let task_waits = waits
        .iter()
        .filter(|wait| wait.task_id.as_deref() == Some(task.id.as_str()))
        .collect::<Vec<_>>();
    let columns = [
        ("Todo", "Queued or unknown", "todo"),
        ("In Progress", "Running or polling", "progress"),
        ("Review", "Blocked or failed gates", "review"),
        ("Done", "Completed evidence", "done"),
    ]
    .iter()
    .map(|(title, subtitle, bucket)| {
        let mut cards = Vec::new();
        for job in &task_jobs {
            if job_bucket(job.status.clone()) == *bucket {
                cards.push(render_job_card(config, job));
            }
        }
        for wait in &task_waits {
            if wait_bucket(wait.status.clone()) == *bucket {
                cards.push(render_wait_card(config, wait));
            }
        }
        format!(
            r##"<div class="kanban-col jobs"><div class="kanban-col-head"><div><strong>{title}</strong><span>{subtitle}</span></div><em>{count}</em></div><div class="kanban-cards">{cards}</div><a class="kanban-add" href="#lead-chat">+ Add job/wait via lead</a></div>"##,
            title = html_escape(title),
            subtitle = html_escape(subtitle),
            count = cards.len(),
            cards = if cards.is_empty() {
                r#"<p class="kanban-empty">No items</p>"#.to_string()
            } else {
                cards.join("")
            },
        )
    })
    .collect::<Vec<_>>()
    .join("");
    let owner = task.owner.as_deref().unwrap_or("unassigned");
    let role = config
        .members
        .iter()
        .find(|member| member.name == owner)
        .map(|member| member.role.as_str())
        .unwrap_or("");
    format!(
        r#"<section class="kanban-board"><div class="board-title"><div><p>Project / Tasks / T{task_id}</p><h4>Jobs Board</h4></div><a class="board-back" href="/?team={team}">← Back to Tasks</a></div>
<div class="task-hero"><div class="task-square">T{task_id}</div><div><span class="status-badge {status_class}">{status}</span><h4>{subject}</h4><p>{description}</p></div><div class="hero-owner"><span>Owner</span><strong><span class="avatar">{initial}</span>{owner}</strong><small>{role}</small></div></div>
<div class="kanban-grid">{columns}</div></section>"#,
        task_id = html_escape(&task.id),
        team = url_encode(&config.id),
        status_class = task_status_css(task.status),
        status = html_escape(&task.status.to_string()),
        subject = html_escape(&compact_one_line(&task.subject, 180)),
        description = html_escape(&compact_one_line(&task.description, 220)),
        initial = html_escape(&avatar_initial(owner)),
        owner = html_escape(owner),
        role = html_escape(role),
        columns = columns,
    )
}

fn render_job_card(config: &TeamConfig, job: &TeamJob) -> String {
    let owner = job.owner.as_deref().unwrap_or("unassigned");
    let role = config
        .members
        .iter()
        .find(|member| member.name == owner)
        .map(|member| member.role.as_str())
        .unwrap_or("");
    format!(
        r#"<article class="kanban-card job-card"><div class="card-line"><code>{id}</code><span class="status-badge {status_class}">{status:?}</span></div><strong>{command}</strong><div class="assignee"><span class="avatar">{initial}</span><span>{owner}</span><small>{role}</small></div><p class="kanban-note">node={node} cwd={cwd} exit={exit}</p></article>"#,
        id = html_escape(&job.id),
        status_class = job_status_css(job.status.clone()),
        status = job.status,
        command = html_escape(&compact_one_line(&job.command, 150)),
        initial = html_escape(&avatar_initial(owner)),
        owner = html_escape(owner),
        role = html_escape(role),
        node = html_escape(&job.node),
        cwd = html_escape(&job.cwd),
        exit = job
            .exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "-".to_string()),
    )
}

fn render_wait_card(config: &TeamConfig, wait: &TeamWait) -> String {
    let owner = wait.owner.as_deref().unwrap_or("unassigned");
    let role = config
        .members
        .iter()
        .find(|member| member.name == owner)
        .map(|member| member.role.as_str())
        .unwrap_or("");
    let evidence = wait
        .evidence
        .as_deref()
        .filter(|evidence| !evidence.trim().is_empty())
        .unwrap_or("-");
    format!(
        r#"<article class="kanban-card wait-card"><div class="card-line"><code>{id}</code><span class="status-badge {status_class}">{status}</span></div><strong>{title}</strong><div class="assignee"><span class="avatar">{initial}</span><span>{owner}</span><small>{role}</small></div><p class="kanban-note">{progress}</p><p class="kanban-note">evidence={evidence}</p></article>"#,
        id = html_escape(&wait.id),
        status_class = wait_status_css(wait.status.clone()),
        status = html_escape(&wait.status.to_string()),
        title = html_escape(&compact_one_line(&wait.title, 150)),
        initial = html_escape(&avatar_initial(owner)),
        owner = html_escape(owner),
        role = html_escape(role),
        progress = html_escape(&compact_one_line(&wait.progress, 170)),
        evidence = html_escape(&compact_one_line(evidence, 120)),
    )
}

fn render_member_assignment_board(
    config: &TeamConfig,
    tasks: &[TeamTask],
    nodes: &[TeamNode],
    selected_member: &str,
    team_dir: Option<&Path>,
) -> String {
    let node_by_id = nodes
        .iter()
        .map(|node| (node.id.clone(), node.clone()))
        .collect::<HashMap<_, _>>();
    let rows = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .map(|member| {
            let node_id = member.node.as_deref().unwrap_or("local");
            let location = node_by_id
                .get(node_id)
                .map(format_node_location)
                .unwrap_or_else(|| node_id.to_string());
            format!(
                r#"<tr class="{selected}"><td><span class="avatar">{initial}</span><strong>{name}</strong></td><td>{role}</td><td><span class="status-badge {status_class}">{status:?}</span></td><td>{location}</td></tr>"#,
                selected = if member.name == selected_member { "selected" } else { "" },
                initial = html_escape(&avatar_initial(&member.name)),
                name = html_escape(&member.name),
                role = html_escape(&member.role),
                status_class = member_status_css(member.status.clone()),
                status = member.status,
                location = html_escape(&location),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let mut member_tasks = tasks
        .iter()
        .filter(|task| task.owner.as_deref() == Some(selected_member))
        .collect::<Vec<_>>();
    if member_tasks.is_empty() {
        member_tasks = tasks.iter().take(8).collect();
    }
    let task_rows = member_tasks
        .into_iter()
        .map(|task| {
            format!(
                r#"<tr><td>T{id}</td><td>{subject}</td><td><span class="status-badge {status_class}">{status}</span></td></tr>"#,
                id = html_escape(&task.id),
                subject = html_escape(&compact_one_line(&task.subject, 120)),
                status_class = task_status_css(task.status),
                status = html_escape(&task.status.to_string()),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let member_tabs = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .map(|member| {
            format!(
                r#"<span class="member-tab {selected}">{name}</span>"#,
                selected = if member.name == selected_member {
                    "active"
                } else {
                    ""
                },
                name = html_escape(&member.name),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let journal_panel = render_member_journal_panel(team_dir, config, selected_member);
    format!(
        r#"<section class="member-board"><div class="member-panel"><div class="panel-title"><h4>Member Assignment</h4><p>Manage and track departments working on this team.</p></div><table><tr><th>Member Name</th><th>Role</th><th>Status</th><th>Location</th></tr>{rows}</table></div><div class="member-panel"><div class="panel-title"><h4>Member Tasks</h4><p>Browse assigned work for the selected member.</p><div class="member-tabs">{member_tabs}</div></div><table><tr><th>Task ID</th><th>Task Name</th><th>Status</th></tr>{task_rows}</table></div>{journal_panel}</section>"#,
        rows = rows,
        member_tabs = member_tabs,
        task_rows = if task_rows.is_empty() {
            r#"<tr><td colspan="3">No assigned tasks</td></tr>"#.to_string()
        } else {
            task_rows
        },
        journal_panel = journal_panel,
    )
}

fn render_member_journal_panel(
    team_dir: Option<&Path>,
    config: &TeamConfig,
    selected_member: &str,
) -> String {
    let Some(team_dir) = team_dir else {
        return r#"<div class="member-panel"><div class="panel-title"><h4>Member Journal</h4><p>No team state loaded.</p></div></div>"#.to_string();
    };
    let member_tabs = config
        .members
        .iter()
        .filter(|member| member.role != "lead")
        .map(|member| {
            format!(
                r#"<span class="member-tab {selected}">Member {name}</span>"#,
                selected = if member.name == selected_member {
                    "active"
                } else {
                    ""
                },
                name = html_escape(&member.name),
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let selected = config
        .members
        .iter()
        .find(|member| member.name == selected_member)
        .or_else(|| config.members.iter().find(|member| member.role != "lead"));
    let Some(member) = selected else {
        return r#"<div class="member-panel"><div class="panel-title"><h4>Member Journal</h4><p>No member selected.</p></div></div>"#.to_string();
    };
    let entries =
        read_jsonl::<MemberJournalEntry>(&member_journal_entries_path(team_dir, &member.name))
            .unwrap_or_default();
    let latest = entries.last().cloned().or_else(|| {
        let tasks = load_tasks(team_dir).unwrap_or_default();
        let jobs = load_jobs(team_dir).unwrap_or_default();
        let waits = load_waits(team_dir).unwrap_or_default();
        let events =
            read_jsonl::<TeamEventRecord>(&team_dir.join("events.jsonl")).unwrap_or_default();
        build_member_journal_entry(team_dir, member, &tasks, &jobs, &waits, &events, &now()).ok()
    });
    let body = if let Some(entry) = latest {
        let tasks = render_member_journal_list(&entry.tasks, 5);
        let jobs = render_member_journal_list(&entry.jobs, 4);
        let waits = render_member_journal_list(&entry.waits, 4);
        let received = render_member_journal_list(&entry.messages_received, 5);
        let sent = render_member_journal_list(&entry.messages_sent, 5);
        let events = render_member_journal_list(&entry.events, 5);
        let history = entries
            .iter()
            .rev()
            .take(8)
            .map(|entry| {
                format!(
                    r#"<tr><td>{}</td><td>{}</td></tr>"#,
                    html_escape(&timestamp_for_ui(&entry.timestamp)),
                    html_escape(&compact_one_line(&entry.summary, 160))
                )
            })
            .collect::<Vec<_>>()
            .join("");
        let last_output = if entry.last_output_excerpt.trim().is_empty() {
            String::new()
        } else {
            format!(
                r#"<details><summary>Last Output Excerpt</summary><pre>{}</pre></details>"#,
                html_escape(&entry.last_output_excerpt)
            )
        };
        format!(
            r#"<div class="journal-summary"><span class="status-badge active">{status}</span><span>{node}</span><span>{updated}</span></div>
<p class="kanban-note">{summary}</p>
<details open><summary>Tasks</summary>{tasks}</details>
<details><summary>Jobs</summary>{jobs}</details>
<details><summary>Waits</summary>{waits}</details>
<details open><summary>Received</summary>{received}</details>
<details><summary>Sent</summary>{sent}</details>
<details><summary>Events</summary>{events}</details>
{last_output}
<details><summary>Snapshot History</summary><table><tr><th>Time</th><th>Summary</th></tr>{history}</table></details>"#,
            status = html_escape(&entry.status),
            node = html_escape(&entry.node),
            updated = html_escape(&timestamp_for_ui(&entry.timestamp)),
            summary = html_escape(&entry.summary),
            tasks = tasks,
            jobs = jobs,
            waits = waits,
            received = received,
            sent = sent,
            events = events,
            last_output = last_output,
            history = if history.is_empty() {
                r#"<tr><td colspan="2">No snapshots yet</td></tr>"#.to_string()
            } else {
                history
            },
        )
    } else {
        r#"<p class="kanban-empty">No journal entries yet. The runtime writes this periodically while the team is active.</p>"#.to_string()
    };
    let journal_path = member_journal_markdown_path(team_dir, &member.name);
    let digest_path = member_digest_markdown_path(team_dir, &member.name);
    let digest = fs::read_to_string(&digest_path)
        .ok()
        .filter(|text| !text.trim().is_empty())
        .map(|text| {
            format!(
                r#"<details open><summary>AI Digest</summary><p class="hint">Digest: <code>{path}</code></p><pre>{digest}</pre></details>"#,
                path = html_escape(&digest_path.display().to_string()),
                digest = html_escape(&tail_chars(text.trim(), 6000)),
            )
        })
        .unwrap_or_else(|| {
            r#"<details><summary>AI Digest</summary><p class="hint">No AI digest yet. It is generated after task/job/wait/member milestone changes.</p></details>"#.to_string()
        });
    format!(
        r#"<div class="member-panel journal-panel"><div class="panel-title"><h4>Member Journal</h4><p>Periodic department activity diary for the selected member.</p><div class="member-tabs">{member_tabs}</div></div><p class="hint">Machine Markdown: <code>{path}</code></p>{digest}{body}</div>"#,
        member_tabs = member_tabs,
        path = html_escape(&journal_path.display().to_string()),
        digest = digest,
        body = body,
    )
}

fn render_member_journal_list(values: &[String], limit: usize) -> String {
    if values.is_empty() {
        return r#"<ul class="journal-list"><li>none</li></ul>"#.to_string();
    }
    let items = values
        .iter()
        .take(limit)
        .map(|value| format!(r#"<li>{}</li>"#, html_escape(value)))
        .collect::<Vec<_>>()
        .join("");
    format!(r#"<ul class="journal-list">{items}</ul>"#)
}

fn job_bucket(status: TeamJobStatus) -> &'static str {
    match status {
        TeamJobStatus::Running => "progress",
        TeamJobStatus::Completed => "done",
        TeamJobStatus::Failed => "review",
        TeamJobStatus::Stopped | TeamJobStatus::Unknown => "todo",
    }
}

fn wait_bucket(status: TeamWaitStatus) -> &'static str {
    match status {
        TeamWaitStatus::Waiting => "todo",
        TeamWaitStatus::Running | TeamWaitStatus::Polling => "progress",
        TeamWaitStatus::Blocked | TeamWaitStatus::Failed => "review",
        TeamWaitStatus::Completed | TeamWaitStatus::Cancelled => "done",
    }
}

fn task_status_css(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending | TaskStatus::Ready | TaskStatus::Waiting => "todo",
        TaskStatus::InProgress => "active",
        TaskStatus::Review => "review",
        TaskStatus::Blocked | TaskStatus::Failed => "blocked",
        TaskStatus::Completed => "completed",
        TaskStatus::Cancelled => "muted",
    }
}

fn job_status_css(status: TeamJobStatus) -> &'static str {
    match status {
        TeamJobStatus::Running => "active",
        TeamJobStatus::Completed => "completed",
        TeamJobStatus::Failed => "blocked",
        TeamJobStatus::Stopped | TeamJobStatus::Unknown => "todo",
    }
}

fn wait_status_css(status: TeamWaitStatus) -> &'static str {
    match status {
        TeamWaitStatus::Waiting => "todo",
        TeamWaitStatus::Running | TeamWaitStatus::Polling => "active",
        TeamWaitStatus::Blocked | TeamWaitStatus::Failed => "blocked",
        TeamWaitStatus::Completed => "completed",
        TeamWaitStatus::Cancelled => "muted",
    }
}

fn member_status_css(status: MemberStatus) -> &'static str {
    match status {
        MemberStatus::Online | MemberStatus::Running => "active",
        MemberStatus::Standby => "todo",
        MemberStatus::Completed => "completed",
        MemberStatus::Failed | MemberStatus::Offline => "blocked",
    }
}

fn avatar_initial(name: &str) -> String {
    name.chars()
        .find(|ch| ch.is_alphanumeric())
        .map(|ch| ch.to_uppercase().collect::<String>())
        .unwrap_or_else(|| "?".to_string())
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

fn render_agent_flow_console_view(team_id: &str) -> String {
    format!(
        r#"<section class="af-card" data-agent-flow-team="{team}">
<div class="af-head">
  <div class="af-title"><span class="af-mark">⌁</span><span>Agent Flow Console</span></div>
  <div class="af-actions">
    <input type="search" data-af-search placeholder="Search events, members, messages">
    <select data-af-window>
      <option value="80">Last 80</option>
      <option value="160" selected>Last 160</option>
      <option value="320">Last 320</option>
      <option value="600">Last 600</option>
    </select>
    <button type="button" data-af-autoscroll>Auto-scroll: on</button>
    <button type="button" data-af-refresh>Refresh</button>
  </div>
</div>
<div class="af-layout">
  <aside class="af-side">
    <h4>Event Type</h4>
    <div class="af-filter">
      <label><input type="checkbox" data-af-kind="message" checked> Messages</label>
      <label><input type="checkbox" data-af-kind="system" checked> System wakeups</label>
      <label><input type="checkbox" data-af-kind="event" checked> Runtime events</label>
      <label><input type="checkbox" data-af-kind="side" checked> Side-channel</label>
      <label><input type="checkbox" data-af-kind="live" checked> Live buffers</label>
      <label><input type="checkbox" data-af-kind="last" checked> Last replies</label>
    </div>
    <h4>Reading Guide</h4>
    <p class="hint">Each vertical lane is a member or system actor. Chips are turns, messages, wakeups, side-channel replies, and live/last buffer updates. Arrows show actor to recipient when the event has both.</p>
  </aside>
  <div class="af-flow-wrap">
    <div class="af-toolbar"><span data-af-summary>loading</span><span>Realtime sequence view</span></div>
    <div class="af-stage"><p class="hint">Loading agent flow...</p></div>
  </div>
  <aside class="af-detail">
    <h4>Selected Event</h4>
    <dl>
      <dt>Type</dt><dd data-af-detail-kind>-</dd>
      <dt>From</dt><dd data-af-detail-actor>-</dd>
      <dt>To</dt><dd data-af-detail-target>-</dd>
      <dt>Time</dt><dd data-af-detail-time>-</dd>
      <dt>Title</dt><dd data-af-detail-title>-</dd>
    </dl>
    <div class="af-preview" data-af-detail-body>Select an event chip to inspect the content.</div>
  </aside>
</div>
<div class="af-status">loading</div>
</section>"#,
        team = html_escape(team_id),
    )
}

fn render_team_runtime_controls(team_id: &str, status: UiTeamRunStatus) -> String {
    format!(
        r#"<section class="runtime-card">
<div><strong>Runtime</strong> <span class="run-state {class}">{label}</span></div>
<form class="inline-form" method="post" action="/resume">
  <input type="hidden" name="team" value="{team}">
  <button type="submit">Resume Runtime</button>
</form>
<form class="inline-form" method="post" action="/stop" onsubmit="return confirm('Pause this team runtime? Team state is preserved.');">
  <input type="hidden" name="team" value="{team}">
  <button type="submit">Pause Runtime</button>
</form>
<p class="hint">Resume restarts the keep-alive team runtime from the preserved state. Pause stops local and node runtimes without deleting artifacts.</p>
</section>"#,
        team = html_escape(team_id),
        class = status.css_class(),
        label = status.label(),
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
