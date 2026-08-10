//! Rendering a recall into the text an agent actually reads.
//!
//! This is the part that decides whether the whole system helps or hurts. A
//! memory tool that dumps 4k tokens of loosely-related history makes the agent
//! worse, not better. So output is hard-capped by a token budget, ordered best
//! first, and every line is a standalone statement — an agent reading line 7 has
//! no other context.

use crate::config::DAY;
use crate::graph::FactRow;
use crate::retrieve::Recall;

/// Rough token count. The usual 4-chars-per-token approximation, floored by the
/// word count so a wall of short tokens can't blow the budget.
pub fn est_tokens(s: &str) -> usize {
    let chars = s.chars().count();
    let words = s.split_whitespace().count();
    (chars / 4).max(words)
}

/// `YYYY-MM-DD` from a millisecond Unix timestamp, UTC. Hinnant's
/// civil-from-days, so no date dependency for one line of output.
pub fn ymd(ts: i64) -> String {
    let days = ts.div_euclid(DAY);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Parse `YYYY-MM-DD` (or a bare epoch) into a millisecond timestamp. The inverse
/// of [`ymd`], so `as_of` can be written the way a human thinks about dates.
///
/// A bare integer below 10^11 is read as epoch *seconds* and scaled up: nobody
/// means "1970-01-01 00:29" when they paste `1700000000`.
pub fn parse_when(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<i64>() {
        return Some(if n.abs() < 100_000_000_000 {
            n * 1000
        } else {
            n
        });
    }
    let mut it = s.splitn(3, '-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.trim_end_matches('Z').parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * DAY)
}

/// A fact's temporal annotation, or empty when it says nothing useful.
fn when_note(f: &FactRow, now_ts: i64) -> String {
    let start = f.valid_from.unwrap_or(f.recorded_at);
    match (f.invalidated_at.is_some(), f.valid_to) {
        (true, Some(end)) => format!(" [was true {} → {}]", ymd(start), ymd(end)),
        (true, None) => format!(" [retracted, noted {}]", ymd(start)),
        // Only date facts that aren't from the last couple of days; "since
        // yesterday" is noise.
        (false, _) if now_ts - start > 2 * DAY => format!(" [since {}]", ymd(start)),
        _ => String::new(),
    }
}

pub struct PackOptions {
    pub budget_tokens: usize,
    pub scope_label: String,
    /// Show `via` and score. For humans debugging retrieval, not for agents.
    pub debug: bool,
}

/// Render to markdown, stopping at the budget.
pub fn render(recall: &Recall, opts: &PackOptions, now_ts: i64) -> String {
    if recall.hits.is_empty() && recall.episodes.is_empty() {
        return String::new();
    }

    let header = format!("## Memory — {}\n", opts.scope_label);
    let mut out = String::with_capacity(opts.budget_tokens * 4);
    out.push_str(&header);
    let mut used = est_tokens(&header);
    let mut shown = 0usize;

    for h in &recall.hits {
        let mut line = format!(
            "- {}{}",
            h.fact.statement.trim(),
            when_note(&h.fact, now_ts)
        );
        if opts.debug {
            line.push_str(&format!("  ({:.4} via {})", h.score, h.via.join("+")));
        }
        // Point at the file(s) this memory was learned against, when any.
        if let Some(eid) = h.fact.episode_id {
            if let Some(files) = recall.files.get(&eid) {
                for f in files {
                    let where_ = match (f.line_from, f.line_to) {
                        (Some(a), Some(b)) if a != b => format!(":{}–{}", a, b),
                        (Some(a), _) => format!(":{a}"),
                        _ => String::new(),
                    };
                    let marker = if opts.debug {
                        // Debug builds show the bounded snippet so humans can
                        // verify the reference landed where it should.
                        format!(
                            "\n    `{}`{}\n    ```{}",
                            f.path,
                            where_,
                            f.lang.as_deref().unwrap_or("")
                        )
                    } else {
                        format!("\n    `{}`{}", f.path, where_)
                    };
                    line.push_str(&marker);
                    if opts.debug && !f.snippet.trim().is_empty() {
                        line.push('\n');
                        line.push_str(f.snippet.trim_end());
                        line.push_str("\n    ```");
                    }
                }
            }
        }
        line.push('\n');
        let cost = est_tokens(&line);
        // Always emit at least one line: a budget too small to fit the single
        // best memory should still return that memory.
        if used + cost > opts.budget_tokens && shown > 0 {
            break;
        }
        out.push_str(&line);
        used += cost;
        shown += 1;
    }

    let omitted = recall.hits.len() - shown;
    if omitted > 0 {
        out.push_str(&format!(
            "- … {omitted} more memories not shown (raise the budget or narrow the query)\n"
        ));
    }

    if !recall.episodes.is_empty() {
        out.push_str("\n### Raw notes\n");
        for e in &recall.episodes {
            let line = format!("- ({}) {}\n", ymd(e.recorded_at), e.text.trim());
            if used + est_tokens(&line) > opts.budget_tokens {
                break;
            }
            used += est_tokens(&line);
            out.push_str(&line);
        }
    }
    out
}

/// Ids of the facts that made it into the rendered output, so usage stats
/// reflect what was actually shown rather than what was merely retrieved.
pub fn rendered_ids(recall: &Recall, rendered: &str) -> Vec<i64> {
    recall
        .hits
        .iter()
        .filter(|h| rendered.contains(h.fact.statement.trim()))
        .map(|h| h.fact.id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::FactRow;
    use crate::retrieve::{Hit, Recall};

    fn fact(id: i64, stmt: &str, recorded_at: i64) -> FactRow {
        FactRow {
            id,
            scope_id: 1,
            src: None,
            rel: "relates_to".into(),
            dst: None,
            statement: stmt.into(),
            confidence: 1.0,
            valid_from: Some(recorded_at),
            valid_to: None,
            recorded_at,
            invalidated_at: None,
            hits: 0,
            episode_id: None,
        }
    }

    fn recall_of(facts: Vec<FactRow>) -> Recall {
        Recall {
            hits: facts
                .into_iter()
                .map(|f| Hit {
                    fact: f,
                    score: 0.5,
                    via: vec!["bm25"],
                })
                .collect(),
            episodes: vec![],
            files: std::collections::HashMap::new(),
            semantic: false,
            took_us: 0,
        }
    }

    fn opts(budget: usize) -> PackOptions {
        PackOptions {
            budget_tokens: budget,
            scope_label: "proj".into(),
            debug: false,
        }
    }

    #[test]
    fn ymd_matches_known_dates() {
        assert_eq!(ymd(0), "1970-01-01");
        assert_eq!(ymd(1_767_225_600_000), "2026-01-01");
        // Leap day, to catch off-by-one in the era math.
        assert_eq!(ymd(1_709_164_800_000), "2024-02-29");
    }

    #[test]
    fn parse_when_roundtrips_with_ymd() {
        for ts in [
            0i64,
            1_767_225_600_000,
            1_709_164_800_000,
            1_800_000_000_000,
        ] {
            let day = ts - ts.rem_euclid(DAY);
            assert_eq!(parse_when(&ymd(ts)), Some(day), "failed for {ts}");
        }
    }

    #[test]
    fn parse_when_accepts_epoch_and_rejects_junk() {
        assert_eq!(parse_when("1700000000"), Some(1_700_000_000_000));
        assert_eq!(parse_when("2026-13-01"), None);
        assert_eq!(parse_when("last tuesday"), None);
        assert_eq!(parse_when(""), None);
    }

    #[test]
    fn budget_is_respected_and_omission_reported() {
        let now = 1_800_000_000_000;
        let facts = (0..40)
            .map(|i| {
                fact(
                    i,
                    &format!("memory number {i} with some padding words here"),
                    now,
                )
            })
            .collect();
        let out = render(&recall_of(facts), &opts(60), now);
        assert!(est_tokens(&out) <= 90, "budget blown: {}", est_tokens(&out));
        assert!(out.contains("more memories not shown"));
    }

    #[test]
    fn tiny_budget_still_returns_the_best_hit() {
        let now = 1_800_000_000_000;
        let out = render(&recall_of(vec![fact(1, "use pnpm", now)]), &opts(1), now);
        assert!(out.contains("use pnpm"), "got {out:?}");
    }

    #[test]
    fn empty_recall_renders_nothing() {
        let out = render(&recall_of(vec![]), &opts(500), 0);
        assert!(out.is_empty(), "no header for no memories");
    }

    #[test]
    fn recent_facts_are_not_date_stamped() {
        let now = 1_800_000_000_000;
        let out = render(
            &recall_of(vec![fact(1, "fresh thing", now - 100_000)]),
            &opts(500),
            now,
        );
        assert!(!out.contains("since"), "got {out:?}");
    }

    #[test]
    fn files_render_as_backticked_paths_and_snippets_in_debug() {
        let now = 1_800_000_000_000;
        let mut f = fact(7, "the deploy target is a make target", now);
        f.episode_id = Some(42);
        let mut r = recall_of(vec![f]);
        r.files.insert(
            42,
            vec![crate::store::FileRef {
                path: "Makefile".into(),
                lang: Some("make".into()),
                snippet: "deploy:\n\tfly deploy\n".into(),
                line_from: Some(1),
                line_to: Some(2),
            }],
        );

        let normal = render(&r, &opts(500), now);
        assert!(normal.contains("`Makefile`:1–2"), "got {normal:?}");
        assert!(
            !normal.contains("fly deploy"),
            "snippet hidden in normal: {normal:?}"
        );

        let mut dbg = opts(500);
        dbg.debug = true;
        let debug = render(&r, &dbg, now);
        assert!(debug.contains("```make"), "got {debug:?}");
        assert!(
            debug.contains("fly deploy"),
            "snippet shown in debug: {debug:?}"
        );
    }

    #[test]
    fn old_facts_are_date_stamped() {
        let now = 1_800_000_000_000;
        let out = render(
            &recall_of(vec![fact(1, "old thing", now - 40 * DAY)]),
            &opts(500),
            now,
        );
        assert!(out.contains("[since "), "got {out:?}");
    }

    #[test]
    fn retracted_fact_shows_its_window() {
        let now = 1_800_000_000_000;
        let mut f = fact(1, "used npm", now - 100 * DAY);
        f.invalidated_at = Some(now - 10 * DAY);
        f.valid_to = Some(now - 10 * DAY);
        let out = render(&recall_of(vec![f]), &opts(500), now);
        assert!(out.contains("was true"), "got {out:?}");
        assert!(out.contains(" → "), "got {out:?}");
    }

    #[test]
    fn rendered_ids_only_counts_shown_lines() {
        let now = 1_800_000_000_000;
        let facts: Vec<FactRow> = (0..20)
            .map(|i| fact(i, &format!("statement {i} padded out with words"), now))
            .collect();
        let r = recall_of(facts);
        let out = render(&r, &opts(40), now);
        let ids = rendered_ids(&r, &out);
        assert!(ids.len() < 20 && !ids.is_empty());
    }
}
