//! Host-runnable probe for the GB (RP) pronunciation path. The full library
//! cannot LINK on the host (sherpa espeak symbols), so this bin includes the
//! dependency-free gb_english.rs directly and exercises both the transform
//! and the bundled GB dictionary.
//!
//!     cargo run --bin gb_probe            # built-in checks
//!     cargo run --bin gb_probe word ...   # look up / transform words

#[path = "../gb_english.rs"]
mod gb_english;
use gb_english::us_to_rp;

fn main() {
    let dict: std::collections::HashMap<String, String> =
        serde_json::from_slice(include_bytes!("../../data/gb_dict.json"))
            .expect("parse gb_dict.json");
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        for w in &args {
            let key = w.to_lowercase();
            match dict.get(&key) {
                Some(ipa) => println!("{w}: {ipa}  (dict)"),
                None => println!("{w}: not in GB dict (would take the US+transform path)"),
            }
        }
        return;
    }

    // Dictionary sanity: espeak-verified expectations (offline ground truth).
    let expect = [
        ("computer", "kəmpjˈuːtɐ"),
        ("bath", "bˈɑːθ"),
        ("water", "wˈɔːtɐ"),
        ("near", "nˈiə"),
        ("square", "skwˈeə"),
        ("happy", "hˈæpɪ"),
        ("garden", "ɡˈɑːdn"),
    ];
    let mut fails = 0;
    for (w, e) in expect {
        match dict.get(w) {
            Some(got) if got == e => println!("OK   {w}: {got}"),
            Some(got) => {
                println!("DIFF {w}: {got} (expected {e})");
                fails += 1;
            }
            None => {
                println!("MISS {w}");
                fails += 1;
            }
        }
    }

    // Transform sanity on US g2p style inputs.
    let t = |s: &str| -> String {
        us_to_rp(s.chars().map(|c| c.to_string()).collect()).concat()
    };
    for (us, want) in [
        ("stˈɑːɹ", "stˈɑː"),
        ("ɡˈoʊ", "ɡˈəʊ"),
        ("hˈɑt", "hˈɒt"),
        ("kˈuːbɚnˌɛtiːz", "kˈuːbənˌɛtiːz"),
    ] {
        let got = t(us);
        if got == want {
            println!("OK   transform {us} -> {got}");
        } else {
            println!("DIFF transform {us} -> {got} (expected {want})");
            fails += 1;
        }
    }
    println!("gb dict entries: {}", dict.len());
    std::process::exit(if fails == 0 { 0 } else { 1 });
}
