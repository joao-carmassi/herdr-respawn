//! Turns a pane's foreground process (argv) back into the shell text that started it.
use crate::herdr::{ProcInfo, R};
use std::path::Path;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Shell {
    Pwsh,
    Cmd,
    Posix,
}

impl Shell {
    pub fn from_name(name: &str) -> Shell {
        match stem(name).as_str() {
            "pwsh" | "powershell" => Shell::Pwsh,
            "cmd" => Shell::Cmd,
            _ => Shell::Posix,
        }
    }

    pub fn platform_default() -> Shell {
        if cfg!(windows) {
            Shell::Pwsh
        } else {
            Shell::Posix
        }
    }

    /// Text that changes to `dir`, then runs `cmd`.
    pub fn cd_and(self, dir: &str, cmd: &str) -> String {
        match self {
            Shell::Pwsh => format!(
                "Set-Location -LiteralPath '{}'; {cmd}",
                dir.replace('\'', "''")
            ),
            Shell::Cmd => format!("cd /d \"{dir}\" && {cmd}"),
            Shell::Posix => format!("cd '{}' && {cmd}", dir.replace('\'', "'\\''")),
        }
    }

    fn quote(self, arg: &str) -> String {
        let plain = |c: char| {
            c.is_ascii_alphanumeric()
                || "-_./:=@%+,".contains(c)
                || (c == '\\' && self != Shell::Posix)
        };
        if !arg.is_empty() && arg.chars().all(plain) {
            return arg.to_string();
        }
        match self {
            Shell::Pwsh => format!("'{}'", arg.replace('\'', "''")),
            Shell::Cmd => format!("\"{arg}\""),
            Shell::Posix => format!("'{}'", arg.replace('\'', "'\\''")),
        }
    }
}

/// File name without directory and extension, lowercased: `C:\x\Node.EXE` -> `node`.
fn stem(p: &str) -> String {
    let file = p.rsplit(['/', '\\']).next().unwrap_or(p).to_lowercase();
    file.rsplit_once('.')
        .map_or(file.clone(), |(s, _)| s.to_string())
}

fn file_name(p: &str) -> String {
    p.rsplit(['/', '\\']).next().unwrap_or(p).to_lowercase()
}

/// Is `name` found on PATH? (Presence only: a different binary with the same name would also match.)
pub fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts = ["", ".exe", ".cmd", ".bat"];
    std::env::split_paths(&path).any(|d| {
        exts.iter()
            .any(|e| Path::new(&d).join(format!("{name}{e}")).is_file())
    })
}

/// Returns (command text, cwd) of the pane's running process.
pub fn extract(
    info: &ProcInfo,
    shell: Shell,
    resolves: &dyn Fn(&str) -> bool,
) -> R<(String, String)> {
    let skip = |n: &str| matches!(n.to_lowercase().as_str(), "conhost.exe" | "cmd.exe");
    let procs: Vec<_> = info.children().collect();
    let p = procs
        .iter()
        .find(|p| !skip(&p.name))
        .or(procs.first())
        .ok_or("the shell is idle: pass the command after `--`")?;
    let cwd = p.cwd.clone();
    let argv = &p.argv;
    let Some(argv0) = argv.first() else {
        return Err("process has no argv: pass the command after `--`".into());
    };
    let q = |args: &[String]| {
        args.iter()
            .map(|a| shell.quote(a))
            .collect::<Vec<_>>()
            .join(" ")
    };

    // cmd.exe /d /s /c "<x>": the text is already shell text.
    if stem(argv0) == "cmd" {
        if let Some(i) = argv.iter().position(|a| a.eq_ignore_ascii_case("/c")) {
            return Ok((argv[i + 1..].join(" "), cwd));
        }
    }
    // node <...>/npm-cli.js args: undo the npm/pnpm/yarn shims.
    if stem(argv0) == "node" {
        let tool = argv
            .get(1)
            .map(|s| file_name(s))
            .and_then(|f| match f.as_str() {
                "npm-cli.js" => Some("npm"),
                "npx-cli.js" => Some("npx"),
                "pnpm.cjs" | "pnpm.js" => Some("pnpm"),
                "yarn.js" | "yarn.cjs" => Some("yarn"),
                _ => None,
            });
        if let Some(tool) = tool {
            return Ok((format!("{tool} {}", q(&argv[2..])).trim().to_string(), cwd));
        }
    }
    // powershell -File x.ps1 args: run the script itself.
    if matches!(stem(argv0).as_str(), "pwsh" | "powershell") {
        if let Some(i) = argv.iter().position(|a| a.eq_ignore_ascii_case("-file")) {
            if i + 1 < argv.len() {
                return Ok((prefix_call(shell, q(&argv[i + 1..])), cwd));
            }
        }
    }
    let head = if resolves(&stem(argv0)) {
        stem(argv0)
    } else {
        shell.quote(argv0)
    };
    let rest = q(&argv[1..]);
    Ok((
        prefix_call(shell, format!("{head} {rest}").trim().to_string()),
        cwd,
    ))
}

/// PowerShell needs `&` to run a quoted first token.
fn prefix_call(shell: Shell, text: String) -> String {
    if shell == Shell::Pwsh && text.starts_with('\'') {
        format!("& {text}")
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::Proc;

    fn info(pid: u32, argv: &[&str], name: &str) -> ProcInfo {
        let p = Proc {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            name: name.into(),
            pid,
            cwd: "D:\\app\\".into(),
        };
        ProcInfo {
            shell_pid: 1,
            foreground_processes: vec![p],
        }
    }
    fn ex(argv: &[&str], name: &str, shell: Shell) -> String {
        extract(&info(2, argv, name), shell, &|n| {
            n == "node" || n == "dotnet"
        })
        .unwrap()
        .0
    }

    #[test]
    fn idle_shell_is_an_error() {
        let i = info(1, &["pwsh.exe"], "pwsh.exe");
        assert!(extract(&i, Shell::Pwsh, &|_| true).is_err());
    }

    #[test]
    fn unwraps() {
        let w = Shell::Pwsh;
        assert_eq!(
            ex(&["cmd.exe", "/d", "/s", "/c", "npm run dev"], "cmd.exe", w),
            "npm run dev"
        );
        assert_eq!(
            ex(
                &["node", "C:\\n\\npm-cli.js", "run", "start"],
                "node.exe",
                w
            ),
            "npm run start"
        );
        assert_eq!(
            ex(&["node", "/x/npx-cli.js", "vite"], "node", Shell::Posix),
            "npx vite"
        );
        assert_eq!(
            ex(&["node.exe", "C:\\p\\pnpm.cjs", "dev"], "node.exe", w),
            "pnpm dev"
        );
        assert_eq!(ex(&["node", "C:\\y\\yarn.js"], "node", w), "yarn");
        assert_eq!(
            ex(
                &["pwsh", "-NoProfile", "-File", "C:\\a b\\x.ps1", "go"],
                "pwsh.exe",
                w
            ),
            "& 'C:\\a b\\x.ps1' go"
        );
    }

    #[test]
    fn head_and_quoting() {
        assert_eq!(
            ex(&["C:\\bin\\node.exe", "server.js"], "node.exe", Shell::Pwsh),
            "node server.js"
        );
        assert_eq!(
            ex(&["C:\\bin\\tool.exe", "a b"], "tool.exe", Shell::Pwsh),
            "C:\\bin\\tool.exe 'a b'"
        );
        assert_eq!(
            ex(&["C:\\my bin\\tool.exe", "it's"], "tool.exe", Shell::Pwsh),
            "& 'C:\\my bin\\tool.exe' 'it''s'"
        );
        assert_eq!(
            ex(&["/opt/t", "it's"], "t", Shell::Posix),
            "/opt/t 'it'\\''s'"
        );
        assert_eq!(ex(&["dotnet", "run"], "dotnet", Shell::Cmd), "dotnet run");
    }

    #[test]
    fn picks_real_process_over_conhost() {
        let mut i = info(2, &["conhost.exe"], "conhost.exe");
        i.foreground_processes.push(Proc {
            argv: vec!["node".into(), "a.js".into()],
            name: "node.exe".into(),
            pid: 3,
            cwd: "D:\\x".into(),
        });
        assert_eq!(extract(&i, Shell::Pwsh, &|_| true).unwrap().0, "node a.js");
    }

    #[test]
    fn cd_prefixes() {
        assert_eq!(
            Shell::Pwsh.cd_and("D:\\it's", "x"),
            "Set-Location -LiteralPath 'D:\\it''s'; x"
        );
        assert_eq!(Shell::Cmd.cd_and("D:\\a", "x"), "cd /d \"D:\\a\" && x");
        assert_eq!(Shell::Posix.cd_and("/a", "x"), "cd '/a' && x");
    }
}
