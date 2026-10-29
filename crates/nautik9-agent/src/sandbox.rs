//! The `sandbox` tool: run shell, Python or Node code in a throw-away directory with limits.
//!
//! Off unless `agent.json` has `"sandbox": {"enabled": true, ...}` (the tool is then registered;
//! when it is off the model is never offered it). Owner only. Each run gets:
//!
//! - a fresh temporary directory that is the working directory and is deleted afterwards;
//! - a wall-clock timeout (the whole process group is killed);
//! - a cap on captured output (the process is killed once it is exceeded);
//! - `setrlimit` limits: CPU seconds, address space (not for Node, whose V8 reserves a huge
//!   virtual range; Node gets `--max-old-space-size` instead), file size, open files;
//! - no network by default;
//! - an empty environment (no tokens or secrets reach the code).
//!
//! When `bwrap` (bubblewrap) works on the host the code also runs in new user, pid, ipc, uts and
//! network namespaces with a read-only `/usr` and no view of the home directory. Without it the
//! tool falls back to the rlimits and the temp directory only, and every result SAYS so: the
//! code can then read files the server user can read and, unless the host blocks it, use the
//! network. `"isolation": "none"` forces that fallback.
//!
//! agent.json: `sandbox: {enabled, timeout_secs (10, max 300), max_output_bytes (65536),
//! memory_mb (512), cpu_secs (10), network (false), isolation ("auto" | "none")}`.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::tools::{Tool, ToolCtx};

#[derive(Clone, Debug)]
pub struct SandboxConfig {
    pub enabled: bool,
    pub timeout_secs: u64,
    pub max_output_bytes: usize,
    pub memory_mb: u64,
    pub cpu_secs: u64,
    pub network: bool,
    /// Use bubblewrap when it works (`auto`), or never (`none`).
    pub use_bwrap: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig { enabled: false, timeout_secs: 10, max_output_bytes: 64 * 1024, memory_mb: 512, cpu_secs: 10, network: false, use_bwrap: true }
    }
}

impl SandboxConfig {
    /// From agent.json `sandbox`; absent or `enabled` not true = off. Bad values are errors.
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let mut c = SandboxConfig::default();
        if v.is_null() {
            return Ok(c);
        }
        let o = v.as_object().ok_or("agent.json sandbox: must be an object")?;
        let num = |k: &str, d: u64| -> Result<u64, String> {
            match o.get(k) {
                None | Some(Value::Null) => Ok(d),
                Some(x) => x.as_u64().filter(|n| *n > 0).ok_or_else(|| format!("agent.json sandbox.{k}: must be a positive integer")),
            }
        };
        c.enabled = o.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        c.timeout_secs = num("timeout_secs", c.timeout_secs)?.min(300);
        c.max_output_bytes = num("max_output_bytes", c.max_output_bytes as u64)?.min(4 * 1024 * 1024) as usize;
        c.memory_mb = num("memory_mb", c.memory_mb)?;
        c.cpu_secs = num("cpu_secs", c.cpu_secs)?;
        c.network = o.get("network").and_then(Value::as_bool).unwrap_or(false);
        match o.get("isolation").and_then(Value::as_str) {
            None | Some("auto") => {}
            Some("none") => c.use_bwrap = false,
            Some(x) => return Err(format!("agent.json sandbox.isolation: `{x}` is not auto or none")),
        }
        Ok(c)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Shell,
    Python,
    Node,
}

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> {
        match s.to_lowercase().as_str() {
            "shell" | "sh" | "bash" => Some(Lang::Shell),
            "python" | "python3" | "py" => Some(Lang::Python),
            "node" | "nodejs" | "js" | "javascript" => Some(Lang::Node),
            _ => None,
        }
    }
    fn file(self) -> &'static str {
        match self {
            Lang::Shell => "main.sh",
            Lang::Python => "main.py",
            Lang::Node => "main.js",
        }
    }
}

#[derive(Debug, Clone)]
pub struct RunResult {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub output_truncated: bool,
    /// "bubblewrap" or "rlimits + temp dir only".
    pub isolation: &'static str,
    pub network_blocked: bool,
    pub millis: u128,
}

/// Does `bwrap` exist and can it make the namespaces here? Checked once per process.
fn bwrap_works() -> bool {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OK.get_or_init(|| {
        std::process::Command::new("bwrap")
            .args(["--unshare-all", "--die-with-parent", "--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64", "/usr/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

fn fresh_dir() -> Result<PathBuf, String> {
    let d = std::env::temp_dir().join(format!("nautik9-sandbox-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).map_err(|e| format!("cannot create the sandbox directory: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    }
    Ok(d)
}

async fn drain<R: tokio::io::AsyncRead + Unpin>(mut r: R, cap: usize, total: std::sync::Arc<std::sync::atomic::AtomicUsize>, over: tokio::sync::mpsc::UnboundedSender<()>) -> Vec<u8> {
    use std::sync::atomic::Ordering;
    let mut keep = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let before = total.fetch_add(n, Ordering::SeqCst);
                if before < cap {
                    keep.extend_from_slice(&buf[..n.min(cap - before)]);
                }
                if before + n > cap {
                    let _ = over.send(());
                    break;
                }
            }
        }
    }
    keep
}

/// Run `code` in `lang` under `cfg`. Errors are for failures to start; a failing program is a
/// normal result with its exit code.
pub async fn run(cfg: &SandboxConfig, lang: Lang, code: &str, timeout_secs: Option<u64>) -> Result<RunResult, String> {
    let dir = fresh_dir()?;
    let out = run_in(cfg, lang, code, timeout_secs, &dir).await;
    let _ = std::fs::remove_dir_all(&dir);
    out
}

async fn run_in(cfg: &SandboxConfig, lang: Lang, code: &str, timeout_secs: Option<u64>, dir: &Path) -> Result<RunResult, String> {
    let file = lang.file();
    std::fs::write(dir.join(file), code).map_err(|e| format!("cannot write the script: {e}"))?;
    let timeout = Duration::from_secs(timeout_secs.unwrap_or(cfg.timeout_secs).clamp(1, cfg.timeout_secs.max(1)));
    let bwrap = cfg.use_bwrap && bwrap_works();
    let interp: Vec<String> = match lang {
        Lang::Shell => vec!["sh".into(), file.into()],
        Lang::Python => vec!["python3".into(), "-I".into(), file.into()],
        Lang::Node => vec!["node".into(), format!("--max-old-space-size={}", cfg.memory_mb), file.into()],
    };
    let path_env = "/usr/local/bin:/usr/bin:/bin";
    let mut cmd;
    if bwrap {
        cmd = Command::new("bwrap");
        cmd.args(["--unshare-all", "--die-with-parent", "--new-session", "--ro-bind", "/usr", "/usr", "--symlink", "usr/bin", "/bin", "--symlink", "usr/sbin", "/sbin", "--symlink", "usr/lib", "/lib", "--symlink", "usr/lib64", "/lib64"]);
        if Path::new("/etc/alternatives").exists() {
            cmd.args(["--ro-bind", "/etc/alternatives", "/etc/alternatives"]);
        }
        if cfg.network {
            cmd.arg("--share-net");
            for f in ["/etc/resolv.conf", "/etc/hosts", "/etc/ssl"] {
                if Path::new(f).exists() {
                    cmd.args(["--ro-bind", f, f]);
                }
            }
        }
        cmd.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp", "--bind"]).arg(dir).args(["/work", "--chdir", "/work", "--setenv", "HOME", "/work", "--setenv", "PATH", path_env]);
        cmd.args(&interp);
    } else {
        cmd = Command::new(&interp[0]);
        cmd.args(&interp[1..]).current_dir(dir).env("HOME", dir).env("PATH", path_env);
    }
    cmd.env("LANG", "C.UTF-8").env("TMPDIR", if bwrap { "/tmp" } else { dir.to_str().unwrap_or("/tmp") });
    if !bwrap {
        cmd.env_clear().env("HOME", dir).env("PATH", path_env).env("LANG", "C.UTF-8").env("TMPDIR", dir);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
    #[cfg(unix)]
    {
        let (mem, cpu, fsize, node) = (cfg.memory_mb * 1024 * 1024, cfg.cpu_secs, (cfg.max_output_bytes as u64).max(1 << 20) * 64, lang == Lang::Node);
        // SAFETY: only async-signal-safe calls (setsid, setrlimit) run between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                libc::setsid();
                let set = |res: libc::__rlimit_resource_t, v: u64| {
                    let l = libc::rlimit { rlim_cur: v as libc::rlim_t, rlim_max: v as libc::rlim_t };
                    libc::setrlimit(res, &l);
                };
                set(libc::RLIMIT_CPU, cpu);
                if !node {
                    // bwrap itself needs some headroom on top of the program's share.
                    set(libc::RLIMIT_AS, mem + 64 * 1024 * 1024);
                }
                set(libc::RLIMIT_FSIZE, fsize);
                set(libc::RLIMIT_NOFILE, 256);
                set(libc::RLIMIT_CORE, 0);
                Ok(())
            });
        }
    }
    let started = std::time::Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("cannot start the sandbox process: {e}"))?;
    let pid = child.id();
    let total = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let so = tokio::spawn(drain(child.stdout.take().unwrap(), cfg.max_output_bytes, total.clone(), tx.clone()));
    let se = tokio::spawn(drain(child.stderr.take().unwrap(), cfg.max_output_bytes, total.clone(), tx));
    let (mut timed_out, mut truncated) = (false, false);
    let status = tokio::select! {
        s = child.wait() => s.map_err(|e| e.to_string())?,
        _ = tokio::time::sleep(timeout) => { timed_out = true; kill_group(pid); child.wait().await.map_err(|e| e.to_string())? }
        _ = rx.recv() => { truncated = true; kill_group(pid); child.wait().await.map_err(|e| e.to_string())? }
    };
    // Whatever is left in the process group (background jobs) goes too.
    kill_group(pid);
    let stdout = tokio::time::timeout(Duration::from_secs(2), so).await.ok().and_then(|r| r.ok()).unwrap_or_default();
    let stderr = tokio::time::timeout(Duration::from_secs(2), se).await.ok().and_then(|r| r.ok()).unwrap_or_default();
    truncated |= total.load(std::sync::atomic::Ordering::SeqCst) > cfg.max_output_bytes;
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal = None;
    Ok(RunResult {
        exit_code: status.code(),
        signal,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
        output_truncated: truncated,
        isolation: if bwrap { "bubblewrap" } else { "rlimits + temp dir only" },
        network_blocked: bwrap && !cfg.network,
        millis: started.elapsed().as_millis(),
    })
}

fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(p) = pid {
        // SAFETY: plain kill(2) on the process group we created with setsid.
        unsafe {
            libc::kill(-(p as i32), libc::SIGKILL);
        }
    }
}

pub struct SandboxTool {
    pub cfg: SandboxConfig,
}

#[async_trait]
impl Tool for SandboxTool {
    fn name(&self) -> &str {
        "sandbox"
    }
    fn description(&self) -> &str {
        "Run shell, Python 3 or Node.js code in a throw-away directory with a timeout, an output cap, memory/CPU limits and (by default) no network. Nothing persists between runs; to keep a result, put it in your answer or save it with `write`. The result states how strongly the run was isolated."
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{"language":{"type":"string","enum":["shell","python","node"]},"code":{"type":"string","description":"the program text"},"timeout_secs":{"type":"integer","description":"wall-clock limit (cannot exceed the configured maximum)"}},"required":["language","code"]})
    }
    async fn call(&self, a: &Value, _c: &ToolCtx) -> Result<String, String> {
        if !self.cfg.enabled {
            return Err("the sandbox is disabled (agent.json sandbox.enabled is not true)".into());
        }
        let lang_s = a["language"].as_str().ok_or("missing string argument `language`")?;
        let lang = Lang::parse(lang_s).ok_or_else(|| format!("unknown language `{lang_s}` (shell, python or node)"))?;
        let code = a["code"].as_str().ok_or("missing string argument `code`")?;
        let r = run(&self.cfg, lang, code, a["timeout_secs"].as_u64()).await?;
        let mut head = format!("[sandbox: {}; network {}; limit {}s wall, {}s cpu, {} MB; {} ms]", r.isolation, if r.network_blocked { "blocked" } else if self.cfg.network { "allowed" } else { "NOT blocked (isolation is weaker)" }, self.cfg.timeout_secs, self.cfg.cpu_secs, self.cfg.memory_mb, r.millis);
        if r.isolation != "bubblewrap" {
            head.push_str("\nNOTE: bubblewrap is not available here, so this run had only resource limits and a temp directory. The code could read files the server user can read.");
        }
        let mut body = String::new();
        if r.timed_out {
            body.push_str(&format!("\nTIMED OUT after {}s and was killed.", timeout_note(&self.cfg, a)));
        }
        if r.output_truncated {
            body.push_str(&format!("\nOUTPUT CAP of {} bytes reached; the process was killed and the output below is cut.", self.cfg.max_output_bytes));
        }
        match (r.exit_code, r.signal) {
            (Some(c), _) => body.push_str(&format!("\nexit code: {c}")),
            (None, Some(s)) => body.push_str(&format!("\nkilled by signal {s}{}", if s == 24 { " (CPU limit)" } else { "" })),
            _ => body.push_str("\nexit status unknown"),
        }
        body.push_str(&format!("\n--- stdout ---\n{}\n--- stderr ---\n{}", r.stdout, r.stderr));
        let text = format!("{head}{body}");
        if r.timed_out {
            Err(text)
        } else {
            Ok(text)
        }
    }
}

fn timeout_note(cfg: &SandboxConfig, a: &Value) -> u64 {
    a["timeout_secs"].as_u64().unwrap_or(cfg.timeout_secs).clamp(1, cfg.timeout_secs.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(timeout: u64) -> SandboxConfig {
        SandboxConfig { enabled: true, timeout_secs: timeout, max_output_bytes: 2000, ..Default::default() }
    }

    #[test]
    fn config_parsing() {
        assert!(!SandboxConfig::from_json(&Value::Null).unwrap().enabled);
        assert!(!SandboxConfig::from_json(&json!({"timeout_secs": 5})).unwrap().enabled);
        let c = SandboxConfig::from_json(&json!({"enabled": true, "timeout_secs": 9999, "isolation": "none", "network": true})).unwrap();
        assert!(c.enabled && c.network && !c.use_bwrap);
        assert_eq!(c.timeout_secs, 300);
        assert!(SandboxConfig::from_json(&json!({"enabled": true, "memory_mb": 0})).is_err());
        assert!(SandboxConfig::from_json(&json!({"isolation": "maybe"})).is_err());
    }

    #[tokio::test]
    async fn runs_shell_python_and_node() {
        for (l, code) in [(Lang::Shell, "echo hi; pwd; echo err >&2; exit 3"), (Lang::Python, "import sys; print('hi'); print('err', file=sys.stderr); sys.exit(3)"), (Lang::Node, "console.log('hi'); console.error('err'); process.exit(3)")] {
            let r = run(&cfg(10), l, code, None).await.unwrap();
            assert_eq!(r.exit_code, Some(3), "{l:?}: {r:?}");
            assert!(r.stdout.contains("hi") && r.stderr.contains("err"), "{l:?}: {r:?}");
        }
    }

    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        let t = std::time::Instant::now();
        let r = run(&cfg(1), Lang::Shell, "sleep 30 & sleep 30", None).await.unwrap();
        assert!(r.timed_out);
        assert!(t.elapsed() < Duration::from_secs(8));
    }

    #[tokio::test]
    async fn output_is_capped_and_the_runaway_killed() {
        let r = run(&cfg(10), Lang::Shell, "yes aaaaaaaaaaaaaaaa", None).await.unwrap();
        assert!(r.output_truncated);
        assert!(r.stdout.len() <= 2000, "{}", r.stdout.len());
        assert!(!r.timed_out);
    }

    #[tokio::test]
    async fn fresh_dir_each_run_and_removed() {
        let r1 = run(&cfg(10), Lang::Shell, "echo x > leftover.txt; ls", None).await.unwrap();
        assert!(r1.stdout.contains("leftover.txt"));
        let r2 = run(&cfg(10), Lang::Shell, "ls", None).await.unwrap();
        assert!(!r2.stdout.contains("leftover.txt"));
    }

    #[tokio::test]
    async fn memory_limit_stops_a_hog() {
        let c = SandboxConfig { memory_mb: 64, ..cfg(10) };
        let r = run(&c, Lang::Python, "x = bytearray(400*1024*1024)\nprint('allocated')", None).await.unwrap();
        assert!(!r.stdout.contains("allocated"), "{r:?}");
        assert_ne!(r.exit_code, Some(0));
    }

    #[tokio::test]
    async fn bwrap_blocks_network_and_home_when_present() {
        if !bwrap_works() {
            eprintln!("bwrap not usable here; skipping");
            return;
        }
        let r = run(&cfg(10), Lang::Python, "import socket\ns=socket.socket()\ns.settimeout(2)\ntry:\n    s.connect(('127.0.0.1', 9))\n    print('CONNECTED')\nexcept OSError as e:\n    print('blocked', e.errno)\nimport os\nprint(os.path.exists('/home'))", None).await.unwrap();
        assert_eq!(r.isolation, "bubblewrap");
        assert!(r.network_blocked);
        assert!(r.stdout.contains("blocked") && !r.stdout.contains("CONNECTED"), "{r:?}");
        assert!(r.stdout.contains("False"), "home is not visible: {r:?}");
    }

    #[tokio::test]
    async fn fallback_says_isolation_is_weaker() {
        let c = SandboxConfig { use_bwrap: false, ..cfg(10) };
        let t = SandboxTool { cfg: c };
        // The tool needs a ToolCtx only for the trait; build a minimal one.
        let ctx = crate::tools::ToolCtx {
            vault: std::sync::Arc::new(nautik9_wayfinder::WayfinderVault::in_memory().unwrap()),
            store: crate::store::Store::in_memory().unwrap(),
            http: reqwest::Client::new(),
            secrets: Default::default(),
            search: Default::default(),
            owner: true,
            mcp_local: Default::default(),
            memory: std::sync::Arc::new(crate::memory::MemoryIndex::in_memory(None).unwrap()),
        };
        let out = t.call(&json!({"language":"shell","code":"echo ok"}), &ctx).await.unwrap();
        assert!(out.contains("rlimits + temp dir only") && out.contains("weaker") && out.contains("ok"), "{out}");
        let err = t.call(&json!({"language":"shell","code":"sleep 20","timeout_secs":1}), &ctx).await.unwrap_err();
        assert!(err.contains("TIMED OUT"), "{err}");
    }

    #[tokio::test]
    async fn disabled_tool_refuses_and_is_owner_only() {
        let t = SandboxTool { cfg: SandboxConfig::default() };
        assert!(t.owner_only());
        let reg = crate::tools::Registry::with_extra(vec![std::sync::Arc::new(t)]).0;
        assert!(reg.allowed("sandbox", false, false).is_none(), "non-owner gets no tool");
        assert!(!reg.schema(false, false, &[]).to_string().contains("sandbox"));
    }
}
