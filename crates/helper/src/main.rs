//! Privileged helper, run as root through `pkexec` when TaskMonitor itself gets EPERM.
//! Deliberately tiny: a fixed set of commands, numeric arguments only, no shell.
//!
//!   taskmonitor-helper signal <TERM|KILL|STOP|CONT> <pid>...
//!   taskmonitor-helper nice <-20..19> <pid>...
//!   taskmonitor-helper efficiency <on|off> <pid>...
//!   taskmonitor-helper affinity <cpu,cpu,...> <pid>...

use std::process::ExitCode;

use taskmonitor_core::actions;
use taskmonitor_core::actions::Signal;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("taskmonitor-helper: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<(), String> {
    let [command, arg, pids @ ..] = args else { return Err("usage: <command> <argument> <pid>...".into()) };
    if pids.is_empty() {
        return Err("no PIDs given".into());
    }
    let pids = pids
        .iter()
        .map(|p| p.parse::<u32>().map_err(|_| format!("invalid PID {p:?}")))
        .collect::<Result<Vec<_>, _>>()?;
    // Signalling init (or "all processes" via 0/-1) would take the system down.
    if pids.iter().any(|&p| p <= 1) {
        return Err("refusing to act on PID 0 or 1".into());
    }

    let mut failed = Vec::new();
    for &pid in &pids {
        let result = match command.as_str() {
            "signal" => {
                let sig = match arg.as_str() {
                    "TERM" => Signal::SIGTERM,
                    "KILL" => Signal::SIGKILL,
                    "STOP" => Signal::SIGSTOP,
                    "CONT" => Signal::SIGCONT,
                    _ => return Err(format!("unsupported signal {arg:?}")),
                };
                actions::signal(pid, sig)
            }
            "nice" => {
                let nice: i32 = arg.parse().map_err(|_| format!("invalid nice value {arg:?}"))?;
                if !(-20..=19).contains(&nice) {
                    return Err("nice value must be between -20 and 19".into());
                }
                actions::set_nice(pid, nice)
            }
            "efficiency" => actions::set_efficiency(pid, arg == "on"),
            "affinity" => {
                let cpus = arg
                    .split(',')
                    .map(|c| c.parse::<usize>().map_err(|_| format!("invalid CPU {c:?}")))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut allowed = vec![false; cpus.iter().max().map_or(0, |m| m + 1)];
                for c in cpus {
                    allowed[c] = true;
                }
                actions::set_affinity(pid, &allowed)
            }
            _ => return Err(format!("unknown command {command:?}")),
        };
        if let Err(e) = result {
            failed.push(format!("{pid}: {e}"));
        }
    }
    if failed.is_empty() { Ok(()) } else { Err(failed.join(", ")) }
}
