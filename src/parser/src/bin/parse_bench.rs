//! Parse-speed benchmark harness for the parse-speedup project (see PARSE_SPEEDUP_GOAL.md).
//! Representative tick parse, timed wall-clock, ST and/or MT, N iterations ->
//! min / median / max. In `both` mode, ST and MT outputs are compared before timing.
//!
//! Usage: parse_bench <demo.dem> [iters=7] [mode=both|st|mt] [threads]

use ahash::AHashMap;
use memmap2::MmapOptions;
use parser::first_pass::parser_settings::ParserInputs;
use parser::parse_demo::{DemoOutput, Parser, ParsingMode};
use parser::second_pass::parser_settings::create_huffman_lookup_table;
use std::env;
use std::fs::File;
use std::time::Instant;

/// Representative workload: the props a typical stats/positions parse requests. Broad enough to
/// exercise the entity + prop-decode hot path without being a synthetic micro-case.
fn wanted_props() -> Vec<String> {
    [
        "tick",
        "health",
        "X",
        "Y",
        "Z",
        "CCSPlayerPawn.m_angEyeAngles",
        "is_alive",
        "team_num",
        "active_weapon_name",
        "FORWARD",
        "LEFT",
        "RIGHT",
        "BACK",
        "FIRE",
        "is_walking",
        "is_airborne",
        "flash_duration",
        "armor_value",
        "balance",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn settings<'a>(huf: &'a Vec<(u8, u8)>) -> ParserInputs<'a> {
    let wanted = wanted_props();
    ParserInputs {
        wanted_player_props: wanted.clone(),
        wanted_events: vec![],
        real_name_to_og_name: AHashMap::default(),
        wanted_other_props: vec![],
        parse_ents: true,
        wanted_players: vec![],
        wanted_ticks: vec![],
        parse_projectiles: false,
        parse_grenades: false,
        only_header: false,
        list_props: false,
        only_convars: false,
        huffman_lookup_table: huf,
        order_by_steamid: false,
        wanted_prop_states: AHashMap::default(),
        fallback_bytes: None,
    }
}

fn parse_once(mmap: &[u8], huf: &Vec<(u8, u8)>, mode: ParsingMode, thread_count: Option<usize>) -> (f64, DemoOutput) {
    let mut parser = Parser::new(settings(huf), mode);
    if let Some(thread_count) = thread_count {
        parser.set_thread_count(thread_count).expect("valid thread count");
    }
    let t = Instant::now();
    let out = parser.parse_demo(mmap).expect("parse");
    let secs = t.elapsed().as_secs_f64();
    // touch output so the optimizer can't elide the parse
    std::hint::black_box(&out.df);
    // Deterministic golden checksum of the output DataFrame (CS2_CKSUM=1). DefaultHasher uses
    // fixed keys, so the hash is stable across runs/builds — used for before/after identity checks.
    if std::env::var("CS2_CKSUM").is_ok() {
        use std::collections::BTreeMap;
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let df: BTreeMap<_, _> = out.df.iter().collect();
        format!("{:?}", df).hash(&mut h);
        let dfp: BTreeMap<_, _> = out.df_per_player.iter().map(|(k, v)| (k, v.iter().collect::<BTreeMap<_, _>>())).collect();
        format!("{:?}", dfp).hash(&mut h);
        eprintln!("[cksum] df={:016x} cols={} per_player={}", h.finish(), out.df.len(), out.df_per_player.len());
    }
    (secs, out)
}

fn verify_outputs(mmap: &[u8], huf: &Vec<(u8, u8)>, thread_count: Option<usize>) {
    let (_, single) = parse_once(mmap, huf, ParsingMode::ForceSingleThreaded, None);
    let (_, multi) = parse_once(mmap, huf, ParsingMode::ForceMultiThreaded, thread_count);
    assert_eq!(single.df, multi.df, "single- and multi-threaded dataframes differ");
    assert_eq!(
        single.df_per_player, multi.df_per_player,
        "single- and multi-threaded per-player dataframes differ"
    );
    println!("verify: ST and MT outputs match ({} columns)", single.df.len());
}

fn bench(mmap: &[u8], huf: &Vec<(u8, u8)>, mode: ParsingMode, label: &str, iters: usize, thread_count: Option<usize>) -> f64 {
    // one warm-up (page cache / branch predictors) excluded from stats
    let _ = parse_once(mmap, huf, mode, thread_count);
    let mut times: Vec<f64> = Vec::with_capacity(iters);
    for _ in 0..iters {
        times.push(parse_once(mmap, huf, mode, thread_count).0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min = times[0];
    let med = times[times.len() / 2];
    let max = *times.last().unwrap();
    let throughput = mmap.len() as f64 / 1e6 / med;
    println!("{label:>4}: min {min:.3}s  median {med:.3}s  max {max:.3}s  {throughput:.1} MB/s  (n={iters})");
    med
}

fn main() {
    let demo_path = env::args().nth(1).expect("usage: parse_bench <demo.dem> [iters] [st|mt|both] [threads]");
    let iters: usize = env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(7);
    let mode = env::args().nth(3).unwrap_or_else(|| "both".to_string());
    let thread_count = env::args().nth(4).map(|s| s.parse::<usize>().expect("threads must be a positive integer"));
    assert!(iters > 0, "iters must be greater than zero");
    assert!(matches!(mode.as_str(), "st" | "mt" | "both"), "mode must be st, mt, or both");
    assert!(thread_count != Some(0), "threads must be greater than zero");

    let huf = create_huffman_lookup_table();
    let file = File::open(&demo_path).expect("open demo");
    let mmap = unsafe { MmapOptions::new().map(&file).unwrap() };
    println!("demo: {demo_path}  ({:.1} MB)", mmap.len() as f64 / 1e6);

    if mode == "both" {
        verify_outputs(&mmap, &huf, thread_count);
    }

    let mut single_median = None;
    if mode == "st" || mode == "both" {
        single_median = Some(bench(&mmap, &huf, ParsingMode::ForceSingleThreaded, "ST", iters, None));
    }
    let mut multi_median = None;
    if mode == "mt" || mode == "both" {
        multi_median = Some(bench(&mmap, &huf, ParsingMode::ForceMultiThreaded, "MT", iters, thread_count));
    }
    if let (Some(single), Some(multi)) = (single_median, multi_median) {
        println!("speedup: {:.2}x", single / multi);
    }
}
