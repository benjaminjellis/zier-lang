use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use mondc::{
    ast::{Declaration, Expr, Pattern},
    lexer::TokenKind,
    sexpr::SExpr,
};
use tower_lsp::lsp_types::{
    Position, SemanticToken, SemanticTokenModifier, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions,
    SemanticTokensServerCapabilities, WorkDoneProgressOptions,
};

use crate::parse_module;

const TOKEN_TYPE_FUNCTION: u32 = 0;
const TOKEN_TYPE_PARAMETER: u32 = 1;
const TOKEN_TYPE_VARIABLE: u32 = 2;
const TOKEN_TYPE_ENUM_MEMBER: u32 = 3;
const TOKEN_TYPE_TYPE: u32 = 4;

const MOD_DECLARATION: u32 = 1 << 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalKind {
    Parameter,
    Local,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AbsoluteToken {
    start: usize,
    end: usize,
    token_type: u32,
    token_modifiers_bitset: u32,
    priority: u8,
}

#[derive(Default)]
struct TypeClassifier {
    constructors: HashSet<String>,
    record_types: HashSet<String>,
}

impl TypeClassifier {
    fn absorb_type_decl(&mut self, type_decl: &mondc::ast::TypeDecl) {
        match type_decl {
            mondc::ast::TypeDecl::Record { name, .. } => {
                self.record_types.insert(name.clone());
            }
            mondc::ast::TypeDecl::Variant {
                name, constructors, ..
            } => {
                self.record_types.insert(name.clone());
                for (ctor, _) in constructors {
                    self.constructors.insert(ctor.clone());
                }
            }
        }
    }

    fn from_decls_and_imports(
        decls: &[Declaration],
        imported_type_decls: &[mondc::ast::TypeDecl],
    ) -> Self {
        let mut this = Self::default();
        for decl in decls {
            if let Declaration::Type(type_decl) = decl {
                this.absorb_type_decl(type_decl);
            }
        }
        for type_decl in imported_type_decls {
            this.absorb_type_decl(type_decl);
        }
        this
    }

    fn classify_nonlocal_name(&self, name: &str) -> Option<u32> {
        if self.constructors.contains(name) {
            Some(TOKEN_TYPE_ENUM_MEMBER)
        } else if self.record_types.contains(name) {
            Some(TOKEN_TYPE_TYPE)
        } else if is_constructor_name(name) {
            Some(TOKEN_TYPE_ENUM_MEMBER)
        } else {
            None
        }
    }
}

#[derive(Default)]
struct TokenCollector {
    tokens: Vec<AbsoluteToken>,
    token_index_by_span: HashMap<(usize, usize), usize>,
    classifier: TypeClassifier,
}

impl TokenCollector {
    fn new(classifier: TypeClassifier) -> Self {
        Self {
            tokens: Vec::new(),
            token_index_by_span: HashMap::new(),
            classifier,
        }
    }
}

impl TokenCollector {
    fn push(
        &mut self,
        source: &str,
        start: usize,
        end: usize,
        token_type: u32,
        token_modifiers_bitset: u32,
        priority: u8,
    ) {
        if start >= end || end > source.len() {
            return;
        }
        if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return;
        }

        if let Some(index) = self.token_index_by_span.get(&(start, end)).copied() {
            let existing = &mut self.tokens[index];
            if priority >= existing.priority {
                existing.token_type = token_type;
                existing.token_modifiers_bitset = token_modifiers_bitset;
                existing.priority = priority;
            } else {
                existing.token_modifiers_bitset |= token_modifiers_bitset;
            }
            return;
        }

        let index = self.tokens.len();
        self.tokens.push(AbsoluteToken {
            start,
            end,
            token_type,
            token_modifiers_bitset,
            priority,
        });
        self.token_index_by_span.insert((start, end), index);
    }

    fn collect_decl(&mut self, source: &str, decl: &Declaration) {
        match decl {
            Declaration::Expression(expr) => {
                self.collect_expr(source, expr, &HashMap::new());
            }
            Declaration::ExternLet { name_span, .. } => {
                self.push(
                    source,
                    name_span.start,
                    name_span.end,
                    TOKEN_TYPE_FUNCTION,
                    MOD_DECLARATION,
                    30,
                );
            }
            Declaration::Test { body, .. } => {
                self.collect_expr(source, body, &HashMap::new());
            }
            Declaration::Type(_) | Declaration::ExternType { .. } | Declaration::Use { .. } => {}
        }
    }

    fn collect_expr(&mut self, source: &str, expr: &Expr, locals: &HashMap<String, LocalKind>) {
        match expr {
            Expr::Literal(_, _) => {}
            Expr::Debug { value, .. } => {
                self.collect_expr(source, value, locals);
            }
            Expr::Variable(name, span) => {
                self.push_variable_token(source, name, span.start, span.end, locals);
            }
            Expr::List(items, _) => {
                for item in items {
                    self.collect_expr(source, item, locals);
                }
            }
            Expr::LetFunc {
                name,
                args,
                arg_spans,
                name_span,
                value,
                ..
            } => {
                self.push(
                    source,
                    name_span.start,
                    name_span.end,
                    TOKEN_TYPE_FUNCTION,
                    MOD_DECLARATION,
                    30,
                );

                let mut inner = locals.clone();
                inner.insert(name.clone(), LocalKind::Local);
                for (arg, span) in args.iter().zip(arg_spans.iter()) {
                    self.push(
                        source,
                        span.start,
                        span.end,
                        TOKEN_TYPE_PARAMETER,
                        MOD_DECLARATION,
                        40,
                    );
                    inner.insert(arg.clone(), LocalKind::Parameter);
                }
                self.collect_expr(source, value, &inner);
            }
            Expr::LetLocal {
                name,
                name_span,
                value,
                body,
                ..
            } => {
                self.collect_expr(source, value, locals);
                let mut inner = locals.clone();
                self.push(
                    source,
                    name_span.start,
                    name_span.end,
                    TOKEN_TYPE_VARIABLE,
                    MOD_DECLARATION,
                    20,
                );
                inner.insert(name.clone(), LocalKind::Local);
                self.collect_expr(source, body, &inner);
            }
            Expr::If {
                cond, then, els, ..
            } => {
                self.collect_expr(source, cond, locals);
                self.collect_expr(source, then, locals);
                self.collect_expr(source, els, locals);
            }
            Expr::Call { func, args, .. } => {
                match func.as_ref() {
                    Expr::Variable(name, span) => {
                        if let Some(kind) = locals.get(name) {
                            match kind {
                                LocalKind::Parameter => self.push(
                                    source,
                                    span.start,
                                    span.end,
                                    TOKEN_TYPE_PARAMETER,
                                    0,
                                    35,
                                ),
                                LocalKind::Local => self.push(
                                    source,
                                    span.start,
                                    span.end,
                                    TOKEN_TYPE_VARIABLE,
                                    0,
                                    10,
                                ),
                            }
                        } else if let Some(token_type) =
                            self.classifier.classify_nonlocal_name(name)
                        {
                            self.push(source, span.start, span.end, token_type, 0, 24);
                        } else if is_alpha_identifier(name) {
                            self.push(source, span.start, span.end, TOKEN_TYPE_FUNCTION, 0, 25);
                        }
                    }
                    other => self.collect_expr(source, other, locals),
                }

                for arg in args {
                    self.collect_expr(source, arg, locals);
                }
            }
            Expr::Match { targets, arms, .. } => {
                for target in targets {
                    self.collect_expr(source, target, locals);
                }
                for arm in arms {
                    let mut inner = locals.clone();
                    for pat in &arm.patterns {
                        self.bind_pattern_locals(source, pat, &mut inner);
                    }
                    if let Some(guard) = &arm.guard {
                        self.collect_expr(source, guard, &inner);
                    }
                    self.collect_expr(source, &arm.body, &inner);
                }
            }
            Expr::FieldAccess { record, .. } => {
                self.collect_expr(source, record, locals);
            }
            Expr::RecordConstruct { fields, .. } => {
                for (_, value) in fields {
                    self.collect_expr(source, value, locals);
                }
            }
            Expr::RecordUpdate {
                record, updates, ..
            } => {
                self.collect_expr(source, record, locals);
                for (_, value) in updates {
                    self.collect_expr(source, value, locals);
                }
            }
            Expr::Lambda {
                args,
                arg_spans,
                body,
                ..
            } => {
                let mut inner = locals.clone();
                for (arg, span) in args.iter().zip(arg_spans.iter()) {
                    self.push(
                        source,
                        span.start,
                        span.end,
                        TOKEN_TYPE_PARAMETER,
                        MOD_DECLARATION,
                        40,
                    );
                    inner.insert(arg.clone(), LocalKind::Parameter);
                }
                self.collect_expr(source, body, &inner);
            }
            Expr::QualifiedCall { fn_span, args, .. } => {
                self.push(
                    source,
                    fn_span.start,
                    fn_span.end,
                    TOKEN_TYPE_FUNCTION,
                    0,
                    25,
                );
                for arg in args {
                    self.collect_expr(source, arg, locals);
                }
            }
        }
    }

    fn bind_pattern_locals(
        &mut self,
        source: &str,
        pat: &Pattern,
        locals: &mut HashMap<String, LocalKind>,
    ) {
        match pat {
            Pattern::Variable(name, span) => {
                self.push(
                    source,
                    span.start,
                    span.end,
                    TOKEN_TYPE_VARIABLE,
                    MOD_DECLARATION,
                    20,
                );
                locals.insert(name.clone(), LocalKind::Local);
            }
            Pattern::Constructor(_, args, _) | Pattern::Or(args, _) => {
                for arg in args {
                    self.bind_pattern_locals(source, arg, locals);
                }
            }
            Pattern::Cons(head, tail, _) => {
                self.bind_pattern_locals(source, head, locals);
                self.bind_pattern_locals(source, tail, locals);
            }
            Pattern::Record { fields, .. } => {
                for (_, pat, _) in fields {
                    self.bind_pattern_locals(source, pat, locals);
                }
            }
            Pattern::Any(_) | Pattern::Literal(_, _) | Pattern::EmptyList(_) => {}
        }
    }

    fn push_variable_token(
        &mut self,
        source: &str,
        name: &str,
        start: usize,
        end: usize,
        locals: &HashMap<String, LocalKind>,
    ) {
        match locals.get(name) {
            Some(LocalKind::Parameter) => {
                self.push(source, start, end, TOKEN_TYPE_PARAMETER, 0, 35)
            }
            Some(LocalKind::Local) => self.push(source, start, end, TOKEN_TYPE_VARIABLE, 0, 10),
            None if self.classifier.classify_nonlocal_name(name).is_some() => self.push(
                source,
                start,
                end,
                self.classifier
                    .classify_nonlocal_name(name)
                    .unwrap_or(TOKEN_TYPE_VARIABLE),
                0,
                24,
            ),
            None if is_alpha_identifier(name) => {
                self.push(source, start, end, TOKEN_TYPE_VARIABLE, 0, 5)
            }
            None => {}
        }
    }

    fn encode(self, source: &str) -> Vec<SemanticToken> {
        let mut offsets = Vec::with_capacity(self.tokens.len() * 2);
        for token in &self.tokens {
            offsets.push(token.start);
            offsets.push(token.end);
        }
        let positions = positions_for_offsets(source, &offsets);

        let mut positioned = self
            .tokens
            .into_iter()
            .filter_map(|token| {
                let start = *positions.get(&token.start)?;
                let end = *positions.get(&token.end)?;
                if start.line != end.line || end.character <= start.character {
                    return None;
                }
                Some((
                    start.line,
                    start.character,
                    end.character - start.character,
                    token.token_type,
                    token.token_modifiers_bitset,
                ))
            })
            .collect::<Vec<_>>();

        positioned.sort_by_key(|token| (token.0, token.1, token.3, token.4));

        let mut data = Vec::with_capacity(positioned.len());
        let mut prev_line = 0u32;
        let mut prev_start = 0u32;
        for (line, start, length, token_type, modifiers) in positioned {
            let delta_line = line.saturating_sub(prev_line);
            let delta_start = if delta_line == 0 {
                start.saturating_sub(prev_start)
            } else {
                start
            };
            data.push(SemanticToken {
                delta_line,
                delta_start,
                length,
                token_type,
                token_modifiers_bitset: modifiers,
            });
            prev_line = line;
            prev_start = start;
        }
        data
    }
}

fn positions_for_offsets(source: &str, offsets: &[usize]) -> HashMap<usize, Position> {
    let mut targets = offsets.to_vec();
    targets.sort_unstable();
    targets.dedup();

    let mut positions = HashMap::with_capacity(targets.len());
    if targets.is_empty() {
        return positions;
    }

    let mut target_index = 0usize;
    let mut line = 0u32;
    let mut col = 0u32;
    let mut byte = 0usize;

    while target_index < targets.len() {
        let target = targets[target_index];
        while byte < target {
            let ch = source[byte..]
                .chars()
                .next()
                .expect("target offset always points into source");
            byte += ch.len_utf8();
            if ch == '\n' {
                line += 1;
                col = 0;
            } else {
                col += ch.len_utf16() as u32;
            }
        }
        positions.insert(target, Position::new(line, col));
        target_index += 1;
    }

    positions
}

fn is_constructor_name(name: &str) -> bool {
    name.chars()
        .next()
        .map(|c| c.is_ascii_uppercase())
        .unwrap_or(false)
}

fn collect_record_construct_head_tokens(
    source: &str,
    sexpr: &SExpr,
    collector: &mut TokenCollector,
) {
    match sexpr {
        SExpr::Round(items, _) => {
            if let [SExpr::Atom(head), SExpr::Atom(second), ..] = items.as_slice()
                && matches!(head.kind, TokenKind::Ident)
                && matches!(second.kind, TokenKind::NamedField(_))
            {
                let text = &source[head.span.clone()];
                if text
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_uppercase())
                    .unwrap_or(false)
                {
                    collector.push(
                        source,
                        head.span.start,
                        head.span.end,
                        TOKEN_TYPE_TYPE,
                        0,
                        50,
                    );
                }
            }

            for item in items {
                collect_record_construct_head_tokens(source, item, collector);
            }
        }
        SExpr::Square(items, _) | SExpr::Curly(items, _) => {
            for item in items {
                collect_record_construct_head_tokens(source, item, collector);
            }
        }
        SExpr::Atom(_) => {}
    }
}

fn is_alpha_identifier(name: &str) -> bool {
    name.chars()
        .next()
        .map(|c| c.is_ascii_alphabetic() || c == '_')
        .unwrap_or(false)
}

pub(crate) fn semantic_tokens_capabilities() -> SemanticTokensServerCapabilities {
    SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
        work_done_progress_options: WorkDoneProgressOptions::default(),
        legend: SemanticTokensLegend {
            token_types: vec![
                SemanticTokenType::FUNCTION,
                SemanticTokenType::PARAMETER,
                SemanticTokenType::VARIABLE,
                SemanticTokenType::ENUM_MEMBER,
                SemanticTokenType::TYPE,
            ],
            token_modifiers: vec![SemanticTokenModifier::DECLARATION],
        },
        range: None,
        full: Some(SemanticTokensFullOptions::Bool(true)),
    })
}

pub(crate) fn compute_semantic_tokens_full(
    source_path: &Path,
    source: &str,
    imported_type_decls: &[mondc::ast::TypeDecl],
) -> std::result::Result<SemanticTokens, String> {
    let (sexprs, decls) = parse_module(source_path, source)?;
    let classifier = TypeClassifier::from_decls_and_imports(&decls, imported_type_decls);
    let mut collector = TokenCollector::new(classifier);
    for decl in &decls {
        collector.collect_decl(source, decl);
    }
    for sexpr in &sexprs {
        collect_record_construct_head_tokens(source, sexpr, &mut collector);
    }
    Ok(SemanticTokens {
        result_id: None,
        data: collector.encode(source),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offset_to_position;

    fn decode(tokens: &[SemanticToken]) -> Vec<(u32, u32, u32, u32, u32)> {
        let mut out = Vec::with_capacity(tokens.len());
        let mut line = 0u32;
        let mut start = 0u32;
        for token in tokens {
            line += token.delta_line;
            start = if token.delta_line == 0 {
                start + token.delta_start
            } else {
                token.delta_start
            };
            out.push((
                line,
                start,
                token.length,
                token.token_type,
                token.token_modifiers_bitset,
            ));
        }
        out
    }

    #[test]
    fn tags_parameter_references_and_constructor_calls() {
        let src = "(pub let with_selctor {value selector}\n\
                     (match value\n\
                       (Continue payload) ~> (Continue\n\
                         (ContinuePayload :state (:state payload) :selector (Some selector)))\n\
                       (Stop _) ~> value))";
        let tokens = compute_semantic_tokens_full(Path::new("src/main.mond"), src, &[])
            .expect("semantic tokens")
            .data;
        let decoded = decode(&tokens);

        let value_decl_start = src.find("value").expect("value declaration");
        let selector_decl_start = src.find("selector").expect("selector declaration");
        let some_start = src.find("Some").expect("Some constructor");
        let selector_ref_start = src.rfind("selector").expect("selector reference");
        let value_ref_start = src.rfind("value").expect("value reference");

        let value_decl_pos = offset_to_position(src, value_decl_start);
        let selector_decl_pos = offset_to_position(src, selector_decl_start);
        let some_pos = offset_to_position(src, some_start);
        let selector_ref_pos = offset_to_position(src, selector_ref_start);
        let value_ref_pos = offset_to_position(src, value_ref_start);

        assert!(decoded.iter().any(|token| {
            token.0 == value_decl_pos.line
                && token.1 == value_decl_pos.character
                && token.3 == TOKEN_TYPE_PARAMETER
                && token.4 == MOD_DECLARATION
        }));
        assert!(decoded.iter().any(|token| {
            token.0 == selector_decl_pos.line
                && token.1 == selector_decl_pos.character
                && token.3 == TOKEN_TYPE_PARAMETER
                && token.4 == MOD_DECLARATION
        }));
        assert!(decoded.iter().any(|token| {
            token.0 == some_pos.line
                && token.1 == some_pos.character
                && token.3 == TOKEN_TYPE_ENUM_MEMBER
        }));
        assert!(decoded.iter().any(|token| {
            token.0 == selector_ref_pos.line
                && token.1 == selector_ref_pos.character
                && token.3 == TOKEN_TYPE_PARAMETER
                && token.4 == 0
        }));
        assert!(decoded.iter().any(|token| {
            token.0 == value_ref_pos.line
                && token.1 == value_ref_pos.character
                && token.3 == TOKEN_TYPE_PARAMETER
                && token.4 == 0
        }));
    }

    #[test]
    fn tags_record_construct_head_as_type() {
        let src = "(pub let continue {state}\n\
                     (Continue (ContinuePayload :state state :selector None)))";
        let tokens = compute_semantic_tokens_full(Path::new("src/main.mond"), src, &[])
            .expect("semantic tokens")
            .data;
        let decoded = decode(&tokens);
        let head_start = src
            .find("ContinuePayload")
            .expect("record constructor head");
        let head_pos = offset_to_position(src, head_start);
        assert!(decoded.iter().any(|token| {
            token.0 == head_pos.line && token.1 == head_pos.character && token.3 == TOKEN_TYPE_TYPE
        }));
    }
}
