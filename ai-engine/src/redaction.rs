// SPDX-License-Identifier: BUSL-1.1

use std::collections::HashMap;
use zeroize::Zeroize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityKind {
    Email,
    Phone,
    Iban,
    Card,
    Ipv4,
    Ipv6,
    Uuid,
    Secret,
    Path,
    Url,
    Term,
}

impl EntityKind {
    fn label(self) -> &'static str {
        match self {
            EntityKind::Email => "EMAIL",
            EntityKind::Phone => "PHONE",
            EntityKind::Iban => "IBAN",
            EntityKind::Card => "CARD",
            EntityKind::Ipv4 => "IP",
            EntityKind::Ipv6 => "IP",
            EntityKind::Uuid => "ID",
            EntityKind::Secret => "SECRET",
            EntityKind::Path => "PATH",
            EntityKind::Url => "URL",
            EntityKind::Term => "NAME",
        }
    }
}

#[derive(Default)]
pub struct Redactor {
    forward: HashMap<String, String>,
    reverse: HashMap<String, String>,
    counters: HashMap<&'static str, u32>,
    terms: Vec<String>,
}

impl Drop for Redactor {
    fn drop(&mut self) {
        for (mut k, mut v) in self.forward.drain() {
            k.zeroize();
            v.zeroize();
        }
        for (mut k, mut v) in self.reverse.drain() {
            k.zeroize();
            v.zeroize();
        }
        for t in self.terms.iter_mut() {
            t.zeroize();
        }
    }
}

const TRIM_LEAD: &[char] = &['(', '[', '{', '<', '"', '\'', '`', '¿', '¡'];
const TRIM_TAIL: &[char] = &[
    '.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\'', '`',
];

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_terms<I: IntoIterator<Item = String>>(terms: I) -> Self {
        let mut r = Self::new();
        r.terms = terms
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| t.chars().count() >= 2)
            .collect();
        r.terms.sort_by_key(|t| std::cmp::Reverse(t.len()));
        r
    }

    pub fn entity_count(&self) -> usize {
        self.forward.len()
    }

    fn placeholder_for(&mut self, value: &str, kind: EntityKind) -> String {
        if let Some(p) = self.forward.get(value) {
            return p.clone();
        }
        let label = kind.label();
        let n = self.counters.entry(label).or_insert(0);
        *n += 1;
        let placeholder = format!("[[{label}_{n}]]");
        self.forward.insert(value.to_string(), placeholder.clone());
        self.reverse.insert(placeholder.clone(), value.to_string());
        placeholder
    }

    pub fn redact(&mut self, text: &str) -> String {
        let after_terms = self.redact_terms(text);
        let after_groups = self.redact_grouped_numbers(&after_terms);
        self.redact_words(&after_groups)
    }

    fn redact_terms(&mut self, text: &str) -> String {
        if self.terms.is_empty() {
            return text.to_string();
        }
        let mut out = text.to_string();
        let terms = self.terms.clone();
        for term in terms {
            let lower_term = term.to_lowercase();
            let mut result = String::with_capacity(out.len());
            let mut rest = out.as_str();
            loop {
                let lower_rest = rest.to_lowercase();
                if lower_rest.len() != rest.len() {
                    result.push_str(rest);
                    break;
                }
                let Some(pos) = lower_rest.find(&lower_term) else {
                    result.push_str(rest);
                    break;
                };
                let end = pos + lower_term.len();
                let before_ok = rest[..pos]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_alphanumeric());
                let after_ok = rest[end..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric());
                result.push_str(&rest[..pos]);
                if before_ok && after_ok {
                    let original = rest[pos..end].to_string();
                    let key = original.to_lowercase();
                    let placeholder = self.placeholder_for(&key, EntityKind::Term);
                    result.push_str(&placeholder);
                } else {
                    result.push_str(&rest[pos..end]);
                }
                rest = &rest[end..];
            }
            out = result;
        }
        out
    }

    fn redact_grouped_numbers(&mut self, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0usize;
        while i < chars.len() {
            let boundary = i == 0 || !chars[i - 1].is_alphanumeric();
            if boundary {
                if let Some((end, kind)) = match_grouped(&chars, i) {
                    let value: String = chars[i..end].iter().collect();
                    let placeholder = self.placeholder_for(value.trim(), kind);
                    out.push_str(&placeholder);
                    i = end;
                    continue;
                }
            }
            out.push(chars[i]);
            i += 1;
        }
        out
    }

    fn redact_words(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut word = String::new();
        for c in text.chars() {
            if c.is_whitespace() {
                self.flush_word(&mut word, &mut out);
                out.push(c);
            } else {
                word.push(c);
            }
        }
        self.flush_word(&mut word, &mut out);
        out
    }

    fn flush_word(&mut self, word: &mut String, out: &mut String) {
        if word.is_empty() {
            return;
        }
        let lead_len: usize = word
            .chars()
            .take_while(|c| TRIM_LEAD.contains(c))
            .map(char::len_utf8)
            .sum();
        let core_and_tail = &word[lead_len..];
        let tail_len: usize = core_and_tail
            .chars()
            .rev()
            .take_while(|c| TRIM_TAIL.contains(c))
            .map(char::len_utf8)
            .sum();
        let core_end = core_and_tail.len() - tail_len;
        let core = &core_and_tail[..core_end];
        match classify(core) {
            Some(kind) if !core.starts_with("[[") => {
                let placeholder = self.placeholder_for(core, kind);
                out.push_str(&word[..lead_len]);
                out.push_str(&placeholder);
                out.push_str(&core_and_tail[core_end..]);
            }
            _ => out.push_str(word),
        }
        word.clear();
    }

    /// Puts the real values back into an answer. A placeholder this
    /// redactor never issued, written by a model on its own, becomes a
    /// plain slot (see [`plain_slots`]).
    pub fn restore(&self, text: &str) -> String {
        if !text.contains("[[") {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("[[") {
            out.push_str(&rest[..start]);
            let after = &rest[start..];
            match after.find("]]") {
                Some(end) if end <= 24 => {
                    let token = &after[..end + 2];
                    match self.reverse.get(token) {
                        Some(original) => out.push_str(original),
                        None => match plain_slot(token) {
                            Some(slot) => out.push_str(&slot),
                            None => out.push_str(token),
                        },
                    }
                    rest = &after[end + 2..];
                }
                _ => {
                    out.push_str("[[");
                    rest = &after[2..];
                }
            }
        }
        out.push_str(rest);
        out
    }
}

/// Turns placeholders nobody issued into plain slots, for answers with no
/// redactor behind them. Small models copy the `[[NAME_1]]` shape they see
/// in instructions into answers to questions that had none; people should
/// read `[name]`, a slot they recognise, not the internal token.
pub fn plain_slots(text: &str) -> String {
    Redactor::new().restore(text)
}

/// `[[FRIEND_NAME_2]]` becomes `[friend name]`. Only the placeholder shape
/// (capital letters and underscores, then a number) is touched, so
/// double brackets in code or wiki links pass unchanged.
fn plain_slot(token: &str) -> Option<String> {
    let inner = token.strip_prefix("[[")?.strip_suffix("]]")?;
    let (label, number) = inner.rsplit_once('_')?;
    let label_ok = !label.is_empty()
        && !label.starts_with('_')
        && !label.ends_with('_')
        && label.chars().all(|c| c.is_ascii_uppercase() || c == '_');
    let number_ok = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
    if !label_ok || !number_ok {
        return None;
    }
    Some(format!("[{}]", label.to_lowercase().replace('_', " ")))
}

fn classify(word: &str) -> Option<EntityKind> {
    if word.len() < 4 {
        return None;
    }
    if is_url_with_credentials(word) {
        return Some(EntityKind::Url);
    }
    if is_email(word) {
        return Some(EntityKind::Email);
    }
    if is_path_with_user(word) {
        return Some(EntityKind::Path);
    }
    if word.parse::<std::net::Ipv4Addr>().is_ok() && word.matches('.').count() == 3 {
        return Some(EntityKind::Ipv4);
    }
    if word.contains(':')
        && word.matches(':').count() >= 2
        && word.parse::<std::net::Ipv6Addr>().is_ok()
    {
        return Some(EntityKind::Ipv6);
    }
    if is_uuid(word) {
        return Some(EntityKind::Uuid);
    }
    if is_secret(word) {
        return Some(EntityKind::Secret);
    }
    None
}

fn is_email(word: &str) -> bool {
    let Some((local, domain)) = word.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local
            .chars()
            .all(|c| c.is_alphanumeric() || "._%+-".contains(c))
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && domain
            .chars()
            .all(|c| c.is_alphanumeric() || c == '.' || c == '-')
}

fn is_url_with_credentials(word: &str) -> bool {
    let Some((_, rest)) = word.split_once("://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    authority.contains('@') && authority.split('@').next().is_some_and(|u| u.contains(':'))
}

fn is_path_with_user(word: &str) -> bool {
    let lower = word.to_lowercase();
    ["/home/", "/users/", "c:\\users\\", "c:/users/"]
        .iter()
        .any(|p| lower.starts_with(p) && lower.len() > p.len())
}

fn is_uuid(word: &str) -> bool {
    let b = word.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| {
        if [8, 13, 18, 23].contains(&i) {
            *c == b'-'
        } else {
            c.is_ascii_hexdigit()
        }
    })
}

fn is_secret(word: &str) -> bool {
    const PREFIXES: [&str; 8] = [
        "sk-", "sk_live_", "pk_live_", "ghp_", "gho_", "xoxb-", "xoxp-", "AKIA",
    ];
    if PREFIXES.iter().any(|p| word.starts_with(p)) && word.len() >= 16 {
        return true;
    }
    if word.starts_with("eyJ") && word.matches('.').count() == 2 && word.len() >= 30 {
        return true;
    }
    if word.len() >= 24 && word.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    if word.len() >= 32
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+/=_-".contains(c))
    {
        let digits = word.chars().filter(|c| c.is_ascii_digit()).count();
        let upper = word.chars().filter(|c| c.is_ascii_uppercase()).count();
        let lower = word.chars().filter(|c| c.is_ascii_lowercase()).count();
        return digits > 0 && upper > 0 && lower > 0;
    }
    false
}

fn match_grouped(chars: &[char], start: usize) -> Option<(usize, EntityKind)> {
    let first = chars[start];
    if first == '+' {
        return match_phone(chars, start);
    }
    if first.is_ascii_uppercase() {
        return match_iban(chars, start);
    }
    if first.is_ascii_digit() {
        return match_card(chars, start);
    }
    None
}

fn collect_grouped(chars: &[char], start: usize, accept: impl Fn(char) -> bool) -> (usize, String) {
    let mut end = start;
    let mut compact = String::new();
    let mut i = start;
    while i < chars.len() {
        let c = chars[i];
        if accept(c) {
            compact.push(c);
            i += 1;
            end = i;
        } else if (c == ' ' || c == '-')
            && i + 1 < chars.len()
            && accept(chars[i + 1])
            && !compact.is_empty()
        {
            i += 1;
        } else {
            break;
        }
    }
    (end, compact)
}

fn followed_by_boundary(chars: &[char], end: usize) -> bool {
    end >= chars.len() || !chars[end].is_alphanumeric()
}

fn match_phone(chars: &[char], start: usize) -> Option<(usize, EntityKind)> {
    let (end, compact) = collect_grouped(chars, start + 1, |c| c.is_ascii_digit());
    (compact.len() >= 8 && compact.len() <= 15 && followed_by_boundary(chars, end))
        .then_some((end, EntityKind::Phone))
}

fn match_card(chars: &[char], start: usize) -> Option<(usize, EntityKind)> {
    let (end, compact) = collect_grouped(chars, start, |c| c.is_ascii_digit());
    if !followed_by_boundary(chars, end) {
        return None;
    }
    if (13..=19).contains(&compact.len()) && luhn_ok(&compact) {
        return Some((end, EntityKind::Card));
    }
    if (9..=15).contains(&compact.len()) && chars[start..end].iter().any(|c| *c == ' ' || *c == '-')
    {
        return Some((end, EntityKind::Phone));
    }
    None
}

fn match_iban(chars: &[char], start: usize) -> Option<(usize, EntityKind)> {
    if start + 4 > chars.len()
        || !chars[start + 1].is_ascii_uppercase()
        || !chars[start + 2].is_ascii_digit()
        || !chars[start + 3].is_ascii_digit()
    {
        return None;
    }
    let (end, compact) = collect_grouped(chars, start, |c| {
        c.is_ascii_uppercase() || c.is_ascii_digit()
    });
    ((15..=34).contains(&compact.len()) && iban_ok(&compact) && followed_by_boundary(chars, end))
        .then_some((end, EntityKind::Iban))
}

fn luhn_ok(digits: &str) -> bool {
    let mut sum = 0u32;
    for (i, c) in digits.chars().rev().enumerate() {
        let Some(mut d) = c.to_digit(10) else {
            return false;
        };
        if i % 2 == 1 {
            d *= 2;
            if d > 9 {
                d -= 9;
            }
        }
        sum += d;
    }
    sum.is_multiple_of(10)
}

fn iban_ok(iban: &str) -> bool {
    let rearranged: String = iban[4..].chars().chain(iban[..4].chars()).collect();
    let mut rem: u32 = 0;
    for c in rearranged.chars() {
        let v = match c {
            '0'..='9' => c as u32 - '0' as u32,
            'A'..='Z' => c as u32 - 'A' as u32 + 10,
            _ => return false,
        };
        let digits = if v >= 10 { 2 } else { 1 };
        rem = (rem * 10u32.pow(digits) + v) % 97;
    }
    rem == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_are_replaced_and_restored() {
        let mut r = Redactor::new();
        let text = "Escribe a ana.perez@example.com desde 10.0.0.1. Mi IBAN es ES91 2100 0418 4502 0005 1332, tel +34 612 345 678.";
        let redacted = r.redact(text);
        assert!(!redacted.contains("ana.perez"));
        assert!(!redacted.contains("10.0.0.1"));
        assert!(!redacted.contains("2100"));
        assert!(!redacted.contains("612"));
        assert!(redacted.contains("[[EMAIL_1]]"));
        assert!(redacted.contains("[[IBAN_1]]"));
        assert_eq!(r.restore(&redacted), text);
    }

    #[test]
    fn the_same_value_keeps_the_same_placeholder_across_turns() {
        let mut r = Redactor::new();
        let a = r.redact("mail bob@corp.io");
        let b = r.redact("again bob@corp.io and eve@corp.io");
        assert!(a.contains("[[EMAIL_1]]"));
        assert!(b.contains("[[EMAIL_1]]"));
        assert!(b.contains("[[EMAIL_2]]"));
    }

    #[test]
    fn cards_need_a_valid_checksum() {
        let mut r = Redactor::new();
        assert!(r
            .redact("card 4111 1111 1111 1111 ok")
            .contains("[[CARD_1]]"));
        let mut r2 = Redactor::new();
        assert!(!r2.redact("number 1234567890123").contains("[["));
    }

    #[test]
    fn secrets_paths_and_ipv6_are_caught() {
        let mut r = Redactor::new();
        let out = r.redact(
            "key sk-abcdefghijklmnop1234 at /home/vlad/.ssh/id_ed25519 via 2001:db8::1 id 123e4567-e89b-12d3-a456-426614174000",
        );
        assert!(!out.contains("sk-abc"));
        assert!(!out.contains("vlad"));
        assert!(!out.contains("2001:db8"));
        assert!(!out.contains("123e4567"));
    }

    #[test]
    fn ordinary_prose_and_versions_survive() {
        let mut r = Redactor::new();
        let text = "Version 1.2.3 released in 2026, see chapter 12: results.";
        assert_eq!(r.redact(text), text);
    }

    #[test]
    fn configured_names_are_replaced_on_word_boundaries() {
        let mut r = Redactor::with_terms(["Javier".to_string()]);
        let out = r.redact("Javier said javier's plan beats Javiera's.");
        assert!(out.starts_with("[[NAME_1]] said [[NAME_1]]'s"));
        assert!(out.contains("Javiera"));
    }

    #[test]
    fn placeholders_nobody_issued_become_plain_slots() {
        let mut r = Redactor::new();
        let mine = r.redact("mail ana@example.com");
        assert!(mine.contains("[[EMAIL_1]]"));
        assert_eq!(
            r.restore("Write to [[EMAIL_1]], then try [[RESTAURANT_1]] or [[FRIEND_NAME_2]]."),
            "Write to ana@example.com, then try [restaurant] or [friend name]."
        );
        assert_eq!(plain_slots("Hi [[NAME_1]],"), "Hi [name],");
    }

    #[test]
    fn other_double_brackets_are_left_alone() {
        let r = Redactor::new();
        for text in [
            "[[x",
            "df[[0, 1]] and arr[[2]]",
            "see [[Main Page]]",
            "[[1440]]",
            "[[_X_1]] [[X_]] [[x_1]]",
        ] {
            assert_eq!(r.restore(text), text);
            assert_eq!(plain_slots(text), text);
        }
    }
}
