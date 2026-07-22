//! Stage 3: Inverse Text Normalization (ITN).
//!
//! Converts spoken forms to written forms:
//! - Numbers: "twenty three" -> "23"
//! - Currency: "twenty three dollars" -> "$23"
//! - Dates: "january fifth" -> "January 5"
//! - Common abbreviations: "mister" -> "Mr."

use std::collections::HashMap;
use std::sync::LazyLock;

/// Word-to-number mapping for cardinal numbers.
static CARDINALS: LazyLock<HashMap<&'static str, u64>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("zero", 0);
    m.insert("one", 1);
    m.insert("two", 2);
    m.insert("three", 3);
    m.insert("four", 4);
    m.insert("five", 5);
    m.insert("six", 6);
    m.insert("seven", 7);
    m.insert("eight", 8);
    m.insert("nine", 9);
    m.insert("ten", 10);
    m.insert("eleven", 11);
    m.insert("twelve", 12);
    m.insert("thirteen", 13);
    m.insert("fourteen", 14);
    m.insert("fifteen", 15);
    m.insert("sixteen", 16);
    m.insert("seventeen", 17);
    m.insert("eighteen", 18);
    m.insert("nineteen", 19);
    m.insert("twenty", 20);
    m.insert("thirty", 30);
    m.insert("forty", 40);
    m.insert("fifty", 50);
    m.insert("sixty", 60);
    m.insert("seventy", 70);
    m.insert("eighty", 80);
    m.insert("ninety", 90);
    m
});

static MULTIPLIERS: LazyLock<HashMap<&'static str, u64>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("hundred", 100);
    m.insert("thousand", 1_000);
    m.insert("million", 1_000_000);
    m.insert("billion", 1_000_000_000);
    m
});

static ORDINALS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("first", "1st");
    m.insert("second", "2nd");
    m.insert("third", "3rd");
    m.insert("fourth", "4th");
    m.insert("fifth", "5th");
    m.insert("sixth", "6th");
    m.insert("seventh", "7th");
    m.insert("eighth", "8th");
    m.insert("ninth", "9th");
    m.insert("tenth", "10th");
    m.insert("eleventh", "11th");
    m.insert("twelfth", "12th");
    m.insert("thirteenth", "13th");
    m.insert("fourteenth", "14th");
    m.insert("fifteenth", "15th");
    m.insert("sixteenth", "16th");
    m.insert("seventeenth", "17th");
    m.insert("eighteenth", "18th");
    m.insert("nineteenth", "19th");
    m.insert("twentieth", "20th");
    m.insert("thirtieth", "30th");
    m
});

static MONTHS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("january", "January");
    m.insert("february", "February");
    m.insert("march", "March");
    m.insert("april", "April");
    m.insert("may", "May");
    m.insert("june", "June");
    m.insert("july", "July");
    m.insert("august", "August");
    m.insert("september", "September");
    m.insert("october", "October");
    m.insert("november", "November");
    m.insert("december", "December");
    m
});

static ABBREVIATIONS: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    m.insert("mister", "Mr.");
    m.insert("missus", "Mrs.");
    m.insert("miss", "Ms.");
    m.insert("doctor", "Dr.");
    m.insert("professor", "Prof.");
    m.insert("versus", "vs.");
    m.insert("etcetera", "etc.");
    m.insert("et cetera", "etc.");
    m
});

/// Apply inverse text normalization to convert spoken forms to written forms.
pub fn normalize(text: &str) -> String {
    let mut result = text.to_string();

    // Multi-word abbreviations first
    result = replace_abbreviation(&result, "et cetera", "etc.");

    // Process word by word with lookahead for number sequences
    let words: Vec<&str> = result.split_whitespace().collect();
    let mut output: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;

    while i < words.len() {
        let lower = words[i].to_ascii_lowercase();
        let bare = lower.trim_matches(|c: char| c.is_ascii_punctuation());

        // Try to parse a number sequence starting here
        if let Some((num_str, consumed)) = try_parse_number_sequence(&words, i) {
            // Standalone "one" is almost always a pronoun or an idiom in
            // speech ("this one", "one by one", "at one point"), not a
            // numeral — converting it is the pipeline's most common ITN
            // error. Convert it only when a neighbouring word is numeric
            // too (a counting sequence like "one two three"); compounds
            // ("twenty one", "one hundred") consume more than one word and
            // never reach this guard.
            if consumed == 1 && bare == "one" && !numeric_neighbour(&words, i) {
                output.push(words[i].to_string());
                i += 1;
                continue;
            }

            let last_punct = trailing_punct(words[i + consumed - 1]);

            // Check for currency/percent suffix (only when number has no trailing punct)
            let next = if i + consumed < words.len() {
                words[i + consumed].to_ascii_lowercase()
            } else {
                String::new()
            };
            let next_bare = next.trim_matches(|c: char| c.is_ascii_punctuation());

            match next_bare {
                "dollars" | "dollar" if last_punct.is_empty() => {
                    let suffix = trailing_punct(words[i + consumed]);
                    output.push(format!("${num_str}{suffix}"));
                    i += consumed + 1;
                }
                "percent" if last_punct.is_empty() => {
                    let suffix = trailing_punct(words[i + consumed]);
                    output.push(format!("{num_str}%{suffix}"));
                    i += consumed + 1;
                }
                _ => {
                    output.push(format!("{num_str}{last_punct}"));
                    i += consumed;
                }
            }
            continue;
        }

        // Single-word abbreviations
        if let Some(abbr) = ABBREVIATIONS.get(bare) {
            output.push(abbr.to_string());
            i += 1;
            continue;
        }

        // Month + ordinal: "january fifth" -> "January 5"
        if let Some(month) = MONTHS.get(bare) {
            if i + 1 < words.len() {
                let next_lower = words[i + 1].to_ascii_lowercase();
                let next_bare = next_lower.trim_matches(|c: char| c.is_ascii_punctuation());
                if let Some(ord) = ORDINALS.get(next_bare) {
                    let num: String = ord.chars().take_while(|c| c.is_ascii_digit()).collect();
                    let punct = trailing_punct(words[i + 1]);
                    output.push(format!("{month} {num}{punct}"));
                    i += 2;
                    continue;
                }
            }
            let punct = trailing_punct(words[i]);
            output.push(format!("{month}{punct}"));
            i += 1;
            continue;
        }

        output.push(words[i].to_string());
        i += 1;
    }

    output.join(" ")
}

/// Try to parse a sequence of number words starting at index `start`.
/// Returns (formatted_number, words_consumed) or None.
fn try_parse_number_sequence(words: &[&str], start: usize) -> Option<(String, usize)> {
    let mut value: u64 = 0;
    let mut current: u64 = 0;
    let mut consumed = 0;
    let mut found_any = false;
    let mut last_cardinal: u64 = 0;

    let mut i = start;
    while i < words.len() {
        let lower = words[i].to_ascii_lowercase();
        let bare = lower.trim_matches(|c: char| c.is_ascii_punctuation());

        if let Some(&n) = CARDINALS.get(bare) {
            // Only allow adding to current when it makes sense as a
            // compound number: units (1-9) after tens (20-90).
            // "twenty three" -> 23 (good: 3 after 20)
            // "nineteen eighty" -> break (bad: 80 after 19, not a compound)
            if found_any && (n >= 10 || last_cardinal < 20) && last_cardinal != 0 {
                // Two non-combinable cardinals in a row. Stop here.
                // Exception: units after a tens (e.g. "twenty" + "three")
                if !(last_cardinal >= 20 && last_cardinal <= 90 && n < 10) {
                    break;
                }
            }
            current += n;
            last_cardinal = n;
            found_any = true;
            consumed = i - start + 1;
            i += 1;
            // Trailing punctuation (e.g. "one," in a list) ends the sequence
            if !trailing_punct(words[i - 1]).is_empty() {
                break;
            }
        } else if let Some(&mult) = MULTIPLIERS.get(bare) {
            if !found_any {
                break;
            }
            if mult >= 1000 {
                current = if current == 0 { 1 } else { current };
                value += current * mult;
                current = 0;
            } else {
                current = if current == 0 { 1 } else { current };
                current *= mult;
            }
            last_cardinal = 0;
            consumed = i - start + 1;
            i += 1;
            if !trailing_punct(words[i - 1]).is_empty() {
                break;
            }
        } else if bare == "and" && found_any && i + 1 < words.len() {
            // "one hundred and twenty" - skip "and"
            let next_lower = words[i + 1].to_ascii_lowercase();
            let next_bare = next_lower.trim_matches(|c: char| c.is_ascii_punctuation());
            if CARDINALS.contains_key(next_bare) {
                i += 1;
                continue;
            }
            break;
        } else {
            break;
        }
    }

    if found_any {
        value += current;
        Some((value.to_string(), consumed))
    } else {
        None
    }
}

/// Whether the word before or after position `i` is itself numeric — a
/// cardinal/multiplier word or a token containing a digit.
fn numeric_neighbour(words: &[&str], i: usize) -> bool {
    let is_numeric = |w: &str| {
        let lower = w.to_ascii_lowercase();
        let bare = lower.trim_matches(|c: char| c.is_ascii_punctuation());
        CARDINALS.contains_key(bare)
            || MULTIPLIERS.contains_key(bare)
            || bare.chars().any(|c| c.is_ascii_digit())
    };
    (i > 0 && is_numeric(words[i - 1])) || (i + 1 < words.len() && is_numeric(words[i + 1]))
}

/// Return the trailing ASCII punctuation of a word, if any.
fn trailing_punct(word: &str) -> &str {
    let trimmed = word.trim_end_matches(|c: char| c.is_ascii_punctuation());
    &word[trimmed.len()..]
}

fn replace_abbreviation(text: &str, phrase: &str, replacement: &str) -> String {
    let lower = text.to_lowercase();
    let mut result = String::with_capacity(text.len());
    let mut pos = 0;

    while let Some(idx) = lower[pos..].find(phrase) {
        let abs_idx = pos + idx;
        result.push_str(&text[pos..abs_idx]);
        result.push_str(replacement);
        pos = abs_idx + phrase.len();
    }
    result.push_str(&text[pos..]);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_number() {
        assert_eq!(normalize("twenty three"), "23");
    }

    #[test]
    fn number_with_hundred() {
        assert_eq!(normalize("one hundred twenty three"), "123");
    }

    #[test]
    fn currency() {
        assert_eq!(normalize("twenty three dollars"), "$23");
    }

    #[test]
    fn percent() {
        assert_eq!(normalize("fifty percent"), "50%");
    }

    #[test]
    fn date() {
        assert_eq!(normalize("january fifth"), "January 5");
    }

    #[test]
    fn abbreviation() {
        assert_eq!(normalize("mister smith"), "Mr. smith");
    }

    #[test]
    fn ordinal_not_converted_standalone() {
        assert_eq!(normalize("do this first"), "do this first");
        assert_eq!(normalize("the third item"), "the third item");
    }

    #[test]
    fn mixed_text() {
        assert_eq!(
            normalize("I paid twenty three dollars for three items"),
            "I paid $23 for 3 items"
        );
    }

    #[test]
    fn no_numbers() {
        assert_eq!(normalize("hello world"), "hello world");
    }

    #[test]
    fn thousand() {
        assert_eq!(normalize("two thousand twenty six"), "2026");
    }

    #[test]
    fn large_number() {
        assert_eq!(normalize("one million"), "1000000");
    }

    #[test]
    fn comma_separated_list() {
        assert_eq!(normalize("one, two, three"), "1, 2, 3");
    }

    #[test]
    fn counting_sequence_not_summed() {
        assert_eq!(
            normalize("one, two, three, four, five, six, seven, eight, nine, ten"),
            "1, 2, 3, 4, 5, 6, 7, 8, 9, 10"
        );
    }

    #[test]
    fn trailing_period_preserved() {
        assert_eq!(normalize("I have two."), "I have 2.");
    }

    #[test]
    fn standalone_one_not_converted() {
        assert_eq!(
            normalize("can you talk me through this one please?"),
            "can you talk me through this one please?"
        );
        assert_eq!(
            normalize("I can kill that one, that doesn't matter."),
            "I can kill that one, that doesn't matter."
        );
        assert_eq!(normalize("we did it one by one"), "we did it one by one");
        assert_eq!(
            normalize("at one point we were creating one"),
            "at one point we were creating one"
        );
        assert_eq!(normalize("one thing I don't understand"), "one thing I don't understand");
        assert_eq!(normalize("every single one"), "every single one");
    }

    #[test]
    fn one_converts_in_counting_and_compounds() {
        assert_eq!(normalize("testing one two three"), "testing 1 2 3");
        assert_eq!(normalize("twenty one"), "21");
        assert_eq!(normalize("one hundred twenty three"), "123");
        assert_eq!(normalize("one thousand"), "1000");
    }

    #[test]
    fn standalone_one_currency_stays_spoken() {
        // Consequence of the standalone-"one" guard, and matches written
        // style anyway: spell out one, digits from two upwards.
        assert_eq!(normalize("it costs one dollar"), "it costs one dollar");
        assert_eq!(normalize("it costs two dollars"), "it costs $2");
    }

    #[test]
    fn number_then_comma_then_text() {
        assert_eq!(
            normalize("I bought three, maybe four items"),
            "I bought 3, maybe 4 items"
        );
    }

    #[test]
    fn compound_still_works() {
        // No punctuation between words: compound number
        assert_eq!(normalize("twenty three"), "23");
        assert_eq!(normalize("one hundred twenty three"), "123");
        assert_eq!(normalize("two thousand twenty six"), "2026");
    }

    #[test]
    fn currency_with_trailing_punct() {
        assert_eq!(normalize("twenty three dollars."), "$23.");
    }

    #[test]
    fn ordinal_preserved_with_trailing_punct() {
        assert_eq!(normalize("the third, fourth, and fifth"), "the third, fourth, and fifth");
    }

    #[test]
    fn date_ordinal_still_works() {
        assert_eq!(normalize("march third"), "March 3");
    }

    #[test]
    fn year_not_summed() {
        // "nineteen eighty nine" should NOT become 108 (19+80+9).
        // "eighty nine" correctly compounds to 89, but "nineteen" stays separate.
        assert_eq!(
            normalize("first of may nineteen eighty nine"),
            "first of May 19 89"
        );
    }

    #[test]
    fn consecutive_cardinals_not_summed() {
        // Two separate numbers, not a compound
        assert_eq!(normalize("five ten"), "5 10");
        assert_eq!(normalize("twelve fifteen"), "12 15");
    }
}
