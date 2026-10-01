//! The summary `axon` prints to the terminal after its first scan.

use axon::summary::Summary;

pub fn print_cli_summary(s: &Summary, login_link: &str) {
    let top_agents: Vec<&str> = s
        .by_agent
        .iter()
        .take(4)
        .map(|a| a.agent.as_str())
        .collect();
    println!("\n  Axon — local AI-agent observability\n");
    println!(
        "  Events        {} across {} sessions",
        commafy(s.events),
        commafy(s.sessions)
    );
    println!(
        "  Tokens out    {}  (in {}, cache-read {})",
        commafy(s.tokens_out),
        commafy(s.tokens_in),
        commafy(s.cache_read)
    );
    println!(
        "  Lines edited  {} added / {} removed",
        commafy(s.loc_added),
        commafy(s.loc_removed)
    );
    println!(
        "  Cost          {}  (all logged history, EUR)",
        eur(s.cost_eur)
    );
    {
        let span = |spent: f64, cap: Option<f64>| match cap {
            Some(c) => format!("{} / {} ({:.0}%)", eur(spent), eur(c), pct(spent, c)),
            None => eur(spent),
        };
        println!(
            "  Spend         today {} · week {}",
            span(s.today_cost_eur, s.budget_day_eur),
            span(s.week_cost_eur, s.budget_week_eur)
        );
    }
    if let Some(r) = &s.rtk {
        println!(
            "  RTK saved     {} tokens ({:.1}%) over {} commands",
            compact(r.tokens_saved),
            r.saved_pct,
            commafy(r.commands)
        );
    }
    if !s.by_harness.is_empty() {
        let parts: Vec<String> = s
            .by_harness
            .iter()
            .map(|h| format!("{} {}", h.harness, eur(h.cost_eur)))
            .collect();
        println!("  Harnesses     {}", parts.join(" · "));
    }
    if !top_agents.is_empty() {
        println!("  Top agents    {}", top_agents.join(", "));
    }
    if !s.unpriced_models.is_empty() {
        println!(
            "  Unpriced      {}  (cost is a floor, not exact)",
            s.unpriced_models.join(", ")
        );
    }
    if !s.credit_priced_models.is_empty() {
        println!(
            "  Credits       {}  ({:.2} ChatGPT credits est., included-plan estimate)",
            s.credit_priced_models.join(", "),
            s.total_credits
        );
    }
    if !s.preview_priced_models.is_empty() {
        println!(
            "  Preview       {}  (separate research-preview limit, no exact monetary rate)",
            s.preview_priced_models.join(", ")
        );
    }
    println!("Dashboard: {login_link}");
}

/// Percent of a budget cap consumed (0 if the cap is non-positive).
fn pct(spent: f64, cap: f64) -> f64 {
    if cap > 0.0 {
        100.0 * spent / cap
    } else {
        0.0
    }
}

/// Format euros with thousands separators: 5607.61 → "€5,607.61".
fn eur(n: f64) -> String {
    let cents = (n * 100.0).round() as u64;
    format!("€{}.{:02}", commafy(cents / 100), cents % 100)
}

/// Compact large counts: 134_504_579 → "134.5M".
fn compact(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.1}B", n as f64 / 1e9),
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}K", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// Group a number into thousands with commas (35929 → "35,929").
fn commafy(n: u64) -> String {
    let s = n.to_string();
    let len = s.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
