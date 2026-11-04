//! The herdr CLI as a trait, so the restore logic can be tested with a fake.
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;
use std::process::Command;

pub type R<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize, Clone, Default, Debug)]
pub struct Pane {
    pub pane_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub cwd: String,
    pub agent: Option<String>,
}

#[derive(Deserialize, Clone, Default, Debug)]
pub struct Workspace {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Deserialize, Clone, Default, Debug)]
pub struct Snapshot {
    #[serde(default)]
    pub panes: Vec<Pane>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    pub focused_workspace_id: Option<String>,
}

#[derive(Deserialize, Clone, Default, Debug)]
pub struct Proc {
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub cwd: String,
}

#[derive(Deserialize, Clone, Default, Debug)]
pub struct ProcInfo {
    #[serde(default)]
    pub shell_pid: u32,
    #[serde(default)]
    pub foreground_processes: Vec<Proc>,
}

impl ProcInfo {
    /// Foreground processes other than the shell itself.
    pub fn children(&self) -> impl Iterator<Item = &Proc> {
        self.foreground_processes
            .iter()
            .filter(|p| p.pid != self.shell_pid)
    }
}

pub trait Herdr {
    fn snapshot(&self) -> R<Snapshot>;
    fn process_info(&self, pane: &str) -> R<ProcInfo>;
    /// Types `text` plus Enter into the pane.
    fn run(&self, pane: &str, text: &str) -> R<()>;
    /// Creates a tab without focusing it and returns its root pane id.
    fn create_tab(&self, workspace: &str, cwd: &str, label: &str) -> R<String>;
    fn read_visible(&self, pane: &str) -> R<String>;
    fn notify(&self, title: &str, body: &str);
}

pub struct Cli;

fn herdr_cmd(args: &[&str]) -> Command {
    let bin = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into());
    let mut cmd = Command::new(bin);
    cmd.args(args);
    #[cfg(windows)]
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
    cmd
}

impl Cli {
    fn raw(&self, args: &[&str]) -> R<Vec<u8>> {
        let out = herdr_cmd(args).output()?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("herdr {} failed: {}", args.join(" "), err.trim()).into());
        }
        Ok(out.stdout)
    }

    fn get<T: DeserializeOwned>(&self, args: &[&str], pointer: &str) -> R<T> {
        let v: Value = serde_json::from_slice(&self.raw(args)?)?;
        let part = v.pointer(pointer).ok_or(format!("no {pointer} in reply"))?;
        Ok(serde_json::from_value(part.clone())?)
    }
}

impl Herdr for Cli {
    fn snapshot(&self) -> R<Snapshot> {
        self.get(&["api", "snapshot"], "/result/snapshot")
    }
    fn process_info(&self, pane: &str) -> R<ProcInfo> {
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut info: ProcInfo = self.get(
            &["pane", "process-info", "--pane", pane],
            "/result/process_info",
        )?;
        #[cfg(windows)]
        if let Some(cwd) = info.foreground_processes.first().map(|p| p.cwd.clone()) {
            let kids = crate::winproc::children(info.shell_pid, &cwd);
            info.foreground_processes.extend(kids);
        }
        Ok(info)
    }
    fn run(&self, pane: &str, text: &str) -> R<()> {
        self.raw(&["pane", "run", pane, text]).map(|_| ())
    }
    fn create_tab(&self, workspace: &str, cwd: &str, label: &str) -> R<String> {
        let args = [
            "tab",
            "create",
            "--workspace",
            workspace,
            "--cwd",
            cwd,
            "--label",
            label,
        ];
        self.get(&args, "/result/root_pane/pane_id")
    }
    fn read_visible(&self, pane: &str) -> R<String> {
        let out = self.raw(&["pane", "read", pane, "--source", "visible"])?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
    fn notify(&self, title: &str, body: &str) {
        let _ = self.raw(&["notification", "show", title, "--body", body]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        serde_json::from_str(&std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap())
            .unwrap()
    }

    #[test]
    fn fixtures_parse() {
        let snap: Snapshot =
            serde_json::from_value(fixture("snapshot.json")["result"]["snapshot"].clone()).unwrap();
        assert!(snap.panes[0].agent.is_some() && snap.panes[1].agent.is_none());
        assert_eq!(snap.workspaces[0].label, "Carmassi");
        for (f, kids) in [
            ("procinfo_idle.json", 0),
            ("procinfo_busy.json", 1),
            ("procinfo_bash.json", 1),
            ("procinfo_npm_windows.json", 0),
        ] {
            let i: ProcInfo =
                serde_json::from_value(fixture(f)["result"]["process_info"].clone()).unwrap();
            assert_eq!(i.children().count(), kids, "{f}");
        }
        assert!(fixture("tab_create.json")
            .pointer("/result/root_pane/pane_id")
            .is_some());
    }
}
