mod extract;
mod herdr;
mod restore;
mod store;
mod winproc;

use herdr::{Cli, Herdr, R};
use store::{Service, Store};

const USAGE: &str = "usage: herdr-respawn <save|list|remove|restore|startup|version>
  save <name> [--pane ID] [--cwd PATH] [-- <command...>]
  save --from-context          (plugin action)
  list [--json]
  remove <name> | remove --from-context
  restore [--name NAME]
  startup";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = dispatch(&args) {
        eprintln!("herdr-respawn: {e}");
        // startup must never fail the plugin runner; everything else reports it.
        std::process::exit(if args.first().is_some_and(|a| a == "startup") {
            0
        } else {
            1
        });
    }
}

fn dispatch(args: &[String]) -> R<()> {
    let rest = &args[args.len().min(1)..];
    match args.first().map(String::as_str) {
        Some("version") => println!("herdr-respawn {}", env!("CARGO_PKG_VERSION")),
        Some("save") => save(rest)?,
        Some("list") => list(rest.iter().any(|a| a == "--json"))?,
        Some("remove") => remove(rest)?,
        Some("restore") => restore_cmd(rest, false)?,
        Some("startup") => restore_cmd(&[], true)?,
        _ => println!("{USAGE}"),
    }
    Ok(())
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let i = args.iter().position(|a| a == name)?;
    args.get(i + 1).cloned()
}

fn restore_cmd(args: &[String], startup: bool) -> R<()> {
    let only = flag(args, "--name");
    restore::run(
        &Cli,
        &store::path()?,
        only.as_deref(),
        startup,
        &restore::Timing::real(),
    )
}

/// Pane the action was invoked from: the context's pane, else its focused pane, else the env.
fn context_pane() -> Option<String> {
    let ctx: serde_json::Value =
        serde_json::from_str(&std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok()?).ok()?;
    ["pane_id", "focused_pane_id"]
        .iter()
        .find_map(|k| ctx[k].as_str().map(String::from))
}

fn pane_from_context() -> R<String> {
    context_pane()
        .or_else(|| std::env::var("HERDR_PANE_ID").ok())
        .ok_or_else(|| "no pane in the action context".into())
}

fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

fn save(args: &[String]) -> R<()> {
    let from_ctx = args.iter().any(|a| a == "--from-context");
    let (opts, cmd) = match args.iter().position(|a| a == "--") {
        Some(i) => (&args[..i], Some(args[i + 1..].join(" "))),
        None => (args, None),
    };
    let pane = if from_ctx {
        pane_from_context()?
    } else {
        flag(opts, "--pane")
            .or_else(|| std::env::var("HERDR_PANE_ID").ok())
            .ok_or("no pane: pass --pane")?
    };
    let result = save_pane(&Cli, opts, &pane, cmd, from_ctx);
    if from_ctx {
        match &result {
            Ok(s) => Cli.notify(&format!("Respawn: saved {}", s.name), &s.command),
            Err(e) => Cli.notify("Respawn: not saved", &e.to_string()),
        }
    }
    let s = result?;
    println!(
        "saved {} -> {} (in {}, {})\nedit services.json if this is wrong",
        s.name, s.command, s.cwd, s.pane_id
    );
    Ok(())
}

fn save_pane(
    h: &dyn Herdr,
    opts: &[String],
    pane: &str,
    cmd: Option<String>,
    from_ctx: bool,
) -> R<Service> {
    let snap = h.snapshot()?;
    let p = snap
        .panes
        .iter()
        .find(|p| p.pane_id == pane)
        .ok_or(format!("pane {pane} not found"))?;
    if p.agent.is_some() {
        return Err("this pane runs an agent".into());
    }
    let info = h.process_info(pane)?;
    let (command, proc_cwd) = match cmd {
        Some(c) if !c.trim().is_empty() => (c, String::new()),
        _ => extract::extract(&info, extract::Shell::platform_default(), &extract::on_path)?,
    };
    if command.len() > 4096 {
        return Err("command is longer than 4096 bytes".into());
    }
    let cwd = flag(opts, "--cwd")
        .or(Some(proc_cwd).filter(|c| !c.is_empty()))
        .unwrap_or(p.cwd.clone());
    let cwd = cwd.trim_end_matches(['\\', '/']).to_string();
    let label = snap
        .workspaces
        .iter()
        .find(|w| w.workspace_id == p.workspace_id)
        .map(|w| w.label.clone());

    let path = store::path()?;
    let mut st = store::load_from(&path)?;
    let given = opts.iter().find(|a| {
        !a.starts_with("--")
            && flag(opts, "--pane").as_ref() != Some(a)
            && flag(opts, "--cwd").as_ref() != Some(a)
    });
    let existing = st.services.iter().position(|s| s.pane_id == pane);
    let name = match (given, from_ctx, existing) {
        (Some(n), _, _) => n.clone(),
        (None, true, Some(i)) => st.services[i].name.clone(),
        (None, true, None) => unique_name(&st, &default_name(&command, &cwd)),
        _ => return Err("save needs a <name>".into()),
    };
    if !valid_name(&name) {
        return Err(format!("bad name {name:?}: use letters, digits, . _ -").into());
    }
    let svc = Service {
        name,
        command,
        cwd,
        pane_id: pane.into(),
        workspace_id: p.workspace_id.clone(),
        workspace_label: label.unwrap_or_default(),
        enabled: true,
        extra: Default::default(),
    };
    match st.services.iter_mut().find(|s| s.name == svc.name) {
        Some(old) => {
            *old = Service {
                extra: std::mem::take(&mut old.extra),
                ..svc.clone()
            }
        }
        None => st.services.push(svc.clone()),
    }
    store::save_to(&path, &st)?;
    Ok(svc)
}

/// The program the command runs (`cinemix` for `C:\bin\cinemix.cmd`); for package managers,
/// runtimes and build tools the folder name says more (`npm run start` in `D:\cinemix` -> `cinemix`).
fn default_name(command: &str, cwd: &str) -> String {
    let c = command.trim_start_matches("& ");
    let first = match c.chars().next() {
        Some(q @ ('\'' | '"')) => c[1..].split(q).next().unwrap_or(""),
        _ => c.split_whitespace().next().unwrap_or(""),
    };
    let file = first.rsplit(['\\', '/']).next().unwrap_or("");
    let prog = file.split('.').next().unwrap_or("");
    let folder = cwd.rsplit(['\\', '/']).next().unwrap_or("");
    match prog {
        "" | "npm" | "npx" | "pnpm" | "yarn" | "bun" | "deno" | "node" | "cargo" | "go"
        | "make" | "python" | "python3" | "py" | "uv" | "dotnet" => folder.into(),
        p => p.into(),
    }
}

/// `base`, or `base-2`, `base-3`... when taken. Characters outside the name alphabet become `-`.
fn unique_name(st: &Store, base: &str) -> String {
    let base: String = base
        .chars()
        .map(|c| if valid_name(&c.to_string()) { c } else { '-' })
        .collect();
    let base = if base.is_empty() {
        "service".into()
    } else {
        base
    };
    let taken = |n: &str| st.services.iter().any(|s| s.name == n);
    (1..)
        .map(|i| {
            if i == 1 {
                base.clone()
            } else {
                format!("{base}-{i}")
            }
        })
        .find(|n| !taken(n))
        .unwrap()
}

fn remove(args: &[String]) -> R<()> {
    let from_ctx = args.iter().any(|a| a == "--from-context");
    let path = store::path()?;
    let mut st = store::load_from(&path)?;
    let before = st.services.len();
    if from_ctx {
        let pane = pane_from_context()?;
        st.services.retain(|s| s.pane_id != pane);
    } else {
        let name = args.first().ok_or("remove needs a <name>")?;
        st.services.retain(|s| &s.name != name);
    }
    let removed = before - st.services.len();
    if removed > 0 {
        store::save_to(&path, &st)?;
    }
    let msg = format!("removed {removed} service(s)");
    if from_ctx {
        Cli.notify("Respawn", &msg);
    }
    println!("{msg}");
    Ok(())
}

fn list(json: bool) -> R<()> {
    let st = store::load_from(&store::path()?)?;
    let snap = Cli.snapshot().ok();
    let status = |s: &Service| match &snap {
        None => "unknown",
        Some(sn) if !sn.panes.iter().any(|p| p.pane_id == s.pane_id) => "missing",
        Some(_)
            if Cli
                .process_info(&s.pane_id)
                .is_ok_and(|i| i.children().next().is_some()) =>
        {
            "running"
        }
        Some(_) => "idle",
    };
    if json {
        let rows: Vec<_> = st
            .services
            .iter()
            .map(|s| serde_json::json!({"service": s, "status": status(s)}))
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    for s in &st.services {
        let off = if s.enabled { "" } else { " (disabled)" };
        println!(
            "{:<16} {:<8} {:<10} {}  [{}]{off}",
            s.name,
            status(s),
            s.pane_id,
            s.command,
            s.cwd
        );
    }
    Ok(())
}

#[cfg(test)]
mod name_tests {
    use super::default_name;

    #[test]
    fn names_come_from_the_program() {
        assert_eq!(
            default_name(r"C:\Users\u\.local\bin\cinemix.cmd", r"C:\Users\u"),
            "cinemix"
        );
        assert_eq!(default_name("cswap auto --tui", r"C:\Users\u"), "cswap");
        assert_eq!(default_name("npm run start", r"D:\cinemix"), "cinemix");
        assert_eq!(
            default_name(r"& 'C:\Program Files\x\srv.exe' -p 1", "/home/u"),
            "srv"
        );
    }
}
