fn resolve_or_spawn_node_app_server(
    team_dir: &Path,
    node: &TeamNode,
    relay_port: u16,
) -> Result<(String, Option<NodeAppServerProcess>)> {
    ensure_node_cwd_exists(team_dir, node)?;
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

fn ensure_node_cwd_exists(team_dir: &Path, node: &TeamNode) -> Result<()> {
    let Some(cwd) = node.cwd.as_deref().filter(|cwd| !cwd.trim().is_empty()) else {
        return Ok(());
    };
    match node.kind {
        TeamNodeKind::Local | TeamNodeKind::Manual => Ok(()),
        TeamNodeKind::Ssh => {
            let host = node.host.as_deref().with_context(|| {
                format!("ssh node `{}` needs host before cwd bootstrap", node.id)
            })?;
            let command = format!("mkdir -p {}", shell_quote(cwd));
            let output = Command::new("ssh")
                .arg("-o")
                .arg("BatchMode=yes")
                .arg(host)
                .arg(format!("bash -lc {}", shell_quote(&command)))
                .output()
                .with_context(|| format!("create cwd `{cwd}` on ssh node `{}`", node.id))?;
            if output.status.success() {
                append_event(
                    team_dir,
                    "node_cwd_ensured",
                    serde_json::json!({ "node": node.id, "kind": node.kind, "cwd": cwd }),
                )?;
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                append_event(
                    team_dir,
                    "node_cwd_ensure_failed",
                    serde_json::json!({
                        "node": node.id,
                        "kind": node.kind,
                        "cwd": cwd,
                        "status": output.status.code(),
                        "stderr": stderr,
                    }),
                )?;
                bail!(
                    "failed to create cwd `{cwd}` on ssh node `{}`: {stderr}",
                    node.id
                )
            }
        }
        TeamNodeKind::Docker => {
            let container = node.container.as_deref().with_context(|| {
                format!(
                    "docker node `{}` needs container before cwd bootstrap",
                    node.id
                )
            })?;
            ensure_docker_node_cwd_exists(team_dir, node, None, container, cwd)
        }
        TeamNodeKind::SshDocker => {
            let host = node.host.as_deref().with_context(|| {
                format!(
                    "ssh-docker node `{}` needs host before cwd bootstrap",
                    node.id
                )
            })?;
            let container = node.container.as_deref().with_context(|| {
                format!(
                    "ssh-docker node `{}` needs container before cwd bootstrap",
                    node.id
                )
            })?;
            ensure_docker_node_cwd_exists(team_dir, node, Some(host), container, cwd)
        }
    }
}

fn ensure_docker_node_cwd_exists(
    team_dir: &Path,
    node: &TeamNode,
    host: Option<&str>,
    container: &str,
    cwd: &str,
) -> Result<()> {
    let inner = format!(
        "docker exec {} mkdir -p {}",
        shell_quote(container),
        shell_quote(cwd)
    );
    let output = match host {
        Some(host) => Command::new("ssh")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg(host)
            .arg(format!("bash -lc {}", shell_quote(&inner)))
            .output()
            .with_context(|| {
                format!(
                    "create cwd `{cwd}` in container `{container}` on ssh node `{}`",
                    node.id
                )
            })?,
        None => Command::new("bash")
            .arg("-lc")
            .arg(&inner)
            .output()
            .with_context(|| {
                format!(
                    "create cwd `{cwd}` in local container `{container}` for node `{}`",
                    node.id
                )
            })?,
    };
    if output.status.success() {
        append_event(
            team_dir,
            "node_cwd_ensured",
            serde_json::json!({
                "node": node.id,
                "kind": node.kind,
                "host": host,
                "container": container,
                "cwd": cwd,
            }),
        )?;
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        append_event(
            team_dir,
            "node_cwd_ensure_failed",
            serde_json::json!({
                "node": node.id,
                "kind": node.kind,
                "host": host,
                "container": container,
                "cwd": cwd,
                "status": output.status.code(),
                "stderr": stderr,
            }),
        )?;
        bail!(
            "failed to create cwd `{cwd}` in container `{container}` for node `{}`: {stderr}",
            node.id
        )
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
    cleanup_node_app_servers_before_spawn(team_dir, node, &config.id);
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
    cleanup_node_app_servers_before_spawn(team_dir, node, &config.id);
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
    cleanup_node_app_servers_before_spawn(team_dir, node, &config.id);
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
if [ "\${{1:-}}" = "wait" ]; then
  shift
  sub="\${{1:-}}"
  shift || true
  case "\$sub" in
    list)
      owner=""
      task=""
      status=""
      limit=""
      while [ "\$#" -gt 0 ]; do
        case "\$1" in
          --owner) owner="\${{2:-}}"; shift 2 ;;
          --task) task="\${{2:-}}"; shift 2 ;;
          --status) status="\${{2:-}}"; shift 2 ;;
          --limit) limit="\${{2:-}}"; shift 2 ;;
          *) echo "codex-team wait list: unknown argument \$1" >&2; exit 2 ;;
        esac
      done
      args=(-G "\$CODEX_TEAM_RELAY_URL/wait/list" --data-urlencode "team=\$CODEX_TEAM_ID")
      [ -n "\$owner" ] && args+=(--data-urlencode "owner=\$owner")
      [ -n "\$task" ] && args+=(--data-urlencode "task=\$task")
      [ -n "\$status" ] && args+=(--data-urlencode "status=\$status")
      [ -n "\$limit" ] && args+=(--data-urlencode "limit=\$limit")
      exec curl -fsS "\${{args[@]}}"
      ;;
    add)
      title="\${{1:-}}"
      shift || true
      if [ -z "\$title" ]; then
        echo "codex-team wait add requires a title" >&2
        exit 2
      fi
      owner=""
      task=""
      node=""
      condition=""
      status="waiting"
      progress=""
      evidence=""
      while [ "\$#" -gt 0 ]; do
        case "\$1" in
          --owner) owner="\${{2:-}}"; shift 2 ;;
          --task) task="\${{2:-}}"; shift 2 ;;
          --node) node="\${{2:-}}"; shift 2 ;;
          --condition) condition="\${{2:-}}"; shift 2 ;;
          --status) status="\${{2:-}}"; shift 2 ;;
          --progress) progress="\${{2:-}}"; shift 2 ;;
          --evidence) evidence="\${{2:-}}"; shift 2 ;;
          *) echo "codex-team wait add: unknown argument \$1" >&2; exit 2 ;;
        esac
      done
      exec curl -fsS -X POST "\$CODEX_TEAM_RELAY_URL/wait/add" \
        --data-urlencode "team=\$CODEX_TEAM_ID" \
        --data-urlencode "title=\$title" \
        --data-urlencode "owner=\$owner" \
        --data-urlencode "task=\$task" \
        --data-urlencode "node=\$node" \
        --data-urlencode "condition=\$condition" \
        --data-urlencode "status=\$status" \
        --data-urlencode "progress=\$progress" \
        --data-urlencode "evidence=\$evidence"
      ;;
    set)
      id="\${{1:-}}"
      shift || true
      if [ -z "\$id" ]; then
        echo "codex-team wait set requires an id" >&2
        exit 2
      fi
      status=""
      progress=""
      evidence=""
      clear_evidence="false"
      while [ "\$#" -gt 0 ]; do
        case "\$1" in
          --status) status="\${{2:-}}"; shift 2 ;;
          --progress) progress="\${{2:-}}"; shift 2 ;;
          --evidence) evidence="\${{2:-}}"; shift 2 ;;
          --clear-evidence) clear_evidence="true"; shift ;;
          *) echo "codex-team wait set: unknown argument \$1" >&2; exit 2 ;;
        esac
      done
      exec curl -fsS -X POST "\$CODEX_TEAM_RELAY_URL/wait/set" \
        --data-urlencode "team=\$CODEX_TEAM_ID" \
        --data-urlencode "id=\$id" \
        --data-urlencode "status=\$status" \
        --data-urlencode "progress=\$progress" \
        --data-urlencode "evidence=\$evidence" \
        --data-urlencode "clear_evidence=\$clear_evidence"
      ;;
    *)
      echo "Usage: codex-team wait {{list|add|set}}" >&2
      exit 2
      ;;
  esac
fi
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
