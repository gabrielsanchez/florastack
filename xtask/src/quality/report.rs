//! The HTML report, `target/quality/index.html`: one self-contained page (its style and its zoom control inline, no
//! request to anywhere) beside the crops the runner wrote into `crops/`. For each stack it has a table of every run's
//! measures with their change from the baseline, and the same regions of every output side by side at 100%, the truth
//! first when there is one.

use std::fmt::Write as _;

use serde_json::Value;

use super::gate::{Baseline, Check, Comparison, Host, MEASURES, Outcome, State};

/// What the page says about the run as a whole.
pub struct Info<'a> {
    pub product: &'a str,
    /// `cargo xtask quality` and its options.
    pub command: String,
    pub profile: &'a str,
    pub host: &'a Host,
    pub revision: Option<String>,
    pub dirty: Option<bool>,
    pub created_unix_s: u64,
    pub settings: Option<&'a Value>,
    pub baseline: Option<&'a Baseline>,
}

/// The page.
pub fn html(info: &Info<'_>, outcomes: &[Outcome], comparison: Option<&Comparison>) -> String {
    let mut s = String::new();
    let title = format!("{} quality report", info.product);
    let _ = write!(
        s,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n<header>\n<h1>{}</h1>\n",
        esc(&title),
        esc(&title)
    );
    verdict(&mut s, info, comparison);
    facts(&mut s, info, comparison);
    s.push_str(
        "<p class=\"zoom\">Crops at <button type=\"button\" data-zoom=\"1\" aria-pressed=\"true\">100%</button>\
         <button type=\"button\" data-zoom=\"2\" aria-pressed=\"false\">200%</button>\
         <button type=\"button\" data-zoom=\"4\" aria-pressed=\"false\">400%</button> of the screen's pixels</p>\n</header>\n<main>\n",
    );
    if let Some(comparison) = comparison {
        lines(&mut s, "Fails the gate", "failures", &comparison.failures);
        lines(&mut s, "Better than the baseline by more than the tolerance", "improvements", &comparison.improvements);
        lines(&mut s, "Not in the baseline", "notes", &comparison.unbaselined);
        lines(&mut s, "In the baseline, not run here", "notes", &comparison.not_run);
    }
    if outcomes.len() > 1 {
        s.push_str("<nav><h2>Stacks</h2><ul>\n");
        for o in outcomes {
            let _ = writeln!(s, "<li><a href=\"#{}\">{}</a></li>", anchor(&o.id), esc(&o.id));
        }
        s.push_str("</ul></nav>\n");
    }
    for outcome in outcomes {
        stack(&mut s, outcome, comparison);
    }
    s.push_str("</main>\n<script>");
    s.push_str(SCRIPT);
    s.push_str("</script>\n</body>\n</html>\n");
    s
}

fn verdict(s: &mut String, info: &Info<'_>, comparison: Option<&Comparison>) {
    let (class, text) = match (info.baseline, comparison) {
        (None, _) | (_, None) => ("none", "No baseline to compare with.".to_owned()),
        (Some(_), Some(c)) if c.passed() => ("pass", "The gate passes: nothing is worse than the baseline by more than its tolerance.".to_owned()),
        (Some(_), Some(c)) => ("fail", format!("The gate fails: {} problem(s), listed below.", c.failures.len())),
    };
    let _ = writeln!(s, "<p class=\"verdict {class}\">{}</p>", esc(&text));
}

fn facts(s: &mut String, info: &Info<'_>, comparison: Option<&Comparison>) {
    s.push_str("<dl class=\"facts\">\n");
    let mut fact = |term: &str, value: String| {
        let _ = writeln!(s, "<div><dt>{}</dt><dd>{}</dd></div>", esc(term), esc(&value));
    };
    fact("Command", info.command.clone());
    let revision = match (&info.revision, info.dirty) {
        (Some(r), Some(true)) => format!("{r}, with uncommitted changes"),
        (Some(r), _) => r.clone(),
        (None, _) => "unknown".to_owned(),
    };
    fact("Revision", revision);
    fact("Made", utc(info.created_unix_s));
    fact("Build", format!("{} profile, on {}", info.profile, info.host.describe()));
    if let Some(settings) = info.settings {
        fact("Settings", format!("stack.set {settings} before every stack"));
    }
    if let Some(baseline) = info.baseline {
        let runtime = match comparison {
            Some(c) if c.runtime_compared => "runtimes compared",
            _ => "runtimes not compared: another machine or build profile",
        };
        fact("Baseline", format!("{} runs, {} profile, on {}; {runtime}", baseline.runs.len(), baseline.profile, baseline.host.describe()));
    }
    s.push_str("</dl>\n");
}

fn lines(s: &mut String, heading: &str, class: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    let _ = writeln!(s, "<section class=\"{class}\"><h2>{} ({})</h2><ul>", esc(heading), items.len());
    for item in items {
        let _ = writeln!(s, "<li>{}</li>", esc(item));
    }
    s.push_str("</ul></section>\n");
}

fn stack(s: &mut String, outcome: &Outcome, comparison: Option<&Comparison>) {
    let _ = writeln!(s, "<section class=\"stack\" id=\"{}\">\n<h2>{}</h2>", anchor(&outcome.id), esc(&outcome.id));
    let result = match &outcome.result {
        Ok(result) => result,
        Err(e) => {
            let _ = writeln!(s, "<p class=\"error\">The runner failed: {}</p>\n</section>", esc(e));
            return;
        }
    };
    let mut about = Vec::new();
    if let Some(d) = &result.description {
        about.push(d.clone());
    }
    about.push(format!("{} frames, {}x{}", result.frames, result.width, result.height));
    about.push(format!("on {} thread{}", result.threads, if result.threads == 1 { "" } else { "s" }));
    about.push(if result.truth { "scored against its truth".to_owned() } else { "no truth to score against".to_owned() });
    let _ = writeln!(s, "<p>{}</p>", esc(&about.join(" · ")));
    for note in &result.notes {
        let _ = writeln!(s, "<p class=\"note\">{}</p>", esc(note));
    }

    // The table: the measures some run has, in MEASURES' order.
    let shown: Vec<_> = MEASURES.iter().filter(|m| result.runs.iter().any(|r| r.measures().contains_key(m.key))).collect();
    s.push_str("<div class=\"scroll\"><table>\n<thead><tr><th>Run</th>");
    for m in &shown {
        let _ = write!(s, "<th>{}</th>", esc(m.name));
    }
    s.push_str("</tr></thead>\n<tbody>\n");
    for run in &result.runs {
        let mut name = esc(&run.title);
        if let (Some(slabs), Some(reused)) = (run.slabs, run.slabs_reused) {
            let _ = write!(name, "<small>{slabs} slabs{}</small>", if reused > 0 { format!(", {reused} reused") } else { String::new() });
        }
        let _ = write!(s, "<tr><th scope=\"row\">{name}</th>");
        if let Some(e) = &run.error {
            let _ = write!(s, "<td class=\"error\" colspan=\"{}\">{}</td></tr>", shown.len(), esc(e));
            continue;
        }
        let measures = run.measures();
        let checks = comparison.and_then(|c| c.checks.get(&run.id));
        for m in &shown {
            let digits = m.digits;
            match measures.get(m.key) {
                None => s.push_str("<td></td>"),
                Some(v) => {
                    let check = checks.and_then(|c| c.get(m.key));
                    let _ = write!(s, "<td>{v:.digits$}{}</td>", delta(check, digits));
                }
            }
        }
        s.push_str("</tr>\n");
    }
    s.push_str("</tbody></table></div>\n");

    // The crops.
    for (i, region) in result.regions.iter().enumerate() {
        let _ = writeln!(
            s,
            "<h3>Region {}: {} <small>({}x{} at {}, {})</small></h3>\n<div class=\"strip\">",
            i + 1,
            esc(&region.why),
            region.width,
            region.height,
            region.x,
            region.y
        );
        if let Some(truth) = &region.truth {
            figure(s, truth, region.width, region.height, "Truth");
        }
        for run in &result.runs {
            if let Some(crop) = run.crops.get(i) {
                let ssim = run.metrics.get("ssim").map_or(String::new(), |v| format!(" · SSIM {v:.4}"));
                figure(s, crop, region.width, region.height, &format!("{}{ssim}", run.title));
            }
        }
        s.push_str("</div>\n");
    }
    s.push_str("</section>\n");
}

/// A measure's change from the baseline, marked when it is beyond the tolerance.
fn delta(check: Option<&Check>, digits: usize) -> String {
    let Some(check) = check else { return String::new() };
    let change = check.value - check.baseline;
    let digits = digits.max(1);
    let (class, title) = match check.state {
        State::Regressed => ("worse", format!("worse than the baseline by more than the tolerance ({:.digits$})", check.allowed)),
        State::Improved => ("better", format!("better than the baseline by more than the tolerance ({:.digits$})", check.allowed)),
        State::Within => ("same", format!("within the tolerance ({:.digits$}) of the baseline", check.allowed)),
        State::NotCompared => ("same", "the baseline's, measured on another machine or build".to_owned()),
    };
    let text = if check.state == State::NotCompared { format!("{:.digits$} there", check.baseline) } else { format!("{change:+.digits$}") };
    format!("<span class=\"{class}\" title=\"{}\">{}</span>", esc(&title), esc(&text))
}

fn figure(s: &mut String, src: &str, width: usize, height: usize, caption: &str) {
    let _ = writeln!(
        s,
        "<figure><img src=\"{}\" width=\"{width}\" height=\"{height}\" style=\"--w:{width}px\" alt=\"{}\" loading=\"lazy\"><figcaption>{}</figcaption></figure>",
        esc(src),
        esc(caption),
        esc(caption)
    );
}

/// An id usable as an HTML anchor.
fn anchor(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect()
}

/// Text with HTML's special characters escaped.
pub fn esc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Seconds since 1970 as a UTC date and time (`2026-10-29 14:05 UTC`), by Howard Hinnant's `civil_from_days`.
pub fn utc(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(0);
    let (hour, minute) = ((seconds % 86_400) / 3600, (seconds % 3600) / 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year}-{month:02}-{day:02} {hour:02}:{minute:02} UTC")
}

const STYLE: &str = r"
:root { --bg: #fbfbf9; --fg: #1d1f1c; --muted: #5d625b; --line: #d9dbd5; --panel: #f1f2ee; --worse: #b3261e; --better: #1e7a34;
  --pass-bg: #e3f1e5; --fail-bg: #f8e1df; --scale: 1; color-scheme: light; }
@media (prefers-color-scheme: dark) {
  :root:not([data-theme=light]) { --bg: #161815; --fg: #e6e8e3; --muted: #a2a79f; --line: #353933; --panel: #1f221e; --worse: #ff8a80;
    --better: #7fd88f; --pass-bg: #1f3323; --fail-bg: #3d2220; color-scheme: dark; }
}
:root[data-theme=dark] { --bg: #161815; --fg: #e6e8e3; --muted: #a2a79f; --line: #353933; --panel: #1f221e; --worse: #ff8a80;
  --better: #7fd88f; --pass-bg: #1f3323; --fail-bg: #3d2220; color-scheme: dark; }
* { box-sizing: border-box; }
body { margin: 0; padding: 16px; background: var(--bg); color: var(--fg); font: 14px/1.45 system-ui, sans-serif; }
header, main { max-width: 1600px; margin: 0 auto; }
h1 { font-size: 22px; margin: 0 0 8px; }
h2 { font-size: 18px; margin: 28px 0 8px; }
h3 { font-size: 15px; margin: 18px 0 6px; }
small { color: var(--muted); font-weight: normal; margin-left: 6px; }
.verdict { padding: 8px 12px; border-radius: 6px; font-weight: 600; }
.verdict.pass { background: var(--pass-bg); }
.verdict.fail { background: var(--fail-bg); }
.verdict.none { background: var(--panel); }
.facts { display: grid; grid-template-columns: repeat(auto-fit, minmax(280px, 1fr)); gap: 4px 24px; margin: 12px 0; }
.facts div { display: flex; gap: 8px; } .facts dt { color: var(--muted); min-width: 72px; } .facts dd { margin: 0; overflow-wrap: anywhere; }
.zoom button { font: inherit; padding: 2px 10px; margin-left: 4px; border: 1px solid var(--line); background: var(--panel); color: var(--fg); border-radius: 4px; cursor: pointer; }
.zoom button[aria-pressed=true] { border-color: var(--fg); font-weight: 600; }
section.failures li { color: var(--worse); } section.improvements li { color: var(--better); }
section.notes li, .note { color: var(--muted); }
nav ul { columns: 3 220px; padding-left: 18px; }
.scroll { overflow-x: auto; }
table { border-collapse: collapse; font-variant-numeric: tabular-nums; }
th, td { padding: 3px 10px; border-bottom: 1px solid var(--line); text-align: right; white-space: nowrap; }
th[scope=row], thead th:first-child { text-align: left; }
td span { display: block; font-size: 11px; }
.worse { color: var(--worse); font-weight: 600; } .better { color: var(--better); font-weight: 600; } .same { color: var(--muted); }
.error { color: var(--worse); }
.strip { display: flex; gap: 10px; overflow-x: auto; padding-bottom: 8px; }
figure { margin: 0; flex: none; }
figure img { display: block; width: calc(var(--w) * var(--scale)); height: auto; image-rendering: pixelated; background: var(--panel); }
figcaption { font-size: 12px; color: var(--muted); max-width: max(calc(var(--w) * var(--scale)), 120px); }
";

/// The zoom control: 100% is one image pixel per screen pixel, whatever the screen's pixel ratio.
const SCRIPT: &str = r"
(function () {
  var zoom = 1;
  function apply() {
    document.documentElement.style.setProperty('--scale', String(zoom / (window.devicePixelRatio || 1)));
    document.querySelectorAll('.zoom button').forEach(function (b) { b.setAttribute('aria-pressed', String(Number(b.dataset.zoom) === zoom)); });
  }
  document.querySelectorAll('.zoom button').forEach(function (b) { b.addEventListener('click', function () { zoom = Number(b.dataset.zoom); apply(); }); });
  window.addEventListener('resize', apply);
  apply();
})();
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::gate::{Region, RunResult, StackResult, compare, updated};

    fn result() -> StackResult {
        let run = |run: &str, title: &str, ssim: f64| RunResult {
            id: format!("synthetic/a/{run}"),
            run: run.into(),
            title: title.into(),
            runtime_s: 0.5,
            peak_mib: 10.0,
            metrics: [("ssim".to_owned(), ssim)].into(),
            uncovered: Some(0),
            slabs: (run == "slabbed").then_some(4),
            slabs_reused: (run == "slabbed").then_some(4),
            crops: vec![format!("crops/synthetic--a/r1-{run}.png")],
            error: None,
        };
        StackResult {
            id: "synthetic/a".into(),
            synthetic: true,
            description: Some("Bristles <over> discs".into()),
            frames: 12,
            width: 480,
            height: 360,
            threads: 1,
            truth: true,
            runs: vec![run("pyramid", "Pyramid", 0.93), run("slabbed", "Pyramid slabs, then pyramid", 0.931)],
            regions: vec![Region {
                x: 8,
                y: 16,
                width: 120,
                height: 120,
                why: "where the methods differ most".into(),
                truth: Some("crops/synthetic--a/r1-truth.png".into()),
            }],
            notes: vec!["skipped frame-03.tif: unreadable".into()],
        }
    }

    #[test]
    fn the_report_renders_every_stack_run_measure_and_crop() {
        let host = Host { os: "linux".into(), arch: "x86_64".into(), logical_cpus: 4, cpu: None };
        let outcomes =
            [Outcome { id: "synthetic/a".into(), result: Ok(result()) }, Outcome { id: "local/wasp".into(), result: Err("no frames".into()) }];
        let baseline = updated(None, &outcomes[..1], "dev", &host).unwrap();
        let mut worse = result();
        worse.runs[0].metrics.insert("ssim".into(), 0.9);
        let degraded =
            [Outcome { id: "synthetic/a".into(), result: Ok(worse) }, Outcome { id: "local/wasp".into(), result: Err("no frames".into()) }];
        let comparison = compare(&baseline, &degraded, &|_| true, false);
        let info = Info {
            product: "FloraStack",
            command: "cargo xtask quality --gate".into(),
            profile: "dev",
            host: &host,
            revision: Some("abc123".into()),
            dirty: Some(true),
            created_unix_s: 1_793_282_700,
            settings: None,
            baseline: Some(&baseline),
        };
        let page = html(&info, &degraded, Some(&comparison));
        assert!(page.starts_with("<!doctype html>") && page.ends_with("</html>\n"));
        assert!(page.contains("<title>FloraStack quality report</title>"));
        assert!(page.contains("<p class=\"verdict fail\">The gate fails: 1 problem(s), listed below.</p>"), "{page}");
        assert!(page.contains("synthetic/a/pyramid: SSIM 0.90000, worse than the baseline&#39;s 0.93000"), "{page}");
        assert!(page.contains("Bristles &lt;over&gt; discs · 12 frames, 480x360 · on 1 thread · scored against its truth"), "{page}");
        assert!(page.contains("<td>0.9000<span class=\"worse\""), "{page}");
        assert!(page.contains("Pyramid slabs, then pyramid<small>4 slabs, 4 reused</small>"), "{page}");
        assert!(page.contains("runtimes not compared"), "{page}");
        assert!(page.contains("<img src=\"crops/synthetic--a/r1-truth.png\" width=\"120\" height=\"120\""), "{page}");
        assert!(page.contains("<img src=\"crops/synthetic--a/r1-slabbed.png\""), "{page}");
        assert!(page.contains("The runner failed: no frames"), "{page}");
        assert!(page.contains("<a href=\"#local_wasp\">local/wasp</a>"), "{page}");
        assert!(page.contains("abc123, with uncommitted changes") && page.contains("2026-10-29 "), "{page}");
        // Every element opened is closed, in order.
        let mut open: Vec<String> = Vec::new();
        let voids = ["meta", "img", "br", "!doctype"];
        for tag in page.split('<').skip(1).filter_map(|t| t.split(['>', ' ']).next()) {
            if let Some(name) = tag.strip_prefix('/') {
                assert_eq!(open.pop().as_deref(), Some(name), "</{name}> closes another element");
            } else if !voids.contains(&tag.to_ascii_lowercase().as_str()) {
                open.push(tag.to_owned());
            }
        }
        assert!(open.is_empty(), "{open:?} left open");
    }

    #[test]
    fn text_is_escaped_and_dates_read() {
        assert_eq!(esc("<a href=\"x\">'&'</a>"), "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;");
        assert_eq!(utc(0), "1970-01-01 00:00 UTC");
        assert_eq!(utc(951_782_400), "2000-02-29 00:00 UTC");
        assert_eq!(utc(1_793_282_700), "2026-10-29 14:05 UTC");
        assert_eq!(anchor("synthetic/layered-clean"), "synthetic_layered-clean");
    }
}
