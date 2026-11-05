//! `iced-mcp run-suite`: every saved flow (or the ones matching `--only`),
//! one line per flow, a summary and an exit status — the regression suite
//! for CI or a pre-commit check.
//!
//! End-to-end flows launch their own app, as with `run-flow`. Fragments
//! (flows without `launch`) need an app to run on: `--launch CMD` starts one
//! and shares it between them, relaunching it if it dies. A failing flow
//! leaves a failure bundle under `.iced-mcp/failures/`.

use crate::flow::{self, FlowLaunch};
use crate::params::FlowRunParams;
use crate::tools::{run_flow, Apps};

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) const USAGE: &str = "\
iced-mcp run-suite            run every flow in .iced-mcp/flows/ as a suite
    --root DIR                project root holding .iced-mcp/ (default: current dir)
    --only GLOB               only flows whose name matches (* and ?); repeatable
    --repeat N                run the suite N times
    --until-fail N            run it up to N times, stopping after a round with a failure
    --fail-fast               stop at the first failing flow
    --update-baselines        rewrite screenshot_diff baselines from this run
    --launch CMD              start CMD once and run fragments (flows without launch) on it
    --prerequisite-ok         confirm the prerequisites of fragments hold
Exit status: 0 when every flow passed, 1 otherwise, 2 on bad arguments or no flows.";

/// The suite's options, from the command line.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Options {
    pub(crate) root: Option<PathBuf>,
    pub(crate) only: Vec<String>,
    pub(crate) rounds: u32,
    /// Stop after the first round with a failure.
    pub(crate) until_fail: bool,
    pub(crate) fail_fast: bool,
    pub(crate) update_baselines: bool,
    pub(crate) launch: Option<String>,
    pub(crate) prerequisite_ok: bool,
}

pub(crate) fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        rounds: 1,
        ..Options::default()
    };
    let mut repeat = None;
    let mut until = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = |flag: &str| {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        let count = |flag: &str, text: String| {
            text.parse::<u32>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| format!("{flag} needs a positive number, not `{text}`"))
        };
        match arg.as_str() {
            "--root" => options.root = Some(PathBuf::from(value("--root")?)),
            "--only" => options.only.push(value("--only")?),
            "--repeat" => repeat = Some(count("--repeat", value("--repeat")?)?),
            "--until-fail" => until = Some(count("--until-fail", value("--until-fail")?)?),
            "--fail-fast" => options.fail_fast = true,
            "--update-baselines" => options.update_baselines = true,
            "--launch" => options.launch = Some(value("--launch")?),
            "--prerequisite-ok" => options.prerequisite_ok = true,
            other => return Err(format!("unexpected argument `{other}`")),
        }
    }
    match (repeat, until) {
        (Some(_), Some(_)) => return Err("give --repeat or --until-fail, not both".into()),
        (Some(n), None) => options.rounds = n,
        (None, Some(n)) => {
            options.rounds = n;
            options.until_fail = true;
        }
        (None, None) => {}
    }
    Ok(options)
}

/// `*` matches any run of characters, `?` one; everything else itself.
pub(crate) fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

/// The flows to run, sorted: all, or those matching any `only` pattern.
pub(crate) fn select(all: Vec<String>, only: &[String]) -> Vec<String> {
    all.into_iter()
        .filter(|name| only.is_empty() || only.iter().any(|p| glob(p, name)))
        .collect()
}

/// The shared app fragments run on.
const SHARED: &str = "suite";

/// Runs the suite and returns its exit status and report.
pub(crate) async fn run(apps: &mut Apps, options: &Options) -> (i32, String) {
    if let Some(root) = &options.root {
        apps.set_root(root.clone());
    }
    let root = apps.root().to_path_buf();
    let all = flow::list(&root);
    let names = select(all.clone(), &options.only);
    if names.is_empty() {
        let text = if all.is_empty() {
            format!("No flows in {}.", flow::dir(&root).display())
        } else {
            format!(
                "No flow matches {:?}. Flows: {}",
                options.only,
                all.join(", ")
            )
        };
        return (2, text);
    }

    let started = Instant::now();
    let mut out = String::new();
    let mut passed = 0;
    let mut failed: Vec<String> = Vec::new();
    let mut rounds_run = 0;
    'rounds: for round in 1..=options.rounds {
        rounds_run = round;
        let failures_before = failed.len();
        for name in &names {
            let prefix = if options.rounds > 1 {
                format!("[{round}/{}] ", options.rounds)
            } else {
                String::new()
            };
            let flow_started = Instant::now();
            let fragment = flow::load(&root, name).is_ok_and(|f| f.launch.is_none());
            let mut app = None;
            if fragment {
                if let Some(command) = &options.launch {
                    if let Err(error) = ensure_shared(apps, command).await {
                        let _ = writeln!(out, "{prefix}FAIL {name} — shared app: {error}");
                        failed.push(name.clone());
                        if options.fail_fast {
                            break 'rounds;
                        }
                        continue;
                    }
                    app = Some(SHARED.to_owned());
                } else {
                    let _ = writeln!(
                        out,
                        "{prefix}FAIL {name} — a fragment (no launch): give --launch CMD to run it on a shared app"
                    );
                    failed.push(name.clone());
                    if options.fail_fast {
                        break 'rounds;
                    }
                    continue;
                }
            }
            let run = run_flow(
                apps,
                &FlowRunParams {
                    name: name.clone(),
                    app,
                    update_baselines: Some(options.update_baselines),
                    prerequisite_ok: Some(options.prerequisite_ok),
                },
            )
            .await;
            let secs = flow_started.elapsed().as_secs_f32();
            if run.passed {
                passed += 1;
                let _ = writeln!(out, "{prefix}PASS {name} ({secs:.1}s)");
            } else {
                failed.push(name.clone());
                let _ = writeln!(
                    out,
                    "{prefix}FAIL {name} ({secs:.1}s) — {}",
                    failing_line(&run.text)
                );
                if let Some(bundle) = &run.bundle {
                    let _ = writeln!(out, "    bundle: {}", bundle.display());
                }
                if options.fail_fast {
                    break 'rounds;
                }
            }
        }
        if options.until_fail && failed.len() > failures_before {
            break;
        }
    }
    apps.drop_app(SHARED).await;

    let total = passed + failed.len();
    let _ = write!(
        out,
        "{} — {passed} passed, {} failed of {total} run ({} flow(s) × {rounds_run} round(s)) in {:.1}s",
        if failed.is_empty() { "PASS" } else { "FAIL" },
        failed.len(),
        names.len(),
        started.elapsed().as_secs_f32()
    );
    if !failed.is_empty() {
        let mut unique = failed.clone();
        unique.sort();
        unique.dedup();
        let _ = write!(out, "\nFailed: {}", unique.join(", "));
    }
    (i32::from(!failed.is_empty()), out)
}

/// The step line that failed (`[3] ✗ click "Save" … — why`), or the first
/// line of the error when no step ran.
fn failing_line(report: &str) -> String {
    report
        .lines()
        .find(|l| l.contains("] ✗ ") || l.contains("] ! "))
        .or_else(|| report.lines().next())
        .unwrap_or("")
        .chars()
        .take(300)
        .collect()
}

/// Launches the shared app unless it is up.
async fn ensure_shared(apps: &mut Apps, command: &str) -> Result<(), String> {
    if apps.is_connected(SHARED) {
        return Ok(());
    }
    apps.drop_app(SHARED).await;
    let launch = FlowLaunch {
        command: command.to_owned(),
        cwd: None,
        env: Default::default(),
    };
    apps.launch_named(SHARED, launch, Duration::from_secs(300))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn globs_match_star_and_question_mark() {
        assert!(glob("todo-*", "todo-add"));
        assert!(glob("*", "anything"));
        assert!(glob("a?c", "abc"));
        assert!(!glob("a?c", "ac"));
        assert!(!glob("todo-*", "login"));
        assert!(glob("*-smoke", "cart-smoke"));
    }

    #[test]
    fn selection_keeps_matching_flows() {
        let all = vec!["cart-add".into(), "cart-pay".into(), "login".into()];
        assert_eq!(select(all.clone(), &[]), all);
        assert_eq!(
            select(all.clone(), &["cart-*".into()]),
            ["cart-add", "cart-pay"]
        );
        assert_eq!(
            select(all, &["login".into(), "*-pay".into()]),
            ["cart-pay", "login"]
        );
    }

    #[test]
    fn arguments_parse_and_conflicts_are_refused() {
        let o = parse(&args("--only cart-* --repeat 3 --fail-fast --root /tmp/x")).unwrap();
        assert_eq!(o.only, ["cart-*"]);
        assert_eq!(o.rounds, 3);
        assert!(!o.until_fail && o.fail_fast);
        assert_eq!(o.root, Some(PathBuf::from("/tmp/x")));
        let o = parse(&args("--until-fail 20 --update-baselines")).unwrap();
        assert!(o.until_fail && o.update_baselines);
        assert_eq!(o.rounds, 20);
        assert_eq!(parse(&[]).unwrap().rounds, 1);
        assert!(parse(&args("--repeat 2 --until-fail 3")).is_err());
        assert!(parse(&args("--repeat 0")).is_err());
        assert!(parse(&args("--repeat")).is_err());
        assert!(parse(&args("--bogus")).is_err());
    }

    #[test]
    fn the_failing_step_line_is_picked_from_a_report() {
        let report = "Flow x\n[1] ✓ click \"A\" (0.1s)\n[2] ✗ wait_for \"B\" (3.0s) — never appeared\nFAIL — …";
        assert_eq!(
            failing_line(report),
            "[2] ✗ wait_for \"B\" (3.0s) — never appeared"
        );
        assert_eq!(failing_line("cannot read flow"), "cannot read flow");
    }

    #[tokio::test]
    async fn an_empty_project_exits_2() {
        let root = std::env::temp_dir().join(format!("iced-mcp-suite-{}", std::process::id()));
        let mut apps = Apps::new(root.clone());
        let (code, text) = run(
            &mut apps,
            &Options {
                rounds: 1,
                ..Options::default()
            },
        )
        .await;
        assert_eq!(code, 2);
        assert!(text.starts_with("No flows in"), "{text}");

        // A fragment without --launch fails, and so does the suite.
        crate::flow::save(
            &root,
            "frag",
            &crate::flow::Flow {
                schema: None,
                prerequisite: None,
                launch: None,
                fixture: None,
                fixture_args: None,
                steps: Vec::new(),
            },
        )
        .unwrap();
        let (code, text) = run(
            &mut apps,
            &Options {
                rounds: 1,
                ..Options::default()
            },
        )
        .await;
        assert_eq!(code, 1, "{text}");
        assert!(text.contains("FAIL frag — a fragment"), "{text}");
        assert!(text.contains("FAIL — 0 passed, 1 failed"), "{text}");
        let (code, _) = run(
            &mut apps,
            &Options {
                rounds: 1,
                only: vec!["nope-*".into()],
                ..Options::default()
            },
        )
        .await;
        assert_eq!(code, 2);
        let _ = std::fs::remove_dir_all(root);
    }
}
