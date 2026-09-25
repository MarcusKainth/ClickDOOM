use clickdoom_native::sql::{self, Statement, sim::tick};
use clickdoom_native::{load, wad::Wad};

mod support;

fn x(bytes: &[u8]) -> u64 {
    xxhash_rust::xxh64::xxh64(bytes, 0)
}

fn h(s: &Statement) -> String {
    format!("{:016x} {:016x} {:?}", x(s.sql.as_bytes()), x(&s.body), s.settings)
}

#[test]
fn print_hashes() {
    let (a, b) = tick::resident_statements("nat");
    println!("stage1 {} {:016x}", a.len(), x(a.as_bytes()));
    println!("stage2 {} {:016x}", b.len(), x(b.as_bytes()));
    let rows = [tick::Input { tic: 7, source: 1, keys: 3, mouse: (1, -2) }];
    for s in tick::run_statement("nat", &rows) {
        println!("run {}", h(&s));
    }
    for s in tick::demo_statement("nat", 1, 40) {
        println!("demo {}", h(&s));
    }
    let bytes = support::doom1();
    let wad = Wad::parse(&bytes).unwrap();
    let mut all = String::new();
    for s in load::plan("nat", &wad)
        .iter()
        .chain(&sql::level_statements("nat", "E1M1", "demo3"))
        .chain(&sql::render_statements("nat", "SKY1"))
    {
        all.push_str(&h(s));
        all.push('\n');
    }
    println!("load+level+render {:016x}", x(all.as_bytes()));
    let p = [sql::parity::first_divergence("nat"), sql::parity::field_summary("nat")].concat();
    println!("parity {:016x}", x(p.as_bytes()));
}
