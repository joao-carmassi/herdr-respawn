//! Re-runs saved services in their panes. `plan` is pure; `run` does the herdr calls.
use crate::extract::Shell;
use crate::herdr::{Herdr, ProcInfo, Snapshot, R};
use crate::store::{self, Service};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub enum Step {
    Run {
        service: String,
        pane: String,
        workspace: String,
        text: String,
    },
    CreateTab {
        service: String,
        workspace: String,
        cwd: String,
        text: String,
    },
    Skip {
        service: String,
        reason: String,
    },
}

/// Case-insensitive, slash-agnostic, no trailing separator, no `\\?\` prefix.
/// shortcut: also folds case on case-sensitive file systems, upgrade if two dirs differ only by case.
pub fn norm(p: &str) -> String {
    let p = p.strip_prefix(r"\\?\").unwrap_or(p).replace('\\', "/");
    let t = p.trim_end_matches('/');
    (if t.is_empty() && !p.is_empty() {
        "/"
    } else {
        t
    })
    .to_lowercase()
}

fn busy(procs: &HashMap<String, ProcInfo>, pane: &str) -> bool {
    procs
        .get(pane)
        .is_some_and(|i| i.children().next().is_some())
}

fn shell_of(procs: &HashMap<String, ProcInfo>, pane: &str) -> Shell {
    let first = procs.get(pane).and_then(|i| i.foreground_processes.first());
    first.map_or(Shell::platform_default(), |p| Shell::from_name(&p.name))
}

pub fn plan(services: &[Service], snap: &Snapshot, procs: &HashMap<String, ProcInfo>) -> Vec<Step> {
    let mut claimed: HashSet<String> = HashSet::new();
    let mut steps = vec![];
    for s in services.iter().filter(|s| s.enabled) {
        let skip = |reason: &str| Step::Skip {
            service: s.name.clone(),
            reason: reason.into(),
        };
        let ws = snap
            .workspaces
            .iter()
            .find(|w| w.workspace_id == s.workspace_id)
            .or_else(|| {
                snap.workspaces
                    .iter()
                    .find(|w| !s.workspace_label.is_empty() && w.label == s.workspace_label)
            })
            .map(|w| w.workspace_id.clone());

        // a) the saved pane, b) a free pane of the workspace in the same directory.
        let by_id = snap.panes.iter().find(|p| p.pane_id == s.pane_id);
        let in_cwd = || {
            let cands = snap.panes.iter().filter(|p| {
                Some(&p.workspace_id) == ws.as_ref()
                    && norm(&p.cwd) == norm(&s.cwd)
                    && p.agent.is_none()
                    && !claimed.contains(&p.pane_id)
            });
            let cands: Vec<_> = cands.collect();
            cands
                .iter()
                .find(|p| !busy(procs, &p.pane_id))
                .or(cands.first())
                .map(|p| (*p).clone())
        };
        let pane = by_id.cloned().or_else(in_cwd);

        let Some(pane) = pane else {
            let Some(workspace) = ws.or_else(|| snap.focused_workspace_id.clone()) else {
                steps.push(skip("no pane and no workspace to put a tab in"));
                continue;
            };
            let text = command_text(s, Shell::platform_default(), &s.cwd);
            steps.push(Step::CreateTab {
                service: s.name.clone(),
                workspace,
                cwd: s.cwd.clone(),
                text,
            });
            continue;
        };
        claimed.insert(pane.pane_id.clone());
        if pane.agent.is_some() {
            steps.push(skip("the pane runs an agent"));
        } else if busy(procs, &pane.pane_id) {
            steps.push(skip("busy (already running)"));
        } else {
            let text = command_text(s, shell_of(procs, &pane.pane_id), &pane.cwd);
            steps.push(Step::Run {
                service: s.name.clone(),
                pane: pane.pane_id,
                workspace: pane.workspace_id,
                text,
            });
        }
    }
    steps
}

/// The command, with a cd in front only when the pane is not already in the service's directory.
fn command_text(s: &Service, shell: Shell, pane_cwd: &str) -> String {
    if norm(pane_cwd) == norm(&s.cwd) {
        s.command.clone()
    } else {
        shell.cd_and(&s.cwd, &s.command)
    }
}

pub struct Timing {
    pub snapshot: Duration,
    pub ready: Duration,
    pub verify: Duration,
    pub poll: Duration,
}

impl Timing {
    pub fn real() -> Timing {
        Timing {
            snapshot: Duration::from_secs(30),
            ready: Duration::from_secs(15),
            verify: Duration::from_secs(5),
            poll: Duration::from_millis(500),
        }
    }
}

/// One restore at a time: a lock file, stale after 2 minutes.
struct Lock(std::path::PathBuf);

impl Lock {
    fn take(dir: &std::path::Path) -> Option<Lock> {
        let p = dir.join("restore.lock");
        let _ = std::fs::create_dir_all(dir);
        let stale = std::fs::metadata(&p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|e| e > Duration::from_secs(120));
        if stale {
            let _ = std::fs::remove_file(&p);
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .ok()?;
        let _ = std::io::Write::write_all(&mut f, std::process::id().to_string().as_bytes());
        Some(Lock(p))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn wait_until(limit: Duration, poll: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + limit;
    loop {
        if done() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(poll);
    }
}

/// Restores every enabled service (or just `only`). `startup` also waits for herdr and for each shell prompt.
pub fn run(
    h: &dyn Herdr,
    services_path: &std::path::Path,
    only: Option<&str>,
    startup: bool,
    t: &Timing,
) -> R<()> {
    let Some(_lock) = Lock::take(&store::state_dir()?) else {
        println!("another restore is running; nothing to do");
        return Ok(());
    };
    let mut all = store::load_from(services_path)?;
    let wanted: Vec<Service> = all
        .services
        .iter()
        .filter(|s| only.is_none_or(|n| s.name == n))
        .cloned()
        .collect();
    if wanted.is_empty() {
        println!("no saved services");
        return Ok(());
    }
    let mut snap = None;
    wait_until(
        if startup { t.snapshot } else { Duration::ZERO },
        t.poll,
        || {
            snap = h.snapshot().ok();
            snap.is_some()
        },
    );
    let snap = snap.ok_or("herdr is not answering")?;
    let procs: HashMap<String, ProcInfo> = snap
        .panes
        .iter()
        .filter(|p| p.agent.is_none())
        // asking the OS about every pane is slow on Windows: only panes a service could use
        .filter(|p| {
            wanted
                .iter()
                .any(|s| s.pane_id == p.pane_id || norm(&s.cwd) == norm(&p.cwd))
        })
        .filter_map(|p| Some((p.pane_id.clone(), h.process_info(&p.pane_id).ok()?)))
        .collect();

    let mut changed = false;
    for step in plan(&wanted, &snap, &procs) {
        let (service, target) = match step {
            Step::Skip { service, reason } => {
                println!("{service}: skipped, {reason}");
                continue;
            }
            Step::Run {
                service,
                pane,
                workspace,
                text,
            } => (service, Ok((pane, workspace, text))),
            Step::CreateTab {
                service,
                workspace,
                cwd,
                text,
            } => {
                let made = h.create_tab(&workspace, &cwd, &service);
                (service, made.map(|pane| (pane, workspace, text)))
            }
        };
        let (pane, workspace, text) = match target {
            Ok(x) => x,
            Err(e) => {
                println!("{service}: could not create a tab: {e}");
                continue;
            }
        };
        if startup {
            wait_until(t.ready, t.poll, || {
                h.read_visible(&pane).is_ok_and(|s| !s.trim().is_empty())
            });
        }
        if let Err(e) = h.run(&pane, &text) {
            println!("{service}: run failed: {e}");
            continue;
        }
        let seen = wait_until(t.verify, t.poll, || {
            h.process_info(&pane)
                .is_ok_and(|i| i.children().next().is_some())
        });
        println!(
            "{service}: {} in {pane}: {text}",
            if seen {
                "started"
            } else {
                "sent, but no process seen yet"
            }
        );
        if let Some(s) = all.services.iter_mut().find(|s| s.name == service) {
            if s.pane_id != pane || s.workspace_id != workspace {
                s.pane_id = pane;
                s.workspace_id = workspace;
                changed = true;
            }
        }
    }
    if changed {
        store::save_to(services_path, &all)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{Pane, Proc, Workspace};
    use std::cell::RefCell;

    fn pane(id: &str, ws: &str, cwd: &str, agent: bool) -> Pane {
        Pane {
            pane_id: id.into(),
            workspace_id: ws.into(),
            cwd: cwd.into(),
            agent: agent.then(|| "claude".into()),
        }
    }
    fn svc(name: &str, pane_id: &str, ws: &str, cwd: &str) -> Service {
        Service {
            name: name.into(),
            command: "npm run start".into(),
            cwd: cwd.into(),
            pane_id: pane_id.into(),
            workspace_id: ws.into(),
            workspace_label: "Main".into(),
            enabled: true,
            ..Default::default()
        }
    }
    fn snap(panes: Vec<Pane>) -> Snapshot {
        Snapshot {
            panes,
            workspaces: vec![Workspace {
                workspace_id: "w5".into(),
                label: "Main".into(),
            }],
            focused_workspace_id: Some("w5".into()),
        }
    }
    fn idle() -> ProcInfo {
        ProcInfo {
            shell_pid: 1,
            foreground_processes: vec![Proc {
                pid: 1,
                name: "pwsh.exe".into(),
                ..Default::default()
            }],
        }
    }
    fn busy_info() -> ProcInfo {
        let mut i = idle();
        i.foreground_processes.push(Proc {
            pid: 9,
            name: "node.exe".into(),
            ..Default::default()
        });
        i
    }
    fn procs(list: &[(&str, ProcInfo)]) -> HashMap<String, ProcInfo> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }
    fn run_step(pane: &str, text: &str) -> Step {
        Step::Run {
            service: "a".into(),
            pane: pane.into(),
            workspace: "w5".into(),
            text: text.into(),
        }
    }

    #[test]
    fn norm_paths() {
        assert_eq!(norm(r"C:\Users\Joao\"), norm("c:/users/joao"));
        assert_eq!(norm(r"\\?\C:\x"), "c:/x");
        assert_eq!(norm("/"), "/");
    }

    #[test]
    fn matches_by_pane_id_without_cd() {
        let s = snap(vec![pane("w5:p1", "w5", r"D:\app", false)]);
        let got = plan(
            &[svc("a", "w5:p1", "w5", r"D:\app\")],
            &s,
            &procs(&[("w5:p1", idle())]),
        );
        assert_eq!(got, vec![run_step("w5:p1", "npm run start")]);
    }

    #[test]
    fn rehomes_by_cwd_and_adds_cd_when_needed() {
        let s = snap(vec![
            pane("w5:p7", "w5", r"D:\app", false),
            pane("w5:p8", "w5", r"D:\other", false),
        ]);
        let p = procs(&[("w5:p7", idle()), ("w5:p8", idle())]);
        assert_eq!(
            plan(&[svc("a", "gone", "w5", r"d:/app")], &s, &p),
            vec![run_step("w5:p7", "npm run start")]
        );
        let got = plan(&[svc("a", "w5:p8", "w5", r"D:\app")], &s, &p);
        assert_eq!(
            got,
            vec![run_step(
                "w5:p8",
                r"Set-Location -LiteralPath 'D:\app'; npm run start"
            )]
        );
    }

    #[test]
    fn workspace_label_fallback() {
        let s = snap(vec![pane("w5:p7", "w5", r"D:\app", false)]);
        let got = plan(
            &[svc("a", "gone", "w9", r"D:\app")],
            &s,
            &procs(&[("w5:p7", idle())]),
        );
        assert_eq!(got, vec![run_step("w5:p7", "npm run start")]);
    }

    #[test]
    fn creates_a_tab_when_nothing_matches() {
        let s = snap(vec![]);
        let got = plan(&[svc("a", "gone", "w5", r"D:\app")], &s, &HashMap::new());
        let Step::CreateTab { workspace, cwd, .. } = &got[0] else {
            panic!("{got:?}")
        };
        assert_eq!((workspace.as_str(), cwd.as_str()), ("w5", r"D:\app"));
        let mut lost = svc("a", "gone", "w9", r"D:\app");
        lost.workspace_label = "none".into();
        let Step::CreateTab { workspace, .. } = &plan(&[lost], &s, &HashMap::new())[0] else {
            panic!()
        };
        assert_eq!(workspace, "w5"); // focused workspace
    }

    #[test]
    fn busy_agent_claimed_disabled_are_skipped() {
        let s = snap(vec![
            pane("w5:p1", "w5", r"D:\app", false),
            pane("w5:p2", "w5", r"D:\app", true),
        ]);
        let skipped = |got: &[Step], i: usize| matches!(got[i], Step::Skip { .. });
        let busy_p = procs(&[("w5:p1", busy_info())]);
        assert!(skipped(
            &plan(&[svc("a", "w5:p1", "w5", r"D:\app")], &s, &busy_p),
            0
        ));
        assert!(skipped(
            &plan(&[svc("a", "w5:p2", "w5", r"D:\app")], &s, &procs(&[])),
            0
        ));
        // only a busy pane in that cwd: skip, never a duplicate tab
        assert!(skipped(
            &plan(&[svc("a", "gone", "w5", r"D:\app")], &s, &busy_p),
            0
        ));
        // two services, one free pane: the second one gets a tab
        let two = [
            svc("a", "gone", "w5", r"D:\app"),
            svc("b", "gone2", "w5", r"D:\app"),
        ];
        let got = plan(&two, &s, &procs(&[("w5:p1", idle())]));
        assert!(matches!(got[0], Step::Run { .. }) && matches!(got[1], Step::CreateTab { .. }));
        let mut off = svc("a", "w5:p1", "w5", r"D:\app");
        off.enabled = false;
        assert!(plan(&[off], &s, &HashMap::new()).is_empty());
    }

    struct Fake {
        snap: Snapshot,
        ran: RefCell<Vec<(String, String)>>,
        created: RefCell<Vec<String>>,
    }
    impl Herdr for Fake {
        fn snapshot(&self) -> R<Snapshot> {
            Ok(self.snap.clone())
        }
        fn process_info(&self, pane: &str) -> R<ProcInfo> {
            // a pane is busy once something was run in it
            let started = self.ran.borrow().iter().any(|(p, _)| p == pane);
            Ok(if started { busy_info() } else { idle() })
        }
        fn run(&self, pane: &str, text: &str) -> R<()> {
            self.ran.borrow_mut().push((pane.into(), text.into()));
            Ok(())
        }
        fn create_tab(&self, _: &str, _: &str, label: &str) -> R<String> {
            self.created.borrow_mut().push(label.into());
            Ok("w5:pNew".into())
        }
        fn read_visible(&self, _: &str) -> R<String> {
            Ok("PS> ".into())
        }
        fn notify(&self, _: &str, _: &str) {}
    }

    #[test]
    fn run_executes_rehomes_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("respawn-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("HERDR_PLUGIN_STATE_DIR", &dir);
        let path = dir.join("services.json");
        let st = store::Store {
            services: vec![svc("a", "gone", "w5", r"D:\app")],
            ..Default::default()
        };
        store::save_to(&path, &st).unwrap();
        let fake = Fake {
            snap: snap(vec![]),
            ran: RefCell::default(),
            created: RefCell::default(),
        };
        let t = Timing {
            snapshot: Duration::ZERO,
            ready: Duration::ZERO,
            verify: Duration::ZERO,
            poll: Duration::ZERO,
        };

        run(&fake, &path, None, true, &t).unwrap();
        assert_eq!(fake.created.borrow().as_slice(), ["a"]);
        assert_eq!(
            fake.ran.borrow()[0],
            ("w5:pNew".to_string(), "npm run start".to_string())
        );
        assert_eq!(
            store::load_from(&path).unwrap().services[0].pane_id,
            "w5:pNew"
        );

        // second run sees the pane busy: nothing new is run
        let fake2 = Fake {
            snap: snap(vec![pane("w5:pNew", "w5", r"D:\app", false)]),
            ran: RefCell::default(),
            created: RefCell::default(),
        };
        fake2.ran.borrow_mut().push(("w5:pNew".into(), "x".into()));
        run(&fake2, &path, None, false, &t).unwrap();
        assert_eq!(fake2.ran.borrow().len(), 1);
        std::fs::remove_dir_all(dir).ok();
    }
}
