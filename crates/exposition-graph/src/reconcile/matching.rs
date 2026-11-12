//! Identity matching between the declarations known for a file and those
//! extracted from its new text.
//!
//! Anchors combine the declaration's identity (syntax + declared name), its
//! containing context, and two fingerprints (the construct's normalized text
//! and its header/context/preceding text); ranges are never used. A match
//! must be unique on both sides at each step, otherwise nothing is guessed:
//!
//! 1. Named: the same declared name (label, qualified name, citation key).
//!    One old and one new with the same kind → the same entity. A name
//!    declared more than once in the new text is *ambiguous*: the existing
//!    entity is kept (binding ambiguous) and no new entity is created.
//! 2. Same syntax and identical construct text (body fingerprint).
//! 3. Same syntax and identical header/context (context fingerprint) — the
//!    body was edited.
//! 4. Same syntax and same containing context, exactly one candidate left on
//!    each side.
//!
//! Tombstoned candidates (declarations removed earlier) are revived only by
//! steps 1 and 2 (an exact name or exact text), never by context alone.

use std::collections::{BTreeMap, HashMap};

use super::extract::Declaration;
use crate::vocabulary::EntityKind;

/// The anchor of a known declaration, as stored in its binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownAnchor {
    pub kind: EntityKind,
    pub syntax: String,
    pub name: Option<String>,
    pub body: String,
    pub context: String,
    pub scope: String,
    pub tombstoned: bool,
}

const PREFIX: &str = "decl:v1:";

/// Encode a declaration's fingerprints for its binding.
pub fn encode_fingerprint(d: &Declaration) -> String {
    let scope = super::extract::fingerprint(d.context.as_deref().unwrap_or(""));
    format!("{PREFIX}{}:{}:{}", d.context_fingerprint, d.body_fingerprint, &scope[..8])
}

/// Decode `(context, body, scope)` fingerprints of a stored binding.
pub fn decode_fingerprint(raw: &str) -> Option<(String, String, String)> {
    let rest = raw.strip_prefix(PREFIX)?;
    let mut parts = rest.split(':');
    let (c, b, s) = (parts.next()?, parts.next()?, parts.next()?);
    Some((c.to_owned(), b.to_owned(), s.to_owned()))
}

/// The outcome of matching.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Matching {
    /// (known index, new index).
    pub pairs: Vec<(usize, usize)>,
    /// Known entities whose name is now declared more than once (kept, bound ambiguous).
    pub ambiguous_known: Vec<(usize, usize)>,
    /// New declarations not admitted because their name is ambiguous.
    pub ambiguous_new: Vec<usize>,
    /// Names declared more than once (name → count).
    pub ambiguous_names: BTreeMap<String, usize>,
    /// Known declarations with no match (removed, or kind changed).
    pub unmatched_known: Vec<usize>,
    /// New declarations with no match (to be created).
    pub unmatched_new: Vec<usize>,
}

fn scope_of(d: &Declaration) -> String {
    super::extract::fingerprint(d.context.as_deref().unwrap_or(""))[..8].to_owned()
}

pub fn match_declarations(known: &[KnownAnchor], new: &[Declaration]) -> Matching {
    let mut m = Matching::default();
    let mut known_taken = vec![false; known.len()];
    let mut new_taken = vec![false; new.len()];

    // 1. Named.
    let mut new_by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, d) in new.iter().enumerate() {
        if let Some(n) = &d.name {
            new_by_name.entry(n.as_str()).or_default().push(i);
        }
    }
    let mut known_by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, k) in known.iter().enumerate() {
        if let Some(n) = &k.name {
            known_by_name.entry(n.as_str()).or_default().push(i);
        }
    }
    let mut names: Vec<&str> = new_by_name.keys().copied().collect();
    names.sort();
    for name in names {
        let news = &new_by_name[name];
        // Prefer live entities over tombstones with the same name.
        let mut olds: Vec<usize> = known_by_name.get(name).cloned().unwrap_or_default();
        olds.sort_by_key(|&i| known[i].tombstoned);
        if news.len() > 1 {
            m.ambiguous_names.insert(name.to_owned(), news.len());
            for &n in news {
                new_taken[n] = true;
                m.ambiguous_new.push(n);
            }
            if let Some(&o) = olds.iter().find(|&&o| !known[o].tombstoned) {
                known_taken[o] = true;
                m.ambiguous_known.push((o, news.len()));
            }
            continue;
        }
        let n = news[0];
        let live: Vec<usize> = olds.iter().copied().filter(|&o| !known[o].tombstoned).collect();
        let candidates = if live.is_empty() { olds.clone() } else { live };
        let same_kind: Vec<usize> = candidates.into_iter().filter(|&o| known[o].kind == new[n].kind).collect();
        if same_kind.len() == 1 {
            let o = same_kind[0];
            known_taken[o] = true;
            new_taken[n] = true;
            m.pairs.push((o, n));
        }
    }

    // 2–4. By syntax: exact text, then header/context, then the only one left in its context.
    type KeyOf<'k> = &'k dyn Fn(&KnownAnchor, &Declaration) -> Option<(String, String)>;
    let steps: [KeyOf<'_>; 3] = [
        &|k, d| Some((k.body.clone(), d.body_fingerprint.clone())),
        &|k, d| (!k.tombstoned).then(|| (k.context.clone(), d.context_fingerprint.clone())),
        &|k, d| (!k.tombstoned).then(|| (k.scope.clone(), scope_of(d))),
    ];
    for key_of in steps.iter() {
        let mut known_keys: HashMap<(String, String), Vec<usize>> = HashMap::new();
        let mut new_keys: HashMap<(String, String), Vec<usize>> = HashMap::new();
        for (o, k) in known.iter().enumerate() {
            if known_taken[o] {
                continue;
            }
            for (n, d) in new.iter().enumerate() {
                if new_taken[n] || d.syntax != k.syntax || d.kind != k.kind {
                    continue;
                }
                // Named declarations whose name differs are different entities
                // (the label was not removed but changed): leave them to creation.
                if d.name.is_some() && k.name.is_some() && d.name != k.name {
                    continue;
                }
                if let Some((kk, nk)) = key_of(k, d)
                    && kk == nk
                {
                    known_keys.entry((k.syntax.clone(), kk)).or_default().push(o);
                    new_keys.entry((d.syntax.clone(), nk)).or_default().push(n);
                }
            }
        }
        let mut keys: Vec<(String, String)> = known_keys.keys().cloned().collect();
        keys.sort();
        for key in keys {
            let mut olds = known_keys[&key].clone();
            olds.sort();
            olds.dedup();
            let mut news = new_keys.get(&key).cloned().unwrap_or_default();
            news.sort();
            news.dedup();
            if olds.len() == 1 && news.len() == 1 && !known_taken[olds[0]] && !new_taken[news[0]] {
                // Unique on both sides for this key (for the last step: the
                // only unmatched candidate of its syntax in its context).
                known_taken[olds[0]] = true;
                new_taken[news[0]] = true;
                m.pairs.push((olds[0], news[0]));
            }
        }
    }

    m.unmatched_known = (0..known.len()).filter(|&o| !known_taken[o] && !known[o].tombstoned).collect();
    m.unmatched_new = (0..new.len()).filter(|&n| !new_taken[n]).collect();
    m.pairs.sort();
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconcile::extract::{context_fingerprint, fingerprint};
    use exposition_core::text::TextRange;

    fn decl(kind: EntityKind, name: Option<&str>, header: &str, body: &str, context: Option<&str>) -> Declaration {
        Declaration {
            kind,
            syntax: "latex.env:theorem".into(),
            name: name.map(str::to_owned),
            title: None,
            range: TextRange::new(0, 1),
            context: context.map(str::to_owned),
            body_fingerprint: fingerprint(body),
            context_fingerprint: context_fingerprint(header, context, "before"),
            attributes: Default::default(),
        }
    }

    fn known(d: &Declaration) -> KnownAnchor {
        let (c, b, s) = decode_fingerprint(&encode_fingerprint(d)).unwrap();
        KnownAnchor {
            kind: d.kind,
            syntax: d.syntax.clone(),
            name: d.name.clone(),
            body: b,
            context: c,
            scope: s,
            tombstoned: false,
        }
    }

    const T: EntityKind = EntityKind::Theorem;

    #[test]
    fn named_declarations_keep_identity_across_edits_and_moves() {
        let old = [decl(T, Some("thm:a"), "h", "A", None), decl(T, Some("thm:b"), "h", "B", None)];
        // Reordered and both bodies edited.
        let new = vec![decl(T, Some("thm:b"), "h", "B2", None), decl(T, Some("thm:a"), "h", "A2", None)];
        let m = match_declarations(&old.iter().map(known).collect::<Vec<_>>(), &new);
        assert_eq!(m.pairs, vec![(0, 1), (1, 0)]);
        assert!(m.unmatched_known.is_empty() && m.unmatched_new.is_empty());
    }

    #[test]
    fn duplicate_names_are_ambiguous_not_merged() {
        let old = [decl(T, Some("x"), "h", "A", None)];
        let new = vec![decl(T, Some("x"), "h", "A", None), decl(T, Some("x"), "h2", "C", None)];
        let m = match_declarations(&old.iter().map(known).collect::<Vec<_>>(), &new);
        assert!(m.pairs.is_empty());
        assert_eq!(m.ambiguous_known, vec![(0, 2)]);
        assert_eq!(m.ambiguous_new, vec![0, 1]);
        assert!(m.unmatched_new.is_empty(), "duplicates are not admitted");
        assert!(m.unmatched_known.is_empty(), "the known entity is kept, not tombstoned");
    }

    #[test]
    fn unnamed_declarations_match_by_text_then_context_never_by_position() {
        let a = decl(T, None, "\\begin{theorem}[First]", "first body", Some("Intro"));
        let b = decl(T, None, "\\begin{theorem}[Second]", "second body", Some("Intro"));
        let olds: Vec<KnownAnchor> = [&a, &b].iter().map(|d| known(d)).collect();
        // Body of `a` edited (context unchanged), `b` moved earlier unchanged.
        let a2 = decl(T, None, "\\begin{theorem}[First]", "first body, edited", Some("Intro"));
        let m = match_declarations(&olds, &[b.clone(), a2]);
        assert_eq!(m.pairs, vec![(0, 1), (1, 0)]);
        // Both edited in header and body: nothing to anchor on → removed + created.
        let a3 = decl(T, None, "\\begin{theorem}[Primo]", "x", Some("Intro"));
        let b3 = decl(T, None, "\\begin{theorem}[Secundo]", "y", Some("Intro"));
        let m = match_declarations(&olds, &[a3, b3]);
        assert!(m.pairs.is_empty());
        assert_eq!(m.unmatched_known, vec![0, 1]);
        assert_eq!(m.unmatched_new, vec![0, 1]);
        // The only theorem of its section, fully rewritten: same entity.
        let only = vec![known(&decl(T, None, "h1", "b1", Some("Sec")))];
        let m = match_declarations(&only, &[decl(T, None, "h2", "b2", Some("Sec"))]);
        assert_eq!(m.pairs, vec![(0, 0)]);
    }

    #[test]
    fn kind_changes_and_tombstones() {
        let old = [decl(EntityKind::Lemma, Some("l"), "h", "A", None)];
        let m = match_declarations(&old.iter().map(known).collect::<Vec<_>>(), &[decl(T, Some("l"), "h", "A", None)]);
        assert!(m.pairs.is_empty(), "a lemma that became a theorem is a new entity");
        assert_eq!(m.unmatched_known, vec![0]);
        // A removed declaration restored with the same text is revived.
        let mut gone = known(&decl(T, None, "h", "text", None));
        gone.tombstoned = true;
        let m = match_declarations(&[gone.clone()], &[decl(T, None, "h", "text", None)]);
        assert_eq!(m.pairs, vec![(0, 0)]);
        // ...but not by context alone.
        let m = match_declarations(&[gone], &[decl(T, None, "h", "other", None)]);
        assert!(m.pairs.is_empty());
        assert!(m.unmatched_known.is_empty(), "tombstones are never reported for removal again");
    }
}
