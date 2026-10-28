//! Access log parser, router and normalizer against lines produced by the real servers.
//!
//! `tests/fixtures/*.jsonl` were captured from nginx 1.30.6, Apache 2.4.69 and Caddy 2.11.7
//! running the access-log configuration of `crates/forge-render/templates` (docs/14 section 2):
//! `log_format forgebox` of `nginx.conf.j2`, `LogFormat forgebox_json` of `httpd.conf.j2` and
//! `format json` of `Caddyfile.j2`. The formats below are the `logs[].access` mapping of the
//! manifests of those tools (docs/04), written out because this crate does not read manifests.

use std::fmt::Write as _;
use std::time::Instant;

use forge_core::ids::SiteId;
use forge_metrics::{
    AccessFormat, AccessParser, AccessRecord, FieldMap, HostRouter, MAX_LINE_BYTES, Multi,
    ParseError, Routed, SiteHosts, Unit, UpstreamMap,
};

fn nginx() -> AccessFormat {
    AccessFormat {
        ts: FieldMap::new("/t", Unit::Seconds),
        ts_is_start: false,
        host: "/host".into(),
        status: "/s".into(),
        duration: FieldMap::new("/rt", Unit::Seconds),
        upstream_duration: Some(UpstreamMap {
            field: FieldMap::new("/ut", Unit::Seconds),
            multi: Multi::Sum,
        }),
        bytes: Some("/b".into()),
        method: Some("/m".into()),
        uri: Some("/u".into()),
        lenient_escapes: false,
    }
}

fn apache() -> AccessFormat {
    AccessFormat {
        // %{msec}t is the instant the request was received.
        ts: FieldMap::new("/t", Unit::Millis),
        ts_is_start: true,
        host: "/host".into(),
        status: "/s".into(),
        duration: FieldMap::new("/rt_us", Unit::Micros),
        upstream_duration: None,
        bytes: Some("/b".into()),
        method: Some("/m".into()),
        uri: Some("/u".into()),
        lenient_escapes: true,
    }
}

fn caddy() -> AccessFormat {
    AccessFormat {
        ts: FieldMap::new("/ts", Unit::Seconds),
        ts_is_start: false,
        host: "/request/host".into(),
        status: "/status".into(),
        duration: FieldMap::new("/duration", Unit::Seconds),
        upstream_duration: None,
        bytes: Some("/size".into()),
        method: Some("/request/method".into()),
        uri: Some("/request/uri".into()),
        lenient_escapes: false,
    }
}

fn parser(format: &AccessFormat) -> AccessParser {
    AccessParser::new(format).expect("valid format")
}

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn render(result: &Result<AccessRecord, ParseError>) -> String {
    match result {
        Ok(r) => format!(
            "{} host={} status={} dur={}us up={:?} bytes={} {} route={}",
            r.end_ts, r.host, r.status, r.duration_us, r.upstream_us, r.bytes, r.method, r.route
        ),
        Err(e) => format!("ERR {e:?}"),
    }
}

fn render_fixture(name: &str, parser: &AccessParser) -> String {
    let mut out = String::new();
    for (i, line) in fixture(name).lines().enumerate() {
        writeln!(out, "{:02} {}", i + 1, render(&parser.parse_str(line))).unwrap();
    }
    out
}

#[test]
fn nginx_1_30_6_lines() {
    insta::assert_snapshot!(render_fixture("nginx-1.30.6.jsonl", &parser(&nginx())));
}

#[test]
fn apache_2_4_69_lines() {
    insta::assert_snapshot!(render_fixture("apache-2.4.69.jsonl", &parser(&apache())));
}

#[test]
fn caddy_2_11_7_lines() {
    insta::assert_snapshot!(render_fixture("caddy-2.11.7.jsonl", &parser(&caddy())));
}

#[test]
fn garbage_lines() {
    let nginx_ok = r#"{"t":"1793167336.296","host":"loja.test","s":200,"rt":0.012,"ut":"-","b":3,"m":"GET","u":"/x","up":""}"#;
    let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
    let huge = format!(
        r#"{{"t":"1","host":"a","s":200,"rt":0,"pad":"{}"}}"#,
        "x".repeat(MAX_LINE_BYTES)
    );
    let at_limit = {
        let head = r#"{"t":"1.5","host":"a.test","s":200,"rt":0.5,"pad":""#;
        let tail = r#""}"#;
        let pad = "x".repeat(MAX_LINE_BYTES - head.len() - tail.len());
        format!("{head}{pad}{tail}")
    };
    assert_eq!(at_limit.len(), MAX_LINE_BYTES);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", b"".to_vec()),
        ("blank", b"  \t ".to_vec()),
        ("plain text", b"not json at all".to_vec()),
        (
            "nginx error log line",
            b"2026/10/28 03:04:05 [error] 1234#5678: *1 CreateFile() failed".to_vec(),
        ),
        ("array", b"[1,2,3]".to_vec()),
        ("scalar", b"42".to_vec()),
        ("null", b"null".to_vec()),
        ("empty object", b"{}".to_vec()),
        (
            "truncated object",
            br#"{"t":"1793167336.296","host":"loj"#.to_vec(),
        ),
        ("trailing junk", format!("{nginx_ok} trailing").into_bytes()),
        ("two objects", format!("{nginx_ok}{nginx_ok}").into_bytes()),
        (
            "surrounded by whitespace",
            format!("  {nginx_ok}\r\n").into_bytes(),
        ),
        (
            "missing host",
            br#"{"t":"1793167336.296","s":200,"rt":0.1}"#.to_vec(),
        ),
        (
            "missing timestamp",
            br#"{"host":"a.test","s":200,"rt":0.1}"#.to_vec(),
        ),
        (
            "host is a number",
            br#"{"t":"1.5","host":12,"s":200,"rt":0.1}"#.to_vec(),
        ),
        (
            "status out of range",
            br#"{"t":"1.5","host":"a","s":999,"rt":0.1}"#.to_vec(),
        ),
        (
            "status as string",
            br#"{"t":"1.5","host":"a","s":"404","rt":0.1}"#.to_vec(),
        ),
        (
            "status is null",
            br#"{"t":"1.5","host":"a","s":null,"rt":0.1}"#.to_vec(),
        ),
        (
            "negative duration",
            br#"{"t":"1.5","host":"a","s":200,"rt":-0.1}"#.to_vec(),
        ),
        (
            "duration is text",
            br#"{"t":"1.5","host":"a","s":200,"rt":"fast"}"#.to_vec(),
        ),
        (
            "timestamp is an object",
            br#"{"t":{"a":1},"host":"a","s":200,"rt":0.1}"#.to_vec(),
        ),
        (
            "huge duration",
            br#"{"t":"1.5","host":"a","s":200,"rt":1e300}"#.to_vec(),
        ),
        (
            "duplicate keys keep the last",
            br#"{"t":"1.5","host":"a","host":"B.Test:81","s":200,"rt":0.25,"u":"/q?x=1"}"#.to_vec(),
        ),
        (
            "bad method and bytes",
            br#"{"t":"1.5","host":"a","s":200,"rt":0.25,"b":"-","m":"G E T","u":"/"}"#.to_vec(),
        ),
        (
            "invalid utf-8",
            b"{\"t\":\"1.5\",\"host\":\"\xff\xfe\"}".to_vec(),
        ),
        ("binary", (0u8..=255).collect()),
        ("deeply nested", deep.into_bytes()),
        ("over the line limit", huge.into_bytes()),
        ("exactly at the line limit", at_limit.into_bytes()),
        ("nul bytes", vec![0; 64]),
    ];
    let p = parser(&nginx());
    let mut out = String::new();
    for (name, line) in &cases {
        writeln!(out, "{name}: {}", render(&p.parse(line))).unwrap();
    }
    insta::assert_snapshot!(out);
}

#[test]
fn nginx_upstream_time_variants() {
    let p = parser(&nginx());
    let line = |ut: &str| {
        format!(
            r#"{{"t":"1793167336.296","host":"a.test","s":200,"rt":0.1,"ut":"{ut}","b":1,"m":"GET","u":"/"}}"#
        )
    };
    let up = |ut: &str| p.parse_str(&line(ut)).unwrap().upstream_us;
    assert_eq!(up("-"), None);
    assert_eq!(up(""), None);
    assert_eq!(up("0.016"), Some(16_000));
    assert_eq!(up("2.051, 0.049"), Some(2_100_000));
    assert_eq!(up("0.016 : 0.000"), Some(16_000));
    assert_eq!(up("0.001, 0.002 : 0.003"), Some(6_000));

    let mut last = nginx();
    last.upstream_duration.as_mut().unwrap().multi = Multi::Last;
    let p = parser(&last);
    assert_eq!(
        p.parse_str(&line("0.001, 0.002 : 0.003"))
            .unwrap()
            .upstream_us,
        Some(3_000)
    );
    // A number (not a string) works too.
    let numeric = r#"{"t":"1.5","host":"a","s":200,"rt":0.1,"ut":0.25}"#;
    assert_eq!(p.parse_str(numeric).unwrap().upstream_us, Some(250_000));
}

#[test]
fn apache_end_ts_is_start_plus_duration_and_escapes_are_decoded() {
    let p = parser(&apache());
    let line = r#"{"t":"1793167414971","host":"Loja.Test:8080","s":200,"rt_us":15638,"b":9,"m":"GET","u":"/caf\xc3\xa9/a\"b/\x01?q=1","up":"x"}"#;
    let r = p.parse_str(line).unwrap();
    assert_eq!(r.end_ts.as_microsecond(), 1_793_167_414_971_000 + 15_638);
    assert_eq!(r.host, "loja.test");
    assert_eq!(r.route, "/café/a\"b/_");
    // Without the lenient pre-pass the same line is not JSON.
    let mut strict = apache();
    strict.lenient_escapes = false;
    assert_eq!(
        parser(&strict).parse_str(line).unwrap_err(),
        ParseError::Malformed
    );
}

#[test]
fn no_query_is_retained_in_any_record() {
    for (name, format) in [
        ("nginx-1.30.6.jsonl", nginx()),
        ("apache-2.4.69.jsonl", apache()),
        ("caddy-2.11.7.jsonl", caddy()),
    ] {
        let p = parser(&format);
        for line in fixture(name).lines() {
            let r = p.parse_str(line).unwrap();
            let all = format!("{r:?}");
            assert!(!all.contains("secret"), "{name}: {all}");
            assert!(!r.route.contains(['?', '#']), "{name}: {all}");
        }
    }
}

#[test]
fn parsed_hosts_route_to_sites() {
    let loja = SiteHosts {
        id: SiteId::new(),
        host: "loja.test".into(),
        aliases: vec![],
    };
    let local = SiteHosts {
        id: SiteId::new(),
        host: "127.0.0.1".into(),
        aliases: vec![],
    };
    let router = HostRouter::new(&[loja.clone(), local.clone()]);
    let mut by_site = std::collections::HashMap::<Option<SiteId>, usize>::new();
    for (name, format) in [
        ("nginx-1.30.6.jsonl", nginx()),
        ("apache-2.4.69.jsonl", apache()),
        ("caddy-2.11.7.jsonl", caddy()),
    ] {
        let p = parser(&format);
        for line in fixture(name).lines() {
            let r = p.parse_str(line).unwrap();
            *by_site
                .entry(router.route_normalized(&r.host).site())
                .or_default() += 1;
        }
    }
    // loja.test, shop.loja.test (wildcard) and Loja.Test:8080 -> loja; 127.0.0.1[:port] -> local.
    assert!(by_site[&Some(loja.id)] >= 12);
    assert!(by_site[&Some(local.id)] >= 5);
    // "_", "-" (Apache without Host) and the empty host of the 400 request.
    assert_eq!(router.unrouted() as usize, by_site[&None]);
    assert!(by_site[&None] >= 2);
    assert_eq!(router.route("loja.test"), Routed::Site(loja.id));
}

/// Deterministic xorshift64*: the tests need reproducible "random" input, not entropy.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// What every successful parse must satisfy, whatever the input was.
fn check_invariants(r: &AccessRecord) {
    assert!((100..=599).contains(&r.status));
    assert!(r.host.len() <= 255);
    assert!(!r.host.bytes().any(|b| b.is_ascii_uppercase()));
    assert!(!r.host.ends_with('.'));
    assert!(r.route.len() <= 256, "{:?}", r.route);
    assert!(r.route.is_empty() || r.route.starts_with('/'));
    assert!(!r.route.contains(['?', '#']));
    assert!(!r.route.chars().any(char::is_control));
    assert!(r.route.split('/').filter(|s| !s.is_empty()).count() <= 8);
    assert!(r.method.len() <= 16);
}

#[test]
fn fuzz_never_panics() {
    let parsers = [
        (parser(&nginx()), "nginx-1.30.6.jsonl"),
        (parser(&apache()), "apache-2.4.69.jsonl"),
        (parser(&caddy()), "caddy-2.11.7.jsonl"),
    ];
    // Every parser sees every kind of input: a log source can contain anything.
    let mut corpus: Vec<Vec<u8>> = Vec::new();
    for (_, name) in &parsers {
        corpus.extend(fixture(name).lines().map(|l| l.as_bytes().to_vec()));
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut parsed = 0usize;
    let mut total = 0usize;
    let mut run = |parsers: &[(AccessParser, &str)], line: &[u8]| {
        for (p, _) in parsers {
            total += 1;
            if let Ok(r) = p.parse(line) {
                parsed += 1;
                check_invariants(&r);
            }
        }
    };

    // Every prefix and every suffix of every line.
    for line in &corpus {
        for end in 0..=line.len() {
            run(&parsers, &line[..end]);
            run(&parsers, &line[end..]);
        }
    }
    // Byte flips, insertions, deletions, duplicated and swapped chunks.
    for _ in 0..4_000 {
        let mut line = corpus[rng.below(corpus.len())].clone();
        for _ in 0..=rng.below(6) {
            if line.is_empty() {
                break;
            }
            let at = rng.below(line.len());
            match rng.below(6) {
                0 => line[at] = rng.next() as u8,
                1 => line.insert(at, rng.next() as u8),
                2 => {
                    line.remove(at);
                }
                3 => {
                    let end = (at + rng.below(40)).min(line.len());
                    let chunk = line[at..end].to_vec();
                    line.splice(at..at, chunk);
                }
                4 => {
                    let tokens: [&[u8]; 8] = [
                        b"\\x", b"\\xc3", b"\\\\", b"\"", b"\\u00", b"\\", b"{", b"[",
                    ];
                    let token = tokens[rng.below(tokens.len())];
                    line.splice(at..at, token.iter().copied());
                }
                _ => line.truncate(at),
            }
        }
        run(&parsers, &line);
    }
    // Random bytes and random JSON-looking soup.
    for _ in 0..2_000 {
        let len = rng.below(300);
        let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        run(&parsers, &bytes);
        let mut soup = b"{".to_vec();
        for _ in 0..rng.below(30) {
            let piece: &[u8] = [
                &b"\"t\":"[..],
                b"\"host\":",
                b"\"s\":",
                b"\"rt\":",
                b"\"ut\":",
                b"\"u\":",
                b"\"request\":{",
                b"\"duration\":",
                b"\"status\":",
                b"\"ts\":",
                b"1e999",
                b"-1",
                b"18446744073709551616",
                b"\"\\x41\"",
                b"\"1, 2 : 3\"",
                b"\"-\"",
                b"null",
                b"true",
                b"[]",
                b"{}",
                b"}",
                b",",
                b" ",
            ][rng.below(23)];
            soup.extend_from_slice(piece);
        }
        run(&parsers, &soup);
    }
    // Pathological sizes.
    run(&parsers, &vec![b'{'; MAX_LINE_BYTES]);
    run(&parsers, &vec![b'\\'; MAX_LINE_BYTES]);
    run(&parsers, &vec![b'['; MAX_LINE_BYTES + 1]);
    assert!(total > 50_000, "total={total}");
    // The corpus itself must keep parsing, so the fuzz is not only exercising error paths.
    assert!(parsed > 100, "parsed={parsed}");
}

/// `cargo test --release -p forge-metrics -- --ignored throughput --nocapture`
///
/// One core, whole fixtures cycled, all three formats. The target is 200k lines/s (M2-E5-T3).
#[test]
#[ignore = "performance measurement: run in release mode on a quiet machine"]
fn throughput_at_least_200k_lines_per_second() {
    const TARGET: f64 = 200_000.0;
    let mut slowest = f64::MAX;
    // Spin for a moment so the core is at its steady frequency before measuring.
    let warm = parser(&nginx());
    let warm_lines: Vec<String> = fixture("nginx-1.30.6.jsonl")
        .lines()
        .map(str::to_owned)
        .collect();
    let warm_start = Instant::now();
    let mut i = 0usize;
    while warm_start.elapsed().as_millis() < 2_000 {
        std::hint::black_box(warm.parse(warm_lines[i % warm_lines.len()].as_bytes()).ok());
        i += 1;
    }
    for (name, format) in [
        ("nginx-1.30.6.jsonl", nginx()),
        ("apache-2.4.69.jsonl", apache()),
        ("caddy-2.11.7.jsonl", caddy()),
    ] {
        let p = parser(&format);
        let lines: Vec<String> = fixture(name).lines().map(str::to_owned).collect();
        let n = 50_000usize;
        // Short runs, best of twenty: the number reflects the code, not another build that
        // happens to be compiling on the same machine (the median is printed as well).
        let mut rates = Vec::new();
        // The first runs of a fresh process are noisy (caches, CPU frequency): one extra
        // run is thrown away.
        for _ in 0..=20 {
            let start = Instant::now();
            let mut ok = 0usize;
            for i in 0..n {
                if let Ok(r) = p.parse(lines[i % lines.len()].as_bytes()) {
                    ok += std::hint::black_box(r).status as usize & 1;
                }
            }
            std::hint::black_box(ok);
            rates.push(n as f64 / start.elapsed().as_secs_f64());
        }
        rates.remove(0);
        rates.sort_by(f64::total_cmp);
        let (best, median) = (rates[rates.len() - 1], rates[rates.len() / 2]);
        println!("{name}: best {best:.0} lines/s, median {median:.0} lines/s");
        slowest = slowest.min(best);
    }
    assert!(slowest >= TARGET, "{slowest:.0} lines/s < {TARGET:.0}");
}
