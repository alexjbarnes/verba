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

/// The dictionary key a pseudo-key ("read1") is stored/looked-up under. The
/// piper-plus-g2p tokenizer STRIPS digits from words, so "read1" would resolve
/// to "read" (the dict default) and silently defeat the whole override. Map the
/// trailing digit to letters the tokenizer keeps, giving a distinct pure-alpha
/// key. MUST be used at BOTH dict insertion and lookup.
pub fn dict_key(pseudo: &str) -> String {
    pseudo.replace('1', "xaa").replace('2', "xbb")
}

/// Pseudo-key -> ARPAbet, merged into the phonemizer dictionary at load (under
/// `dict_key(pseudo)`, not the raw pseudo-key).
pub const PRONS: &[(&str, &str)] = &[
    ("read1", "R IY1 D"), ("read2", "R EH1 D"),
    ("live1", "L IH1 V"), ("live2", "L AY1 V"),
    ("lives1", "L IH1 V Z"), ("lives2", "L AY1 V Z"),
    ("lived1", "L IH1 V D"), ("lived2", "L AY1 V D"),
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
    ("intimate1", "IH1 N T AH0 M AH0 T"), ("intimate2", "IH1 N T AH0 M EY2 T"),
    ("animate1", "AE1 N AH0 M AH0 T"), ("animate2", "AE1 N AH0 M EY2 T"),
    ("articulate1", "AA0 R T IH1 K Y AH0 L AH0 T"), ("articulate2", "AA0 R T IH1 K Y AH0 L EY2 T"),
    ("subordinate1", "S AH0 B AO1 R D AH0 N AH0 T"), ("subordinate2", "S AH0 B AO1 R D AH0 N EY2 T"),
    // Noun/adj (key1, first-syllable stress) vs verb (key2, second-syllable
    // stress). Default is the noun/adj; verb fires on a to/modal/pronoun signal.
    ("object1", "AA1 B JH EH0 K T"), ("object2", "AH0 B JH EH1 K T"),
    ("subject1", "S AH1 B JH IH0 K T"), ("subject2", "S AH0 B JH EH1 K T"),
    ("project1", "P R AA1 JH EH0 K T"), ("project2", "P R AH0 JH EH1 K T"),
    ("projects1", "P R AA1 JH EH0 K T S"), ("projects2", "P R AH0 JH EH1 K T S"),
    ("contract1", "K AA1 N T R AE2 K T"), ("contract2", "K AH0 N T R AE1 K T"),
    ("conduct1", "K AA1 N D AH0 K T"), ("conduct2", "K AH0 N D AH1 K T"),
    ("conflict1", "K AA1 N F L IH0 K T"), ("conflict2", "K AH0 N F L IH1 K T"),
    ("contrast1", "K AA1 N T R AE0 S T"), ("contrast2", "K AH0 N T R AE1 S T"),
    ("increase1", "IH1 N K R IY0 S"), ("increase2", "IH0 N K R IY1 S"),
    ("decrease1", "D IY1 K R IY0 S"), ("decrease2", "D IH0 K R IY1 S"),
    ("permit1", "P ER1 M IH0 T"), ("permit2", "P ER0 M IH1 T"),
    ("progress1", "P R AA1 G R EH2 S"), ("progress2", "P R AH0 G R EH1 S"),
    ("protest1", "P R OW1 T EH2 S T"), ("protest2", "P R AH0 T EH1 S T"),
    ("rebel1", "R EH1 B AH0 L"), ("rebel2", "R IH0 B EH1 L"),
    ("refund1", "R IY1 F AH0 N D"), ("refund2", "R IH0 F AH1 N D"),
    ("contest1", "K AA1 N T EH0 S T"), ("contest2", "K AH0 N T EH1 S T"),
    ("convert1", "K AA1 N V ER0 T"), ("convert2", "K AH0 N V ER1 T"),
    ("export1", "EH1 K S P AO0 R T"), ("export2", "IH0 K S P AO1 R T"),
    ("exports1", "EH1 K S P AO0 R T S"), ("exports2", "IH0 K S P AO1 R T S"),
    ("insult1", "IH1 N S AH0 L T"), ("insult2", "IH0 N S AH1 L T"),
    ("suspect1", "S AH1 S P EH0 K T"), ("suspect2", "S AH0 S P EH1 K T"),
    ("survey1", "S ER1 V EY0"), ("survey2", "S ER0 V EY1"),
    ("transport1", "T R AE1 N S P AO0 R T"), ("transport2", "T R AE0 N S P AO1 R T"),
    ("console1", "K AA1 N S OW0 L"), ("console2", "K AH0 N S OW1 L"),
    ("compound1", "K AA1 M P AW0 N D"), ("compound2", "K AH0 M P AW1 N D"),
    ("torment1", "T AO1 R M EH2 N T"), ("torment2", "T AO0 R M EH1 N T"),
    ("convict1", "K AA1 N V IH0 K T"), ("convict2", "K AH0 N V IH1 K T"),
    ("discount1", "D IH1 S K AW0 N T"), ("discount2", "D IH0 S K AW1 N T"),
    ("desert1", "D EH1 Z ER0 T"), ("desert2", "D IH0 Z ER1 T"),
    ("attribute1", "AE1 T R AH0 B Y UW2 T"), ("attribute2", "AH0 T R IH1 B Y UW0 T"),
    // Special defaults (see wants_alt for the flip condition):
    ("content1", "K AA1 N T EH0 N T"), ("content2", "K AH0 N T EH1 N T"),
    ("perfect1", "P ER1 F IH0 K T"), ("perfect2", "P ER0 F EH1 K T"),
    ("invalid1", "IH0 N V AE1 L AH0 D"), ("invalid2", "IH1 N V AH0 L AH0 D"),
    ("combine1", "K AH0 M B AY1 N"), ("combine2", "K AA1 M B AY0 N"),
    ("resume1", "R IH0 Z UW1 M"), ("resume2", "R EH1 Z AH0 M EY2"),
    // Voicing pairs: noun /s/ (key1) vs verb /z/ (key2).
    ("house1", "HH AW1 S"), ("house2", "HH AW1 Z"),
    ("abuse1", "AH0 B Y UW1 S"), ("abuse2", "AH0 B Y UW1 Z"),
    ("excuse1", "IH0 K S K Y UW1 S"), ("excuse2", "IH0 K S K Y UW1 Z"),
];

/// Words whose noun/adjective form (default) flips to a verb reading after a
/// to/modal/nominative-pronoun signal.
const ATE_FAMILY: &[&str] = &[
    "separate", "estimate", "estimates", "graduate", "graduates", "duplicate",
    "advocate", "advocates", "associate", "delegate", "moderate", "deliberate",
    "elaborate", "aggregate", "alternate", "appropriate", "approximate",
    "coordinate", "intimate", "animate", "articulate", "subordinate",
];

// Noun/adj (first-syllable stress) vs verb (second-syllable stress) pairs, all
// defaulting to the noun/adj and flipping to the verb on a to/modal/pronoun
// signal. (Words with a non-verb_signal rule are handled explicitly below.)
const STRESS_NV: &[&str] = &[
    "object", "subject", "project", "projects", "contract", "conduct",
    "conflict", "contrast", "increase", "decrease", "permit", "progress",
    "protest", "rebel", "refund", "contest", "convert", "export", "exports",
    "insult", "suspect", "survey", "transport", "console", "compound",
    "torment", "convict", "discount", "desert", "attribute",
];
// Content-adjective ("I'm content", "content with") signals.
const CONTENT_ADJ: &[&str] = &[
    "am", "is", "are", "was", "were", "be", "been", "feel", "feels", "felt",
    "seem", "seems", "seemed", "perfectly", "quite", "so", "very", "really",
    "not", "remain", "remains", "stay",
];
// Résumé (CV) signals before "resume".
const RESUME_CV: &[&str] = &[
    "a", "your", "my", "his", "her", "their", "our", "update", "updated",
    "submit", "attach", "attached", "send", "review", "strong", "impressive",
    "polished", "one-page",
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
        // Adjective /laɪvd/ only in compounds ("long-lived", "short-lived");
        // otherwise the past-tense verb /lɪvd/ ("he lived there").
        "lived" => has(&["long", "short", "high", "well", "shortest", "longest"], p1),
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
        // Noun/adj -> verb on a to/modal/pronoun signal (stress shift).
        w if STRESS_NV.contains(&w) => verb_signal(p1),
        // "content": default noun /ˈkɒntɛnt/; adjective /kənˈtɛnt/ after a
        // copula/degree word or before "with".
        "content" => has(CONTENT_ADJ, p1) || n1 == Some("with"),
        // "perfect": default adjective; verb /pərˈfɛkt/ on a verb signal.
        "perfect" => verb_signal(p1),
        // "invalid": always the adjective /ɪnˈvælɪd/ ("invalid input"). The
        // noun (a sick person) is archaic; not worth the false-positive risk.
        "invalid" => false,
        // "combine": default verb; noun (the farm machine) before "harvester".
        "combine" => n1 == Some("harvester"),
        // "resume": default verb (continue); résumé (CV) after a CV signal.
        "resume" => has(RESUME_CV, p1),
        // Voicing pairs: default noun /s/; verb /z/ on a verb signal.
        "house" | "abuse" => verb_signal(p1),
        "excuse" => verb_signal(p1) || n1 == Some("me") || n1 == Some("my"),
        w if ATE_FAMILY.contains(&w) => verb_signal(p1),
        _ => false,
    }
}

fn is_heteronym(word: &str) -> bool {
    matches!(word,
        "read" | "live" | "lives" | "lived" | "lead" | "record" | "records"
        | "present" | "presents" | "use" | "used" | "wound" | "tear" | "tears"
        | "wind" | "winds" | "close" | "minute" | "dove"
        | "content" | "perfect" | "invalid" | "combine" | "resume"
        | "house" | "abuse" | "excuse")
        || ATE_FAMILY.contains(&word)
        || STRESS_NV.contains(&word)
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
    ("lived", "lived1", "lived2"),
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
    ("intimate", "intimate1", "intimate2"),
    ("animate", "animate1", "animate2"),
    ("articulate", "articulate1", "articulate2"),
    ("subordinate", "subordinate1", "subordinate2"),
    ("object", "object1", "object2"),
    ("subject", "subject1", "subject2"),
    ("project", "project1", "project2"),
    ("projects", "projects1", "projects2"),
    ("contract", "contract1", "contract2"),
    ("conduct", "conduct1", "conduct2"),
    ("conflict", "conflict1", "conflict2"),
    ("contrast", "contrast1", "contrast2"),
    ("increase", "increase1", "increase2"),
    ("decrease", "decrease1", "decrease2"),
    ("permit", "permit1", "permit2"),
    ("progress", "progress1", "progress2"),
    ("protest", "protest1", "protest2"),
    ("rebel", "rebel1", "rebel2"),
    ("refund", "refund1", "refund2"),
    ("contest", "contest1", "contest2"),
    ("convert", "convert1", "convert2"),
    ("export", "export1", "export2"),
    ("exports", "exports1", "exports2"),
    ("insult", "insult1", "insult2"),
    ("suspect", "suspect1", "suspect2"),
    ("survey", "survey1", "survey2"),
    ("transport", "transport1", "transport2"),
    ("console", "console1", "console2"),
    ("compound", "compound1", "compound2"),
    ("torment", "torment1", "torment2"),
    ("convict", "convict1", "convict2"),
    ("discount", "discount1", "discount2"),
    ("desert", "desert1", "desert2"),
    ("attribute", "attribute1", "attribute2"),
    ("content", "content1", "content2"),
    ("perfect", "perfect1", "perfect2"),
    ("invalid", "invalid1", "invalid2"),
    ("combine", "combine1", "combine2"),
    ("resume", "resume1", "resume2"),
    ("house", "house1", "house2"),
    ("abuse", "abuse1", "abuse2"),
    ("excuse", "excuse1", "excuse2"),
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
    fn lives_verb_after_proper_noun() {
        // "where Storybook lives" — verb (/lɪvz/), no determiner/possessive.
        assert_eq!(keys_for("and where Storybook lives"), vec!["lives1"]);
        assert_eq!(keys_for("the town where she lives"), vec!["lives1"]);
    }

    #[test]
    fn lived_past_tense_vs_compound() {
        // Past tense /lɪvd/ by default; /laɪvd/ only in long-/short-lived.
        assert_eq!(keys_for("he lived life to its fullest"), vec!["lived1"]);
        assert_eq!(keys_for("and lived happily ever after"), vec!["lived1"]);
        assert_eq!(keys_for("a long lived tradition"), vec!["lived2"]);
        assert_eq!(keys_for("short lived fame"), vec!["lived2"]);
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
    fn stress_noun_verb_pairs() {
        // Default = noun (first-stress); verb (second-stress) on a verb signal.
        assert_eq!(keys_for("a survey of readers"), vec!["survey1"]);
        assert_eq!(keys_for("we survey the field"), vec!["survey2"]);
        assert_eq!(keys_for("the suspect fled"), vec!["suspect1"]);
        assert_eq!(keys_for("I suspect not"), vec!["suspect2"]);
        assert_eq!(keys_for("a big discount"), vec!["discount1"]);
        assert_eq!(keys_for("they discount the risk"), vec!["discount2"]);
        assert_eq!(keys_for("the object on the table"), vec!["object1"]);
        assert_eq!(keys_for("I object to that"), vec!["object2"]);
        assert_eq!(keys_for("a permit is required"), vec!["permit1"]);
        assert_eq!(keys_for("to permit access"), vec!["permit2"]);
        assert_eq!(keys_for("a modest increase"), vec!["increase1"]);
        assert_eq!(keys_for("we increase output"), vec!["increase2"]);
        assert_eq!(keys_for("the subject of the email"), vec!["subject1"]);
    }

    #[test]
    fn special_default_heteronyms() {
        // perfect: adjective by default, verb on a signal.
        assert_eq!(keys_for("a perfect day"), vec!["perfect1"]);
        assert_eq!(keys_for("to perfect the craft"), vec!["perfect2"]);
        // invalid: always the adjective.
        assert_eq!(keys_for("an invalid argument"), vec!["invalid1"]);
        assert_eq!(keys_for("the invalid input"), vec!["invalid1"]);
        // content: noun by default, adjective after a copula / before "with".
        assert_eq!(keys_for("the content of the page"), vec!["content1"]);
        assert_eq!(keys_for("I am content with this"), vec!["content2"]);
        // combine: verb by default; noun before "harvester".
        assert_eq!(keys_for("combine the ingredients"), vec!["combine1"]);
        assert_eq!(keys_for("a combine harvester"), vec!["combine2"]);
        // resume: continue by default; CV after a signal.
        assert_eq!(keys_for("resume the meeting"), vec!["resume1"]);
        assert_eq!(keys_for("attach your resume"), vec!["resume2"]);
        // excuse / house voicing.
        assert_eq!(keys_for("excuse me"), vec!["excuse2"]);
        assert_eq!(keys_for("a poor excuse"), vec!["excuse1"]);
        assert_eq!(keys_for("to house refugees"), vec!["house2"]);
        assert_eq!(keys_for("the house on the hill"), vec!["house1"]);
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
        assert_eq!(keys_for("wind down the road"), vec!["wind2"]);
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
        let prons: std::collections::HashMap<&str, &str> = PRONS.iter().copied().collect();
        for (word, k1, k2) in KEYS {
            let a = prons.get(k1).unwrap_or_else(|| panic!("missing PRONS for {k1}"));
            let b = prons.get(k2).unwrap_or_else(|| panic!("missing PRONS for {k2}"));
            // The two readings must actually differ, or the override is a no-op.
            assert_ne!(a, b, "'{word}': {k1} and {k2} have identical ARPAbet");
        }
        for (_, p) in PRONS {
            for tok in p.split_whitespace() {
                assert!(tok.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
                        "bad ARPAbet token {tok} in {p}");
            }
        }
    }
}
