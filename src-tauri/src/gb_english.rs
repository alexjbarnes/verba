//! British English (RP) fallback transform, espeak-free.
//!
//! GB-locale Piper voices (espeak voice "en-gb*") get their pronunciations
//! from the bundled GB dictionary (data/gb_dict.json, wikipron-derived, with
//! stress transferred from CMUdict). This module handles the words that
//! dictionary lacks — tech terms, proper nouns, glued compounds — by taking
//! the US phonemizer's IPA output and rewriting it into the espeak
//! en-gb-x-rp conventions the voice was trained on (verified against espeak
//! output offline; espeak itself is never shipped or linked).
//!
//! Kept dependency-free (std only) so `src/bin/gb_probe.rs` can include and
//! RUN it on the host, where the full library cannot link (sherpa espeak
//! symbols).

/// First codepoints that start a vowel in the g2p/espeak IPA inventory.
fn is_vowel_char(c: char) -> bool {
    matches!(
        c,
        'ɑ' | 'æ' | 'ʌ' | 'ə' | 'ɐ' | 'ɔ' | 'a' | 'ɛ' | 'ɜ' | 'e' | 'ɪ' | 'i' | 'ɒ' | 'o'
            | 'ʊ' | 'u' | 'ɚ'
    )
}

fn is_stress(c: char) -> bool {
    c == 'ˈ' || c == 'ˌ'
}

/// Rewrite one word's US IPA (piper-plus-g2p output, single-codepoint tokens,
/// stress marks inline) into RP. Returns single-codepoint tokens.
///
/// Rules, in order (all "ɹ" rules apply only when the ɹ is NOT prevocalic —
/// linking r inside a word, as in "starring", stays):
///   ɑːɹ -> ɑː   ɔːɹ -> ɔː   ɜːɹ -> ɜː  (START/NORTH/NURSE)
///   iːɹ/ɪɹ -> iə (NEAR)   ɛɹ -> eə (SQUARE)   uːɹ/ʊɹ -> ɔː (CURE merger)
///   əɹ -> ə   other Vɹ -> V   (lettER, leftovers)
///   ɚ -> ə   oʊ -> əʊ (GOAT)   bare ɑ -> ɒ (LOT)
///   word-final ə -> ɐ (unless a centring-diphthong tail)
///   word-final unstressed iː -> ɪ (happY)
pub fn us_to_rp(tokens: Vec<String>) -> Vec<String> {
    rp_transform(tokens, true)
}

/// `us_to_rp` for letter-by-letter spellings ("CVE", "LLM"): the per-char
/// rules still apply (R's rhotic vowel, O's GOAT diphthong), but the
/// word-final adjustments must not — they would clip the last letter's name
/// ("CVE" -> "see-vee-ih", "ABCs" -> "ay-bee-sih-z"), because letter names
/// carry no stress marks for the happY rule's stressed-FLEECE escape to see.
pub fn us_to_rp_spelled(tokens: Vec<String>) -> Vec<String> {
    rp_transform(tokens, false)
}

fn rp_transform(tokens: Vec<String>, word_final_adjust: bool) -> Vec<String> {
    let joined: String = tokens.concat();
    let chars: Vec<char> = joined.chars().collect();
    let n = chars.len();

    // Is the char at `i` (an ɹ) followed by a vowel, skipping stress marks?
    let prevocalic = |i: usize| -> bool {
        let mut j = i + 1;
        while j < n && is_stress(chars[j]) {
            j += 1;
        }
        j < n && is_vowel_char(chars[j])
    };

    let mut out: Vec<char> = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c == 'ɹ' && !prevocalic(i) {
            // Non-rhotic context: fold the ɹ into the preceding vowel.
            match out.last().copied() {
                Some('ː') => {
                    // ɑːɹ/ɔːɹ/ɜːɹ -> drop ɹ. iːɹ -> iə. uːɹ -> ɔː (CURE).
                    let v = out.get(out.len().wrapping_sub(2)).copied();
                    if v == Some('i') {
                        out.pop();
                        out.push('ə');
                    } else if v == Some('u') {
                        out.pop();
                        out.pop();
                        out.push('ɔ');
                        out.push('ː');
                    }
                }
                Some('ɪ') => {
                    out.pop();
                    out.push('i');
                    out.push('ə');
                }
                Some('ɛ') => {
                    out.pop();
                    out.push('e');
                    out.push('ə');
                }
                Some('ʊ') => {
                    // pˈʊɹ (CURE) -> pˈɔː, but only when the ʊ starts the
                    // vowel. In aʊɹ ("Naur", "sour") the ʊ is the MOUTH
                    // diphthong's tail: centring ə instead (naʊə), or the
                    // merger would invent aɔː.
                    let tail_of_diphthong =
                        out.len() >= 2 && is_vowel_char(out[out.len() - 2]);
                    if tail_of_diphthong {
                        out.push('ə');
                    } else {
                        out.pop();
                        out.push('ɔ');
                        out.push('ː');
                    }
                }
                _ => {} // əɹ and any other Vɹ: just drop the ɹ
            }
            i += 1;
            continue;
        }
        match c {
            // R-coloured schwa. Before a vowel RP keeps a linking /r/
            // ("generated" dʒˈɛnɚˌeɪtəd -> dʒˈɛnəɹˌeɪtəd, "trickery" -> tɹˈɪkəɹɪ);
            // word-finally or before a consonant the r folds away ("letter" ->
            // lˈɛtə). The standalone-ɹ rules above already gate on prevocalic;
            // this arm was dropping the r unconditionally.
            'ɚ' if prevocalic(i) => {
                out.push('ə');
                out.push('ɹ');
            }
            // A STRESSED r-coloured schwa is the NURSE vowel (the g2p maps
            // ER1 to ɜː itself but leaves ER2 as ˌɚ): "gigahertz" ended
            // ˌɚts and folded to a weak "huhts" instead of "hurts".
            'ɚ' if out.last().is_some_and(|&b| is_stress(b)) => {
                out.push('ɜ');
                out.push('ː');
            }
            'ɚ' => out.push('ə'),
            'o' if i + 1 < n && chars[i + 1] == 'ʊ' => {
                out.push('ə');
                out.push('ʊ');
                i += 2;
                continue;
            }
            'ɑ' if !(i + 1 < n && chars[i + 1] == 'ː') => out.push('ɒ'),
            _ => out.push(c),
        }
        i += 1;
    }

    // Word-final adjustments.
    if word_final_adjust {
        let m = out.len();
        if m >= 1 && out[m - 1] == 'ə' && !(m >= 2 && is_vowel_char(out[m - 2])) {
            out[m - 1] = 'ɐ';
        } else if m >= 2 && out[m - 1] == 'ː' && out[m - 2] == 'i' {
            let stressed = m >= 3 && is_stress(out[m - 3]);
            if !stressed {
                out.truncate(m - 2);
                out.push('ɪ');
            }
        }
    }

    out.into_iter().map(|c| c.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rp(s: &str) -> String {
        us_to_rp(s.chars().map(|c| c.to_string()).collect()).concat()
    }

    #[test]
    fn non_rhotic_sets() {
        assert_eq!(rp("stˈɑːɹ"), "stˈɑː"); // START
        assert_eq!(rp("nˈɔːɹθ"), "nˈɔːθ"); // NORTH
        assert_eq!(rp("lˈɛtɚ"), "lˈɛtɐ"); // lettER -> final ɐ
        assert_eq!(rp("nˈɪɹ"), "nˈiə"); // NEAR
        assert_eq!(rp("skwˈɛɹ"), "skwˈeə"); // SQUARE
        assert_eq!(rp("pˈʊɹ"), "pˈɔː"); // CURE merger
    }

    #[test]
    fn linking_r_stays() {
        assert_eq!(rp("stˈɑːɹɪŋ"), "stˈɑːɹɪŋ"); // starring
    }

    #[test]
    fn goat_and_lot() {
        assert_eq!(rp("ɡˈoʊ"), "ɡˈəʊ"); // GOAT
        assert_eq!(rp("hˈɑt"), "hˈɒt"); // LOT
        assert_eq!(rp("fˈɑːðɚ"), "fˈɑːðɐ"); // PALM keeps ɑː
    }

    #[test]
    fn happy_tensing_reversed() {
        assert_eq!(rp("hˈæpiː"), "hˈæpɪ"); // final unstressed iː
        assert_eq!(rp("fɹˈiː"), "fɹˈiː"); // stressed FLEECE stays
    }

    #[test]
    fn final_schwa_not_after_vowel() {
        assert_eq!(rp("faɪɚ"), "faɪə"); // fire: diphthong tail stays ə
    }

    #[test]
    fn spelled_letters_keep_final_fleece() {
        // Letter-by-letter spellings must not happY-clip the last letter's
        // name: "CVE" is see-vee-EE, not see-vee-ih (reported 2026-07-11).
        let spell = |s: &str| {
            us_to_rp_spelled(s.chars().map(|c| c.to_string()).collect()).concat()
        };
        assert_eq!(spell("siːviːiː"), "siːviːiː"); // CVE
        assert_eq!(spell("ɛlɛlɛm"), "ɛlɛlɛm"); // LLM (no final iː: unchanged)
        assert_eq!(spell("ɑːɹdʒiː"), "ɑːdʒiː"); // RG: rhotic fold still applies
        // The word path still clips (happY) — the two must stay different.
        assert_eq!(rp("hˈæpiː"), "hˈæpɪ");
    }

    #[test]
    fn linking_r_in_rhotic_schwa() {
        // ɚ before a vowel keeps the RP linking /r/ (was dropped, giving
        // "gen-uh-ated", "trick-uh-ee", "sep-uh-ate").
        assert_eq!(rp("dʒˈɛnɚˌeɪtəd"), "dʒˈɛnəɹˌeɪtəd"); // generated
        assert_eq!(rp("tɹˈɪkɚiː"), "tɹˈɪkəɹɪ"); // trickery (+ happY tensing)
        assert_eq!(rp("sˈɛpɚət"), "sˈɛpəɹət"); // separate (adjective)
        // Word-final / preconsonantal ɚ still folds to a plain schwa.
        assert_eq!(rp("lˈɛtɚ"), "lˈɛtɐ"); // letter
        assert_eq!(rp("fˈɑːðɚz"), "fˈɑːðəz"); // fathers: ɚ before consonant
    }
}
