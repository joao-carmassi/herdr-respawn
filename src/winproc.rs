//! Windows only: herdr lists just the shell as foreground process, even while it runs `npm run start`.
//! The shell's children come from WMI instead.
use crate::herdr::Proc;
use serde::Deserialize;

#[cfg(windows)]
pub fn children(shell_pid: u32, cwd: &str) -> Vec<Proc> {
    let script = format!(
        "ConvertTo-Json -Compress -InputObject @(Get-CimInstance Win32_Process -Filter 'ParentProcessId={shell_pid}' | Select-Object ProcessId,Name,CommandLine)"
    );
    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
    let out = cmd.output().map(|o| o.stdout).unwrap_or_default();
    parse_children(&String::from_utf8_lossy(&out), cwd)
}

/// WMI rows -> processes. WMI has no cwd, so they get the shell's.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn parse_children(json: &str, cwd: &str) -> Vec<Proc> {
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Row {
        process_id: u32,
        name: String,
        command_line: Option<String>,
    }
    let rows: Vec<Row> = serde_json::from_str(json).unwrap_or_default();
    rows.into_iter()
        .filter(|r| !r.name.eq_ignore_ascii_case("conhost.exe"))
        .map(|r| Proc {
            argv: split_cmdline(&r.command_line.unwrap_or_default()),
            name: r.name,
            pid: r.process_id,
            cwd: cwd.into(),
        })
        .collect()
}

/// Windows command line -> argv (CommandLineToArgvW rules: backslashes only matter before a quote).
#[cfg_attr(not(windows), allow(dead_code))]
pub fn split_cmdline(line: &str) -> Vec<String> {
    let (mut args, mut cur, mut quoted, mut started) = (vec![], String::new(), false, false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let mut n = 1;
                while chars.next_if_eq(&'\\').is_some() {
                    n += 1;
                }
                started = true;
                if chars.peek() == Some(&'"') {
                    cur.extend(std::iter::repeat_n('\\', n / 2));
                    if n % 2 == 1 {
                        cur.push('"');
                        chars.next();
                    }
                } else {
                    cur.extend(std::iter::repeat_n('\\', n));
                }
            }
            '"' => (quoted, started) = (!quoted, true),
            c if c.is_whitespace() && !quoted => {
                if started {
                    args.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            c => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started {
        args.push(cur);
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::{extract, Shell};
    use crate::herdr::ProcInfo;
    use serde_json::Value;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap()
    }

    #[test]
    fn npm_shell_child_becomes_npm_run_start() {
        // herdr lists only the shell; WMI lists node running npm-cli.js
        let v: Value = serde_json::from_str(&fixture("procinfo_npm_windows.json")).unwrap();
        let mut info: ProcInfo =
            serde_json::from_value(v["result"]["process_info"].clone()).unwrap();
        assert_eq!(info.children().count(), 0);
        let cwd = info.foreground_processes[0].cwd.clone();
        info.foreground_processes
            .extend(parse_children(&fixture("winchildren_npm.json"), &cwd));
        let (cmd, dir) = extract(&info, Shell::Pwsh, &|n| n == "node").unwrap();
        assert_eq!(cmd, "npm run start");
        assert!(dir.ends_with("respawn-npm\\"));
    }

    #[test]
    fn splits_windows_command_lines() {
        let got = split_cmdline(r#""C:\a b\x.exe" -e "say \"hi\"" c:\p\"#);
        assert_eq!(got, ["C:\\a b\\x.exe", "-e", "say \"hi\"", "c:\\p\\"]);
        assert_eq!(split_cmdline(r#"a "" b"#), ["a", "", "b"]);
        assert!(split_cmdline("  ").is_empty());
    }
}
