//! ProseMirror-parity validation over a schema artifact (spec §4.8).
//!
//! The reference behavior is `prosemirror-model`'s `Node.fromJSON` +
//! `Node.check` (the editor's real schema): a document is valid when the
//! schema can build it and the check passes. This module replicates that
//! observable contract from the artifact: every content expression is
//! compiled into the same DFA prosemirror-model builds (content.ts:
//! tokenizer → parser → NFA → DFA), and attribute/mark handling follows
//! `computeAttrs` / `checkAttrs` / `Mark.setFrom` semantics.
//!
//! Parity notes (verified against prosemirror-model 1.25.12 sources):
//! - unknown node type → error, unknown mark type → error (fromJSON throws);
//! - unknown attrs (node or mark) are silently dropped by `create` →
//!   warnings, they never flip the verdict;
//! - a required attribute (no default in the artifact) missing from the
//!   JSON → error ("No value supplied for attribute"); an explicit `null`
//!   value counts as given (PM treats only `undefined` as absent);
//! - `Node.check` validates the content match and the mark-set shape (a
//!   repeated mark type fails `sameSet`) but not marks against the node's
//!   own type — the child-marks rule lives in `validContent`: every
//!   child's marks must be inside the PARENT's mark set (the artifact's
//!   `marks` list of the parent);
//! - text nodes ignore their `content` field (PM text nodes have none) and
//!   reject an empty `text`;
//! - the artifact declares the noteDoc content rule to cover the title
//!   family only (validation_limits): a root without a leading `noteTitle`
//!   is outside the rule — its content match is skipped with an explicit
//!   warning, everything else is still checked.

use std::collections::{BTreeMap, HashMap, VecDeque};

use serde_json::Value;

use crate::error::{ErrorCode, UmError};
use crate::model::{INLINE_NODE_TYPES, KNOWN_NODE_TYPES};
use crate::schema::SchemaArtifact;
use crate::validate::Diag;

// ─── Content expression DFA (port of prosemirror-model content.ts) ───────

/// A DFA state: `valid_end` marks a valid end of the content; edges map a
/// node type index to the next state.
struct Dfa {
    states: Vec<DfaState>,
}

struct DfaState {
    valid_end: bool,
    edges: Vec<(usize, usize)>,
}

impl Dfa {
    /// Empty expression: no content, always a valid end (PM
    /// `ContentMatch.empty`).
    fn empty() -> Self {
        Dfa {
            states: vec![DfaState {
                valid_end: true,
                edges: Vec::new(),
            }],
        }
    }

    /// Match a sequence of node type indices from the start state; valid
    /// only when the final state is a valid end (PM `matchFragment`).
    fn match_sequence(&self, children: &[usize]) -> bool {
        let mut current = 0usize;
        for &child in children {
            match self.states[current].edges.iter().find(|(t, _)| *t == child) {
                Some((_, next)) => current = *next,
                None => return false,
            }
        }
        self.states[current].valid_end
    }
}

#[derive(Debug)]
enum Expr {
    Choice(Vec<Expr>),
    Seq(Vec<Expr>),
    Plus(Box<Expr>),
    Star(Box<Expr>),
    Opt(Box<Expr>),
    Range {
        min: usize,
        max: i64,
        expr: Box<Expr>,
    },
    Name(usize),
}

/// NFA edge: optional term (node type index) and optional target state.
/// `EdgeId` identifies an edge inside the state table so a later
/// `connect` can set its target (the JS port mutates edge objects).
type Nfa = Vec<Vec<(Option<usize>, Option<usize>)>>;
type EdgeId = (usize, usize);

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Port of the JS tokenizer `string.split(/\s*(?=\b|\W|$)/)`: a word run is
/// one token, every non-word character is its own token, whitespace is
/// insignificant.
fn tokenize(string: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    for c in string.chars() {
        if c.is_whitespace() {
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
        } else if is_word(c) {
            word.push(c);
        } else {
            if !word.is_empty() {
                tokens.push(std::mem::take(&mut word));
            }
            tokens.push(c.to_string());
        }
    }
    if !word.is_empty() {
        tokens.push(word);
    }
    tokens
}

struct TokenStream<'a> {
    tokens: Vec<String>,
    /// Context for name resolution and the inline/block consistency check.
    type_inline: &'a [bool],
    pos: usize,
}

impl<'a> TokenStream<'a> {
    fn new(string: &str, type_inline: &'a [bool]) -> Self {
        TokenStream {
            tokens: tokenize(string),
            type_inline,
            pos: 0,
        }
    }

    fn next(&self) -> Option<&String> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, token: &str) -> bool {
        if self.next().is_some_and(|t| t == token) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
}

fn parse_expr(
    stream: &mut TokenStream,
    type_index: &HashMap<String, usize>,
    groups: &HashMap<String, Vec<usize>>,
    inline: &mut Option<bool>,
) -> Result<Expr, String> {
    let mut exprs = vec![parse_expr_seq(stream, type_index, groups, inline)?];
    while stream.eat("|") {
        exprs.push(parse_expr_seq(stream, type_index, groups, inline)?);
    }
    Ok(if exprs.len() == 1 {
        exprs.pop().unwrap()
    } else {
        Expr::Choice(exprs)
    })
}

fn parse_expr_seq(
    stream: &mut TokenStream,
    type_index: &HashMap<String, usize>,
    groups: &HashMap<String, Vec<usize>>,
    inline: &mut Option<bool>,
) -> Result<Expr, String> {
    let mut exprs = vec![parse_expr_subscript(stream, type_index, groups, inline)?];
    while stream.next().is_some_and(|t| t != ")" && t != "|") {
        exprs.push(parse_expr_subscript(stream, type_index, groups, inline)?);
    }
    Ok(if exprs.len() == 1 {
        exprs.pop().unwrap()
    } else {
        Expr::Seq(exprs)
    })
}

fn parse_expr_subscript(
    stream: &mut TokenStream,
    type_index: &HashMap<String, usize>,
    groups: &HashMap<String, Vec<usize>>,
    inline: &mut Option<bool>,
) -> Result<Expr, String> {
    let mut expr = parse_expr_atom(stream, type_index, groups, inline)?;
    loop {
        if stream.eat("+") {
            expr = Expr::Plus(Box::new(expr));
        } else if stream.eat("*") {
            expr = Expr::Star(Box::new(expr));
        } else if stream.eat("?") {
            expr = Expr::Opt(Box::new(expr));
        } else if stream.eat("{") {
            expr = parse_expr_range(stream, expr)?;
        } else {
            break;
        }
    }
    Ok(expr)
}

fn parse_expr_range(stream: &mut TokenStream, expr: Expr) -> Result<Expr, String> {
    let min = parse_num(stream)?;
    let mut max = min as i64;
    if stream.eat(",") {
        if stream.next().is_some_and(|t| t != "}") {
            max = parse_num(stream)? as i64;
        } else {
            max = -1;
        }
    }
    if !stream.eat("}") {
        return Err("Unclosed braced range".to_owned());
    }
    Ok(Expr::Range {
        min,
        max,
        expr: Box::new(expr),
    })
}

fn parse_num(stream: &mut TokenStream) -> Result<usize, String> {
    let token = stream.next().ok_or("Expected number")?;
    if token.is_empty() || !token.bytes().all(|b| b.is_ascii_digit()) {
        return Err(format!("Expected number, got '{token}'"));
    }
    let value: usize = token
        .parse()
        .map_err(|_| format!("Expected number, got '{token}'"))?;
    stream.pos += 1;
    Ok(value)
}

/// Resolve a name to node type indices: an exact type name or every member
/// of a group. Members are sorted for determinism (the JS generator relies
/// on object insertion order; membership is what parity needs).
fn resolve_name(
    name: &str,
    type_index: &HashMap<String, usize>,
    groups: &HashMap<String, Vec<usize>>,
) -> Result<Vec<usize>, String> {
    if let Some(&idx) = type_index.get(name) {
        return Ok(vec![idx]);
    }
    let members = groups
        .get(name)
        .ok_or_else(|| format!("No node type or group '{name}' found"))?
        .clone();
    if members.is_empty() {
        return Err(format!("No node type or group '{name}' found"));
    }
    Ok(members)
}

fn parse_expr_atom(
    stream: &mut TokenStream,
    type_index: &HashMap<String, usize>,
    groups: &HashMap<String, Vec<usize>>,
    inline: &mut Option<bool>,
) -> Result<Expr, String> {
    if stream.eat("(") {
        let expr = parse_expr(stream, type_index, groups, inline)?;
        if !stream.eat(")") {
            return Err("Missing closing paren".to_owned());
        }
        return Ok(expr);
    }
    let name = match stream.next() {
        Some(token)
            if !token.is_empty()
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_') =>
        {
            token.clone()
        }
        other => {
            return Err(format!(
                "Unexpected token '{}'",
                other.map_or("", String::as_str)
            ));
        }
    };
    let indices = resolve_name(&name, type_index, groups)?;
    // PM: the whole expression must not mix inline and block content
    let first_inline = stream.type_inline[indices[0]];
    match inline {
        None => *inline = Some(first_inline),
        Some(current) if *current != first_inline => {
            return Err("Mixing inline and block content".to_owned());
        }
        _ => {}
    }
    stream.pos += 1;
    Ok(if indices.len() == 1 {
        Expr::Name(indices[0])
    } else {
        Expr::Choice(indices.into_iter().map(Expr::Name).collect())
    })
}

fn push_edge(nfa: &mut Nfa, from: usize, term: Option<usize>) -> EdgeId {
    nfa[from].push((term, None));
    (from, nfa[from].len() - 1)
}

fn connect(nfa: &mut Nfa, edges: &[EdgeId], to: usize) {
    for &(from, index) in edges {
        nfa[from][index].1 = Some(to);
    }
}

fn compile(expr: &Expr, from: usize, nfa: &mut Nfa) -> Vec<EdgeId> {
    match expr {
        Expr::Choice(exprs) => {
            let mut out = Vec::new();
            for expr in exprs {
                out.extend(compile(expr, from, nfa));
            }
            out
        }
        Expr::Seq(exprs) => {
            let mut current = from;
            let mut out = Vec::new();
            for (i, expr) in exprs.iter().enumerate() {
                let next = compile(expr, current, nfa);
                if i == exprs.len() - 1 {
                    out = next;
                } else {
                    let bridge = nfa.len();
                    nfa.push(vec![]);
                    connect(nfa, &next, bridge);
                    current = bridge;
                }
            }
            out
        }
        Expr::Star(expr) => {
            let loop_state = nfa.len();
            nfa.push(vec![]);
            let skip = push_edge(nfa, from, None);
            connect(nfa, &[skip], loop_state);
            let inner = compile(expr, loop_state, nfa);
            connect(nfa, &inner, loop_state);
            vec![push_edge(nfa, loop_state, None)]
        }
        Expr::Plus(expr) => {
            let loop_state = nfa.len();
            nfa.push(vec![]);
            let first = compile(expr, from, nfa);
            connect(nfa, &first, loop_state);
            let again = compile(expr, loop_state, nfa);
            connect(nfa, &again, loop_state);
            vec![push_edge(nfa, loop_state, None)]
        }
        Expr::Opt(expr) => {
            let skip = push_edge(nfa, from, None);
            let inner = compile(expr, from, nfa);
            let mut out = vec![skip];
            out.extend(inner);
            out
        }
        Expr::Range { min, max, expr } => {
            let mut current = from;
            for _ in 0..*min {
                let next = nfa.len();
                nfa.push(vec![]);
                let inner = compile(expr, current, nfa);
                connect(nfa, &inner, next);
                current = next;
            }
            if *max == -1 {
                let inner = compile(expr, current, nfa);
                connect(nfa, &inner, current);
            } else {
                for _ in *min..(*max as usize) {
                    let next = nfa.len();
                    nfa.push(vec![]);
                    let skip = push_edge(nfa, current, None);
                    nfa[skip.0][skip.1].1 = Some(next);
                    let inner = compile(expr, current, nfa);
                    connect(nfa, &inner, next);
                    current = next;
                }
            }
            vec![push_edge(nfa, current, None)]
        }
        Expr::Name(idx) => vec![push_edge(nfa, from, Some(*idx))],
    }
}

/// Nodes reachable by null edges; nodes with a single null-out-edge are
/// omitted (they may lead to needless duplicated states) — port of
/// `nullFrom`. Result sorted descending (JS `cmp = b - a`), which is part
/// of the state identity.
fn null_from(nfa: &Nfa, start: usize) -> Vec<usize> {
    let mut result: Vec<usize> = Vec::new();
    scan(start, nfa, &mut result);
    result.sort_unstable_by(|a, b| b.cmp(a));
    result
}

fn scan(node: usize, nfa: &Nfa, result: &mut Vec<usize>) {
    let edges = &nfa[node];
    if edges.len() == 1 && edges[0].0.is_none() {
        let to = edges[0].1.expect("connected edge");
        return scan(to, nfa, result);
    }
    result.push(node);
    for (term, to) in edges {
        let Some(to) = to else { continue };
        if term.is_none() && !result.contains(&to) {
            scan(*to, nfa, result);
        }
    }
}

fn state_key(states: &[usize]) -> String {
    states
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

/// Compile an NFA into a DFA (port of `dfa`; the recursive memoized
/// `explore` becomes a worklist over the same states).
fn build_dfa(nfa: &Nfa, type_names: &[String], required_attr: &[bool]) -> Result<Dfa, String> {
    let success = nfa.len() - 1;
    let mut states: Vec<DfaState> = Vec::new();
    let mut labeled: HashMap<String, usize> = HashMap::new();
    let mut queue: VecDeque<Vec<usize>> = VecDeque::new();

    // create-or-lookup a state for a set of NFA nodes
    macro_rules! intern {
        ($set:expr) => {{
            let key = state_key(&$set);
            if let Some(&idx) = labeled.get(&key) {
                idx
            } else {
                let idx = states.len();
                states.push(DfaState {
                    valid_end: $set.contains(&success),
                    edges: Vec::new(),
                });
                labeled.insert(key, idx);
                queue.push_back($set);
                idx
            }
        }};
    }

    let start = null_from(nfa, 0);
    let _start_idx = intern!(start);
    while let Some(set) = queue.pop_front() {
        let parent_idx = labeled[&state_key(&set)];
        let mut out: Vec<(usize, Vec<usize>)> = Vec::new();
        for &node in &set {
            for (term, to) in &nfa[node] {
                let Some(term) = term else { continue };
                let to = to.expect("connected edge");
                let targets = null_from(nfa, to);
                let position = match out.iter().position(|(t, _)| t == term) {
                    Some(p) => p,
                    None => {
                        out.push((*term, Vec::new()));
                        out.len() - 1
                    }
                };
                for target in targets {
                    if !out[position].1.contains(&target) {
                        out[position].1.push(target);
                    }
                }
            }
        }
        for (term, mut targets) in out {
            targets.sort_unstable_by(|a, b| b.cmp(a));
            let target_idx = intern!(targets);
            states[parent_idx].edges.push((term, target_idx));
        }
    }

    // dead ends: a required position reachable only through non-generatable
    // node types is a compile error (port of `checkForDeadEnds`)
    let mut work: Vec<usize> = vec![0];
    let mut i = 0;
    while i < work.len() {
        let state = &states[work[i]];
        let mut dead = !state.valid_end;
        let mut names: Vec<&str> = Vec::new();
        for (term, next) in &state.edges {
            names.push(&type_names[*term]);
            let generatable = type_names[*term] == "text" || !required_attr[*term];
            if dead && generatable {
                dead = false;
            }
            if !work.contains(next) {
                work.push(*next);
            }
        }
        if dead {
            return Err(format!(
                "Only non-generatable nodes ({}) in a required position",
                names.join(", ")
            ));
        }
        i += 1;
    }

    Ok(Dfa { states })
}

// ─── Compiled schema ──────────────────────────────────────────────────────

/// A compiled node type.
struct PmNodeType {
    name: String,
    /// Mark type names allowed on the CHILDREN of this node (the artifact
    /// `marks` list — PM `markSet` resolved at artifact generation time).
    mark_set: Vec<String>,
    /// (attr name, has_default) in artifact order.
    attrs: Vec<(String, bool)>,
    content: Dfa,
}

/// A compiled mark type.
struct PmMarkType {
    attrs: Vec<(String, bool)>,
}

/// The compiled schema: artifact inventory + content DFAs.
pub struct PmSchema {
    type_index: HashMap<String, usize>,
    types: Vec<PmNodeType>,
    mark_index: HashMap<String, usize>,
    marks: BTreeMap<String, PmMarkType>,
    top_node: Option<String>,
}

impl PmSchema {
    /// Compile every artifact content expression into a DFA. A broken or
    /// self-inconsistent artifact is `SCHEMA_ARTIFACT_INVALID`.
    pub fn build(artifact: &SchemaArtifact) -> Result<Self, UmError> {
        let invalid = |message: String| {
            UmError::new(ErrorCode::SchemaArtifactInvalid, message).with_hint(
                "The artifact is generated by the plugin in its own directory; regenerate it by reloading the plugin.",
            )
        };

        // stable type order: artifact name order (BTreeMap → sorted)
        let names: Vec<String> = artifact.nodes.keys().cloned().collect();
        let mut type_index: HashMap<String, usize> = HashMap::new();
        for (i, name) in names.iter().enumerate() {
            type_index.insert(name.clone(), i);
        }
        let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, name) in names.iter().enumerate() {
            for group in &artifact.nodes[name].groups {
                groups.entry(group.clone()).or_default().push(i);
            }
        }
        for members in groups.values_mut() {
            members.sort_unstable();
        }

        // PM's inline/block mixing check keys on `NodeType.isInline` (the
        // node itself is text-level), not on `inlineContent` ("its content
        // is inline" — textblocks like paragraph are true there). The
        // artifact does not carry `isInline`; in this schema inline nodes
        // are exactly the members of the group "inline".
        let inline_of: Vec<bool> = names
            .iter()
            .map(|name| artifact.nodes[name].groups.iter().any(|g| g == "inline"))
            .collect();
        let required_attr: Vec<bool> = names
            .iter()
            .map(|name| {
                artifact.nodes[name]
                    .attrs
                    .values()
                    .any(|spec| !spec.get("default").is_some())
            })
            .collect();

        let mut mark_index: HashMap<String, usize> = HashMap::new();
        let mut marks: BTreeMap<String, PmMarkType> = BTreeMap::new();
        for (i, (name, spec)) in artifact.marks.iter().enumerate() {
            mark_index.insert(name.clone(), i);
            marks.insert(
                name.clone(),
                PmMarkType {
                    attrs: spec
                        .attrs
                        .iter()
                        .map(|(attr, spec)| (attr.clone(), spec.get("default").is_some()))
                        .collect(),
                },
            );
        }

        let mut types: Vec<PmNodeType> = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            let spec = &artifact.nodes[name];
            let content = match &spec.content {
                None => Dfa::empty(),
                Some(expr) => {
                    let mut stream = TokenStream::new(expr, &inline_of);
                    if stream.next().is_none() {
                        Dfa::empty()
                    } else {
                        let mut inline: Option<bool> = None;
                        let parsed = parse_expr(&mut stream, &type_index, &groups, &mut inline)
                            .map_err(|mut message| {
                                message.push_str(&format!(" (in content expression of {name})"));
                                invalid(message)
                            })?;
                        if stream.next().is_some() {
                            return Err(invalid(format!(
                                "Unexpected trailing text in content expression of {name}"
                            )));
                        }
                        let mut nfa: Nfa = vec![vec![]];
                        let edges = compile(&parsed, 0, &mut nfa);
                        let success = nfa.len();
                        nfa.push(vec![]);
                        connect(&mut nfa, &edges, success);
                        build_dfa(&nfa, &names, &required_attr).map_err(|mut message| {
                            message.push_str(&format!(" (in content expression of {name})"));
                            invalid(message)
                        })?
                    }
                }
            };
            types.push(PmNodeType {
                name: name.clone(),
                mark_set: spec.marks.clone(),
                attrs: spec
                    .attrs
                    .iter()
                    .map(|(attr, attr_spec)| (attr.clone(), attr_spec.get("default").is_some()))
                    .collect(),
                content,
            });
        }

        Ok(PmSchema {
            type_index,
            types,
            mark_index,
            marks,
            top_node: artifact.top_node.clone(),
        })
    }

    pub fn top_node(&self) -> Option<&str> {
        self.top_node.as_deref()
    }

    pub fn has_type(&self, name: &str) -> bool {
        self.type_index.contains_key(name)
    }

    /// Validate one document the way `Node.fromJSON` + `Node.check` would:
    /// errors flip the verdict, warnings are verdict-neutral extras.
    pub fn check_document(&self, doc: &Value) -> crate::validate::NoteValidation {
        let mut out = crate::validate::NoteValidation::default();

        // Family rule from the artifact's own validation_limits: the
        // noteDoc content expression covers the title family; document
        // families and their VERSIONS are application semantics and are
        // not represented in the artifact. A root without a leading
        // noteTitle (plain/note family) and a document carrying legacy
        // node types (a pre-migration version) are outside the rule.
        let root_skips_content_match = self.root_outside_artifact_scope(doc);
        if root_skips_content_match {
            out.warnings.push(Diag {
                code: "SCHEMA_FAMILY_UNCOVERED",
                message: "document root has no leading noteTitle: the noteDoc content rule of this artifact covers the title family only (validation_limits), the content match of the root is not judged".to_owned(),
            });
        }
        self.check_node(doc, true, root_skips_content_match, &mut out);

        // Root type vs artifact top_node: document families are application
        // semantics (§4.7) — a warning, never an error.
        if let (Some(top_node), Some(root_type)) =
            (&self.top_node, doc.get("type").and_then(Value::as_str))
        {
            if root_type != top_node.as_str() {
                out.warnings.push(Diag {
                    code: "SCHEMA_ROOT_MISMATCH",
                    message: format!(
                        "document root type {root_type:?} differs from the artifact top_node {top_node:?} (document families are application semantics)"
                    ),
                });
            }
        }
        out
    }

    fn root_outside_artifact_scope(&self, doc: &Value) -> bool {
        // The scope declaration is specific to the noteDoc top node
        // (the artifact's validation_limits, title family).
        let Some(top_node) = &self.top_node else {
            return false;
        };
        if top_node != "noteDoc" || !self.type_index.contains_key("noteTitle") {
            return false;
        }
        if doc.get("type").and_then(Value::as_str) != Some(top_node.as_str()) {
            return false;
        }
        let first = doc
            .get("content")
            .and_then(Value::as_array)
            .and_then(|children| children.first())
            .and_then(|child| child.get("type"))
            .and_then(Value::as_str);
        if first != Some("noteTitle") {
            return true;
        }
        // A document with node types this artifact does not declare is a
        // legacy version of the family — its content rule cannot be judged.
        !contains_only_declared_types(doc, self)
    }

    fn check_node(
        &self,
        node: &Value,
        is_root: bool,
        skip_content_match: bool,
        out: &mut crate::validate::NoteValidation,
    ) {
        let Value::Object(map) = node else {
            return; // structural pass already reported this
        };
        let Some(type_name) = map.get("type").and_then(Value::as_str) else {
            return; // structural pass already reported this
        };
        let Some(&type_idx) = self.type_index.get(type_name) else {
            // PM: fromJSON throws on an unknown type — subtree unchecked.
            if KNOWN_NODE_TYPES.contains(&type_name) || INLINE_NODE_TYPES.contains(&type_name) {
                out.warnings.push(Diag {
                    code: "SCHEMA_NODE_LEGACY",
                    message: format!(
                        "node type {type_name:?} is not declared in the applicable schema artifact but is a known legacy type (document version semantics are outside the artifact)"
                    ),
                });
            } else {
                out.errors.push(Diag {
                    code: "UNKNOWN_NODE_TYPE",
                    message: format!(
                        "node type {type_name:?} is not declared in the applicable schema artifact"
                    ),
                });
            }
            return;
        };
        let node_type = &self.types[type_idx];

        // Attributes: `create` computes from the spec — unknown input keys
        // are dropped silently (warning), a required attribute missing from
        // the JSON throws (error). Explicit `null` counts as given.
        match map.get("attrs") {
            Some(Value::Object(attrs)) => {
                for (key, _) in attrs {
                    if !node_type.attrs.iter().any(|(name, _)| name == key) {
                        out.warnings.push(Diag {
                            code: "SCHEMA_ATTR_UNKNOWN",
                            message: format!(
                                "attr {key:?} on node type {type_name:?} is not declared in the applicable schema artifact (the editor drops it)"
                            ),
                        });
                    }
                }
                if !attrs.is_empty() {
                    for (name, has_default) in &node_type.attrs {
                        if !has_default && !attrs.contains_key(name) {
                            out.errors.push(Diag {
                                code: "ATTR_REQUIRED",
                                message: format!(
                                    "no value supplied for required attribute {name:?} of node type {type_name:?}"
                                ),
                            });
                        }
                    }
                }
            }
            _ => {
                for (name, has_default) in &node_type.attrs {
                    if !has_default {
                        out.errors.push(Diag {
                            code: "ATTR_REQUIRED",
                            message: format!(
                                "no value supplied for required attribute {name:?} of node type {type_name:?}"
                            ),
                        });
                    }
                }
            }
        }

        // Text nodes: `schema.text` ignores attrs entirely, requires a
        // non-empty string, and has no content of its own.
        if type_name == "text" {
            if map
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.is_empty())
            {
                out.errors.push(Diag {
                    code: "NOTE_DOC_INVALID",
                    message: "empty text nodes are not allowed by the editor schema".to_owned(),
                });
            }
            self.check_marks(map, out);
            return;
        }

        self.check_marks(map, out);

        // Content: child marks against this node's mark set (PM
        // `validContent`), then the content expression match.
        let children: Vec<&Value> = map
            .get("content")
            .and_then(Value::as_array)
            .map(|array| array.iter().collect())
            .unwrap_or_default();

        for child in &children {
            let Some(child_marks) = child.get("marks").and_then(Value::as_array) else {
                continue;
            };
            for mark in child_marks {
                let Some(mark_type) = mark.get("type").and_then(Value::as_str) else {
                    continue; // structural pass reported the shape
                };
                if self.mark_index.contains_key(mark_type)
                    && !node_type
                        .mark_set
                        .iter()
                        .any(|allowed| allowed == mark_type)
                {
                    out.errors.push(Diag {
                        code: "MARK_NOT_ALLOWED",
                        message: format!(
                            "mark type {mark_type:?} is not allowed on a child of {type_name:?} (the parent's mark set does not include it)"
                        ),
                    });
                }
            }
        }

        let child_types: Vec<usize> = children
            .iter()
            .filter_map(|child| child.get("type").and_then(Value::as_str))
            .filter_map(|name| self.type_index.get(name).copied())
            .collect();
        if !skip_content_match && !node_type.content.match_sequence(&child_types) {
            out.errors.push(Diag {
                code: "CONTENT_MISMATCH",
                message: format!(
                    "invalid content for node {type_name:?}: the children do not match the content expression of the applicable schema artifact"
                ),
            });
        }

        for child in children {
            self.check_node(child, false, false, out);
        }
    }

    /// Mark handling of `Node.fromJSON` + the mark part of `Node.check`:
    /// unknown mark types throw (error); mark attrs unknown in the artifact
    /// are dropped by create (warning); a repeated mark type fails
    /// `sameSet` after `addToSet` (error). Mark order is normalized by
    /// `setFrom` (rank sort) and never fails.
    fn check_marks(
        &self,
        map: &serde_json::Map<String, Value>,
        out: &mut crate::validate::NoteValidation,
    ) {
        let Some(Value::Array(marks)) = map.get("marks") else {
            return;
        };
        let mut seen: Vec<&str> = Vec::new();
        for mark in marks {
            let Value::Object(mark_obj) = mark else {
                continue; // structural pass reported the shape
            };
            let Some(mark_type) = mark_obj.get("type").and_then(Value::as_str) else {
                continue;
            };
            let Some(&mark_idx) = self.mark_index.get(mark_type) else {
                out.errors.push(Diag {
                    code: "UNKNOWN_MARK_TYPE",
                    message: format!(
                        "there is no mark type {mark_type:?} in the applicable schema artifact (the editor cannot load it)"
                    ),
                });
                continue;
            };
            if seen.contains(&mark_type) {
                out.errors.push(Diag {
                    code: "MARK_DUPLICATE",
                    message: format!(
                        "invalid collection of marks: {mark_type:?} appears more than once on one node"
                    ),
                });
            } else {
                seen.push(mark_type);
            }
            if let Some(Value::Object(mark_attrs)) = mark_obj.get("attrs") {
                let spec = &self.marks[mark_type];
                for (key, _) in mark_attrs {
                    if !spec.attrs.iter().any(|(name, _)| name == key) {
                        out.warnings.push(Diag {
                            code: "SCHEMA_ATTR_UNKNOWN",
                            message: format!(
                                "attr {key:?} on mark type {mark_type:?} is not declared in the applicable schema artifact (the editor drops it)"
                            ),
                        });
                    }
                }
                let _ = mark_idx;
            }
        }
    }
}

/// True when every node type in the document is declared in the schema.
fn contains_only_declared_types(node: &Value, schema: &PmSchema) -> bool {
    match node {
        Value::Array(items) => items
            .iter()
            .all(|item| contains_only_declared_types(item, schema)),
        Value::Object(map) => {
            if let Some(type_name) = map.get("type").and_then(Value::as_str) {
                if !schema.has_type(type_name) {
                    return false;
                }
            }
            map.get("content")
                .map(|content| contains_only_declared_types(content, schema))
                .unwrap_or(true)
        }
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn artifact() -> SchemaArtifact {
        SchemaArtifact::parse(json!({
            "schema_id": "inscriptum",
            "schema_version": 1,
            "schema_hash": "sha256:x",
            "top_node": "doc",
            "validation_capability": "partial",
            "nodes": {
                "doc": {"content": "section+", "groups": [], "attrs": {}, "marks": [],
                        "addressable": false, "inline_content": false, "searchable": false},
                "section": {"content": "heading para*", "groups": ["block"], "attrs": {},
                            "marks": [], "addressable": true, "inline_content": false,
                            "searchable": false},
                "heading": {"content": "inline*", "groups": ["block"], "attrs": {},
                            "marks": ["bold"], "addressable": true, "inline_content": true,
                            "searchable": true},
                "para": {"content": "inline*", "groups": ["block"], "attrs": {},
                         "marks": ["bold", "italic"], "addressable": true,
                         "inline_content": true, "searchable": true},
                "text": {"content": null, "groups": ["inline"], "attrs": {}, "marks": [],
                         "addressable": false, "inline_content": true, "searchable": false}
            },
            "marks": {
                "bold": {"group": null, "attrs": {}},
                "italic": {"group": null, "attrs": {}}
            }
        }))
        .unwrap()
    }

    fn text(s: &str) -> Value {
        json!({"type": "text", "text": s})
    }

    fn marked_text(s: &str, mark: &str) -> Value {
        json!({"type": "text", "text": s, "marks": [{"type": mark}]})
    }

    #[test]
    fn content_expression_sequences_match() {
        let schema = PmSchema::build(&artifact()).unwrap();
        // doc: section+ — needs at least one section
        let empty = json!({"type": "doc", "content": []});
        assert!(!schema.check_document(&empty).valid());
        let one = json!({"type": "doc", "content": [
            {"type": "section", "content": [
                {"type": "heading", "content": [text("T")]},
                {"type": "para", "content": [text("a")]}
            ]}
        ]});
        let result = schema.check_document(&one);
        assert!(result.valid(), "errors: {:?}", result.errors);
        // section requires heading first
        let missing_heading = json!({"type": "doc", "content": [
            {"type": "section", "content": [{"type": "para", "content": [text("a")]}]}
        ]});
        let result = schema.check_document(&missing_heading);
        assert!(!result.valid());
        assert!(result.errors.iter().any(|e| e.code == "CONTENT_MISMATCH"));
    }

    #[test]
    fn choice_and_optional_work() {
        let schema = PmSchema::build(&artifact()).unwrap();
        // para* allows zero or many, in any count
        let many = json!({"type": "doc", "content": [
            {"type": "section", "content": [
                {"type": "heading", "content": [text("T")]},
                {"type": "para", "content": [text("a")]},
                {"type": "para", "content": [text("b")]}
            ]}
        ]});
        assert!(schema.check_document(&many).valid());
    }

    #[test]
    fn child_marks_are_judged_against_the_parent_mark_set() {
        let schema = PmSchema::build(&artifact()).unwrap();
        let bold_in_para = json!({"type": "doc", "content": [
            {"type": "section", "content": [
                {"type": "heading", "content": [text("T")]},
                {"type": "para", "content": [marked_text("a", "bold")]}
            ]}
        ]});
        let result = schema.check_document(&bold_in_para);
        assert!(result.valid(), "{:?}", result.errors);

        // heading's mark set is [bold] only → italic text inside heading fails
        let italic_in_heading = json!({"type": "doc", "content": [
            {"type": "section", "content": [
                {"type": "heading", "content": [marked_text("T", "italic")]},
                {"type": "para", "content": [text("a")]}
            ]}
        ]});
        let result = schema.check_document(&italic_in_heading);
        assert!(!result.valid());
        assert!(result.errors.iter().any(|e| e.code == "MARK_NOT_ALLOWED"));

        // the same mark is fine where the parent allows it
        let italic_in_para = json!({"type": "doc", "content": [
            {"type": "section", "content": [
                {"type": "heading", "content": [text("T")]},
                {"type": "para", "content": [marked_text("a", "italic")]}
            ]}
        ]});
        assert!(schema.check_document(&italic_in_para).valid());
    }

    #[test]
    fn unknown_mark_type_and_duplicate_marks_are_errors() {
        let schema = PmSchema::build(&artifact()).unwrap();
        let unknown = json!({"type": "doc", "content": [
            {"type": "section", "content": [
                {"type": "heading", "content": [text("T")]},
                {"type": "para", "content": [marked_text("a", "strike")]}
            ]}
        ]});
        let result = schema.check_document(&unknown);
        assert!(!result.valid());
        assert!(result.errors.iter().any(|e| e.code == "UNKNOWN_MARK_TYPE"));

        let duplicated = json!({"type": "doc", "content": [
            {"type": "section", "content": [
                {"type": "heading", "content": [text("T")]},
                {"type": "para", "content": [
                    {"type": "text", "text": "a", "marks": [{"type": "bold"}, {"type": "bold"}]}
                ]}
            ]}
        ]});
        let result = schema.check_document(&duplicated);
        assert!(!result.valid());
        assert!(result.errors.iter().any(|e| e.code == "MARK_DUPLICATE"));
    }

    #[test]
    fn unknown_node_type_is_an_error_and_legacy_a_warning() {
        // noteDoc-shaped artifact so the family rule (validation_limits) is
        // in effect for roots without a leading noteTitle
        let raw = json!({
            "schema_id": "inscriptum", "schema_version": 1, "schema_hash": "sha256:x",
            "top_node": "noteDoc", "validation_capability": "partial",
            "nodes": {
                "noteDoc": {"content": "noteTitle block*", "groups": [], "attrs": {},
                            "marks": [], "addressable": false, "inline_content": false,
                            "searchable": false},
                "noteTitle": {"content": "inline*", "groups": ["block"], "attrs": {},
                              "marks": [], "addressable": false, "inline_content": true,
                              "searchable": true},
                "para": {"content": "inline*", "groups": ["block"], "attrs": {},
                         "marks": ["bold"], "addressable": true, "inline_content": true,
                         "searchable": true},
                "text": {"content": null, "groups": ["inline"], "attrs": {}, "marks": [],
                         "addressable": false, "inline_content": true, "searchable": false}
            },
            "marks": {"bold": {"group": null, "attrs": {}}}
        });
        let schema = PmSchema::build(&SchemaArtifact::parse(raw).unwrap()).unwrap();
        // unknown type as a root child: error (PM fromJSON throws), the
        // subtree is not checked further
        let alien = json!({"type": "noteDoc", "content": [
            {"type": "exotic", "content": []}
        ]});
        let result = schema.check_document(&alien);
        assert!(!result.valid());
        assert!(result.errors.iter().any(|e| e.code == "UNKNOWN_NODE_TYPE"));

        // a legacy type observed in real documents but absent from the
        // artifact: such documents are outside the title-family rule, so
        // the root content match is skipped and the type is a warning
        let legacy = json!({"type": "noteDoc", "content": [
            {"type": "noteSummary", "content": [text("old")]}
        ]});
        let result = schema.check_document(&legacy);
        assert!(result.valid(), "{:?}", result.errors);
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.code == "SCHEMA_NODE_LEGACY")
        );
        assert!(
            result
                .warnings
                .iter()
                .any(|w| w.code == "SCHEMA_FAMILY_UNCOVERED")
        );
    }

    #[test]
    fn empty_text_is_rejected() {
        let schema = PmSchema::build(&artifact()).unwrap();
        let doc = json!({"type": "doc", "content": [
            {"type": "section", "content": [
                {"type": "heading", "content": [text("")]},
                {"type": "para", "content": [text("a")]}
            ]}
        ]});
        let result = schema.check_document(&doc);
        assert!(!result.valid());
    }

    #[test]
    fn title_family_rule_skips_roots_without_note_title() {
        let schema = PmSchema::build(&artifact()).unwrap();
        // top_node "doc" content "section+" — a doc without sections is a
        // content mismatch; the family rule only applies to noteDoc roots.
        let doc = json!({"type": "doc", "content": []});
        let result = schema.check_document(&doc);
        assert!(!result.valid());
        assert!(result.errors.iter().any(|e| e.code == "CONTENT_MISMATCH"));
    }
}
