//! Report formats: Markdown (for humans, PR comments) and SARIF 2.1.0 (for GitHub code scanning).

use crate::detectors::Finding;
use crate::review::Judged;
use serde_json::{json, Value};

/// SARIF `level` for a reviewer severity word.
pub fn sarif_level(severity: &str) -> &'static str {
    match severity.to_ascii_lowercase().as_str() {
        "critical" | "high" => "error",
        "medium" | "moderate" => "warning",
        _ => "note",
    }
}

/// Repo-relative URI with forward slashes: `./a\b.py` -> `a/b.py`.
fn uri(path: &str) -> String {
    let p = path.replace('\\', "/");
    p.trim_start_matches("./").to_string()
}

fn envelope(rules: Vec<Value>, results: Vec<Value>) -> Value {
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": { "driver": {
                "name": "AutoResolve",
                "version": env!("CARGO_PKG_VERSION"),
                "rules": rules
            }},
            "results": results
        }]
    })
}

/// SARIF for a review: confirmed issues keep their severity, uncertain ones are notes,
/// refuted ones are left out (the skeptic knocked them down).
pub fn sarif_from_review(judged: &[Judged]) -> Value {
    let rules = vec![json!({
        "id": "AR-REVIEW",
        "name": "AgentReviewFinding",
        "shortDescription": { "text": "Issue found by the AutoResolve reviewer and checked by the skeptic" }
    })];
    let results = judged
        .iter()
        .filter(|j| j.verdict.verdict != "refuted")
        .map(|j| {
            let i = &j.issue;
            let level = if j.verdict.verdict == "confirmed" { sarif_level(&i.severity) } else { "note" };
            json!({
                "ruleId": "AR-REVIEW",
                "level": level,
                "message": { "text": format!("{}: {}", i.title, i.explanation) },
                "locations": [{ "physicalLocation": {
                    "artifactLocation": { "uri": uri(&i.file) },
                    "region": { "startLine": i.line.max(1) }
                }}],
                "properties": {
                    "severity": i.severity,
                    "skeptic": j.verdict.verdict,
                    "skepticReason": j.verdict.reason,
                    "suggestedFix": i.fix
                }
            })
        })
        .collect();
    envelope(rules, results)
}

/// SARIF for the AST scanner (one rule entry per detector id).
pub fn sarif_from_scan(findings: &[Finding]) -> Value {
    let mut ids: Vec<&str> = findings.iter().map(|f| f.rule).collect();
    ids.sort_unstable();
    ids.dedup();
    let rules = ids
        .iter()
        .map(|id| json!({ "id": id, "name": id, "shortDescription": { "text": format!("AutoResolve detector {id}") } }))
        .collect();
    let results = findings
        .iter()
        .map(|f| {
            json!({
                "ruleId": f.rule,
                "level": "warning",
                "message": { "text": f.message },
                "locations": [{ "physicalLocation": {
                    "artifactLocation": { "uri": uri(&f.file.display().to_string()) },
                    "region": { "startLine": f.line.max(1), "startColumn": f.col.max(1) }
                }}]
            })
        })
        .collect();
    envelope(rules, results)
}

/// Markdown report, grouped by what the skeptic decided.
pub fn markdown_from_review(judged: &[Judged]) -> String {
    let count = |v: &str| judged.iter().filter(|j| j.verdict.verdict == v).count();
    let mut out = String::from("# AutoResolve review\n\n");
    out.push_str(&format!(
        "{} confirmed, {} uncertain, {} refuted by the skeptic.\n",
        count("confirmed"),
        count("uncertain"),
        count("refuted")
    ));
    for (title, want) in [("Confirmed", "confirmed"), ("Uncertain", "uncertain"), ("Refuted by skeptic", "refuted")] {
        let group: Vec<_> = judged.iter().filter(|j| j.verdict.verdict == want).collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("\n## {title} ({})\n", group.len()));
        for j in group {
            let i = &j.issue;
            out.push_str(&format!(
                "\n### [{}] {}\n`{}:{}`\n\n{}\n\n**Suggested fix:** {}\n\n**Skeptic:** {}\n",
                i.severity.to_uppercase(),
                i.title,
                uri(&i.file),
                i.line,
                i.explanation,
                i.fix,
                j.verdict.reason
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::{Issue, Verdict};
    use std::path::PathBuf;

    fn judged(sev: &str, file: &str, line: u32, verdict: &str) -> Judged {
        Judged {
            issue: Issue {
                severity: sev.into(),
                file: file.into(),
                line,
                title: "T".into(),
                explanation: "E".into(),
                fix: "F".into(),
            },
            verdict: Verdict { verdict: verdict.into(), reason: "R".into() },
        }
    }

    #[test]
    fn severity_maps_to_sarif_levels() {
        assert_eq!(sarif_level("HIGH"), "error");
        assert_eq!(sarif_level("critical"), "error");
        assert_eq!(sarif_level("Medium"), "warning");
        assert_eq!(sarif_level("low"), "note");
        assert_eq!(sarif_level("whatever"), "note");
    }

    #[test]
    fn review_sarif_is_valid_shaped_and_drops_refuted() {
        let j = vec![
            judged("high", "./src\\a.py", 0, "confirmed"),
            judged("high", "b.py", 7, "uncertain"),
            judged("high", "c.py", 3, "refuted"),
        ];
        let s = sarif_from_review(&j);
        assert_eq!(s["version"], "2.1.0");
        let results = s["runs"][0]["results"].as_array().unwrap();
        assert_eq!(results.len(), 2); // refuted is gone
        let first = &results[0];
        assert_eq!(first["level"], "error");
        assert_eq!(first["locations"][0]["physicalLocation"]["artifactLocation"]["uri"], "src/a.py");
        assert_eq!(first["locations"][0]["physicalLocation"]["region"]["startLine"], 1); // SARIF lines start at 1
        assert_eq!(results[1]["level"], "note"); // uncertain never claims error
        assert_eq!(s["runs"][0]["tool"]["driver"]["rules"][0]["id"], "AR-REVIEW");
    }

    #[test]
    fn scan_sarif_lists_each_rule_once() {
        let f = |rule: &'static str, line: usize| Finding {
            rule,
            message: "m".into(),
            file: PathBuf::from("./x.py"),
            line,
            col: 1,
        };
        let s = sarif_from_scan(&[f("PY001", 1), f("PY001", 5), f("PY002", 9)]);
        assert_eq!(s["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap().len(), 2);
        assert_eq!(s["runs"][0]["results"].as_array().unwrap().len(), 3);
        assert_eq!(s["runs"][0]["results"][0]["locations"][0]["physicalLocation"]["artifactLocation"]["uri"], "x.py");
    }

    #[test]
    fn markdown_groups_by_verdict() {
        let md = markdown_from_review(&[judged("high", "a.py", 3, "confirmed"), judged("low", "b.py", 4, "refuted")]);
        assert!(md.contains("1 confirmed, 0 uncertain, 1 refuted"));
        assert!(md.contains("## Confirmed (1)"));
        assert!(md.contains("## Refuted by skeptic (1)"));
        assert!(!md.contains("## Uncertain"));
        assert!(md.contains("`a.py:3`"));
    }
}
