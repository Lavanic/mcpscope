//! Aggregate view over recordings: which methods are called, how often they
//! fail, how many were never answered, and how slow the answers are.

use crate::db;
use crate::record::now_ms;
use anyhow::{Context, Result, bail};
use rusqlite::params;
use std::collections::BTreeMap;

#[derive(Default)]
struct Bucket {
    calls: u64,
    answered: u64,
    errors: u64,
    latencies: Vec<i64>,
}

pub fn run(server: Option<&str>, since: Option<&str>) -> Result<()> {
    let cutoff = match since {
        Some(text) => now_ms() - parse_duration_ms(text)?,
        None => 0,
    };
    let conn = db::open(&db::default_path()?)?;
    // notifications carry methods too but are one-way by design; stats is
    // about question-and-answer traffic
    let mut stmt = conn.prepare(
        "SELECT s.server_name, m.kind, m.method, m.error_code IS NOT NULL, m.latency_ms
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE m.method IS NOT NULL
           AND m.kind IN ('request', 'response')
           AND m.ts_ms >= ?1
           AND (?2 IS NULL OR s.server_name = ?2)",
    )?;

    // a BTreeMap iterates in key order, so the table comes out sorted by
    // server then method for free
    let mut table: BTreeMap<(String, String), Bucket> = BTreeMap::new();
    let rows = stmt.query_map(params![cutoff, server], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, Option<i64>>(4)?,
        ))
    })?;
    for row in rows {
        let (server_name, kind, method, is_error, latency) = row?;
        let bucket = table.entry((server_name, method)).or_default();
        if kind == "request" {
            bucket.calls += 1;
        } else {
            bucket.answered += 1;
            if is_error {
                bucket.errors += 1;
            }
            if let Some(ms) = latency {
                bucket.latencies.push(ms);
            }
        }
    }
    if table.is_empty() {
        eprintln!("mcpscope: no recorded calls match");
        return Ok(());
    }

    println!(
        "{:<14} {:<30} {:>6} {:>7} {:>8} {:>8} {:>8}",
        "SERVER", "METHOD", "CALLS", "ERRORS", "PENDING", "P50", "P95"
    );
    for ((server_name, method), mut bucket) in table {
        bucket.latencies.sort_unstable();
        println!(
            "{:<14} {:<30} {:>6} {:>7} {:>8} {:>8} {:>8}",
            server_name,
            method,
            bucket.calls,
            bucket.errors,
            bucket.calls.saturating_sub(bucket.answered),
            fmt_ms(percentile(&bucket.latencies, 0.50)),
            fmt_ms(percentile(&bucket.latencies, 0.95)),
        );
    }
    Ok(())
}

/// Nearest-rank percentile of an already sorted list.
fn percentile(sorted: &[i64], p: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    Some(sorted[idx])
}

fn fmt_ms(ms: Option<i64>) -> String {
    match ms {
        Some(ms) => format!("{ms}ms"),
        None => "-".to_owned(),
    }
}

/// "90s", "30m", "24h", "7d" to milliseconds.
fn parse_duration_ms(text: &str) -> Result<i64> {
    let (number, unit) = text.split_at(text.len().saturating_sub(1));
    let n: i64 = number
        .parse()
        .with_context(|| format!("bad duration \"{text}\": use forms like 30m, 24h, 7d"))?;
    Ok(match unit {
        "s" => n * 1000,
        "m" => n * 60_000,
        "h" => n * 3_600_000,
        "d" => n * 86_400_000,
        _ => bail!("bad duration \"{text}\": use s, m, h or d"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_use_nearest_rank() {
        let sorted = [10, 20, 30, 40, 100];
        assert_eq!(percentile(&sorted, 0.50), Some(30));
        assert_eq!(percentile(&sorted, 0.95), Some(100));
        assert_eq!(percentile(&[], 0.50), None);
        assert_eq!(percentile(&[7], 0.95), Some(7));
    }

    #[test]
    fn durations_parse() {
        assert_eq!(parse_duration_ms("90s").unwrap(), 90_000);
        assert_eq!(parse_duration_ms("30m").unwrap(), 1_800_000);
        assert_eq!(parse_duration_ms("24h").unwrap(), 86_400_000);
        assert_eq!(parse_duration_ms("7d").unwrap(), 604_800_000);
        assert!(parse_duration_ms("24").is_err());
        assert!(parse_duration_ms("h").is_err());
    }
}
