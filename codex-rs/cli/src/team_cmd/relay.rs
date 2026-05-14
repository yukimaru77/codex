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
        ("GET", "/wait/list") => {
            let list_args = WaitListArgs {
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
                    .map(|value| parse_wait_status(value))
                    .transpose()?,
                limit: request
                    .query
                    .get("limit")
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| value.parse::<usize>())
                    .transpose()
                    .context("invalid wait list limit")?,
            };
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                &format_waits_text_filtered(team_dir, &list_args)?,
            )?;
        }
        ("POST", "/wait/add") => {
            let form = parse_form(&request.body);
            add_team_wait(
                team_dir,
                WaitAddArgs {
                    title: form_value(&form, "title")?,
                    owner: form
                        .get("owner")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    task: form
                        .get("task")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    node: form
                        .get("node")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    condition: form.get("condition").cloned().unwrap_or_default(),
                    status: form
                        .get("status")
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| parse_wait_status(value))
                        .transpose()?
                        .unwrap_or(TeamWaitStatus::Waiting),
                    progress: form.get("progress").cloned().unwrap_or_default(),
                    evidence: form
                        .get("evidence")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                },
            )?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "Wait registered\n",
            )?;
        }
        ("POST", "/wait/set") => {
            let form = parse_form(&request.body);
            set_team_wait(
                team_dir,
                WaitSetArgs {
                    id: form_value(&form, "id")?,
                    status: form
                        .get("status")
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| parse_wait_status(value))
                        .transpose()?,
                    progress: form
                        .get("progress")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    evidence: form
                        .get("evidence")
                        .filter(|value| !value.trim().is_empty())
                        .cloned(),
                    clear_evidence: form
                        .get("clear_evidence")
                        .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes")),
                },
            )?;
            write_http_response(
                stream,
                "200 OK",
                "text/plain; charset=utf-8",
                "Wait updated\n",
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

