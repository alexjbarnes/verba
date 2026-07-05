//! Context-sensitive pronunciation for heteronyms ("read" /riːd/ vs /rɛd/,
//! "live" /lɪv/ vs /laɪv/, the -ate noun/verb family, ...).
//!
//! Every listed word resolves to a pseudo dictionary key ("read1" default,
//! "read2" alternate) that piper.rs inserts into the US phonemizer dict at
//! load. Pseudo-keys are deliberately absent from the GB dictionary, so both
//! variants take the US->RP transform path and the two locales always agree
//! on which reading was chosen.
//!
//! Rules are local-context heuristics over a window of neighbouring words:
//! wrong sometimes by design ("I read books" is genuinely ambiguous), but
//! each default is the reading most common in read-aloud article prose.

use std::collections::HashMap;

/// Pseudo-key -> ARPAbet, merged into the phonemizer dictionary at load.
pub const PRONS: &[(&str, &str)] = &[
    ("read1", "R IY1 D"), ("read2", "R EH1 D"),
    ("live1", "L IH1 V"), ("live2", "L AY1 V"),
    ("lives1", "L IH1 V Z"), ("lives2", "L AY1 V Z"),
    ("lead1", "L IY1 D"), ("lead2", "L EH1 D"),
    ("record1", "R EH1 K ER0 D"), ("record2", "R IH0 K AO1 R D"),
    ("records1", "R EH1 K ER0 D Z"), ("records2", "R IH0 K AO1 R D Z"),
    ("present1", "P R EH1 Z AH0 N T"), ("present2", "P R IH0 Z EH1 N T"),
    ("presents1", "P R EH1 Z AH0 N T S"), ("presents2", "P R IH0 Z EH1 N T S"),
    ("use1", "Y UW1 Z"), ("use2", "Y UW1 S"),
    ("used1", "Y UW1 Z D"), ("used2", "Y UW1 S T"),
    ("wound1", "W UW1 N D"), ("wound2", "W AW1 N D"),
    ("tear1", "T EH1 R"), ("tear2", "T IH1 R"),
    ("tears1", "T EH1 R Z"), ("tears2", "T IH1 R Z"),
    ("wind1", "W IH1 N D"), ("wind2", "W AY1 N D"),
    ("winds1", "W IH1 N D Z"), ("winds2", "W AY1 N D Z"),
    ("close1", "K L OW1 S"), ("close2", "K L OW1 Z"),
    ("minute1", "M IH1 N AH0 T"), ("minute2", "M AY0 N UW1 T"),
    ("dove1", "D AH1 V"), ("dove2", "D OW1 V"),
    ("separate1", "S EH1 P ER0 AH0 T"), ("separate2", "S EH1 P ER0 EY2 T"),
    ("estimate1", "EH1 S T AH0 M AH0 T"), ("estimate2", "EH1 S T AH0 M EY2 T"),
    ("estimates1", "EH1 S T AH0 M AH0 T S"), ("estimates2", "EH1 S T AH0 M EY2 T S"),
    ("graduate1", "G R AE1 JH AH0 W AH0 T"), ("graduate2", "G R AE1 JH AH0 W EY2 T"),
    ("graduates1", "G R AE1 JH AH0 W AH0 T S"), ("graduates2", "G R AE1 JH AH0 W EY2 T S"),
    ("duplicate1", "D UW1 P L AH0 K AH0 T"), ("duplicate2", "D UW1 P L AH0 K EY2 T"),
    ("advocate1", "AE1 D V AH0 K AH0 T"), ("advocate2", "AE1 D V AH0 K EY2 T"),
    ("advocates1", "AE1 D V AH0 K AH0 T S"), ("advocates2", "AE1 D V AH0 K EY2 T S"),
    ("associate1", "AH0 S OW1 S IY0 AH0 T"), ("associate2", "AH0 S OW1 S IY0 EY2 T"),
    ("delegate1", "D EH1 L AH0 G AH0 T"), ("delegate2", "D EH1 L AH0 G EY2 T"),
    ("moderate1", "M AA1 D ER0 AH0 T"), ("moderate2", "M AA1 D ER0 EY2 T"),
    ("deliberate1", "D IH0 L IH1 B ER0 AH0 T"), ("deliberate2", "D IH0 L IH1 B ER0 EY2 T"),
    ("elaborate1", "IH0 L AE1 B ER0 AH0 T"), ("elaborate2", "IH0 L AE1 B ER0 EY2 T"),
    ("aggregate1", "AE1 G R AH0 G AH0 T"), ("aggregate2", "AE1 G R AH0 G EY2 T"),
    ("alternate1", "AO1 L T ER0 N AH0 T"), ("alternate2", "AO1 L T ER0 N EY2 T"),
    ("appropriate1", "AH0 P R OW1 P R IY0 AH0 T"), ("appropriate2", "AH0 P R OW1 P R IY0 EY2 T"),
    ("approximate1", "AH0 P R AA1 K S AH0 M AH0 T"), ("approximate2", "AH0 P R AA1 K S AH0 M EY2 T"),
    ("coordinate1", "K OW0 AO1 R D AH0 N AH0 T"), ("coordinate2", "K OW0 AO1 R D AH0 N EY2 T"),
];

/// Words whose noun/adjective form (default) flips to a verb reading after a
/// to/modal/nominative-pronoun signal.
const ATE_FAMILY: &[&str] = &[
    "separate", "estimate", "estimates", "graduate", "graduates", "duplicate",
    "advocate", "advocates", "associate", "delegate", "moderate", "deliberate",
    "elaborate", "aggregate", "alternate", "appropriate", "approximate",
    "coordinate",
];

const NOM_PRONOUN: &[&str] = &["i", "we", "you", "they", "he", "she", "it", "who"];
const MODALISH: &[&str] = &[
    "to", "will", "would", "can", "could", "should", "shall", "may", "might",
    "must", "please", "don't", "doesn't", "didn't", "won't", "wouldn't",
    "couldn't", "shouldn't", "can't", "cannot", "not", "just", "then", "also",
    "and", "or", "help", "helps", "helped",
];
const DET: &[&str] = &[
    "a", "an", "the", "this", "that", "these", "those", "my", "your", "his",
    "her", "its", "our", "their", "any", "every", "each", "no", "some",
    "such", "another", "one", "whose", "all", "both",
];
const LIVE_NOUNS: &[&str] = &[
    "music", "stream", "streams", "streaming", "show", "shows", "event",
    "events", "tv", "audience", "broadcast", "broadcasts", "performance",
    "performances", "coverage", "demo", "demos", "video", "videos", "feed",
    "feeds", "session", "sessions", "concert", "concerts", "album",
    "recording", "recordings", "action", "wire", "chat", "debut", "version",
    "ammunition", "rounds",
];
const LIVES_NOUN_SIGNALS: &[&str] = &[
    "many", "most", "few", "countless", "million", "millions", "billions",
    "human", "daily", "whole", "entire", "real", "digital", "past",
    "previous", "save", "saves", "saved", "saving", "risk", "risks",
    "risked", "risking", "lost", "lose", "loses", "losing", "cost", "costs",
    "claimed", "claims", "ruin", "ruined", "change", "changed", "changes",
    "changing", "improve", "improves", "improved", "touch", "touched", "end",
    "ended", "take", "takes", "took", "taken", "destroy", "destroyed",
    "shape", "shaped", "transform", "transformed",
];
const USE_NOUN_SIGNALS: &[&str] = &[
    "the", "its", "their", "my", "your", "our", "his", "her", "this", "that",
    "of", "in", "no", "any", "one", "single", "general", "common", "wide",
    "widespread", "practical", "fair", "personal", "intended", "actual",
    "everyday", "daily", "heavy", "little", "much", "real", "good", "best",
    "better", "proper", "effective",
];
const LEAD_METAL_NOUNS: &[&str] = &[
    "pipe", "pipes", "paint", "poisoning", "acid", "exposure", "levels",
    "level", "content", "contamination", "dust", "based", "shielding",
    "bullets", "weights", "pencil",
];
const MINUTE_TINY_NOUNS: &[&str] = &[
    "details", "detail", "amounts", "amount", "quantities", "quantity",
    "particles", "traces", "differences", "changes", "variations",
    "fraction", "fractions", "adjustments",
];
const PAST_HAVE: &[&str] = &["have", "has", "had", "having", "was", "were", "been"];
const CLOSE_OBJECTS: &[&str] = &[
    "the", "a", "an", "it", "them", "this", "that", "my", "your", "his",
    "her", "its", "their", "down", "up", "out", "all",
];

fn has(set: &[&str], w: Option<&str>) -> bool {
    w.map(|w| set.contains(&w)).unwrap_or(false)
}

fn verb_signal(prev: Option<&str>) -> bool {
    has(MODALISH, prev) || has(NOM_PRONOUN, prev)
}

/// Decide the reading for one heteronym occurrence. `prev`/`next` are the
/// surrounding words (up to 3 each side, punctuation pieces skipped),
/// nearest first. Returns true for the alternate ("2") reading.
fn wants_alt(word: &str, prev: &[&str], next: &[&str]) -> bool {
    let p1 = prev.first().copied();
    let p2 = prev.get(1).copied();
    let n1 = next.first().copied();
    match word {
        // Past/participle: "have read", "was read", "I'd already read".
        "read" => prev.iter().any(|w| PAST_HAVE.contains(w) || w.ends_with("'ve"))
            || next.iter().any(|w| *w == "yesterday" || *w == "ago"),
        // Adjective /laɪv/: "live music", "went live", "the stream is live".
        "live" => has(LIVE_NOUNS, n1)
            || has(&["gone", "went", "goes", "going"], p1)
            || (has(&["is", "was", "are", "were"], p1)
                && (n1.is_none() || has(&["on", "at", "now", "from", "across"], n1))),
        // Plural of life: "their lives", "save lives", "people's lives".
        "lives" => has(DET, p1)
            || has(LIVES_NOUN_SIGNALS, p1)
            || p1.map(|w| w.ends_with("'s") || w.ends_with("s'")).unwrap_or(false),
        // The metal: "lead paint", "lead poisoning".
        "lead" => has(LEAD_METAL_NOUNS, n1),
        // Verb stress: "to record", "we record", "they present it".
        "record" | "records" | "present" | "presents" => verb_signal(p1),
        // Noun /juːs/: "the use of", "in use", "no use".
        "use" => has(USE_NOUN_SIGNALS, p1),
        // Habitual/familiar /juːst/: "used to live", "get used to" — but not
        // the passive "was used to build".
        "used" => n1 == Some("to")
            && !has(&["was", "were", "is", "are", "be", "been", "being", "am"], p1),
        // Past of wind: "wound up", "wound down", "wound through".
        "wound" => has(&["up", "down", "around", "through", "along", "back", "tight", "tighter"], n1),
        // Crying /tɪə/: "tear gas", "shed a tear", "in tears", "tears of joy".
        "tear" | "tears" => n1 == Some("gas")
            || has(&["shed", "sheds", "shedding"], p1)
            || has(&["shed", "sheds", "shedding"], p2)
            || (word == "tears"
                && (has(&["in", "to"], p1)
                    || has(&["of", "welled", "rolled", "streaming", "fell"], n1))),
        // Verb /waɪnd/: "wind up", "to wind through".
        "wind" | "winds" => has(&["up", "down", "around", "through", "along", "back"], n1)
            || (p1 == Some("to") && n1.is_some()),
        // Verb /kloʊz/: "close the door", "we close it down".
        "close" => verb_signal(p1) && has(CLOSE_OBJECTS, n1),
        // Tiny /maɪˈnjuːt/: "minute details".
        "minute" => has(MINUTE_TINY_NOUNS, n1),
        // Past of dive: "he dove into".
        "dove" => has(NOM_PRONOUN, p1) || has(&["and", "then"], p1),
        w if ATE_FAMILY.contains(&w) => verb_signal(p1),
        _ => false,
    }
}

fn is_heteronym(word: &str) -> bool {
    matches!(word,
        "read" | "live" | "lives" | "lead" | "record" | "records" | "present"
        | "presents" | "use" | "used" | "wound" | "tear" | "tears" | "wind"
        | "winds" | "close" | "minute" | "dove")
        || ATE_FAMILY.contains(&word)
}

/// Map piece index -> pseudo dictionary key for every heteronym occurrence
/// in the token stream. Pieces with no alphanumeric content (punctuation)
/// are transparent to the context windows.
pub fn resolve(pieces: &[String]) -> HashMap<usize, &'static str> {
    // (piece index, lowercase trimmed word) for word pieces only.
    let words: Vec<(usize, String)> = pieces
        .iter()
        .enumerate()
        .filter(|(_, p)| p.chars().any(|c| c.is_alphanumeric()))
        .map(|(i, p)| {
            (i, p.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        })
        .collect();

    let mut out = HashMap::new();
    for (wi, (pi, word)) in words.iter().enumerate() {
        if !is_heteronym(word) {
            continue;
        }
        let prev: Vec<&str> = words[..wi].iter().rev().take(4).map(|(_, w)| w.as_str()).collect();
        let next: Vec<&str> = words[wi + 1..].iter().take(4).map(|(_, w)| w.as_str()).collect();
        let alt = wants_alt(word, &prev, &next);
        let key = KEYS
            .iter()
            .find(|(w, _, _)| w == word)
            .map(|(_, k1, k2)| if alt { *k2 } else { *k1 });
        if let Some(key) = key {
            out.insert(*pi, key);
        }
    }
    out
}

/// word -> (default key, alternate key). Kept next to PRONS so adding a
/// heteronym touches one file.
const KEYS: &[(&str, &str, &str)] = &[
    ("read", "read1", "read2"),
    ("live", "live1", "live2"),
    ("lives", "lives1", "lives2"),
    ("lead", "lead1", "lead2"),
    ("record", "record1", "record2"),
    ("records", "records1", "records2"),
    ("present", "present1", "present2"),
    ("presents", "presents1", "presents2"),
    ("use", "use1", "use2"),
    ("used", "used1", "used2"),
    ("wound", "wound1", "wound2"),
    ("tear", "tear1", "tear2"),
    ("tears", "tears1", "tears2"),
    ("wind", "wind1", "wind2"),
    ("winds", "winds1", "winds2"),
    ("close", "close1", "close2"),
    ("minute", "minute1", "minute2"),
    ("dove", "dove1", "dove2"),
    ("separate", "separate1", "separate2"),
    ("estimate", "estimate1", "estimate2"),
    ("estimates", "estimates1", "estimates2"),
    ("graduate", "graduate1", "graduate2"),
    ("graduates", "graduates1", "graduates2"),
    ("duplicate", "duplicate1", "duplicate2"),
    ("advocate", "advocate1", "advocate2"),
    ("advocates", "advocates1", "advocates2"),
    ("associate", "associate1", "associate2"),
    ("delegate", "delegate1", "delegate2"),
    ("moderate", "moderate1", "moderate2"),
    ("deliberate", "deliberate1", "deliberate2"),
    ("elaborate", "elaborate1", "elaborate2"),
    ("aggregate", "aggregate1", "aggregate2"),
    ("alternate", "alternate1", "alternate2"),
    ("appropriate", "appropriate1", "appropriate2"),
    ("approximate", "approximate1", "approximate2"),
    ("coordinate", "coordinate1", "coordinate2"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_for(text: &str) -> Vec<&'static str> {
        let pieces: Vec<String> = text.split_whitespace().map(|s| s.to_string()).collect();
        let map = resolve(&pieces);
        let mut hits: Vec<(usize, &str)> = map.into_iter().collect();
        hits.sort();
        hits.into_iter().map(|(_, k)| k).collect()
    }

    #[test]
    fn read_present_and_past() {
        assert_eq!(keys_for("read the docs first"), vec!["read1"]);
        assert_eq!(keys_for("worth a read ,"), vec!["read1"]);
        assert_eq!(keys_for("I have read the docs"), vec!["read2"]);
        assert_eq!(keys_for("she had already read it"), vec!["read2"]);
        assert_eq!(keys_for("I read it two years ago"), vec!["read2"]);
        assert_eq!(keys_for("I've never read the spec"), vec!["read2"]);
    }

    #[test]
    fn live_verb_and_adjective() {
        assert_eq!(keys_for("I live in London"), vec!["live1"]);
        assert_eq!(keys_for("they live near the coast"), vec!["live1"]);
        assert_eq!(keys_for("live music every night"), vec!["live2"]);
        assert_eq!(keys_for("the feature went live yesterday"), vec!["live2"]);
        assert_eq!(keys_for("the stream is live"), vec!["live2"]);
    }

    #[test]
    fn lives_noun_and_verb() {
        assert_eq!(keys_for("he lives in Omaha"), vec!["lives1"]);
        assert_eq!(keys_for("it could save lives"), vec!["lives2"]);
        assert_eq!(keys_for("their lives changed forever"), vec!["lives2"]);
        assert_eq!(keys_for("people's lives improved"), vec!["lives2"]);
    }

    #[test]
    fn used_to_and_passive() {
        assert_eq!(keys_for("I used to live there"), vec!["used2", "live1"]);
        assert_eq!(keys_for("the tool was used to build it"), vec!["used1"]);
        assert_eq!(keys_for("we used the hammer"), vec!["used1"]);
    }

    #[test]
    fn use_noun_and_verb() {
        assert_eq!(keys_for("we use it daily"), vec!["use1"]);
        assert_eq!(keys_for("the use of force"), vec!["use2"]);
        assert_eq!(keys_for("it's no use arguing"), vec!["use2"]);
    }

    #[test]
    fn record_stress() {
        assert_eq!(keys_for("a track record of wins"), vec!["record1"]);
        assert_eq!(keys_for("we record every session"), vec!["record2"]);
        assert_eq!(keys_for("to record the audio"), vec!["record2"]);
    }

    #[test]
    fn ate_family() {
        assert_eq!(keys_for("a rough estimate of cost"), vec!["estimate1"]);
        assert_eq!(keys_for("we estimate the cost"), vec!["estimate2"]);
        assert_eq!(keys_for("keep them in separate files"), vec!["separate1"]);
        assert_eq!(keys_for("to separate the concerns"), vec!["separate2"]);
        assert_eq!(keys_for("an appropriate response"), vec!["appropriate1"]);
    }

    #[test]
    fn wind_tear_wound_close_minute() {
        assert_eq!(keys_for("the wind was cold"), vec!["wind1"]);
        assert_eq!(keys_for("wind down the project"), vec!["wind2"]);
        assert_eq!(keys_for("tear it down"), vec!["tear1"]);
        assert_eq!(keys_for("tear gas filled the square"), vec!["tear2"]);
        assert_eq!(keys_for("in tears after the match"), vec!["tears2"]);
        assert_eq!(keys_for("the wound healed slowly"), vec!["wound1"]);
        assert_eq!(keys_for("wound up in Denver"), vec!["wound2"]);
        assert_eq!(keys_for("we sat close to the stage"), vec!["close1"]);
        assert_eq!(keys_for("please close the door"), vec!["close2"]);
        assert_eq!(keys_for("wait a minute please"), vec!["minute1"]);
        assert_eq!(keys_for("minute details matter here"), vec!["minute2"]);
    }

    #[test]
    fn punctuation_is_transparent() {
        // Punctuation pieces sit between words in the real token stream.
        let pieces: Vec<String> = ["I", "have", ",", "in", "fact", ",", "read", "it"]
            .iter().map(|s| s.to_string()).collect();
        let map = resolve(&pieces);
        assert_eq!(map.get(&6), Some(&"read2"));
    }

    #[test]
    fn prons_table_covers_all_keys() {
        for (_, k1, k2) in KEYS {
            for k in [k1, k2] {
                assert!(PRONS.iter().any(|(w, _)| w == k), "missing PRONS for {k}");
            }
        }
        for (_, p) in PRONS {
            for tok in p.split_whitespace() {
                assert!(tok.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
                        "bad ARPAbet token {tok} in {p}");
            }
        }
    }
}
